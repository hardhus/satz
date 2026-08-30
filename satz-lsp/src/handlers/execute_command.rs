use std::collections::HashMap;

use tower_lsp_server::ls_types::{Position, Range, TextEdit, Uri, WorkspaceEdit};

use crate::convert::path_to_uri;
use crate::state::SatzState;

pub const FORMAT_WORKSPACE_COMMAND: &str = "satz.formatWorkspace";

/// One document's computed formatting result: its client URI, the full replacement text (used to
/// keep an open document's in-memory rope in sync after the client confirms the edit), and the
/// whole-document `TextEdit` that replaces its current content with the formatted version.
pub struct FormatChange {
    pub uri: Uri,
    pub formatted: String,
    pub edit: TextEdit,
}

/// Computes formatting changes for every indexed document whose formatted output differs from
/// its current content. Returns an empty list (no-op) if `formatter.enabled` is false or nothing
/// in the vault actually needs reformatting.
pub fn compute_format_changes(state: &SatzState) -> Vec<FormatChange> {
    if !state.config.formatter.enabled {
        return Vec::new();
    }

    let mut changes = Vec::new();

    for doc in state.index.documents() {
        let source = doc.line_index.source();
        let formatted = satz_core::formatter::format_document(source, &state.config.formatter);
        if formatted == source {
            continue;
        }

        let doc_path = match &state.vault_root {
            Some(root) if !doc.path.is_absolute() => root.join(&doc.path),
            _ => doc.path.clone(),
        };
        let Some(uri) = path_to_uri(&doc_path) else {
            continue;
        };

        let end = doc.line_index.byte_to_position(source.len());
        let edit = TextEdit {
            range: Range::new(Position::new(0, 0), Position::new(end.line, end.character)),
            new_text: formatted.clone(),
        };

        changes.push(FormatChange {
            uri,
            formatted,
            edit,
        });
    }

    changes
}

/// Builds the `WorkspaceEdit` to send via `workspace/applyEdit` from a set of format changes.
pub fn build_workspace_edit(changes: &[FormatChange]) -> WorkspaceEdit {
    let mut map: HashMap<Uri, Vec<TextEdit>> = HashMap::with_capacity(changes.len());
    for change in changes {
        map.insert(change.uri.clone(), vec![change.edit.clone()]);
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
        SatzState {
            index: Index::build(docs),
            vault_root: Some(root),
            ..Default::default()
        }
    }

    #[test]
    fn test_only_dirty_documents_produce_changes() {
        let dirty = parse_document("Line 1   \n\n\n\nLine 2   ", Path::new("dirty.md"));
        let clean = parse_document("# Clean\n\nAlready tidy.\n", Path::new("clean.md"));
        let state = state_with(vec![dirty, clean]);

        let changes = compute_format_changes(&state);
        assert_eq!(
            changes.len(),
            1,
            "only the dirty document should need an edit"
        );
        assert!(changes[0].uri.as_str().ends_with("dirty.md"));
        assert_eq!(changes[0].formatted, "Line 1\n\nLine 2\n");
    }

    #[test]
    fn test_no_changes_when_vault_already_clean() {
        let clean_a = parse_document("# A\n\nTidy.\n", Path::new("a.md"));
        let clean_b = parse_document("# B\n\nAlso tidy.\n", Path::new("b.md"));
        let state = state_with(vec![clean_a, clean_b]);

        assert!(compute_format_changes(&state).is_empty());
    }

    #[test]
    fn test_disabled_formatter_produces_no_changes() {
        let dirty = parse_document("Line 1   \n\n\n\nLine 2   ", Path::new("dirty.md"));
        let mut state = state_with(vec![dirty]);
        state.config.formatter.enabled = false;

        assert!(compute_format_changes(&state).is_empty());
    }

    #[test]
    fn test_workspace_edit_has_one_entry_per_changed_document() {
        let dirty_a = parse_document("A   \n", Path::new("a.md"));
        let dirty_b = parse_document("B   \n", Path::new("b.md"));
        let clean = parse_document("# C\n\nTidy.\n", Path::new("c.md"));
        let state = state_with(vec![dirty_a, dirty_b, clean]);

        let changes = compute_format_changes(&state);
        assert_eq!(changes.len(), 2);

        let edit = build_workspace_edit(&changes);
        let map = edit.changes.expect("changes map expected");
        assert_eq!(map.len(), 2);
        for edits in map.values() {
            assert_eq!(
                edits.len(),
                1,
                "each document gets exactly one whole-document edit"
            );
        }
    }
}
