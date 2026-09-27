use std::collections::HashMap;
use std::path::{Path, PathBuf};

use ropey::Rope;
use satz_core::{Index, VaultConfig, walk_vault_with};
use tower_lsp_server::ls_types::{TextDocumentContentChangeEvent, Uri};

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

/// How long to wait before re-parsing after a change: `debounce` since the last change, but not
/// past `max_wait` since the FIRST change of a series -- typing without a pause must not postpone
/// the reparse for ever. `elapsed` is the time since that first change.
pub fn debounce_delay(
    debounce: std::time::Duration,
    max_wait: std::time::Duration,
    elapsed: std::time::Duration,
) -> std::time::Duration {
    debounce.min(max_wait.saturating_sub(elapsed))
}

/// What is remembered about one content hash.
#[derive(Debug)]
enum CachedFormat {
    /// The document is already formatted: nothing needs to be kept.
    Unchanged,
    /// The formatted text, which differs from the source.
    Changed(String),
}

/// What a workspace-format pass learned about one text, for the cache: that the text is already
/// formatted (nothing else is kept), or what it formats to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CacheUpdate {
    /// The text with this content hash is already formatted.
    Unchanged(u64),
    /// The text with this content hash formats to this text.
    Formatted(u64, String),
}

/// Cache mapping a document's content hash to what formatting it produced, used by
/// `satz.formatWorkspace` to skip reformatting files whose content has not changed since the last
/// workspace-format call. Already formatted documents are remembered without a copy of their text.
/// Once at capacity, new distinct hashes are not cached (existing entries keep serving hits);
/// `retain_hashes` is how entries of documents that no longer exist are dropped, so the capacity
/// always goes to current content.
#[derive(Debug)]
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

/// Whether a note that links to itself counts as one of "the notes that link to it", for
/// `SatzState::documents_linking_to`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelfLinks {
    /// The tables' own rule (`backlinks_of`): a note that links to itself is one of its own
    /// backlinks.
    Include,
    /// What is reported to a user as "notes that link here" (`incoming_from_others`): a link a
    /// note has to itself is not read as someone else referencing it.
    Exclude,
}

/// `path`, as an absolute path: joined onto `root`, or `path` itself when there is no root. Every
/// call site this replaces used to guard the join with `!path.is_absolute()` first, as if an
/// already-absolute `path` had to be protected from `root` -- but `Path::join` already discards
/// `root` for an absolute `path` (and, on Windows, for one that merely carries its own drive or
/// root), so the guard never changed the result; the test below tries every shape that guard
/// distinguished (relative, `/abs`, a drive root, a UNC path, root-relative `\x`, drive-relative
/// `C:x`, no root, empty) and none of them do. A free function, not a method, for the handful of
/// callers that have a vault root without a whole `SatzState` at hand.
pub fn absolute_path(path: &Path, root: Option<&Path>) -> PathBuf {
    root.map_or_else(|| path.to_path_buf(), |root| root.join(path))
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
    /// content; a text that is already formatted is remembered as such without a copy.
    pub fn apply_format_cache_updates(&mut self, updates: Vec<CacheUpdate>) {
        // Every text a note has now: what the index holds, and, for an open note, its buffer (which
        // may be newer than the index). Only the hashes are kept.
        let mut live: std::collections::HashSet<u64> =
            self.index.documents().map(|d| d.content_hash).collect();
        live.extend(
            self.open_docs
                .values()
                .map(|open| satz_core::content_hash(&open.rope.to_string())),
        );
        self.format_cache.retain_hashes(&live);
        for update in updates {
            match update {
                CacheUpdate::Unchanged(hash) => self.format_cache.insert_unchanged(hash),
                CacheUpdate::Formatted(hash, formatted) => {
                    self.format_cache.insert(hash, formatted)
                }
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

        let docs = walk_vault_with(&vault_root, config.gitignore_mode())?;
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
        let wanted = self.path_key(path);
        self.open_docs
            .iter()
            .find(|(_, d)| self.path_key(&d.path) == wanted)
    }

    /// What `is_open_path` compares paths by: the path relative to the vault, with `/` for a
    /// separator, in the folded spelling (`fold_key`).
    pub fn path_key(&self, path: &Path) -> String {
        satz_core::slug::fold_key(self.doc_id_for_path(path).as_str())
    }

    /// The id the index would know a note at `path` by: `path`, made relative to the vault root
    /// (as `get_rel_path` does) and spelled the way every `DocId` is (`/`, never `\`).
    pub fn doc_id_for_path(&self, path: &Path) -> satz_core::DocId {
        satz_core::DocId::from_path(&Self::get_rel_path(path, self.vault_root.as_deref()))
    }

    /// The `path_key` of every open document, worked out once: for asking about many paths (or
    /// notes) whether they are open, which `is_open_path` answers by going through every open
    /// document each time.
    pub fn open_path_keys(&self) -> std::collections::HashSet<String> {
        self.open_docs
            .values()
            .map(|d| self.path_key(&d.path))
            .collect()
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

    /// The links of `doc` that resolve to `target` -- what `references`, `document_highlight` and
    /// `rename` each rewrite or report, filtered further by what they are (a link to the whole
    /// note, to one of its headings, to one of its blocks).
    pub fn links_to<'a>(
        &'a self,
        doc: &'a satz_core::Document,
        target: &'a satz_core::DocId,
    ) -> impl Iterator<Item = &'a satz_core::Link> + 'a {
        doc.links
            .iter()
            .filter(move |link| self.link_target_doc(doc, link) == Some(target))
    }

    /// The indexed documents that link to `target`: exactly `target` itself (once, whether or not
    /// it links to itself) plus everything `backlinks_of` names, or -- with `SelfLinks::Exclude`
    /// -- the notes users are told link to it, `incoming_from_others`. A document `backlinks_of`
    /// names but the index no longer holds (should not happen; the tables are kept in step with
    /// it) is skipped rather than panicking.
    pub fn documents_linking_to<'a>(
        &'a self,
        target: &'a satz_core::DocId,
        self_links: SelfLinks,
    ) -> impl Iterator<Item = &'a satz_core::Document> + 'a {
        let ids: Box<dyn Iterator<Item = &'a satz_core::DocId>> = match self_links {
            SelfLinks::Include => Box::new(
                self.index
                    .backlinks_of(target)
                    .filter(move |id| *id != target)
                    .chain(std::iter::once(target)),
            ),
            SelfLinks::Exclude => Box::new(self.index.incoming_from_others(target)),
        };
        ids.filter_map(move |id| self.index.get_doc(id))
    }

    /// The open document for `uri` together with its entry in the index (`None` when it is not open
    /// or not indexed) -- the lookup every handler starts with.
    pub fn doc_for_uri(&self, uri: &str) -> Option<(&OpenDocument, &satz_core::Document)> {
        let open_doc = self.open_docs.get(uri)?;
        let doc_id = self.doc_id_for_path(&open_doc.path);
        Some((open_doc, self.index.get_doc(&doc_id)?))
    }

    /// `doc.path`, made absolute against the vault root: as it is on disk. Every handler that
    /// sends the client a file location (a definition, a link target, a rename, a diagnostic's
    /// document) starts here, so they agree on where a note actually is.
    pub fn doc_path(&self, doc: &satz_core::Document) -> PathBuf {
        absolute_path(&doc.path, self.vault_root())
    }

    /// The URI a client would open `doc` with (`None`: `doc.path` cannot be turned into a file
    /// URI, e.g. a relative path with no vault root to anchor it to).
    pub fn doc_uri(&self, doc: &satz_core::Document) -> Option<Uri> {
        crate::convert::path_to_uri(&self.doc_path(doc))
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
        let doc_id = satz_core::DocId::from_path(&rel_path);
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
        let doc_id = satz_core::DocId::from_path(&rel_path);
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
mod tests;
