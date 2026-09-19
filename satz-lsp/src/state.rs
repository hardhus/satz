use std::collections::HashMap;
use std::path::{Path, PathBuf};

use ropey::Rope;
use satz_core::{Index, VaultConfig, walk_vault};
use tower_lsp_server::ls_types::TextDocumentContentChangeEvent;

use std::time::Instant;
use tokio::task::JoinHandle;

/// In-memory representation of an open text document with a Rope buffer.
pub struct OpenDocument {
    pub uri: String,
    pub path: PathBuf,
    pub rope: Rope,
    pub version: i32,
    pub first_change_at: Option<Instant>,
    pub pending_task: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for OpenDocument {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenDocument")
            .field("uri", &self.uri)
            .field("path", &self.path)
            .field("version", &self.version)
            .field("first_change_at", &self.first_change_at)
            .finish()
    }
}

impl OpenDocument {
    pub fn new(
        uri: impl Into<String>,
        path: PathBuf,
        content: impl Into<String>,
        version: i32,
    ) -> Self {
        let rope = Rope::from_str(&content.into());
        Self {
            uri: uri.into(),
            path,
            rope,
            version,
            first_change_at: None,
            pending_task: None,
        }
    }

    /// Applies a `didChange` to the buffer. A change carrying an older version than the buffer
    /// already has is stale (LSP versions only increase) and must not be applied; returns whether
    /// it was.
    pub fn apply_change_events(
        &mut self,
        version: i32,
        changes: Vec<TextDocumentContentChangeEvent>,
    ) -> bool {
        if version < self.version {
            tracing::warn!(uri = %self.uri, version, current = self.version, "ignoring stale didChange");
            return false;
        }
        crate::sync::apply_changes_to_rope(&mut self.rope, changes);
        self.version = version;
        true
    }
}

/// Simple (non-LRU) cache mapping a document's content hash to its already-computed formatted
/// text, used by `satz.formatWorkspace` to skip reformatting files whose content hasn't changed
/// since the last workspace-format call. Once at capacity, new distinct hashes are silently not
/// cached — existing entries keep serving hits rather than anything being evicted.
#[derive(Debug, Clone)]
pub struct FormatCache {
    entries: HashMap<u64, String>,
    capacity: usize,
}

impl FormatCache {
    pub fn new(capacity: usize) -> Self {
        Self {
            entries: HashMap::new(),
            capacity,
        }
    }

    pub fn get(&self, hash: u64) -> Option<&str> {
        self.entries.get(&hash).map(String::as_str)
    }

    pub fn insert(&mut self, hash: u64, formatted: String) {
        if self.entries.len() >= self.capacity && !self.entries.contains_key(&hash) {
            return;
        }
        self.entries.insert(hash, formatted);
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl Default for FormatCache {
    fn default() -> Self {
        Self::new(satz_core::config::LspConfig::default().format_cache_capacity)
    }
}

/// Global server state.
#[derive(Debug, Default)]
pub struct SatzState {
    /// Vault root path (from LSP initialize params)
    pub vault_root: Option<PathBuf>,

    /// In-memory vault index
    pub index: Index,

    /// Currently open documents tracked by the client
    pub open_docs: HashMap<String, OpenDocument>,

    /// Vault configuration (.satz.toml or default)
    pub config: VaultConfig,

    /// Why `.satz.toml` could not be used, if it exists but is unreadable or invalid. While this
    /// is `Some`, formatting is turned off (`formatting_allowed`): formatting with settings the
    /// user did not choose would silently rewrite their notes.
    pub config_error: Option<String>,

    /// Whether the client supports pull diagnostics
    pub client_supports_pull_diagnostics: bool,

    /// Flag indicating that open document identity keys changed and peers need diagnostic refresh
    pub peers_dirty: bool,

    /// `satz.formatWorkspace` result cache — see `FormatCache`.
    pub format_cache: FormatCache,

    /// Whether the initial vault-wide `walk_vault` scan has finished. Starts `false` (the
    /// `Default` state used until `initialize_index` completes); diagnostics computed before
    /// this is `true` would only see whichever documents happened to already be open, so any
    /// link to a not-yet-indexed peer looks spuriously broken. Handlers should return empty
    /// diagnostics rather than that false positive — the `workspace/diagnostic/refresh` push
    /// sent once indexing finishes will make the client re-pull the real results.
    pub indexing_complete: bool,
}

pub fn identity_keys(d: &satz_core::Document) -> std::collections::HashSet<String> {
    d.identity_keys()
}

/// Everything about a document that OTHER open documents' diagnostics depend on: the keys they can
/// link by (title, aliases, stem), which notes it links to (their orphan status), and its headings
/// and block ids (their anchor-missing warnings). When it changes, peers must be refreshed.
pub fn peer_signature(d: &satz_core::Document) -> std::collections::HashSet<String> {
    use satz_core::model::LinkKind;
    let mut signature: std::collections::HashSet<String> = d
        .identity_keys()
        .into_iter()
        .map(|k| format!("key:{k}"))
        .collect();
    for link in &d.links {
        if matches!(
            link.kind,
            LinkKind::WikiLink | LinkKind::Embed | LinkKind::Markdown
        ) && !link.target_doc.is_empty()
            && !satz_core::model::link::is_external_target(&link.target_doc)
        {
            signature.insert(format!(
                "link:{}",
                satz_core::slug::fold_key(&link.target_doc)
            ));
        }
    }
    signature.extend(d.headings.iter().map(|h| format!("heading:{}", h.slug)));
    signature.extend(d.blocks.iter().map(|b| format!("block:{}", b.id)));
    signature
}

/// The text shown to the user (as a `window/showMessage` warning) when `.satz.toml` can't be
/// used. `error` already names the file and, for TOML errors, the line and column; `fallback`
/// says which settings are in effect meanwhile ("default" at startup, "the previous" on reload).
pub fn config_error_message(error: &str, fallback: &str) -> String {
    format!(
        "satz: {error}\nUsing {fallback} settings. Formatting is turned off until .satz.toml is valid."
    )
}

impl SatzState {
    /// Resolves a link of `doc` the way every handler must: folder-relative Markdown paths, relative
    /// daily aliases from the config, and the heading/block anchor check.
    pub fn resolve<'a>(
        &'a self,
        link: &satz_core::Link,
        doc: &'a satz_core::Document,
    ) -> satz_core::LinkResolution<'a> {
        self.index
            .resolve_link_full_with_config(link, Some(doc), Some(&self.config))
    }

    /// Whether formatting requests may be served: the formatter is enabled in the config AND the
    /// config file is usable (see `config_error`).
    pub fn formatting_allowed(&self) -> bool {
        self.config.formatter.enabled && self.config_error.is_none()
    }

    /// Discovers and indexes all `.md` files in the vault.
    ///
    /// An unusable `.satz.toml` does not stop indexing: the default configuration is used and
    /// the reason is recorded in `config_error` for the caller to show to the user.
    pub fn initialize_index(vault_root: PathBuf) -> anyhow::Result<Self> {
        let (config, config_error) = match VaultConfig::load(&vault_root) {
            Ok(config) => (config, None),
            Err(e) => {
                tracing::warn!("initialize_index: {e}; using default settings");
                (VaultConfig::default(), Some(e.to_string()))
            }
        };
        tracing::debug!(
            config_error = config_error.is_some(),
            "initialize_index: loaded .satz.toml (or default)"
        );

        let docs = walk_vault(&vault_root)?;
        tracing::debug!(
            doc_count = docs.len(),
            "initialize_index: walk_vault returned docs"
        );
        let index = Index::build(docs);
        let format_cache = FormatCache::new(config.lsp.format_cache_capacity);

        Ok(Self {
            vault_root: Some(vault_root),
            index,
            open_docs: HashMap::new(),
            config,
            config_error,
            client_supports_pull_diagnostics: false,
            peers_dirty: false,
            format_cache,
            indexing_complete: true,
        })
    }

    /// Whether `path` is the file of a currently open document.
    ///
    /// Compared by vault-relative, case-folded, `/`-separated path: the file system watcher and
    /// the editor can spell the same file differently (drive letter case, `\\?\` prefix, letter
    /// case on Windows/macOS), and mistaking an open, possibly unsaved document for a closed one
    /// would replace its buffer contents in the index with the disk version.
    pub fn is_open_path(&self, path: &Path) -> bool {
        let root = self.vault_root.as_deref();
        let key = |p: &Path| {
            satz_core::slug::fold_key(
                &Self::get_rel_path(p, root)
                    .to_string_lossy()
                    .replace('\\', "/"),
            )
        };
        let wanted = key(path);
        self.open_docs.values().any(|d| key(&d.path) == wanted)
    }

    pub fn get_rel_path(path: &Path, root: Option<&Path>) -> PathBuf {
        let Some(root) = root else {
            return path.to_path_buf();
        };
        if let Ok(rel) = path.strip_prefix(root) {
            return rel.to_path_buf();
        }

        let mut path_comps = path.components();
        for rc in root.components() {
            let mut clone_comps = path_comps.clone();
            match clone_comps.next() {
                Some(pc)
                    if satz_core::slug::fold_key(&pc.as_os_str().to_string_lossy())
                        == satz_core::slug::fold_key(&rc.as_os_str().to_string_lossy()) =>
                {
                    path_comps = clone_comps;
                }
                _ => return path.to_path_buf(),
            }
        }
        path_comps.as_path().to_path_buf()
    }

    /// Handles opening a new document.
    pub fn open_document(&mut self, uri: &str, content: &str, path: &Path, version: i32) {
        let rel_path = Self::get_rel_path(path, self.vault_root.as_deref());
        let rel_path_str = rel_path.to_string_lossy().replace('\\', "/");
        let doc_id = satz_core::DocId::new(&rel_path_str);
        tracing::debug!(%uri, ?doc_id, version, "open_document");

        let old_keys = self
            .index
            .get_doc(&doc_id)
            .map(peer_signature)
            .unwrap_or_default();
        let new_doc = satz_core::parse_document(content, &rel_path);
        let new_keys = peer_signature(&new_doc);

        // A document not yet in the index has empty `old_keys`, which correctly counts as a
        // change: a note that was just created (e.g. via "Create note") can now resolve links
        // that other open documents currently show as broken.
        if old_keys != new_keys {
            self.peers_dirty = true;
        }

        self.open_docs.insert(
            uri.to_string(),
            OpenDocument::new(uri, path.to_path_buf(), content, version),
        );

        self.index.replace_doc(new_doc);
    }

    /// Re-parses the in-memory rope content of an open document and updates index.
    pub fn reparse_open_document(&mut self, uri: &str) {
        let Some(open_doc) = self.open_docs.get_mut(uri) else {
            return;
        };
        open_doc.first_change_at = None;
        let path = open_doc.path.clone();
        let content = open_doc.rope.to_string();

        let rel_path = Self::get_rel_path(&path, self.vault_root.as_deref());
        let rel_path_str = rel_path.to_string_lossy().replace('\\', "/");
        let doc_id = satz_core::DocId::new(&rel_path_str);
        tracing::trace!(%uri, ?doc_id, "reparse_open_document");

        let old_keys = self
            .index
            .get_doc(&doc_id)
            .map(peer_signature)
            .unwrap_or_default();
        let new_doc = satz_core::parse_document(&content, &rel_path);
        let new_keys = peer_signature(&new_doc);

        if old_keys != new_keys {
            self.peers_dirty = true;
        }

        self.index.replace_doc(new_doc);
    }

    /// Closes and untracks an open document, aborting any background debounce tasks.
    ///
    /// The index held the buffer's text while the document was open; once closed, what counts is
    /// the file, so the index is put back to the disk version (or the entry dropped if there is no
    /// file). Unsaved edits that were thrown away must not keep shaping links and diagnostics.
    pub fn close_document(&mut self, uri: &str) {
        tracing::debug!(%uri, "close_document");
        let Some(mut doc) = self.open_docs.remove(uri) else {
            return;
        };
        if let Some(task) = doc.pending_task.take() {
            task.abort();
        }

        let rel_path = Self::get_rel_path(&doc.path, self.vault_root.as_deref());
        let doc_id = satz_core::DocId::new(rel_path.to_string_lossy().replace('\\', "/"));
        let old_signature = self
            .index
            .get_doc(&doc_id)
            .map(peer_signature)
            .unwrap_or_default();
        let new_signature = match std::fs::read_to_string(&doc.path) {
            Ok(content) => {
                let disk_doc = satz_core::parse_document(&content, &rel_path);
                let signature = peer_signature(&disk_doc);
                self.index.replace_doc(disk_doc);
                signature
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                self.index.remove_doc(&doc_id);
                Default::default()
            }
            Err(e) => {
                // Unreadable right now: keep what the index has rather than guess.
                tracing::warn!(path = ?doc.path, "close_document: cannot read file: {e}");
                return;
            }
        };
        if old_signature != new_signature {
            self.peers_dirty = true;
        }
    }
}

#[cfg(test)]
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use super::*;

    #[test]
    fn get_rel_path_with_turkish_vault_root() {
        let root = Path::new("/notlar/İş");
        let path = Path::new("/notlar/İş/projeler/proje1.md");
        let rel = SatzState::get_rel_path(path, Some(root));
        assert_eq!(rel, PathBuf::from("projeler/proje1.md"));

        // Case-insensitive test on Windows path format
        let root_win = Path::new("C:\\Notlar\\İş");
        let path_win = Path::new("c:\\notlar\\iş\\projeler\\proje1.md");
        let rel_win = SatzState::get_rel_path(path_win, Some(root_win));
        assert_eq!(rel_win, PathBuf::from("projeler\\proje1.md"));
    }

    #[test]
    fn test_identity_keys_change_detected() {
        let doc1 = satz_core::parse_document(
            "---\ntitle: Eski Başlık\naliases: [alias1]\n---\n# Content",
            Path::new("doc.md"),
        );
        let doc2 = satz_core::parse_document(
            "---\ntitle: Yeni Başlık\naliases: [alias1]\n---\n# Content",
            Path::new("doc.md"),
        );

        let keys1 = identity_keys(&doc1);
        let keys2 = identity_keys(&doc2);

        assert_ne!(keys1, keys2);
        assert!(keys1.contains("eski baslik") || keys1.contains("eski başlık"));
        assert!(keys2.contains("yeni baslik") || keys2.contains("yeni başlık"));
    }

    fn state_with_broken_link_to_new() -> SatzState {
        let a = satz_core::parse_document("# A\n\nSee [[new]].", Path::new("a.md"));
        SatzState {
            index: Index::build(vec![a]),
            indexing_complete: true,
            ..Default::default()
        }
    }

    #[test]
    fn open_new_note_marks_peers_dirty() {
        // A note that isn't in the index yet (e.g. just created via "Create note") can resolve
        // links other open documents show as broken, so their diagnostics must be refreshed.
        let mut state = state_with_broken_link_to_new();
        assert!(!state.peers_dirty);
        state.open_document("file:///new.md", "# New", Path::new("new.md"), 1);
        assert!(state.peers_dirty);
    }

    #[test]
    fn reopening_an_unchanged_indexed_note_does_not_mark_peers_dirty() {
        let mut state = state_with_broken_link_to_new();
        state.open_document("file:///a.md", "# A\n\nSee [[new]].", Path::new("a.md"), 1);
        assert!(!state.peers_dirty);
    }

    #[test]
    fn create_note_flow_clears_broken_link_and_orphan_diagnostics() {
        use crate::handlers::diagnostics::compute_diagnostics;
        let mut state = state_with_broken_link_to_new();

        let a_before = state.index.get_doc(&satz_core::DocId::new("a.md")).unwrap();
        let codes = |diags: &[tower_lsp_server::ls_types::Diagnostic]| -> Vec<String> {
            diags
                .iter()
                .filter_map(|d| match &d.code {
                    Some(tower_lsp_server::ls_types::NumberOrString::String(s)) => Some(s.clone()),
                    _ => None,
                })
                .collect()
        };
        assert!(
            codes(&compute_diagnostics(a_before, &state.index, &state.config))
                .contains(&"broken-link".to_string())
        );

        state.open_document("file:///new.md", "# New\n\nBody.", Path::new("new.md"), 1);

        let new_doc = state
            .index
            .get_doc(&satz_core::DocId::new("new.md"))
            .unwrap();
        let new_codes = codes(&compute_diagnostics(new_doc, &state.index, &state.config));
        assert!(
            !new_codes.contains(&"orphan-note".to_string()),
            "freshly created note wrongly flagged orphan: {new_codes:?}"
        );
        let a_after = state.index.get_doc(&satz_core::DocId::new("a.md")).unwrap();
        let a_codes = codes(&compute_diagnostics(a_after, &state.index, &state.config));
        assert!(
            !a_codes.contains(&"broken-link".to_string()),
            "link to the new note still reported broken: {a_codes:?}"
        );
    }

    #[test]
    fn test_debounce_and_max_wait_delay_calculation() {
        let debounce = std::time::Duration::from_millis(200);
        let max_wait = std::time::Duration::from_millis(500);

        // At t = 0
        let elapsed_0 = std::time::Duration::from_millis(0);
        let delay_0 = debounce.min(max_wait.saturating_sub(elapsed_0));
        assert_eq!(delay_0, std::time::Duration::from_millis(200));

        // At t = 100
        let elapsed_100 = std::time::Duration::from_millis(100);
        let delay_100 = debounce.min(max_wait.saturating_sub(elapsed_100));
        assert_eq!(delay_100, std::time::Duration::from_millis(200));

        // At t = 400 (max_wait capping)
        let elapsed_400 = std::time::Duration::from_millis(400);
        let delay_400 = debounce.min(max_wait.saturating_sub(elapsed_400));
        assert_eq!(delay_400, std::time::Duration::from_millis(100));

        // At t = 550 (max_wait exceeded)
        let elapsed_550 = std::time::Duration::from_millis(550);
        let delay_550 = debounce.min(max_wait.saturating_sub(elapsed_550));
        assert_eq!(delay_550, std::time::Duration::from_millis(0));
    }

    #[tokio::test]
    async fn test_async_debounced_task_execution() {
        use std::sync::Arc;
        use tokio::sync::RwLock;

        let state = Arc::new(RwLock::new(SatzState::default()));
        let uri = "file:///test.md";
        let path = Path::new("test.md");

        {
            let mut s = state.write().await;
            s.open_document(uri, "# Initial", path, 1);
        }

        // Send 3 rapid changes
        for i in 2..=4 {
            let mut s = state.write().await;
            if let Some(doc) = s.open_docs.get_mut(uri) {
                if let Some(prev) = doc.pending_task.take() {
                    prev.abort();
                }
                doc.rope = ropey::Rope::from_str(&format!("# Version {}", i));
                doc.version = i;

                let state_clone = state.clone();
                let uri_clone = uri.to_string();
                let handle = tokio::task::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                    let mut st = state_clone.write().await;
                    st.reparse_open_document(&uri_clone);
                });
                doc.pending_task = Some(handle);
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }

        // Wait for final debounced task to complete
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let s = state.read().await;
        let doc_id = satz_core::DocId::new("test.md");
        let parsed = s.index.get_doc(&doc_id).expect("Doc should exist in index");
        assert_eq!(parsed.title, "Version 4");
    }

    /// A unique, self-cleaning vault directory containing one note.
    struct TempVault(PathBuf);
    impl TempVault {
        fn new(tag: &str) -> Self {
            use std::sync::atomic::{AtomicUsize, Ordering};
            static N: AtomicUsize = AtomicUsize::new(0);
            let dir = std::env::temp_dir().join(format!(
                "satz_lsp_{tag}_{}_{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("a.md"), "# A\n\nText.\n").unwrap();
            Self(dir)
        }
        fn config(&self, content: &str) {
            std::fs::write(self.0.join(".satz.toml"), content).unwrap();
        }
    }
    impl Drop for TempVault {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn initialize_index_reports_an_invalid_config_but_still_indexes() {
        let v = TempVault::new("badcfg");
        v.config("[formatter\nline_width = 1\n");

        let state = SatzState::initialize_index(v.0.clone()).unwrap();

        let error = state
            .config_error
            .as_deref()
            .expect("error must be recorded");
        assert!(error.contains(".satz.toml"), "{error}");
        assert!(error.contains("line 1"), "{error}");
        assert_eq!(state.config, VaultConfig::default());
        assert_eq!(state.index.doc_count(), 1, "notes are still indexed");
        assert!(!state.formatting_allowed());
    }

    #[test]
    fn initialize_index_reports_unknown_keys_and_wrong_types() {
        for (label, content, expect) in [
            (
                "unknown key",
                "[formatter.wrap]\nenabled = true\n",
                "enabled",
            ),
            (
                "wrong type",
                "[hover]\npreview_lines = \"many\"\n",
                "preview_lines",
            ),
            (
                "bad daily format",
                "[daily_note]\nformat = \"%Q\"\n",
                "daily_note.format",
            ),
        ] {
            let v = TempVault::new("badkey");
            v.config(content);
            let state = SatzState::initialize_index(v.0.clone()).unwrap();
            let error = state.config_error.as_deref().unwrap_or_else(|| {
                panic!("{label}: config error must be recorded");
            });
            assert!(error.contains(expect), "{label}: {error}");
            assert!(!state.formatting_allowed(), "{label}");
        }
    }

    #[test]
    fn initialize_index_applies_a_valid_config_without_error() {
        let v = TempVault::new("okcfg");
        v.config("[hover]\npreview_lines = 3\n");

        let state = SatzState::initialize_index(v.0.clone()).unwrap();

        assert_eq!(state.config_error, None);
        assert_eq!(state.config.hover.preview_lines, 3);
        assert!(state.formatting_allowed());
    }

    #[test]
    fn initialize_index_without_a_config_file_uses_defaults_without_error() {
        let v = TempVault::new("nocfg");

        let state = SatzState::initialize_index(v.0.clone()).unwrap();

        assert_eq!(state.config_error, None);
        assert_eq!(state.config, VaultConfig::default());
        assert!(state.formatting_allowed());
    }

    #[test]
    fn formatting_allowed_truth_table() {
        for (enabled, error, allowed) in [
            (true, None, true),
            (false, None, false),
            (true, Some("broken"), false),
            (false, Some("broken"), false),
        ] {
            let mut state = SatzState::default();
            state.config.formatter.enabled = enabled;
            state.config_error = error.map(str::to_string);
            assert_eq!(
                state.formatting_allowed(),
                allowed,
                "enabled={enabled} error={error:?}"
            );
        }
    }

    #[test]
    fn config_error_message_names_the_problem_the_fallback_and_the_consequence() {
        let msg = config_error_message("invalid /v/.satz.toml: line 3", "default");
        assert!(msg.contains("invalid /v/.satz.toml: line 3"), "{msg}");
        assert!(msg.contains("default settings"), "{msg}");
        assert!(msg.to_lowercase().contains("formatting"), "{msg}");
        let msg = config_error_message("x", "the previous");
        assert!(msg.contains("the previous settings"), "{msg}");
    }

    // ---- peers_dirty follows everything another open document's diagnostics depend on ----

    /// An open `a.md` (`before`), settled, then re-parsed as `after`; returns `peers_dirty`.
    fn dirty_after_edit(before: &str, after: &str) -> bool {
        let mut state = SatzState::default();
        state.open_document("file:///a.md", before, Path::new("a.md"), 1);
        state.peers_dirty = false;
        state.open_document("file:///a.md", after, Path::new("a.md"), 2);
        state.peers_dirty
    }

    #[test]
    fn a_new_or_removed_link_marks_peers_dirty() {
        // The target's orphan status depends on who links to it.
        assert!(dirty_after_edit("# A\n", "# A\n\nSee [[b]].\n"));
        assert!(dirty_after_edit("# A\n\nSee [[b]].\n", "# A\n"));
        assert!(dirty_after_edit(
            "# A\n\nSee [[b]].\n",
            "# A\n\nSee [[c]].\n"
        ));
        assert!(dirty_after_edit("# A\n", "# A\n\n![[img]]\n"));
        assert!(dirty_after_edit("# A\n", "# A\n\n[t](b.md)\n"));
    }

    #[test]
    fn headings_and_block_anchors_mark_peers_dirty() {
        // Anchor diagnostics elsewhere (`[[a#Section]]`) depend on them.
        assert!(dirty_after_edit("# A\n", "# A\n\n## Section\n"));
        assert!(dirty_after_edit("# A\n\n## One\n", "# A\n\n## Two\n"));
        assert!(dirty_after_edit("# A\n\ntext\n", "# A\n\ntext ^blk\n"));
    }

    #[test]
    fn ordinary_edits_do_not_mark_peers_dirty() {
        assert!(!dirty_after_edit(
            "# A\n\nsome text\n",
            "# A\n\nsome more text\n"
        ));
        assert!(!dirty_after_edit(
            "# A\n\nSee [[b]].\n",
            "# A\n\nSee [[b]] and words.\n"
        ));
        // Display text and link kind spelling are irrelevant to peers.
        assert!(!dirty_after_edit(
            "# A\n\nSee [[b]].\n",
            "# A\n\nSee [[b|shown]].\n"
        ));
        // External links never involve another note.
        assert!(!dirty_after_edit(
            "# A\n",
            "# A\n\n[m](mailto:a@b.c) [w](https://a.b)\n"
        ));
        // Same content parsed again.
        assert!(!dirty_after_edit(
            "# A\n\nSee [[b]].\n",
            "# A\n\nSee [[b]].\n"
        ));
    }

    #[test]
    fn a_link_added_to_one_open_note_removes_the_orphan_hint_of_another() {
        use crate::handlers::diagnostics::compute_diagnostics;
        let mut state = SatzState::default();
        state.open_document("file:///a.md", "# A\n", Path::new("a.md"), 1);
        state.open_document("file:///b.md", "# B\n", Path::new("b.md"), 1);
        let orphan = |state: &SatzState| {
            let b = state.index.get_doc(&satz_core::DocId::new("b.md")).unwrap();
            compute_diagnostics(b, &state.index, &state.config)
                .iter()
                .any(|d| {
                    d.code
                        == Some(tower_lsp_server::ls_types::NumberOrString::String(
                            "orphan-note".into(),
                        ))
                })
        };
        assert!(orphan(&state));
        state.peers_dirty = false;
        state.open_document("file:///a.md", "# A\n\nSee [[b]].\n", Path::new("a.md"), 2);
        assert!(state.peers_dirty, "the peers must be told to refresh");
        assert!(!orphan(&state));
    }

    // ---- closing a document returns the index to what is on disk ----

    /// A fresh, empty directory under the system temp dir (removed by the caller).
    pub(crate) fn temp_dir(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "satz-test-{}-{}-{}",
            std::process::id(),
            tag,
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn state_for(dir: &Path) -> SatzState {
        let mut state = SatzState::default();
        state.vault_root = Some(dir.to_path_buf());
        state.indexing_complete = true;
        state
    }

    fn link_targets(state: &SatzState, id: &str) -> Vec<String> {
        state
            .index
            .get_doc(&satz_core::DocId::new(id))
            .map(|d| d.links.iter().map(|l| l.target_doc.clone()).collect())
            .unwrap_or_default()
    }

    #[test]
    fn closing_an_unsaved_buffer_puts_the_disk_version_back_in_the_index() {
        let dir = temp_dir("close-dirty");
        let path = dir.join("a.md");
        std::fs::write(&path, "# A\n\ndisk [[x]]\n").unwrap();
        let mut state = state_for(&dir);
        state.open_document("file:///a.md", "# A\n\nunsaved [[y]]\n", &path, 1);
        assert_eq!(link_targets(&state, "a.md"), vec!["y"]);

        state.close_document("file:///a.md");

        assert!(state.open_docs.is_empty());
        assert_eq!(link_targets(&state, "a.md"), vec!["x"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn closing_a_buffer_whose_file_does_not_exist_removes_it_from_the_index() {
        let dir = temp_dir("close-missing");
        let path = dir.join("never-saved.md");
        let mut state = state_for(&dir);
        state.open_document("file:///n.md", "# N\n", &path, 1);
        assert!(
            state
                .index
                .get_doc(&satz_core::DocId::new("never-saved.md"))
                .is_some()
        );

        state.close_document("file:///n.md");

        assert!(
            state
                .index
                .get_doc(&satz_core::DocId::new("never-saved.md"))
                .is_none()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn closing_marks_peers_dirty_only_when_what_they_see_changed() {
        let dir = temp_dir("close-peers");
        let path = dir.join("a.md");
        std::fs::write(&path, "# A\n\nSee [[b]].\n").unwrap();

        // Buffer identical to disk: nothing changes for anyone.
        let mut state = state_for(&dir);
        state.open_document("file:///a.md", "# A\n\nSee [[b]].\n", &path, 1);
        state.peers_dirty = false;
        state.close_document("file:///a.md");
        assert!(!state.peers_dirty);

        // Unsaved edit changed a link and a heading: closing reverts them, peers must refresh.
        let mut state = state_for(&dir);
        state.open_document("file:///a.md", "# A\n\n## New\n\nSee [[c]].\n", &path, 1);
        state.peers_dirty = false;
        state.close_document("file:///a.md");
        assert!(state.peers_dirty);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn closing_twice_or_closing_an_unknown_uri_is_harmless() {
        let dir = temp_dir("close-twice");
        let path = dir.join("a.md");
        std::fs::write(&path, "# A\n").unwrap();
        let mut state = state_for(&dir);
        state.open_document("file:///a.md", "# A\n", &path, 1);
        state.close_document("file:///a.md");
        state.close_document("file:///a.md");
        state.close_document("file:///never-opened.md");
        assert!(
            state
                .index
                .get_doc(&satz_core::DocId::new("a.md"))
                .is_some()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_that_cannot_be_read_leaves_the_index_alone_on_close() {
        let dir = temp_dir("close-unreadable");
        // A directory where the file should be: exists, but read_to_string fails.
        let path = dir.join("a.md");
        std::fs::create_dir_all(&path).unwrap();
        let mut state = state_for(&dir);
        state.open_document("file:///a.md", "# A\n\n[[kept]]\n", &path, 1);
        state.close_document("file:///a.md");
        assert_eq!(link_targets(&state, "a.md"), vec!["kept"]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
