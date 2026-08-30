use std::collections::HashMap;

use tower_lsp_server::ls_types::{TextEdit, Uri, WorkspaceEdit};

use crate::convert::{line_edits_to_text_edits, path_to_uri};
use crate::state::SatzState;
use satz_core::formatter::diff::line_diff;

pub const FORMAT_WORKSPACE_COMMAND: &str = "satz.formatWorkspace";

/// One document's computed formatting result: its client URI, the full replacement text (used to
/// keep an open document's in-memory rope in sync after the client confirms the edit), and the
/// minimal set of line-range `TextEdit`s that turn its current content into the formatted version.
pub struct FormatChange {
    pub uri: Uri,
    pub formatted: String,
    pub edits: Vec<TextEdit>,
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
/// its current content. Returns everything empty (no-op) if `formatter.enabled` is false.
///
/// Consults `state.format_cache` first for each document's content hash — on a vault that's
/// already fully formatted, a repeat call does zero `format_document` work at all, just cache
/// hits that immediately compare equal to the source and get skipped.
pub fn compute_format_changes(state: &SatzState) -> FormatWorkspaceResult {
    if !state.config.formatter.enabled {
        return FormatWorkspaceResult {
            changes: Vec::new(),
            cache_updates: Vec::new(),
        };
    }

    let mut changes = Vec::new();
    let mut cache_updates = Vec::new();

    for doc in state.index.documents() {
        let source = doc.line_index.source();
        let hash = doc.content_hash;

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

        let doc_path = match &state.vault_root {
            Some(root) if !doc.path.is_absolute() => root.join(&doc.path),
            _ => doc.path.clone(),
        };
        let Some(uri) = path_to_uri(&doc_path) else {
            continue;
        };

        let line_edits = line_diff(source, &formatted);
        let edits = line_edits_to_text_edits(&doc.line_index, &line_edits);

        changes.push(FormatChange {
            uri,
            formatted,
            edits,
        });
    }

    FormatWorkspaceResult {
        changes,
        cache_updates,
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
            "1   \n2\n3\n4\n5   \n6\n7\n8   \n",
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
}
