use std::collections::HashMap;

use tower_lsp_server::ls_types::{
    DocumentChanges, Location, OneOf, OptionalVersionedTextDocumentIdentifier, TextDocumentEdit,
    TextEdit, Uri, WorkspaceEdit,
};

use crate::convert::{line_edits_to_text_edits, path_to_uri};
use crate::state::SatzState;
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
    let rel_path = SatzState::get_rel_path(&path, state.vault_root());
    let target = satz_core::DocId::new(rel_path.to_string_lossy().replace('\\', "/"));
    if state.index.get_doc(&target).is_none() {
        return Ok(Vec::new());
    }

    let mut locations = Vec::new();
    for source_id in state.index.incoming_from_others(&target) {
        let Some(source) = state.index.get_doc(source_id) else {
            continue;
        };
        let source_path = match state.vault_root() {
            Some(root) if !source.path.is_absolute() => root.join(&source.path),
            _ => source.path.clone(),
        };
        let Some(source_uri) = path_to_uri(&source_path) else {
            continue;
        };
        for link in &source.links {
            let points_here = matches!(
                state.resolve(link, source),
                satz_core::LinkResolution::Resolved { doc, .. }
                    | satz_core::LinkResolution::AnchorMissing { doc } if doc.id == target
            ) && link.kind != satz_core::LinkKind::Footnote
                && !link.target_doc.is_empty();
            if points_here {
                locations.push(Location::new(
                    source_uri.clone(),
                    crate::convert::byte_range_to_lsp(link.range, &source.line_index),
                ));
            }
        }
    }
    locations.sort_by(|a, b| {
        (a.uri.as_str(), a.range.start.line, a.range.start.character).cmp(&(
            b.uri.as_str(),
            b.range.start.line,
            b.range.start.character,
        ))
    });
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

/// One document's computed formatting result: its client URI, the full replacement text (used to
/// keep an open document's in-memory rope in sync after the client confirms the edit), and the
/// minimal set of line-range `TextEdit`s that turn its current content into the formatted version.
pub struct FormatChange {
    pub uri: Uri,
    pub formatted: String,
    pub edits: Vec<TextEdit>,
    /// Version of the open document the edits were computed against (`None`: a file on disk).
    pub version: Option<i32>,
    /// `content_hash` of the text the edits were computed against.
    pub source_hash: u64,
    /// The key of the open document in `SatzState::open_docs`, if it is open.
    pub open_key: Option<String>,
}

/// Result of scanning the vault for formatting changes: the changes themselves (used to build
/// the `WorkspaceEdit`), and any newly-computed `(content_hash, formatted_text)` pairs the caller
/// should merge into `state.format_cache` — kept separate so this function only needs `&SatzState`
/// rather than requiring a write lock just to compute what to send.
pub struct FormatWorkspaceResult {
    pub changes: Vec<FormatChange>,
    pub cache_updates: Vec<(u64, String)>,
}

/// Computes formatting changes for every indexed document whose formatted output differs from
/// its current content. Returns everything empty (no-op) if the formatter is disabled or `.satz.toml` is unusable.
///
/// Consults `state.format_cache` first for each document's content hash — on a vault that's
/// already fully formatted, a repeat call does zero `format_document` work at all, just cache
/// hits that immediately compare equal to the source and get skipped.
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

    let mut changes = Vec::new();
    let mut cache_updates = Vec::new();

    for doc in state.index.documents() {
        // An open document is formatted from its LIVE buffer: the index holds the text of the last
        // (debounced) reparse, which the user may already have typed past.
        let open = state.open_doc_for_path(&doc.path);
        let live_text: Option<String> = open.map(|(_, open_doc)| open_doc.rope.to_string());
        let live_index: Option<satz_core::LineIndex> =
            live_text.as_deref().map(satz_core::LineIndex::new);
        let (source, hash, line_index) = match (&live_text, &live_index) {
            (Some(text), Some(index)) => (text.as_str(), satz_core::content_hash(text), index),
            _ => (doc.line_index.source(), doc.content_hash, &doc.line_index),
        };
        if state.format_cache.is_unchanged(hash) {
            continue; // known to be formatted already
        }

        let formatted = match state.format_cache.get(hash) {
            Some(cached) => cached.to_string(),
            None => {
                let computed =
                    satz_core::formatter::format_document(source, &state.config.formatter);
                cache_updates.push((hash, computed.clone()));
                computed
            }
        };

        if formatted == source {
            continue;
        }

        // The client is addressed with the URI it opened the document with.
        let uri = match open.and_then(|(_, open_doc)| open_doc.uri.parse::<Uri>().ok()) {
            Some(uri) => uri,
            None => {
                let doc_path = match state.vault_root() {
                    Some(root) if !doc.path.is_absolute() => root.join(&doc.path),
                    _ => doc.path.clone(),
                };
                let Some(uri) = path_to_uri(&doc_path) else {
                    continue;
                };
                uri
            }
        };

        let line_edits = line_diff(source, &formatted);
        let edits = line_edits_to_text_edits(line_index, &line_edits);

        changes.push(FormatChange {
            uri,
            formatted,
            edits,
            version: open.map(|(_, open_doc)| open_doc.version),
            source_hash: hash,
            open_key: open.map(|(key, _)| key.clone()),
        });
    }

    FormatWorkspaceResult {
        changes,
        cache_updates,
    }
}

/// Like `build_workspace_edit`, as versioned document edits: an open document names the version its
/// edits were computed against, so the client REJECTS them if the user has typed since, instead of
// applying them to text they do not fit. Files on disk carry no version.
pub fn build_workspace_edit_versioned(changes: &[FormatChange]) -> WorkspaceEdit {
    let edits: Vec<TextDocumentEdit> = changes
        .iter()
        .map(|change| TextDocumentEdit {
            text_document: OptionalVersionedTextDocumentIdentifier {
                uri: change.uri.clone(),
                version: change.version,
            },
            edits: change.edits.iter().cloned().map(OneOf::Left).collect(),
        })
        .collect();
    WorkspaceEdit {
        document_changes: Some(DocumentChanges::Edits(edits)),
        ..Default::default()
    }
}

/// Builds the `WorkspaceEdit` to send via `workspace/applyEdit` from a set of format changes.
pub fn build_workspace_edit(changes: &[FormatChange]) -> WorkspaceEdit {
    let mut map: HashMap<Uri, Vec<TextEdit>> = HashMap::with_capacity(changes.len());
    for change in changes {
        map.insert(change.uri.clone(), change.edits.clone());
    }
    WorkspaceEdit {
        changes: Some(map),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use satz_core::Index;
    use satz_core::parse_document;
    use std::path::Path;

    fn state_with(docs: Vec<satz_core::Document>) -> SatzState {
        // path_to_uri (via Uri::from_file_path) requires an absolute vault root to succeed.
        let root = if cfg!(windows) {
            Path::new("C:\\").to_path_buf()
        } else {
            Path::new("/").to_path_buf()
        };
        let mut state = SatzState::default();
        state.index = Index::build(docs);
        state.set_vault_root(Some(root));
        state
    }

    #[test]
    fn test_only_dirty_documents_produce_changes() {
        let dirty = parse_document("Line 1   \n\n\n\nLine 2   ", Path::new("dirty.md"));
        let clean = parse_document("# Clean\n\nAlready tidy.\n", Path::new("clean.md"));
        let state = state_with(vec![dirty, clean]);

        let result = compute_format_changes(&state);
        assert_eq!(
            result.changes.len(),
            1,
            "only the dirty document should need an edit"
        );
        assert!(result.changes[0].uri.as_str().ends_with("dirty.md"));
        assert_eq!(result.changes[0].formatted, "Line 1\n\nLine 2\n");
        // Both documents were freshly computed (cold cache), so both hashes get recorded.
        assert_eq!(result.cache_updates.len(), 2);
    }

    #[test]
    fn test_no_changes_when_vault_already_clean() {
        let clean_a = parse_document("# A\n\nTidy.\n", Path::new("a.md"));
        let clean_b = parse_document("# B\n\nAlso tidy.\n", Path::new("b.md"));
        let state = state_with(vec![clean_a, clean_b]);

        let result = compute_format_changes(&state);
        assert!(result.changes.is_empty());
        assert_eq!(result.cache_updates.len(), 2);
    }

    #[test]
    fn workspace_format_is_off_while_the_config_is_invalid_and_back_on_once_fixed() {
        let dirty = parse_document("Line 1   \n\n\n\nLine 2   ", Path::new("dirty.md"));
        let mut state = state_with(vec![dirty]);

        // Control: with a valid config the dirty document produces a change.
        assert_eq!(compute_format_changes(&state).changes.len(), 1);

        state.config_error = Some("invalid .satz.toml: line 1".to_string());
        let result = compute_format_changes(&state);
        assert!(
            result.changes.is_empty(),
            "no edits with an unusable config"
        );
        assert!(result.cache_updates.is_empty(), "and nothing cached either");

        state.config_error = None;
        assert_eq!(compute_format_changes(&state).changes.len(), 1);
    }

    #[test]
    fn test_disabled_formatter_produces_no_changes() {
        let dirty = parse_document("Line 1   \n\n\n\nLine 2   ", Path::new("dirty.md"));
        let mut state = state_with(vec![dirty]);
        state.config.formatter.enabled = false;

        let result = compute_format_changes(&state);
        assert!(result.changes.is_empty());
        assert!(result.cache_updates.is_empty());
    }

    #[test]
    fn test_workspace_edit_has_one_entry_per_changed_document() {
        let dirty_a = parse_document("A   \n", Path::new("a.md"));
        let dirty_b = parse_document("B   \n", Path::new("b.md"));
        let clean = parse_document("# C\n\nTidy.\n", Path::new("c.md"));
        let state = state_with(vec![dirty_a, dirty_b, clean]);

        let result = compute_format_changes(&state);
        assert_eq!(result.changes.len(), 2);

        let edit = build_workspace_edit(&result.changes);
        let map = edit.changes.expect("changes map expected");
        assert_eq!(map.len(), 2);
    }

    #[test]
    fn test_scattered_changes_produce_multiple_minimal_edits_not_one_blob() {
        let dirty = parse_document(
            "1 \t \n2\n3\n4\n5 \t \n6\n7\n8   \n",
            Path::new("scattered.md"),
        );
        let state = state_with(vec![dirty]);

        let result = compute_format_changes(&state);
        assert_eq!(result.changes.len(), 1);
        assert!(
            result.changes[0].edits.len() > 1,
            "scattered single-line changes must not collapse into one whole-document edit, got {:?}",
            result.changes[0].edits
        );
    }

    #[test]
    fn test_cache_hit_skips_recomputation_and_is_consistent() {
        let dirty = parse_document("Line 1   \n\n\n\nLine 2   ", Path::new("dirty.md"));
        let content_hash = dirty.content_hash;
        let mut state = state_with(vec![dirty]);

        // Prime the cache as if a previous call had already computed this exact result.
        state
            .format_cache
            .insert(content_hash, "Line 1\n\nLine 2\n".to_string());

        let result = compute_format_changes(&state);
        assert_eq!(result.changes.len(), 1);
        assert_eq!(result.changes[0].formatted, "Line 1\n\nLine 2\n");
        // Served entirely from cache: nothing new to record.
        assert!(result.cache_updates.is_empty());
    }

    #[test]
    fn test_second_call_on_unchanged_vault_produces_no_new_cache_entries() {
        let dirty = parse_document("Line 1   \n\n\n\nLine 2   ", Path::new("dirty.md"));
        let mut state = state_with(vec![dirty]);

        let first = compute_format_changes(&state);
        assert_eq!(first.cache_updates.len(), 1);
        for (hash, formatted) in first.cache_updates {
            state.format_cache.insert(hash, formatted);
        }

        // Second call, same state (as if the client declined to apply / vault re-scanned):
        // every document should now be a cache hit.
        let second = compute_format_changes(&state);
        assert_eq!(
            second.changes.len(),
            1,
            "still reports the same needed edit"
        );
        assert!(
            second.cache_updates.is_empty(),
            "nothing new to compute on a warm cache"
        );
    }

    fn backlinks_of_a(
        files: &[(&str, &str)],
        arg: serde_json::Value,
    ) -> Result<Vec<Location>, String> {
        let state = state_with(
            files
                .iter()
                .map(|(p, t)| parse_document(t, Path::new(p)))
                .collect(),
        );
        show_backlinks(&state, &[arg])
    }

    fn uri_for(state_root_file: &str) -> String {
        let root = if cfg!(windows) {
            Path::new("C:\\").to_path_buf()
        } else {
            Path::new("/").to_path_buf()
        };
        path_to_uri(&root.join(state_root_file))
            .unwrap()
            .as_str()
            .to_string()
    }

    #[test]
    fn both_commands_are_advertised() {
        assert!(SUPPORTED_COMMANDS.contains(&FORMAT_WORKSPACE_COMMAND));
        assert!(SUPPORTED_COMMANDS.contains(&SHOW_BACKLINKS_COMMAND));
        assert_eq!(SHOW_BACKLINKS_COMMAND, "satz.showBacklinks");
    }

    #[test]
    fn backlinks_are_the_links_in_other_notes_that_point_at_the_note() {
        let files = [
            ("a.md", "# A\n\nself [[a]]\n"),
            ("b.md", "# B\n\nsee [[a]] and [t](a.md)\n"),
            ("c.md", "# C\n\n[[b]] only\n"),
            ("d.md", "# D\n\n[[a#Yok]] and [x](https://example.com)\n"),
        ];
        let found = backlinks_of_a(&files, serde_json::json!(uri_for("a.md"))).unwrap();
        let mut spots: Vec<(String, u32, u32, u32)> = found
            .iter()
            .map(|l| {
                (
                    l.uri.as_str().rsplit('/').next().unwrap().to_string(),
                    l.range.start.line,
                    l.range.start.character,
                    l.range.end.character,
                )
            })
            .collect();
        spots.sort();
        // b.md: `[[a]]` at 4..9 and `[t](a.md)` at 14..23; d.md: `[[a#Yok]]`; never a.md itself.
        assert_eq!(
            spots,
            vec![
                ("b.md".to_string(), 2, 4, 9),
                ("b.md".to_string(), 2, 14, 23),
                ("d.md".to_string(), 2, 0, 9),
            ]
        );
    }

    #[test]
    fn the_answer_covers_the_same_notes_the_lens_counts() {
        let files = [
            ("a.md", "# A\n"),
            ("b.md", "[[a]] [[a]]\n"),
            ("c.md", "[[a]]\n"),
            ("d.md", "no link\n"),
        ];
        let state = state_with(
            files
                .iter()
                .map(|(p, t)| parse_document(t, Path::new(p)))
                .collect(),
        );
        let found = show_backlinks(&state, &[serde_json::json!(uri_for("a.md"))]).unwrap();
        let notes: std::collections::BTreeSet<&str> =
            found.iter().map(|l| l.uri.as_str()).collect();
        let id = satz_core::DocId::new("a.md");
        assert_eq!(notes.len(), state.index.incoming_from_others(&id).count());
        assert_eq!(found.len(), 3, "one location per link");
    }

    #[test]
    fn an_unknown_note_or_a_note_without_backlinks_gives_an_empty_answer() {
        let files = [("a.md", "# A\n"), ("b.md", "# B\n")];
        assert_eq!(
            backlinks_of_a(&files, serde_json::json!(uri_for("nope.md"))).unwrap(),
            vec![]
        );
        assert_eq!(
            backlinks_of_a(&files, serde_json::json!(uri_for("a.md"))).unwrap(),
            vec![]
        );
    }

    #[test]
    fn bad_arguments_are_rejected_with_a_reason() {
        let state = state_with(vec![parse_document("# A\n", Path::new("a.md"))]);
        for args in [
            vec![],
            vec![serde_json::json!(42)],
            vec![serde_json::json!(null)],
            vec![serde_json::json!(["a"])],
            vec![serde_json::json!("")],
            vec![serde_json::json!("not a uri at all")],
        ] {
            let err = show_backlinks(&state, &args).unwrap_err();
            assert!(!err.is_empty(), "{args:?}");
        }
    }

    #[test]
    fn a_read_only_command_answers_with_a_json_array_of_locations() {
        let state = state_with(vec![
            parse_document("# A\n", Path::new("a.md")),
            parse_document("see [[a]]\n", Path::new("b.md")),
        ]);
        let answer = run_read_only_command(
            &state,
            SHOW_BACKLINKS_COMMAND,
            &[serde_json::json!(uri_for("a.md"))],
        )
        .expect("the command is handled")
        .expect("valid arguments");
        let list = answer.as_array().expect("an array");
        assert_eq!(list.len(), 1);
        assert!(list[0]["uri"].as_str().unwrap().ends_with("b.md"));
        assert_eq!(list[0]["range"]["start"]["line"], 0);
        assert_eq!(list[0]["range"]["start"]["character"], 4);
        assert_eq!(list[0]["range"]["end"]["character"], 9);
    }

    #[test]
    fn bad_arguments_are_an_error_and_other_commands_are_not_handled_here() {
        let state = state_with(vec![parse_document("# A\n", Path::new("a.md"))]);
        let err = run_read_only_command(&state, SHOW_BACKLINKS_COMMAND, &[])
            .expect("handled")
            .unwrap_err();
        assert!(!err.is_empty());
        for other in [
            FORMAT_WORKSPACE_COMMAND,
            "satz.unknown",
            "",
            "SATZ.SHOWBACKLINKS",
        ] {
            assert!(
                run_read_only_command(&state, other, &[]).is_none(),
                "{other:?}"
            );
        }
    }

    #[test]
    fn every_advertised_command_is_handled_somewhere() {
        let state = state_with(vec![parse_document("# A\n", Path::new("a.md"))]);
        for command in SUPPORTED_COMMANDS {
            let read_only = run_read_only_command(&state, command, &[]).is_some();
            assert!(
                read_only || command == FORMAT_WORKSPACE_COMMAND,
                "{command} is advertised but nothing handles it"
            );
        }
    }

    // ---- open documents: the live buffer is formatted, and the edit carries its version ----

    fn root_dir() -> std::path::PathBuf {
        if cfg!(windows) {
            std::path::PathBuf::from("C:\\")
        } else {
            std::path::PathBuf::from("/")
        }
    }

    /// A vault with `a.md` open at `version` whose buffer is `buffer`, while the index still holds
    /// `indexed` (the debounced reparse has not run yet).
    fn open_state(indexed: &str, buffer: &str, version: i32) -> (SatzState, String) {
        let mut state = SatzState::default();
        state.set_vault_root(Some(root_dir()));
        let uri = uri_for("a.md");
        state.open_document(&uri, indexed, &root_dir().join("a.md"), 1);
        let open = state.open_docs.get_mut(&uri).unwrap();
        open.rope = ropey::Rope::from_str(buffer);
        open.version = version;
        (state, uri)
    }

    #[test]
    fn an_open_document_is_formatted_from_its_live_buffer_not_the_stale_index() {
        let (state, uri) = open_state(
            "# A\n\nclean\n",
            "# A\n\ntyped just now   \n\n\n\nmore   \n",
            7,
        );
        let result = compute_format_changes(&state);
        assert_eq!(result.changes.len(), 1);
        let change = &result.changes[0];
        assert_eq!(change.formatted, "# A\n\ntyped just now\n\nmore\n");
        assert_eq!(change.version, Some(7));
        assert_eq!(change.open_key.as_deref(), Some(uri.as_str()));
        // The edits turn exactly the live text into the formatted text.
        let live = "# A\n\ntyped just now   \n\n\n\nmore   \n";
        assert_eq!(
            crate::convert::apply_text_edits(live, &change.edits),
            change.formatted
        );
    }

    #[test]
    fn an_open_document_that_is_already_formatted_produces_nothing_even_if_the_index_is_dirty() {
        let (state, _) = open_state("dirty   \n\n\n\nindex", "# A\n\nclean\n", 4);
        assert!(compute_format_changes(&state).changes.is_empty());
    }

    #[test]
    fn a_closed_document_is_formatted_from_the_index_and_has_no_version() {
        let dirty = parse_document("Line 1   \n\n\n\nLine 2   ", Path::new("dirty.md"));
        let state = state_with(vec![dirty]);
        let result = compute_format_changes(&state);
        assert_eq!(result.changes.len(), 1);
        assert_eq!(result.changes[0].version, None);
        assert_eq!(result.changes[0].open_key, None);
    }

    #[test]
    fn a_crlf_buffer_is_formatted_like_its_lf_twin_and_the_edits_fit_the_live_text() {
        let live = "# A\r\n\r\ntyped   \r\n\r\n\r\n\r\nmore\r\n";
        let (state, _) = open_state("# A\n", live, 2);
        let change = &compute_format_changes(&state).changes[0];
        assert_eq!(change.formatted, "# A\r\n\r\ntyped\r\n\r\nmore\r\n");
        assert_eq!(
            crate::convert::apply_text_edits(live, &change.edits),
            change.formatted
        );
    }

    #[test]
    fn the_client_is_addressed_with_the_uri_it_opened_the_document_with() {
        // The client spells a Windows drive differently from how a path turns back into a URI.
        let client_uri = "file:///c%3A/notes/a.md".to_string();
        let path = if cfg!(windows) {
            std::path::PathBuf::from("C:\\notes\\a.md")
        } else {
            std::path::PathBuf::from("/notes/a.md")
        };
        let mut state = SatzState::default();
        state.set_vault_root(path.parent().map(|p| p.to_path_buf()));
        state.open_document(&client_uri, "dirty   \n\n\n\nx\n", &path, 3);
        let change = &compute_format_changes(&state).changes[0];
        assert_eq!(
            change.uri.as_str(),
            client_uri,
            "the open document's own URI"
        );
        assert_eq!(change.open_key.as_deref(), Some(client_uri.as_str()));
    }

    #[test]
    fn the_versioned_edit_names_the_version_of_open_documents_only() {
        use tower_lsp_server::ls_types::{DocumentChanges, OneOf};
        let (mut state, uri) = open_state("# A\n", "# A\n\n\n\ntext   \n", 9);
        state.index.replace_doc(parse_document(
            "Line 1   \n\n\n\nLine 2   ",
            Path::new("closed.md"),
        ));
        let changes = compute_format_changes(&state).changes;
        assert_eq!(changes.len(), 2);

        let edit = build_workspace_edit_versioned(&changes);
        assert!(edit.changes.is_none(), "only the versioned form is used");
        let Some(DocumentChanges::Edits(edits)) = edit.document_changes else {
            panic!("expected versioned text document edits");
        };
        assert_eq!(edits.len(), 2);
        for e in &edits {
            let is_open = e.text_document.uri.as_str() == uri;
            assert_eq!(
                e.text_document.version,
                if is_open { Some(9) } else { None }
            );
            assert!(!e.edits.is_empty());
            assert!(e.edits.iter().all(|edit| matches!(edit, OneOf::Left(_))));
        }
        // The same text edits as the unversioned form, file for file.
        let plain = build_workspace_edit(&changes).changes.unwrap();
        for e in &edits {
            let plain_edits = &plain[&e.text_document.uri];
            let versioned: Vec<_> = e
                .edits
                .iter()
                .map(|o| match o {
                    OneOf::Left(t) => t.clone(),
                    OneOf::Right(a) => a.text_edit.clone(),
                })
                .collect();
            assert_eq!(&versioned, plain_edits);
        }
    }

    #[test]
    fn the_cache_is_keyed_by_the_text_that_was_formatted() {
        let (mut state, _) = open_state("# A\n", "# A\n\n\n\ntext   \n", 2);
        let result = compute_format_changes(&state);
        state.apply_format_cache_updates(result.cache_updates);
        // The live buffer's hash is remembered (not pruned as "no document has this text").
        let again = compute_format_changes(&state);
        assert_eq!(again.changes.len(), 1);
        assert!(again.cache_updates.is_empty(), "served from the cache");
    }

    #[test]
    fn a_buffers_cache_entry_survives_the_next_pass() {
        let (mut state, _) = open_state("# A\n", "# A\n\n\n\ntext   \n", 2);
        for pass in 0..3 {
            let result = compute_format_changes(&state);
            assert_eq!(result.changes.len(), 1, "pass {pass}");
            if pass > 0 {
                assert!(
                    result.cache_updates.is_empty(),
                    "pass {pass} is served from the cache"
                );
            }
            state.apply_format_cache_updates(result.cache_updates);
        }
        assert_eq!(state.format_cache.len(), 1);
    }

    #[test]
    fn a_formatted_buffer_is_remembered_as_unchanged() {
        let (mut state, _) = open_state("dirty   \n\n\n\nindex", "# A\n\nclean\n", 2);
        let result = compute_format_changes(&state);
        assert!(result.changes.is_empty());
        state.apply_format_cache_updates(result.cache_updates);
        let hash = satz_core::content_hash("# A\n\nclean\n");
        assert!(state.format_cache.is_unchanged(hash));
    }

    #[test]
    fn the_edit_is_meant_for_the_buffer_it_was_computed_from_not_for_formatted_text() {
        // Why the server leaves an open buffer alone after `applyEdit`: the client reports the edit
        // as a `didChange`, and the same edit applied to text that is already formatted is wrong.
        let live = "# A\n\n\n\ntext   \n\n\nmore   \n";
        let (state, _) = open_state("# A\n", live, 2);
        let change = &compute_format_changes(&state).changes[0];
        assert_eq!(
            crate::convert::apply_text_edits(live, &change.edits),
            change.formatted
        );
        assert_ne!(
            crate::convert::apply_text_edits(&change.formatted, &change.edits),
            change.formatted
        );
    }
}
