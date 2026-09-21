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
    /// The buffer version the index was last parsed from. Different from `version` = the index is
    /// behind the buffer (stale).
    pub indexed_version: i32,
    /// The index was brought up to date by a request (`refresh_stale_open_documents`), not by the
    /// debounced reparse task: the diagnostics and refreshes that follow a reparse have not been
    /// sent yet. The task takes this over when it wakes and finds nothing left to parse.
    pub announce_pending: bool,
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
        // A byte order mark belongs to the file, not to the text: the parser leaves it out of the
        // indexed text, so the live buffer must too, or line 0 would be three bytes off between them.
        let content = content.into();
        let rope = Rope::from_str(content.strip_prefix('\u{feff}').unwrap_or(&content));
        Self {
            uri: uri.into(),
            path,
            rope,
            version,
            first_change_at: None,
            pending_task: None,
            indexed_version: version,
            announce_pending: false,
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
        if self.indexed_version == version {
            // A client that reuses a version number still changed the text: the index is behind.
            self.indexed_version = version.wrapping_sub(1);
        }
        true
    }
}

/// What is remembered about one content hash.
#[derive(Debug, Clone)]
enum CachedFormat {
    /// The document is already formatted: nothing needs to be kept.
    Unchanged,
    /// The formatted text, which differs from the source.
    Changed(String),
}

/// Cache mapping a document's content hash to what formatting it produced, used by
/// `satz.formatWorkspace` to skip reformatting files whose content has not changed since the last
/// workspace-format call. Already formatted documents are remembered without a copy of their text.
/// Once at capacity, new distinct hashes are not cached (existing entries keep serving hits);
/// `retain_hashes` is how entries of documents that no longer exist are dropped, so the capacity
/// always goes to current content.
#[derive(Debug, Clone)]
pub struct FormatCache {
    entries: HashMap<u64, CachedFormat>,
    capacity: usize,
}

impl FormatCache {
    pub fn new(capacity: usize) -> Self {
        Self {
            entries: HashMap::new(),
            capacity,
        }
    }

    /// The formatted text for a document that formatting changes (`None` for an unknown hash and
    /// for an already formatted document -- see `is_unchanged`).
    pub fn get(&self, hash: u64) -> Option<&str> {
        match self.entries.get(&hash) {
            Some(CachedFormat::Changed(text)) => Some(text.as_str()),
            _ => None,
        }
    }

    fn has_room_for(&self, hash: u64) -> bool {
        self.entries.len() < self.capacity || self.entries.contains_key(&hash)
    }

    pub fn insert(&mut self, hash: u64, formatted: String) {
        if self.has_room_for(hash) {
            self.entries.insert(hash, CachedFormat::Changed(formatted));
        }
    }

    /// Remembers that a document with this content hash is already formatted (no copy is kept).
    pub fn insert_unchanged(&mut self, hash: u64) {
        if self.has_room_for(hash) {
            self.entries.insert(hash, CachedFormat::Unchanged);
        }
    }

    pub fn is_unchanged(&self, hash: u64) -> bool {
        matches!(self.entries.get(&hash), Some(CachedFormat::Unchanged))
    }

    /// Drops every entry whose hash is not in `live`.
    pub fn retain_hashes(&mut self, live: &std::collections::HashSet<u64>) {
        self.entries.retain(|hash, _| live.contains(hash));
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
    vault_root: Option<PathBuf>,

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

    /// Settings of `.satz.toml` that were ignored (an unknown key, a value outside its choices):
    /// everything else applies and formatting stays on, but the user is told.
    pub config_warnings: Vec<String>,

    /// Whether the client supports pull diagnostics
    pub client_supports_pull_diagnostics: bool,

    /// Whether the client understands `WorkspaceEdit.documentChanges` (versioned edits).
    pub client_supports_document_changes: bool,

    /// Counts configuration changes (reload, first load): part of every diagnostics result id.
    pub config_revision: u64,

    /// Whether the client answers `workspace/diagnostic/refresh`.
    pub client_supports_diagnostic_refresh: bool,

    /// Whether the client answers `workspace/semanticTokens/refresh`.
    pub client_supports_semantic_tokens_refresh: bool,

    /// Flag indicating that open document identity keys changed and peers need diagnostic refresh
    peers_dirty: bool,

    /// `satz.formatWorkspace` result cache — see `FormatCache`.
    pub format_cache: FormatCache,

    /// Whether the initial vault-wide `walk_vault` scan has finished. Starts `false` (the
    /// `Default` state used until `initialize_index` completes); diagnostics computed before
    /// this is `true` would only see whichever documents happened to already be open, so any
    /// link to a not-yet-indexed peer looks spuriously broken. Handlers should return empty
    /// diagnostics rather than that false positive — the `workspace/diagnostic/refresh` push
    /// sent once indexing finishes will make the client re-pull the real results.
    indexing_complete: bool,
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

/// The message shown to the user when parts of `.satz.toml` were ignored: every setting that was,
/// and that all the rest applies (formatting included). Empty when there is nothing to say.
pub fn config_warnings_message(warnings: &[String]) -> String {
    if warnings.is_empty() {
        return String::new();
    }
    let mut message =
        String::from("satz: some settings in .satz.toml were ignored; everything else applies:");
    for warning in warnings {
        message.push_str("\n- ");
        message.push_str(warning);
    }
    message
}

/// A snapshot of an open document to parse OUTSIDE the state lock.
#[derive(Debug, Clone)]
pub struct ReparseJob {
    pub rel_path: PathBuf,
    pub content: String,
    pub version: i32,
}

impl SatzState {
    /// The vault's root directory (from the client's `initialize`), if there is one.
    pub fn vault_root(&self) -> Option<&Path> {
        self.vault_root.as_deref()
    }

    pub fn set_vault_root(&mut self, root: Option<PathBuf>) {
        self.vault_root = root;
    }

    /// A state for the vault at `root`, everything else as by default.
    pub fn with_vault_root(root: impl Into<PathBuf>) -> Self {
        Self {
            vault_root: Some(root.into()),
            ..Self::default()
        }
    }

    /// Whether the first indexing of the vault has finished. Until then the index may hold only some
    /// of the notes and handlers answer nothing rather than a spurious "broken link".
    pub fn is_indexing_complete(&self) -> bool {
        self.indexing_complete
    }

    pub fn set_indexing_complete(&mut self, done: bool) {
        self.indexing_complete = done;
    }

    /// Whether what the other open documents depend on changed since they were last told.
    pub fn peers_dirty(&self) -> bool {
        self.peers_dirty
    }

    pub fn mark_peers_dirty(&mut self) {
        self.peers_dirty = true;
    }

    pub fn clear_peers_dirty(&mut self) {
        self.peers_dirty = false;
    }
}

/// What has to be told to the other open documents after a change to one of them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PeerRefresh {
    /// Something they depend on changed (`peers_dirty` was set and this change took effect).
    pub dirty: bool,
    /// The client fetches diagnostics itself (pull) instead of being sent them (push).
    pub supports_pull: bool,
    /// The other open documents, in URI order.
    pub others: Vec<String>,
}

/// What the initial indexing came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexingOutcome {
    /// Notes found on disk.
    pub doc_count: usize,
    /// Why `.satz.toml` could not be used (defaults are in effect), if it could not.
    pub config_error: Option<String>,
    /// Settings of `.satz.toml` that were ignored; the rest of it is in effect.
    pub config_warnings: Vec<String>,
    /// Why the vault could not be indexed at all, if it could not.
    pub failure: Option<String>,
}

impl SatzState {
    /// Installs the result of the initial indexing over the state the server has been running with
    /// meanwhile, keeping what the client already told it: the documents it opened (indexed from
    /// their buffers, not their files) and its capabilities.
    ///
    /// A vault that could not be indexed (its folder is missing, the walk panicked) does NOT leave
    /// the server waiting forever: the settings are loaded, the index holds the open documents,
    /// `indexing_complete` is set, and the reason comes back in `IndexingOutcome::failure` for the
    /// caller to show. Calling it again with a new result replaces the previous one.
    pub fn finish_indexing(
        &mut self,
        result: anyhow::Result<SatzState>,
        vault_root: &Path,
    ) -> IndexingOutcome {
        let (mut new_state, failure) = match result {
            Ok(state) => (state, None),
            Err(error) => {
                tracing::error!(%error, "initial indexing failed");
                let (config, config_error, config_warnings) = Self::load_config(vault_root);
                let state = SatzState {
                    vault_root: Some(vault_root.to_path_buf()),
                    format_cache: FormatCache::new(config.lsp.format_cache_capacity),
                    config,
                    config_error,
                    config_warnings,
                    indexing_complete: true,
                    ..SatzState::default()
                };
                (state, Some(error.to_string()))
            }
        };
        let outcome = IndexingOutcome {
            doc_count: new_state.index.doc_count(),
            config_error: new_state.config_error.clone(),
            config_warnings: new_state.config_warnings.clone(),
            failure,
        };
        new_state.client_supports_pull_diagnostics = self.client_supports_pull_diagnostics;
        new_state.client_supports_document_changes = self.client_supports_document_changes;
        new_state.config_revision = self.config_revision + 1;
        new_state.client_supports_diagnostic_refresh = self.client_supports_diagnostic_refresh;
        new_state.client_supports_semantic_tokens_refresh =
            self.client_supports_semantic_tokens_refresh;
        new_state.open_docs = std::mem::take(&mut self.open_docs);
        for doc in new_state.open_docs.values() {
            let rel_path = Self::get_rel_path(&doc.path, new_state.vault_root.as_deref());
            new_state.index.replace_doc(satz_core::parse_document_owned(
                doc.rope.to_string(),
                &rel_path,
            ));
        }
        *self = new_state;
        self.sync_daily(chrono::Local::now().date_naive());
        outcome
    }

    /// Records what a workspace-format pass computed. Entries of documents that no longer exist
    /// (edited or deleted notes) are dropped first, so the cache capacity always goes to current
    /// content; a result equal to its source is remembered as "already formatted" without a copy.
    pub fn apply_format_cache_updates(&mut self, updates: Vec<(u64, String)>) {
        // The text of an open document is its buffer, which may be newer than what the index holds.
        let buffers: Vec<(u64, String)> = self
            .open_docs
            .values()
            .map(|open| {
                let text = open.rope.to_string();
                (satz_core::content_hash(&text), text)
            })
            .collect();
        let mut live: std::collections::HashSet<u64> =
            self.index.documents().map(|d| d.content_hash).collect();
        live.extend(buffers.iter().map(|(hash, _)| *hash));
        self.format_cache.retain_hashes(&live);
        let mut sources: HashMap<u64, &str> = self
            .index
            .documents()
            .map(|d| (d.content_hash, d.line_index.source()))
            .collect();
        sources.extend(buffers.iter().map(|(hash, text)| (*hash, text.as_str())));
        for (hash, formatted) in updates {
            if sources
                .get(&hash)
                .is_some_and(|source| *source == formatted)
            {
                self.format_cache.insert_unchanged(hash);
            } else {
                self.format_cache.insert(hash, formatted);
            }
        }
    }

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
    /// The vault's settings, or the defaults and the reason when `.satz.toml` cannot be used.
    fn load_config(vault_root: &Path) -> (VaultConfig, Option<String>, Vec<String>) {
        match VaultConfig::load_with_warnings(vault_root) {
            Ok((config, warnings)) => {
                for warning in &warnings {
                    tracing::warn!("initialize_index: {warning}");
                }
                (config, None, warnings)
            }
            Err(e) => {
                tracing::warn!("initialize_index: {e}; using default settings");
                (VaultConfig::default(), Some(e.to_string()), Vec::new())
            }
        }
    }

    pub fn initialize_index(vault_root: PathBuf) -> anyhow::Result<Self> {
        let (config, config_error, config_warnings) = Self::load_config(&vault_root);
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
            config_warnings,
            client_supports_pull_diagnostics: false,
            client_supports_document_changes: false,
            config_revision: 0,
            client_supports_diagnostic_refresh: false,
            client_supports_semantic_tokens_refresh: false,
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
        self.open_doc_for_path(path).is_some()
    }

    /// The open document whose file is `path` (compared as `is_open_path` does), with the key it is
    /// stored under -- the URI the client opened it with.
    pub fn open_doc_for_path(&self, path: &Path) -> Option<(&String, &OpenDocument)> {
        let root = self.vault_root.as_deref();
        let key = |p: &Path| {
            satz_core::slug::fold_key(
                &Self::get_rel_path(p, root)
                    .to_string_lossy()
                    .replace('\\', "/"),
            )
        };
        let wanted = key(path);
        self.open_docs.iter().find(|(_, d)| key(&d.path) == wanted)
    }

    /// The note a link of `doc` points at (`None`: external, footnote, no target at all, or the
    /// note does not exist). Every handler that needs "which note is this link to" asks this, so
    /// they agree with diagnostics and go-to-definition: folder-relative Markdown paths, daily
    /// aliases from the config. A link into the note itself (`[[#Heading]]`) names `doc`.
    pub fn link_target_doc<'a>(
        &'a self,
        doc: &'a satz_core::Document,
        link: &satz_core::Link,
    ) -> Option<&'a satz_core::DocId> {
        if link.kind == satz_core::LinkKind::Footnote
            || satz_core::model::link::is_external_target(&link.target_doc)
        {
            return None;
        }
        if link.target_doc.is_empty() {
            // `[[#Heading]]` / `[[#^id]]` point into this note; a link with nothing at all points nowhere.
            return (link.target_heading.is_some() || link.target_block.is_some())
                .then_some(&doc.id);
        }
        match self.resolve(link, doc) {
            satz_core::LinkResolution::Resolved { doc, .. }
            | satz_core::LinkResolution::AnchorMissing { doc } => Some(&doc.id),
            satz_core::LinkResolution::DocMissing => None,
        }
    }

    /// The open document for `uri` together with its entry in the index (`None` when it is not open
    /// or not indexed) -- the lookup every handler starts with.
    pub fn doc_for_uri(&self, uri: &str) -> Option<(&OpenDocument, &satz_core::Document)> {
        let open_doc = self.open_docs.get(uri)?;
        let rel_path = Self::get_rel_path(&open_doc.path, self.vault_root.as_deref());
        let doc_id = satz_core::DocId::new(rel_path.to_string_lossy().replace('\\', "/"));
        Some((open_doc, self.index.get_doc(&doc_id)?))
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

    /// Takes what the other open documents have to be told after a change to `except`. When the
    /// change `applied`, the "peers depend on this" flag is consumed (`dirty`); a change that did
    /// not take effect (a reparse the user typed past) leaves it for the one that does.
    pub fn take_peer_refresh(&mut self, except: &str, applied: bool) -> PeerRefresh {
        let dirty = self.peers_dirty && applied;
        if applied {
            self.peers_dirty = false;
        }
        let mut others: Vec<String> = self
            .open_docs
            .keys()
            .filter(|uri| uri.as_str() != except)
            .cloned()
            .collect();
        others.sort();
        PeerRefresh {
            dirty,
            supports_pull: self.client_supports_pull_diagnostics,
            others,
        }
    }

    /// The daily-note setting the index should have: the configured aliases and `today`.
    fn wanted_daily(
        &self,
        today: chrono::NaiveDate,
    ) -> (satz_core::config::DailyNoteConfig, chrono::NaiveDate) {
        (self.config.daily_note.clone(), today)
    }

    /// Whether the index was built for another daily-note configuration or another day than
    /// `today` -- `[[bugün]]` links would then point at the wrong note.
    pub fn daily_is_stale(&self, today: chrono::NaiveDate) -> bool {
        self.index.daily() != Some(&self.wanted_daily(today))
    }

    /// Brings the index's daily-note aliases up to the configuration and `today`. Returns whether
    /// anything changed; the open documents' diagnostics (orphan notes) may then differ.
    pub fn sync_daily(&mut self, today: chrono::NaiveDate) -> bool {
        if !self.daily_is_stale(today) {
            return false;
        }
        let wanted = self.wanted_daily(today);
        self.index.set_daily(Some(wanted));
        self.peers_dirty = true;
        true
    }

    /// Whether any open document's buffer is ahead of the index: the debounced reparse after the last
    /// edit has not run yet. O(number of open documents).
    pub fn has_stale_open_documents(&self) -> bool {
        self.open_docs
            .values()
            .any(|open| open.indexed_version != open.version)
    }

    /// Reparses the open documents whose buffer is ahead of the index, now, and returns how many.
    /// Requests that turn a client position into an offset (or edit the text) call this first: the
    /// position refers to the buffer, so the index they read must too.
    pub fn refresh_stale_open_documents(&mut self) -> usize {
        let stale: Vec<String> = self
            .open_docs
            .iter()
            .filter(|(_, open)| open.indexed_version != open.version)
            .map(|(uri, _)| uri.clone())
            .collect();
        for uri in &stale {
            self.reparse_open_document(uri);
            if let Some(open) = self.open_docs.get_mut(uri) {
                open.announce_pending = true;
            }
        }
        stale.len()
    }

    /// Whether the index of `uri` was refreshed by a request and its follow-up notifications are
    /// still owed; clears the debt. See `OpenDocument::announce_pending`.
    pub fn take_announce_pending(&mut self, uri: &str) -> bool {
        self.open_docs
            .get_mut(uri)
            .is_some_and(|open| std::mem::take(&mut open.announce_pending))
    }

    /// Takes what is needed to re-parse an open document: its text, path and version. Quick (a copy
    /// of the buffer); the parse itself can then run without any lock.
    pub fn prepare_reparse(&self, uri: &str) -> Option<ReparseJob> {
        let open_doc = self.open_docs.get(uri)?;
        if open_doc.indexed_version == open_doc.version {
            return None; // the index already holds this text
        }
        let rel_path = Self::get_rel_path(&open_doc.path, self.vault_root.as_deref());
        Some(ReparseJob {
            rel_path,
            content: open_doc.rope.to_string(),
            version: open_doc.version,
        })
    }

    /// Puts a parsed document into the index -- but only if the open document is still at the
    /// version the parse was made from. A result the user has typed past is dropped (`false`): a
    /// newer reparse is already queued for the newer text, and applying the old one would briefly
    /// show stale links and diagnostics.
    pub fn apply_reparse(&mut self, uri: &str, version: i32, new_doc: satz_core::Document) -> bool {
        let Some(open_doc) = self.open_docs.get_mut(uri) else {
            return false;
        };
        if open_doc.version != version {
            return false;
        }
        open_doc.first_change_at = None;
        open_doc.indexed_version = version;
        // Whoever applies a parse announces it (the debounced task, save) -- or, for a request's
        // refresh, marks the debt again right after.
        open_doc.announce_pending = false;

        let doc_id = new_doc.id.clone();
        tracing::trace!(%uri, ?doc_id, "apply_reparse");
        let old_keys = self
            .index
            .get_doc(&doc_id)
            .map(peer_signature)
            .unwrap_or_default();
        if old_keys != peer_signature(&new_doc) {
            self.peers_dirty = true;
        }
        self.index.replace_doc(new_doc);
        true
    }

    /// Re-parses the in-memory rope content of an open document and updates the index, at once
    /// (save, format): prepare and apply in one go, so nothing can change in between.
    pub fn reparse_open_document(&mut self, uri: &str) {
        let Some(job) = self.prepare_reparse(uri) else {
            return;
        };
        let new_doc = satz_core::parse_document_owned(job.content, &job.rel_path);
        self.apply_reparse(uri, job.version, new_doc);
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
                let disk_doc = satz_core::parse_document_owned(content, &rel_path);
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
// Test states are built field by field so each test shows exactly what it sets up.
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use super::*;

    #[test]
    fn get_rel_path_with_turkish_vault_root() {
        let root = Path::new("/notlar/İş");
        let path = Path::new("/notlar/İş/projeler/proje1.md");
        let rel = SatzState::get_rel_path(path, Some(root));
        assert_eq!(rel, PathBuf::from("projeler/proje1.md"));

        // Case-insensitive test on Windows path format (`\` separates only on Windows)
        if cfg!(windows) {
            let root_win = Path::new("C:\\Notlar\\İş");
            let path_win = Path::new("c:\\notlar\\iş\\projeler\\proje1.md");
            let rel_win = SatzState::get_rel_path(path_win, Some(root_win));
            assert_eq!(rel_win, PathBuf::from("projeler\\proje1.md"));
        }
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
    fn initialize_index_reports_wrong_types_as_an_error() {
        let v = TempVault::new("badkey");
        v.config("[hover]\npreview_lines = \"many\"\n");
        let state = SatzState::initialize_index(v.0.clone()).unwrap();
        let error = state
            .config_error
            .as_deref()
            .expect("a config error must be recorded");
        assert!(error.contains("preview_lines"), "{error}");
        assert!(!state.formatting_allowed());
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

    fn state_with_open(root: &str, open: &str, rel_files: &[&str]) -> SatzState {
        let mut state = SatzState::default();
        state.index = Index::build(
            rel_files
                .iter()
                .map(|p| satz_core::parse_document("# t\n", Path::new(p)))
                .collect(),
        );
        state.vault_root = Some(PathBuf::from(root));
        state.open_docs.insert(
            "file:///x".to_string(),
            OpenDocument::new("file:///x", PathBuf::from(open), "# t\n", 1),
        );
        state
    }

    #[test]
    fn doc_for_uri_finds_an_open_and_indexed_document() {
        let state = state_with_open("/vault", "/vault/sub/a.md", &["sub/a.md", "b.md"]);
        let (open, doc) = state.doc_for_uri("file:///x").expect("open and indexed");
        assert_eq!(open.path, PathBuf::from("/vault/sub/a.md"));
        assert_eq!(doc.id.as_str(), "sub/a.md");
    }

    #[test]
    fn doc_for_uri_is_none_for_unknown_or_unindexed_documents() {
        let state = state_with_open("/vault", "/vault/sub/a.md", &["b.md"]);
        assert!(
            state.doc_for_uri("file:///x").is_none(),
            "open but not indexed"
        );
        assert!(state.doc_for_uri("file:///other").is_none(), "not open");
        assert!(state.doc_for_uri("").is_none());
    }

    #[test]
    fn doc_for_uri_follows_the_same_path_rules_as_get_rel_path() {
        // Windows separators and a differently cased root still land on the indexed note.
        let state = state_with_open("C:\\Notlar\\İş", "c:\\notlar\\iş\\projeler\\p1.md", &[]);
        assert!(
            state.doc_for_uri("file:///x").is_none(),
            "nothing indexed yet"
        );
        // A path outside the root is looked up as given (and is not indexed under that name).
        let state = state_with_open("/vault", "/elsewhere/a.md", &["a.md"]);
        assert!(state.doc_for_uri("file:///x").is_none());
        // No vault root: the path itself is the id.
        let mut state = SatzState::default();
        state.index = Index::build(vec![satz_core::parse_document("# t\n", Path::new("a.md"))]);
        state.open_docs.insert(
            "file:///y".to_string(),
            OpenDocument::new("file:///y", PathBuf::from("a.md"), "# t\n", 1),
        );
        assert_eq!(
            state.doc_for_uri("file:///y").unwrap().1.id.as_str(),
            "a.md"
        );
    }

    // ---- the workspace-format cache: no stale entries, no copies of already formatted text ----

    fn state_with_docs(texts: &[(&str, &str)], capacity: usize) -> SatzState {
        let mut state = SatzState::default();
        state.index = Index::build(
            texts
                .iter()
                .map(|(p, t)| satz_core::parse_document(t, Path::new(p)))
                .collect(),
        );
        state.format_cache = FormatCache::new(capacity);
        // Formatting results need file locations, so the vault root must be absolute.
        state.vault_root = Some(if cfg!(windows) {
            PathBuf::from("C:\\")
        } else {
            PathBuf::from("/")
        });
        state
    }

    fn hash_of(state: &SatzState, path: &str) -> u64 {
        state
            .index
            .documents()
            .find(|d| d.path == Path::new(path))
            .unwrap()
            .content_hash
    }

    /// One workspace-format pass: compute, then record what was computed the way the server does.
    fn run_pass(state: &mut SatzState) -> usize {
        let result = crate::handlers::execute_command::compute_format_changes(state);
        let changes = result.changes.len();
        state.apply_format_cache_updates(result.cache_updates);
        changes
    }

    const DIRTY_A: &str = "Line 1   \n\n\n\nLine 2   ";
    const DIRTY_B: &str = "Other   \n\n\n\nText   ";
    const CLEAN: &str = "# Clean\n\nAlready tidy.\n";

    #[test]
    fn entries_of_documents_that_no_longer_exist_do_not_keep_current_ones_out() {
        let mut state = state_with_docs(&[("a.md", DIRTY_A), ("b.md", DIRTY_B)], 2);
        // The cache is full of hashes no document has any more (edited or deleted notes).
        state.format_cache.insert(1001, "old one".to_string());
        state.format_cache.insert(1002, "old two".to_string());

        assert_eq!(run_pass(&mut state), 2);
        assert!(state.format_cache.get(1001).is_none());
        assert!(state.format_cache.get(1002).is_none());
        for path in ["a.md", "b.md"] {
            assert!(
                state.format_cache.get(hash_of(&state, path)).is_some(),
                "{path} should be cached"
            );
        }
        assert_eq!(state.format_cache.len(), 2);
    }

    #[test]
    fn an_already_formatted_document_is_remembered_without_a_copy() {
        let mut state = state_with_docs(&[("clean.md", CLEAN)], 10);
        assert_eq!(run_pass(&mut state), 0);
        let hash = hash_of(&state, "clean.md");
        assert!(state.format_cache.is_unchanged(hash));
        assert!(
            state.format_cache.get(hash).is_none(),
            "no formatted text is stored for an unchanged document"
        );
        // The next pass is answered entirely from the cache.
        let again = crate::handlers::execute_command::compute_format_changes(&state);
        assert!(again.changes.is_empty());
        assert!(again.cache_updates.is_empty(), "nothing was recomputed");
    }

    #[test]
    fn changed_and_unchanged_documents_are_both_served_from_the_cache() {
        let mut state = state_with_docs(&[("a.md", DIRTY_A), ("clean.md", CLEAN)], 10);
        assert_eq!(run_pass(&mut state), 1);
        let again = crate::handlers::execute_command::compute_format_changes(&state);
        assert_eq!(
            again.changes.len(),
            1,
            "the dirty note still needs its edit"
        );
        assert_eq!(again.changes[0].formatted, "Line 1\n\nLine 2\n");
        assert!(again.cache_updates.is_empty());
        assert_eq!(state.format_cache.len(), 2);
    }

    #[test]
    fn an_edited_document_replaces_its_old_entry() {
        let mut state = state_with_docs(&[("a.md", DIRTY_A)], 10);
        run_pass(&mut state);
        let old_hash = hash_of(&state, "a.md");
        state
            .index
            .replace_doc(satz_core::parse_document(DIRTY_B, Path::new("a.md")));
        run_pass(&mut state);
        assert!(state.format_cache.get(old_hash).is_none());
        assert!(state.format_cache.get(hash_of(&state, "a.md")).is_some());
        assert_eq!(state.format_cache.len(), 1);
    }

    #[test]
    fn the_capacity_is_never_exceeded_and_zero_capacity_is_harmless() {
        let mut state = state_with_docs(
            &[
                ("a.md", DIRTY_A),
                ("b.md", DIRTY_B),
                ("c.md", "x   \n\n\n\ny"),
            ],
            2,
        );
        assert_eq!(run_pass(&mut state), 3);
        assert_eq!(state.format_cache.len(), 2);

        let mut state = state_with_docs(&[("a.md", DIRTY_A), ("clean.md", CLEAN)], 0);
        assert_eq!(run_pass(&mut state), 1);
        assert_eq!(state.format_cache.len(), 0);
        assert!(state.format_cache.is_empty());
    }

    #[test]
    fn identical_content_in_two_files_is_one_entry() {
        let mut state = state_with_docs(&[("a.md", DIRTY_A), ("copy.md", DIRTY_A)], 10);
        assert_eq!(run_pass(&mut state), 2);
        assert_eq!(state.format_cache.len(), 1);
    }

    #[test]
    fn retaining_hashes_keeps_only_the_live_ones_of_both_kinds() {
        let mut cache = FormatCache::new(10);
        cache.insert(1, "one".to_string());
        cache.insert_unchanged(2);
        cache.insert(3, "three".to_string());
        cache.insert_unchanged(4);
        cache.retain_hashes(&std::collections::HashSet::from([2, 3]));
        assert_eq!(cache.len(), 2);
        assert!(cache.get(1).is_none() && !cache.is_unchanged(4));
        assert!(cache.is_unchanged(2));
        assert_eq!(cache.get(3), Some("three"));
    }

    // ---- the initial indexing: a failure must not leave the server dead ----

    fn scratch_dir(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "satz-index-{}-{tag}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn doc_ids(state: &SatzState) -> Vec<String> {
        let mut ids: Vec<String> = state
            .index
            .documents()
            .map(|d| d.id.as_str().to_string())
            .collect();
        ids.sort();
        ids
    }

    /// A state that already has a document open, as if the client opened it during indexing.
    fn state_with_open_buffer(dir: &Path) -> SatzState {
        let mut state = SatzState::default();
        state.client_supports_pull_diagnostics = true;
        state.open_document(
            "file:///b.md",
            "# B\n\n[[from-the-buffer]]\n",
            &dir.join("b.md"),
            3,
        );
        state
    }

    #[test]
    fn a_finished_index_keeps_the_open_buffers_and_the_client_settings() {
        let dir = scratch_dir("ok");
        std::fs::write(dir.join("a.md"), "# A\n").unwrap();
        std::fs::write(dir.join("b.md"), "# B on disk\n").unwrap();
        let mut state = state_with_open_buffer(&dir);

        let outcome = state.finish_indexing(SatzState::initialize_index(dir.clone()), &dir);

        assert_eq!(outcome.failure, None);
        assert_eq!(outcome.doc_count, 2);
        assert!(state.indexing_complete);
        assert!(state.client_supports_pull_diagnostics);
        assert_eq!(doc_ids(&state), vec!["a.md", "b.md"]);
        // The buffer, not the file, is what is indexed for the open note.
        let b = state.index.get_doc(&satz_core::DocId::new("b.md")).unwrap();
        assert_eq!(b.links[0].target_doc, "from-the-buffer");
        assert_eq!(state.open_docs["file:///b.md"].version, 3);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failed_index_does_not_leave_the_server_waiting_forever() {
        let dir = scratch_dir("fail");
        let missing = dir.join("no-such-vault");
        let mut state = state_with_open_buffer(&missing);
        assert!(!state.indexing_complete);

        let result = SatzState::initialize_index(missing.clone());
        let outcome = state.finish_indexing(result, &missing);

        let failure = outcome.failure.expect("the failure is reported");
        assert!(failure.contains("no-such-vault"), "{failure}");
        assert!(state.indexing_complete, "handlers must not stay silent");
        assert_eq!(state.vault_root.as_deref(), Some(missing.as_path()));
        assert!(state.client_supports_pull_diagnostics);
        // What the user has open still works.
        assert_eq!(doc_ids(&state), vec!["b.md"]);
        assert_eq!(state.open_docs.len(), 1);
        assert_eq!(outcome.doc_count, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn indexing_can_be_tried_again_after_a_failure() {
        let dir = scratch_dir("retry");
        let missing = dir.join("later");
        let mut state = state_with_open_buffer(&missing);
        state.finish_indexing(SatzState::initialize_index(missing.clone()), &missing);
        assert!(state.indexing_complete);

        std::fs::create_dir_all(&missing).unwrap();
        std::fs::write(missing.join("c.md"), "# C\n").unwrap();
        let outcome = state.finish_indexing(SatzState::initialize_index(missing.clone()), &missing);
        assert_eq!(outcome.failure, None);
        assert_eq!(outcome.doc_count, 1);
        assert_eq!(doc_ids(&state), vec!["b.md", "c.md"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unusable_config_is_reported_with_the_index_and_defaults_are_used() {
        let dir = scratch_dir("config");
        std::fs::write(dir.join("a.md"), "# A\n").unwrap();
        std::fs::write(dir.join(".satz.toml"), "this is = = not toml").unwrap();
        let mut state = SatzState::default();
        let outcome = state.finish_indexing(SatzState::initialize_index(dir.clone()), &dir);
        assert_eq!(outcome.failure, None);
        assert!(outcome.config_error.is_some());
        assert!(state.config_error.is_some());
        assert_eq!(doc_ids(&state), vec!["a.md"]);
        // A failed index with a broken config reports the config problem too.
        let missing = dir.join("nope");
        let outcome = state.finish_indexing(SatzState::initialize_index(missing.clone()), &missing);
        assert!(outcome.failure.is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- re-parsing an open document without holding the lock while parsing ----

    fn open_state(text: &str) -> SatzState {
        let mut state = SatzState::default();
        state.vault_root = Some(PathBuf::from("/vault"));
        state.open_document("file:///a.md", text, Path::new("/vault/a.md"), 1);
        state
    }

    fn type_into(state: &mut SatzState, version: i32, new_text: &str) {
        let doc = state.open_docs.get_mut("file:///a.md").unwrap();
        doc.rope = Rope::from_str(new_text);
        doc.version = version;
    }

    fn links_of_a(state: &SatzState) -> Vec<String> {
        state
            .index
            .get_doc(&satz_core::DocId::new("a.md"))
            .unwrap()
            .links
            .iter()
            .map(|l| l.target_doc.clone())
            .collect()
    }

    #[test]
    fn a_reparse_job_carries_the_text_and_version_it_was_prepared_from() {
        let mut state = open_state("# A\n\n[[one]]\n");
        type_into(&mut state, 2, "# A\n\n[[two]] [[three]]\n");
        let job = state
            .prepare_reparse("file:///a.md")
            .expect("an open document");
        assert_eq!(job.version, 2);
        assert_eq!(job.content, "# A\n\n[[two]] [[three]]\n");
        assert_eq!(job.rel_path, PathBuf::from("a.md"));
        assert!(state.prepare_reparse("file:///unknown.md").is_none());
    }

    #[test]
    fn a_reparse_of_the_current_version_is_applied() {
        let mut state = open_state("# A\n\n[[one]]\n");
        type_into(&mut state, 2, "# A\n\n[[two]]\n");
        let job = state.prepare_reparse("file:///a.md").unwrap();
        // The parse happens with no lock and no state at all.
        let parsed = satz_core::parse_document(&job.content, &job.rel_path);
        assert!(state.apply_reparse("file:///a.md", job.version, parsed));
        assert_eq!(links_of_a(&state), vec!["two".to_string()]);
        assert!(state.open_docs["file:///a.md"].first_change_at.is_none());
    }

    #[test]
    fn a_reparse_that_the_user_has_typed_past_is_dropped() {
        let mut state = open_state("# A\n\n[[one]]\n");
        type_into(&mut state, 2, "# A\n\n[[two]]\n");
        let job = state.prepare_reparse("file:///a.md").unwrap();
        let parsed = satz_core::parse_document(&job.content, &job.rel_path);

        // While the parse ran (outside the lock) the user typed more.
        type_into(&mut state, 3, "# A\n\n[[three]]\n");
        state.peers_dirty = false;
        assert!(!state.apply_reparse("file:///a.md", job.version, parsed));
        assert_eq!(
            links_of_a(&state),
            vec!["one".to_string()],
            "the index keeps what it had; the newer text has its own reparse queued"
        );
        assert!(!state.peers_dirty, "a dropped result changes nothing");
    }

    #[test]
    fn peers_are_marked_only_when_a_reparse_is_applied_and_changes_what_they_see() {
        let mut state = open_state("# A\n\n[[one]]\n");
        state.peers_dirty = false;
        // Same signature (the link to `one` stays): not dirty.
        type_into(&mut state, 2, "# A\n\n[[one]] and more text\n");
        let job = state.prepare_reparse("file:///a.md").unwrap();
        let parsed = satz_core::parse_document(&job.content, &job.rel_path);
        assert!(state.apply_reparse("file:///a.md", job.version, parsed));
        assert!(!state.peers_dirty);
        // A new heading is something other documents can link to: dirty.
        type_into(&mut state, 3, "# A\n\n## New heading\n\n[[one]]\n");
        let job = state.prepare_reparse("file:///a.md").unwrap();
        let parsed = satz_core::parse_document(&job.content, &job.rel_path);
        assert!(state.apply_reparse("file:///a.md", job.version, parsed));
        assert!(state.peers_dirty);
    }

    #[test]
    fn a_reparse_for_a_document_that_was_closed_meanwhile_is_dropped() {
        let mut state = open_state("# A\n");
        // A reparse is only prepared for a buffer the index is behind (the user has typed).
        type_into(&mut state, 2, "# A\n\ntyped\n");
        let job = state.prepare_reparse("file:///a.md").unwrap();
        let parsed = satz_core::parse_document(&job.content, &job.rel_path);
        state.close_document("file:///a.md");
        assert!(!state.apply_reparse("file:///a.md", job.version, parsed));
    }

    #[test]
    fn the_synchronous_reparse_still_works_for_save_and_format() {
        let mut state = open_state("# A\n\n[[one]]\n");
        type_into(&mut state, 5, "# A\n\n[[five]]\n");
        state.reparse_open_document("file:///a.md");
        assert_eq!(links_of_a(&state), vec!["five".to_string()]);
        // Unknown documents are ignored.
        state.reparse_open_document("file:///nope.md");
    }

    // ---- an index that always reflects the open buffers ----

    fn edit_buffer(state: &mut SatzState, version: i32, new_text: &str) {
        let doc = state.open_docs.get_mut("file:///a.md").unwrap();
        let changes = vec![TextDocumentContentChangeEvent {
            range: None,
            range_length: None,
            text: new_text.to_string(),
        }];
        assert!(doc.apply_change_events(version, changes));
    }

    #[test]
    fn a_freshly_opened_document_is_not_stale_and_typing_makes_it_so() {
        let mut state = open_state("# A\n\n[[one]]\n");
        assert!(!state.has_stale_open_documents());
        edit_buffer(&mut state, 2, "# A\n\n[[two]]\n");
        assert!(state.has_stale_open_documents());
        assert_eq!(
            links_of_a(&state),
            vec!["one".to_string()],
            "the index is one edit behind"
        );
    }

    #[test]
    fn refreshing_brings_the_index_up_to_the_buffers_once() {
        let mut state = open_state("# A\n\n[[one]]\n");
        edit_buffer(&mut state, 2, "# A\n\n[[two]]\n");
        assert_eq!(state.refresh_stale_open_documents(), 1);
        assert_eq!(links_of_a(&state), vec!["two".to_string()]);
        assert!(!state.has_stale_open_documents());
        assert_eq!(
            state.refresh_stale_open_documents(),
            0,
            "nothing left to do"
        );
        // A reparse task that fires later finds nothing to do either.
        assert!(state.prepare_reparse("file:///a.md").is_none());
    }

    #[test]
    fn a_refresh_by_a_request_leaves_its_notifications_owed_exactly_once() {
        let mut state = open_state("# A\n\n[[one]]\n");
        assert!(
            !state.take_announce_pending("file:///a.md"),
            "a document nobody refreshed owes nothing"
        );

        edit_buffer(&mut state, 2, "# A\n\n[[two]]\n");
        assert!(
            !state.take_announce_pending("file:///a.md"),
            "typing alone is the debounced task's business"
        );
        assert_eq!(state.refresh_stale_open_documents(), 1);
        assert!(state.take_announce_pending("file:///a.md"), "now owed");
        assert!(
            !state.take_announce_pending("file:///a.md"),
            "taken: sent once"
        );

        assert_eq!(state.refresh_stale_open_documents(), 0);
        assert!(
            !state.take_announce_pending("file:///a.md"),
            "a refresh that found nothing to parse owes nothing"
        );
        assert!(!state.take_announce_pending("file:///nope.md"));
    }

    #[test]
    fn whoever_applies_a_parse_next_takes_over_the_notifications() {
        // A request refreshed the buffer (debt), then the user typed on: the debounced task that
        // parses the newer text announces for both, so nothing is owed any more.
        let mut state = open_state("# A\n\n[[one]]\n");
        edit_buffer(&mut state, 2, "# A\n\n[[two]]\n");
        state.refresh_stale_open_documents();
        edit_buffer(&mut state, 3, "# A\n\n[[three]]\n");
        state.reparse_open_document("file:///a.md");
        assert!(!state.take_announce_pending("file:///a.md"));
    }

    #[test]
    fn a_change_that_reuses_the_version_number_is_still_seen_as_stale() {
        let mut state = open_state("# A\n\n[[one]]\n");
        edit_buffer(&mut state, 1, "# A\n\n[[same-version]]\n");
        assert!(state.has_stale_open_documents());
        assert_eq!(state.refresh_stale_open_documents(), 1);
        assert_eq!(links_of_a(&state), vec!["same-version".to_string()]);
    }

    #[test]
    fn only_the_stale_documents_are_reparsed_and_closed_ones_do_not_count() {
        let mut state = open_state("# A\n");
        state.open_document("file:///b.md", "# B\n", Path::new("/vault/b.md"), 1);
        state.open_document("file:///c.md", "# C\n", Path::new("/vault/c.md"), 1);
        edit_buffer(&mut state, 2, "# A\n\nnew\n");
        {
            let c = state.open_docs.get_mut("file:///c.md").unwrap();
            c.apply_change_events(
                2,
                vec![TextDocumentContentChangeEvent {
                    range: None,
                    range_length: None,
                    text: "# C\n\nnew\n".to_string(),
                }],
            );
        }
        assert_eq!(state.refresh_stale_open_documents(), 2);
        state.close_document("file:///a.md");
        assert!(!state.has_stale_open_documents());
    }

    #[test]
    fn refreshing_marks_peers_only_when_what_they_depend_on_changed() {
        let mut state = open_state("# A\n\n[[one]]\n");
        state.peers_dirty = false;
        edit_buffer(&mut state, 2, "# A\n\n[[one]] more words\n");
        state.refresh_stale_open_documents();
        assert!(!state.peers_dirty);
        edit_buffer(&mut state, 3, "# A\n\n## New heading\n\n[[one]]\n");
        state.refresh_stale_open_documents();
        assert!(state.peers_dirty);
    }

    #[test]
    fn a_big_buffer_is_parsed_once_and_then_answered_from_the_index() {
        let mut state = open_state("# A\n");
        let big = format!("# A\n\n{}", "some words [[x]] and more\n".repeat(80_000));
        edit_buffer(&mut state, 2, &big);
        let start = std::time::Instant::now();
        assert_eq!(state.refresh_stale_open_documents(), 1);
        let first = start.elapsed();
        let start = std::time::Instant::now();
        assert_eq!(state.refresh_stale_open_documents(), 0);
        assert!(start.elapsed() < first.max(std::time::Duration::from_millis(50)));
        assert!(links_of_a(&state).len() >= 80_000);
    }

    // ---- daily-note aliases follow the config and the calendar ----

    fn day(y: i32, m: u32, d: u32) -> chrono::NaiveDate {
        chrono::NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    fn daily_vault() -> SatzState {
        let mut state = SatzState::default();
        state.vault_root = Some(PathBuf::from("/vault"));
        state.index = satz_core::Index::build(vec![
            satz_core::parse_document(
                "# Log

[[bugün]]
",
                Path::new("log.md"),
            ),
            satz_core::parse_document(
                "# 14
",
                Path::new("daily/2026-03-14.md"),
            ),
            satz_core::parse_document(
                "# 15
",
                Path::new("daily/2026-03-15.md"),
            ),
        ]);
        state
    }

    fn backlinks_to(state: &SatzState, path: &str) -> usize {
        state
            .index
            .backlinks_of(&satz_core::DocId::new(path))
            .count()
    }

    #[test]
    fn syncing_the_daily_date_gives_the_alias_link_its_backlink() {
        let mut state = daily_vault();
        assert!(state.daily_is_stale(day(2026, 3, 14)), "nothing set yet");
        assert!(state.sync_daily(day(2026, 3, 14)));
        assert_eq!(backlinks_to(&state, "daily/2026-03-14.md"), 1);
        assert!(!state.daily_is_stale(day(2026, 3, 14)));
        assert!(
            !state.sync_daily(day(2026, 3, 14)),
            "nothing changed the second time"
        );
    }

    #[test]
    fn a_new_day_makes_the_daily_setting_stale_and_moves_the_backlink() {
        let mut state = daily_vault();
        state.sync_daily(day(2026, 3, 14));
        assert!(state.daily_is_stale(day(2026, 3, 15)), "midnight passed");
        assert!(state.sync_daily(day(2026, 3, 15)));
        assert_eq!(backlinks_to(&state, "daily/2026-03-14.md"), 0);
        assert_eq!(backlinks_to(&state, "daily/2026-03-15.md"), 1);
    }

    #[test]
    fn a_changed_daily_config_is_stale_even_on_the_same_day() {
        let mut state = daily_vault();
        state.sync_daily(day(2026, 3, 14));
        state.config.daily_note.aliases.today = vec!["heute".to_string()];
        assert!(state.daily_is_stale(day(2026, 3, 14)));
        assert!(state.sync_daily(day(2026, 3, 14)));
        assert_eq!(
            backlinks_to(&state, "daily/2026-03-14.md"),
            0,
            "`bugün` is no longer an alias"
        );
    }

    #[test]
    fn syncing_the_daily_date_asks_for_the_peers_to_be_refreshed_only_when_something_changed() {
        let mut state = daily_vault();
        state.peers_dirty = false;
        state.sync_daily(day(2026, 3, 14));
        assert!(state.peers_dirty, "orphan status of the daily note changed");
        state.peers_dirty = false;
        state.sync_daily(day(2026, 3, 14));
        assert!(!state.peers_dirty);
    }

    // ---- what the other open documents are told after a change ----

    fn three_open_docs() -> SatzState {
        let mut state = SatzState::default();
        state.vault_root = Some(PathBuf::from("/vault"));
        for name in ["c", "a", "b"] {
            state.open_document(
                &format!("file:///{name}.md"),
                "# T\n",
                Path::new(&format!("/vault/{name}.md")),
                1,
            );
        }
        state.peers_dirty = false;
        state
    }

    #[test]
    fn nothing_dirty_means_nothing_to_tell_and_the_others_are_listed_in_order() {
        let mut state = three_open_docs();
        let peers = state.take_peer_refresh("file:///a.md", true);
        assert!(!peers.dirty);
        assert_eq!(peers.others, vec!["file:///b.md", "file:///c.md"]);
    }

    #[test]
    fn a_dirty_flag_is_taken_once_when_the_change_took_effect() {
        let mut state = three_open_docs();
        state.peers_dirty = true;
        state.client_supports_pull_diagnostics = true;
        let peers = state.take_peer_refresh("file:///b.md", true);
        assert!(peers.dirty && peers.supports_pull);
        assert!(!state.peers_dirty, "taken");
        assert!(
            !state.take_peer_refresh("file:///b.md", true).dirty,
            "and not told twice"
        );
    }

    #[test]
    fn a_change_that_did_not_take_effect_leaves_the_flag_for_the_one_that_did() {
        let mut state = three_open_docs();
        state.peers_dirty = true;
        let peers = state.take_peer_refresh("file:///b.md", false);
        assert!(!peers.dirty, "nothing to tell yet");
        assert!(state.peers_dirty, "still pending");
        assert!(state.take_peer_refresh("file:///b.md", true).dirty);
    }

    #[test]
    fn the_changed_document_and_unknown_uris_are_handled() {
        let mut state = three_open_docs();
        let peers = state.take_peer_refresh("file:///nope.md", true);
        assert_eq!(peers.others.len(), 3);
        state.close_document("file:///a.md");
        state.close_document("file:///b.md");
        state.close_document("file:///c.md");
        assert!(
            state
                .take_peer_refresh("file:///a.md", true)
                .others
                .is_empty()
        );
    }

    // ---- the state's own invariants are read and changed through methods ----

    #[test]
    fn a_new_state_has_no_root_is_not_indexed_and_has_nothing_dirty() {
        let state = SatzState::default();
        assert_eq!(state.vault_root(), None);
        assert!(!state.is_indexing_complete());
        assert!(!state.peers_dirty());
    }

    #[test]
    fn the_indexing_flag_and_the_dirty_flag_can_be_set_and_cleared_repeatedly() {
        let mut state = SatzState::default();
        state.set_indexing_complete(true);
        state.set_indexing_complete(true);
        assert!(state.is_indexing_complete());
        state.set_indexing_complete(false);
        assert!(!state.is_indexing_complete());
        state.mark_peers_dirty();
        state.mark_peers_dirty();
        assert!(state.peers_dirty());
        state.clear_peers_dirty();
        state.clear_peers_dirty();
        assert!(!state.peers_dirty());
    }

    #[test]
    fn a_finished_first_index_is_complete_and_keeps_the_root() {
        let mut state = SatzState::with_vault_root("/vault");
        assert_eq!(state.vault_root(), Some(Path::new("/vault")));
        let fresh = SatzState::with_vault_root("/vault");
        let mut fresh = fresh;
        fresh.set_indexing_complete(true);
        state.finish_indexing(Ok(fresh), Path::new("/vault"));
        assert!(state.is_indexing_complete());
        assert_eq!(state.vault_root(), Some(Path::new("/vault")));
    }

    #[test]
    fn changing_the_root_changes_how_paths_are_made_relative() {
        let mut state = SatzState::with_vault_root("/vault");
        let inside =
            |s: &SatzState| SatzState::get_rel_path(Path::new("/vault/sub/a.md"), s.vault_root());
        assert_eq!(inside(&state), PathBuf::from("sub/a.md"));
        state.set_vault_root(Some(PathBuf::from("/vault/sub")));
        assert_eq!(inside(&state), PathBuf::from("a.md"));
        state.set_vault_root(None);
        assert_eq!(inside(&state), PathBuf::from("/vault/sub/a.md"));
    }

    #[test]
    fn a_finished_index_keeps_what_the_client_can_do() {
        let mut state = SatzState::default();
        state.client_supports_pull_diagnostics = true;
        state.client_supports_diagnostic_refresh = true;
        state.finish_indexing(Ok(SatzState::default()), Path::new("/vault"));
        assert!(state.client_supports_pull_diagnostics);
        assert!(state.client_supports_diagnostic_refresh);
    }

    // ---- a config with mistakes: the rest applies, the mistakes are reported ----

    #[test]
    fn unknown_keys_and_bad_values_are_warnings_and_formatting_stays_on() {
        for (label, content, expect) in [
            (
                "unknown key",
                "[formatter.wrap]\nenabled = true\n",
                "enabled",
            ),
            (
                "bad daily format",
                "[daily_note]\nformat = \"%Q\"\n",
                "daily_note.format",
            ),
            (
                "bad choice",
                "[formatter.misc]\nhr_style = \"====\"\n",
                "hr_style",
            ),
        ] {
            let v = TempVault::new("warnkey");
            v.config(&format!("{content}\n[hover]\npreview_lines = 3\n"));
            let state = SatzState::initialize_index(v.0.clone()).unwrap();
            assert_eq!(state.config_error, None, "{label}");
            assert_eq!(
                state.config.hover.preview_lines, 3,
                "{label}: the rest applies"
            );
            assert_eq!(
                state.config_warnings.len(),
                1,
                "{label}: {:?}",
                state.config_warnings
            );
            assert!(
                state.config_warnings[0].contains(expect),
                "{label}: {:?}",
                state.config_warnings
            );
            assert!(state.formatting_allowed(), "{label}");
        }
    }

    #[test]
    fn the_warnings_come_back_with_the_indexing_outcome_and_survive_the_index_swap() {
        let v = TempVault::new("warnswap");
        v.config("[bogus]\nx = 1\n");
        let fresh = SatzState::initialize_index(v.0.clone()).unwrap();
        let mut state = SatzState::default();
        let outcome = state.finish_indexing(Ok(fresh), &v.0);
        assert_eq!(outcome.config_warnings.len(), 1);
        assert_eq!(state.config_warnings, outcome.config_warnings);
        // A failed walk still loads the settings and reports their mistakes.
        let mut state = SatzState::default();
        let outcome = state.finish_indexing(Err(anyhow::anyhow!("no folder")), &v.0);
        assert!(outcome.failure.is_some());
        assert_eq!(outcome.config_warnings.len(), 1);
    }

    #[test]
    fn the_warning_message_lists_every_ignored_setting_and_says_the_rest_applies() {
        let message = config_warnings_message(&[
            ".satz.toml: unknown setting formatter.wrap.enabled (ignored)".to_string(),
            ".satz.toml: invalid formatter.misc.hr_style \"====\"; using \"---\"".to_string(),
        ]);
        assert!(message.contains("formatter.wrap.enabled"), "{message}");
        assert!(message.contains("hr_style"), "{message}");
        assert!(message.contains("everything else"), "{message}");
        assert_eq!(
            message.lines().count(),
            3,
            "a header and one line each: {message}"
        );
        assert_eq!(config_warnings_message(&[]), "");
    }

    // ---- a byte order mark at the start of the text ----

    #[test]
    fn a_byte_order_mark_is_not_part_of_the_open_buffer_so_buffer_and_index_agree() {
        let mut state = SatzState::with_vault_root("/vault");
        state.open_document(
            "file:///a.md",
            "\u{feff}# T\n\n[[b]] here\n",
            Path::new("/vault/a.md"),
            1,
        );
        let open = &state.open_docs["file:///a.md"];
        let buffer = open.rope.to_string();
        let indexed = state.index.get_doc(&satz_core::DocId::new("a.md")).unwrap();
        assert_eq!(buffer, "# T\n\n[[b]] here\n");
        assert_eq!(
            buffer,
            indexed.line_index.source(),
            "the live text and the parsed text are the same text"
        );
        assert!(!state.has_stale_open_documents());
    }

    #[test]
    fn a_buffer_without_a_mark_and_one_that_is_only_a_mark_are_handled() {
        let mut state = SatzState::with_vault_root("/vault");
        state.open_document("file:///a.md", "# T\n", Path::new("/vault/a.md"), 1);
        assert_eq!(state.open_docs["file:///a.md"].rope.to_string(), "# T\n");
        state.open_document("file:///b.md", "\u{feff}", Path::new("/vault/b.md"), 1);
        assert_eq!(state.open_docs["file:///b.md"].rope.to_string(), "");
    }
}
