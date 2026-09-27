use std::borrow::Cow;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use rayon::prelude::*;
use tokio::sync::RwLock;

use tower_lsp_server::ls_types::{
    DocumentChanges, Location, OneOf, OptionalVersionedTextDocumentIdentifier, TextDocumentEdit,
    TextEdit, Uri, WorkspaceEdit,
};

use crate::convert::{line_edits_to_text_edits, path_to_uri};
use crate::state::{CacheUpdate, SatzState, SelfLinks};
use satz_core::config::FormatterConfig;
use satz_core::formatter::diff::line_diff;

pub const FORMAT_WORKSPACE_COMMAND: &str = "satz.formatWorkspace";

pub const SHOW_BACKLINKS_COMMAND: &str = "satz.showBacklinks";

/// Every command `workspace/executeCommand` accepts (advertised in the server capabilities).
pub const SUPPORTED_COMMANDS: [&str; 2] = [FORMAT_WORKSPACE_COMMAND, SHOW_BACKLINKS_COMMAND];

/// The links in other notes that point at the note whose URI is the first argument (the same
/// notes the backlink CodeLens counts; a link from the note to itself is not a backlink).
/// `Err` carries the reason when the arguments are unusable; an unknown note gives no locations.
pub fn show_backlinks(
    state: &SatzState,
    arguments: &[serde_json::Value],
) -> Result<Vec<Location>, String> {
    let uri = arguments
        .first()
        .and_then(|a| a.as_str())
        .ok_or("satz.showBacklinks expects the note's URI as its first argument")?;
    let path = crate::convert::uri_to_path(uri)
        .ok_or_else(|| format!("satz.showBacklinks: '{uri}' is not a file URI"))?;
    let target = state.doc_id_for_path(&path);
    if state.index.get_doc(&target).is_none() {
        return Ok(Vec::new());
    }

    let mut locations = Vec::new();
    for source in state.documents_linking_to(&target, SelfLinks::Exclude) {
        let Some(source_uri) = state.doc_uri(source) else {
            continue;
        };
        for link in state.links_to(source, &target) {
            locations.push(Location::new(
                source_uri.clone(),
                crate::convert::byte_range_to_lsp(link.range, &source.line_index),
            ));
        }
    }
    crate::convert::sort_locations(&mut locations);
    Ok(locations)
}

/// Runs the commands that only read state. `None`: not one of them (the caller handles it, or
/// rejects it as unknown); `Some(Err(reason))`: unusable arguments.
pub fn run_read_only_command(
    state: &SatzState,
    command: &str,
    arguments: &[serde_json::Value],
) -> Option<Result<serde_json::Value, String>> {
    match command {
        SHOW_BACKLINKS_COMMAND => {
            Some(show_backlinks(state, arguments).map(|locations| {
                serde_json::to_value(locations).unwrap_or(serde_json::Value::Null)
            }))
        }
        _ => None,
    }
}

/// One document's formatting result, as it is sent with `workspace/applyEdit`: the URI the client
/// knows it by, the minimal line-range `TextEdit`s that turn its current content into the
/// formatted version, and the version of the open document they were computed against (`None`: a
/// file on disk). The server never applies them to its own copy of an open document: the client
/// applies them to its buffer and reports the change (see `execute_command` in `backend.rs`).
pub struct FormatChange {
    pub uri: Uri,
    pub edits: Vec<TextEdit>,
    pub version: Option<i32>,
}

/// Result of scanning the vault for formatting changes: the changes themselves (used to build
/// the `WorkspaceEdit`), and what was learned about the texts that were formatted, for the caller
/// to merge into `state.format_cache` (see `CacheUpdate`) — kept separate so this function only
/// needs `&SatzState` rather than requiring a write lock just to compute what to send.
pub struct FormatWorkspaceResult {
    pub changes: Vec<FormatChange>,
    pub cache_updates: Vec<CacheUpdate>,
}

/// What formatting one note needs to know, taken out of the state: the text (the live buffer of an
/// open note), what the cache says of it, and where the edits go. Borrowed from the state for a
/// pass that runs under the lock (nothing is copied), or owned for one that works after the lock
/// has been let go.
pub(crate) struct FormatInput<'a> {
    pub(crate) id: satz_core::DocId,
    pub(crate) source: Cow<'a, str>,
    pub(crate) hash: u64,
    /// What formatting this text made before, when the cache holds it.
    pub(crate) cached: Option<Cow<'a, str>>,
    /// The URI the client opened the note with, when it is open.
    pub(crate) open_uri: Option<Cow<'a, str>>,
    /// The version of the open buffer the text is from.
    pub(crate) version: Option<i32>,
    pub(crate) path: Cow<'a, Path>,
    /// The line index of `source`, when the index already holds it.
    pub(crate) line_index: Option<&'a satz_core::LineIndex>,
    /// The content hash the index holds for the note.
    pub(crate) indexed_hash: u64,
}

impl FormatInput<'_> {
    /// The same input, not borrowing from the state.
    pub(crate) fn into_owned(self) -> FormatInput<'static> {
        FormatInput {
            id: self.id,
            source: Cow::Owned(self.source.into_owned()),
            hash: self.hash,
            cached: self.cached.map(|c| Cow::Owned(c.into_owned())),
            open_uri: self.open_uri.map(|u| Cow::Owned(u.into_owned())),
            version: self.version,
            path: Cow::Owned(self.path.into_owned()),
            line_index: None,
            indexed_hash: self.indexed_hash,
        }
    }
}

/// What formatting one note came to.
pub(crate) struct FormatOutcome {
    pub(crate) change: Option<FormatChange>,
    pub(crate) cache_update: Option<CacheUpdate>,
}

/// A way to format a text: `format_document`, or (in tests) one that also does something else.
pub(crate) type Formatter = dyn Fn(&str, &FormatterConfig) -> String + Send + Sync;

/// The open notes of a state by the path they are compared by (`SatzState::path_key`), worked out
/// once: asking `open_doc_for_path` for every note goes through every open note each time.
pub(crate) struct OpenNotes<'a> {
    by_path: HashMap<String, &'a crate::state::OpenDocument>,
}

impl<'a> OpenNotes<'a> {
    pub(crate) fn of(state: &'a SatzState) -> Self {
        Self {
            by_path: state
                .open_docs
                .values()
                .map(|open| (state.path_key(&open.path), open))
                .collect(),
        }
    }

    fn get(&self, state: &SatzState, path: &Path) -> Option<&'a crate::state::OpenDocument> {
        if self.by_path.is_empty() {
            return None;
        }
        self.by_path.get(&state.path_key(path)).copied()
    }
}

/// What formatting `doc` needs, borrowed from the state; `None` for a note the cache knows to be
/// formatted already. An open note is formatted from its LIVE buffer: the index holds the text of the
/// last (debounced) reparse, which the user may already have typed past.
pub(crate) fn format_input<'a>(
    state: &'a SatzState,
    open: &OpenNotes<'a>,
    doc: &'a satz_core::Document,
) -> Option<FormatInput<'a>> {
    let open_doc = open.get(state, &doc.path);
    let (source, hash, line_index): (Cow<'a, str>, u64, Option<&'a satz_core::LineIndex>) =
        match open_doc {
            Some(open_doc) => {
                let text = open_doc.rope.to_string();
                let hash = satz_core::content_hash(&text);
                (Cow::Owned(text), hash, None)
            }
            None => (
                Cow::Borrowed(doc.line_index.source()),
                doc.content_hash,
                Some(&doc.line_index),
            ),
        };
    if state.format_cache.is_unchanged(hash) {
        return None; // known to be formatted already
    }
    Some(FormatInput {
        id: doc.id.clone(),
        source,
        hash,
        cached: state.format_cache.get(hash).map(Cow::Borrowed),
        open_uri: open_doc.map(|open_doc| Cow::Borrowed(open_doc.uri.as_str())),
        version: open_doc.map(|open_doc| open_doc.version),
        path: Cow::Borrowed(doc.path.as_path()),
        line_index,
        indexed_hash: doc.content_hash,
    })
}

/// Formats one note: what it comes to, and what was learned about its text.
pub(crate) fn format_one(
    input: &FormatInput,
    config: &FormatterConfig,
    vault_root: Option<&Path>,
    formatter: &Formatter,
) -> FormatOutcome {
    let source: &str = &input.source;
    // The text is borrowed from the cache, or computed here (and then moved into the cache
    // update below, not copied).
    let mut fresh: Option<String> = None;
    let formatted: &str = match &input.cached {
        Some(cached) => cached,
        None => &*fresh.insert(formatter(source, config)),
    };

    if formatted == source {
        // Already formatted: remembered by its hash, without a copy of the text.
        return FormatOutcome {
            change: None,
            cache_update: fresh.map(|_| CacheUpdate::Unchanged(input.hash)),
        };
    }

    // The client is addressed with the URI it opened the note with.
    let uri = match input
        .open_uri
        .as_deref()
        .and_then(|uri| uri.parse::<Uri>().ok())
    {
        Some(uri) => Some(uri),
        None => path_to_uri(&crate::state::absolute_path(&input.path, vault_root)),
    };
    let Some(uri) = uri else {
        // Nowhere to send it, but the text was worked out and is worth remembering.
        return FormatOutcome {
            change: None,
            cache_update: fresh.map(|text| CacheUpdate::Formatted(input.hash, text)),
        };
    };

    let line_edits = line_diff(source, formatted);
    let owned_index;
    let line_index = match input.line_index {
        Some(line_index) => line_index,
        None => {
            owned_index = satz_core::LineIndex::new(source);
            &owned_index
        }
    };
    let edits = line_edits_to_text_edits(line_index, &line_edits);
    FormatOutcome {
        change: Some(FormatChange {
            uri,
            edits,
            version: input.version,
        }),
        cache_update: fresh.map(|text| CacheUpdate::Formatted(input.hash, text)),
    }
}

/// Computes formatting changes for every indexed document whose formatted output differs from
/// its current content. Returns everything empty (no-op) if the formatter is disabled or `.satz.toml` is unusable.
///
/// Consults `state.format_cache` first for each document's content hash — on a vault that's
/// already fully formatted, a repeat call does zero `format_document` work at all, just cache
/// hits that immediately compare equal to the source and get skipped.
///
/// Works through the vault on the calling thread with the state borrowed: for a caller that holds
/// the state anyway. `format_workspace` is the one that lets the state go while it formats.
pub fn compute_format_changes(state: &SatzState) -> FormatWorkspaceResult {
    tracing::debug!(
        doc_count = state.index.doc_count(),
        "compute_format_changes: starting"
    );
    if !state.formatting_allowed() {
        return FormatWorkspaceResult {
            changes: Vec::new(),
            cache_updates: Vec::new(),
        };
    }

    let open = OpenNotes::of(state);
    let mut changes = Vec::new();
    let mut cache_updates = Vec::new();
    for doc in state.index.documents() {
        let Some(input) = format_input(state, &open, doc) else {
            continue;
        };
        let outcome = format_one(
            &input,
            &state.config.formatter,
            state.vault_root(),
            &satz_core::formatter::format_document,
        );
        cache_updates.extend(outcome.cache_update);
        changes.extend(outcome.change);
    }

    FormatWorkspaceResult {
        changes,
        cache_updates,
    }
}

/// How many notes `format_workspace` takes out of the state, and then works on, at a time.
pub const FORMAT_CHUNK: usize = 128;

/// A change waiting for the whole vault to be gone through, with what it was computed from.
struct PendingChange {
    id: satz_core::DocId,
    path: std::path::PathBuf,
    /// The content hash the index held for the note (a closed note).
    indexed_hash: u64,
    /// The URI and the version of the open buffer it was computed from (an open note).
    open: Option<(String, i32)>,
    change: FormatChange,
}

/// `compute_format_changes` for the running server: the state is not held while the notes are
/// formatted, and the work is shared between the cores.
///
/// The notes are taken a slice at a time: the state is read (for as long as it takes to look at a
/// few hundred notes in the cache and copy the texts that need work), let go, and the slice is
/// formatted on all cores. So a request that needs the state, or an edit that must write it, waits
/// for one slice's worth of reading and not for the whole vault. What comes of it is checked against
/// the state as it is by then: a change computed from a buffer that has been typed in since, or
/// from a file that has changed, is left out (the client would refuse it, or apply it to text it
/// does not fit); what was learned about texts (`cache_updates`) stays true whatever happened
/// since, as it is about the text and nothing else.
///
/// The changes come back in the order of their URIs.
pub async fn format_workspace(state: &Arc<RwLock<SatzState>>) -> FormatWorkspaceResult {
    format_workspace_with(
        state,
        FORMAT_CHUNK,
        Arc::new(satz_core::formatter::format_document),
    )
    .await
}

/// `format_workspace` with the size of a slice and the way a text is formatted chosen by the caller
/// (a test formats with something that also looks at the state).
pub(crate) async fn format_workspace_with(
    state: &Arc<RwLock<SatzState>>,
    chunk: usize,
    formatter: Arc<Formatter>,
) -> FormatWorkspaceResult {
    let (config, vault_root, mut ids) = {
        let s = state.read().await;
        if !s.formatting_allowed() {
            return FormatWorkspaceResult {
                changes: Vec::new(),
                cache_updates: Vec::new(),
            };
        }
        let ids: Vec<satz_core::DocId> = s.index.documents().map(|doc| doc.id.clone()).collect();
        (
            Arc::new(s.config.formatter.clone()),
            Arc::new(s.vault_root().map(Path::to_path_buf)),
            ids,
        )
    };
    ids.sort();

    let mut pending: Vec<PendingChange> = Vec::new();
    let mut cache_updates: Vec<CacheUpdate> = Vec::new();
    for slice in ids.chunks(chunk.max(1)) {
        let inputs: Vec<FormatInput<'static>> = {
            let s = state.read().await;
            let open = OpenNotes::of(&s);
            slice
                .iter()
                .filter_map(|id| s.index.get_doc(id))
                .filter_map(|doc| format_input(&s, &open, doc))
                .map(FormatInput::into_owned)
                .collect()
        };
        if inputs.is_empty() {
            continue;
        }

        let (config, vault_root, formatter) =
            (config.clone(), vault_root.clone(), formatter.clone());
        // Rayon work is started from a blocking thread, never from one of the runtime's workers.
        let done = tokio::task::spawn_blocking(move || {
            inputs
                .into_par_iter()
                .map(|input| {
                    let outcome = format_one(&input, &config, vault_root.as_deref(), &*formatter);
                    let pending = outcome.change.map(|change| PendingChange {
                        open: input
                            .open_uri
                            .as_deref()
                            .zip(input.version)
                            .map(|(uri, version)| (uri.to_string(), version)),
                        id: input.id,
                        path: input.path.into_owned(),
                        indexed_hash: input.indexed_hash,
                        change,
                    });
                    (pending, outcome.cache_update)
                })
                .collect::<Vec<_>>()
        })
        .await;
        match done {
            Ok(done) => {
                for (change, update) in done {
                    pending.extend(change);
                    cache_updates.extend(update);
                }
            }
            Err(error) => {
                tracing::warn!(%error, "format_workspace: a slice of the vault could not be formatted");
            }
        }
    }

    let mut changes = {
        let s = state.read().await;
        let open_paths = s.open_path_keys();
        pending
            .into_iter()
            .filter(|p| match &p.open {
                // The buffer is where it was when the edits were computed.
                Some((uri, version)) => s
                    .open_docs
                    .get(uri)
                    .is_some_and(|open| open.version == *version),
                // The file is what it was, and has not been opened since.
                None => {
                    s.index
                        .get_doc(&p.id)
                        .is_some_and(|doc| doc.content_hash == p.indexed_hash)
                        && !open_paths.contains(&s.path_key(&p.path))
                }
            })
            .map(|p| p.change)
            .collect::<Vec<_>>()
    };
    changes.sort_by(|a, b| a.uri.as_str().cmp(b.uri.as_str()));

    FormatWorkspaceResult {
        changes,
        cache_updates,
    }
}

/// Like `build_workspace_edit`, as versioned document edits: an open document names the version its
/// edits were computed against, so the client REJECTS them if the user has typed since, instead of
// applying them to text they do not fit. Files on disk carry no version.
pub fn build_workspace_edit_versioned(changes: Vec<FormatChange>) -> WorkspaceEdit {
    let edits: Vec<TextDocumentEdit> = changes
        .into_iter()
        .map(|change| TextDocumentEdit {
            text_document: OptionalVersionedTextDocumentIdentifier {
                uri: change.uri,
                version: change.version,
            },
            edits: change.edits.into_iter().map(OneOf::Left).collect(),
        })
        .collect();
    WorkspaceEdit {
        document_changes: Some(DocumentChanges::Edits(edits)),
        ..Default::default()
    }
}

/// Builds the `WorkspaceEdit` to send via `workspace/applyEdit` from a set of format changes, which
/// it takes over: nothing is copied.
pub fn build_workspace_edit(changes: Vec<FormatChange>) -> WorkspaceEdit {
    let mut map: HashMap<Uri, Vec<TextEdit>> = HashMap::with_capacity(changes.len());
    for change in changes {
        map.insert(change.uri, change.edits);
    }
    WorkspaceEdit {
        changes: Some(map),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests;
