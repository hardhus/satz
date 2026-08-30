use crate::convert::line_edits_to_text_edits;
use crate::state::SatzState;
use satz_core::formatter::diff::line_diff;
use tower_lsp_server::ls_types::{DocumentFormattingParams, TextEdit};

/// Formats a document according to the vault's FormatterConfig, returning minimal line-range
/// `TextEdit`s (via a line-based diff) rather than one edit replacing the whole document — this
/// keeps the editor's undo history and the LSP payload proportional to what actually changed.
pub fn formatting(params: DocumentFormattingParams, state: &SatzState) -> Option<Vec<TextEdit>> {
    if !state.config.formatter.enabled {
        return Some(vec![]);
    }

    let uri = params.text_document.uri.as_str();
    let open_doc = state.open_docs.get(uri)?;
    let original = open_doc.rope.to_string();

    let formatted = satz_core::formatter::format_document(&original, &state.config.formatter);

    if formatted == original {
        return Some(vec![]);
    }

    let line_index = satz_core::LineIndex::new(&original);
    let edits = line_diff(&original, &formatted);
    Some(line_edits_to_text_edits(&line_index, &edits))
}

#[cfg(test)]
mod tests {
    use super::*;
    use satz_core::{Index, parse_document};
    use std::path::Path;
    use tower_lsp_server::ls_types::TextDocumentIdentifier;

    #[test]
    fn test_formatting_applied() {
        let text = "Line 1   \n\n\n\nLine 2   ";
        let rel_path = Path::new("test.md");
        let doc = parse_document(text, rel_path);

        let mut state = SatzState {
            index: Index::build(vec![doc]),
            vault_root: Some(Path::new("").to_path_buf()),
            ..Default::default()
        };
        state.open_docs.insert(
            "file:///test.md".to_string(),
            crate::state::OpenDocument::new("file:///test.md", rel_path.to_path_buf(), text, 1),
        );

        let params = DocumentFormattingParams {
            text_document: TextDocumentIdentifier {
                uri: "file:///test.md".parse().unwrap(),
            },
            options: tower_lsp_server::ls_types::FormattingOptions {
                tab_size: 2,
                insert_spaces: true,
                ..Default::default()
            },
            work_done_progress_params: Default::default(),
        };

        let edits = formatting(params, &state).expect("Edits expected");
        // Two scattered changes (trailing whitespace on line 1, and the collapsed blank lines +
        // trailing whitespace around line 2) — minimal-diff must not collapse these into one
        // whole-document edit.
        assert!(
            edits.len() > 1,
            "expected multiple minimal edits, got {}: {edits:?}",
            edits.len()
        );

        // Applying every edit's replacement text in range order must reconstruct the exact
        // formatted output.
        let mut cursor = tower_lsp_server::ls_types::Position::new(0, 0);
        let mut rebuilt = String::new();
        let li = satz_core::LineIndex::new(text);
        for edit in &edits {
            let from = li.position_to_byte(satz_core::Position::new(cursor.line, cursor.character));
            let to = li.position_to_byte(satz_core::Position::new(
                edit.range.start.line,
                edit.range.start.character,
            ));
            rebuilt.push_str(&text[from..to]);
            rebuilt.push_str(&edit.new_text);
            cursor = edit.range.end;
        }
        let from = li.position_to_byte(satz_core::Position::new(cursor.line, cursor.character));
        rebuilt.push_str(&text[from..]);
        assert_eq!(rebuilt, "Line 1\n\nLine 2\n");
    }

    #[test]
    fn test_formatting_already_clean() {
        let text = "Line 1\n\nLine 2\n";
        let rel_path = Path::new("test.md");
        let doc = parse_document(text, rel_path);

        let mut state = SatzState {
            index: Index::build(vec![doc]),
            vault_root: Some(Path::new("").to_path_buf()),
            ..Default::default()
        };
        state.open_docs.insert(
            "file:///test.md".to_string(),
            crate::state::OpenDocument::new("file:///test.md", rel_path.to_path_buf(), text, 1),
        );

        let params = DocumentFormattingParams {
            text_document: TextDocumentIdentifier {
                uri: "file:///test.md".parse().unwrap(),
            },
            options: tower_lsp_server::ls_types::FormattingOptions {
                tab_size: 2,
                insert_spaces: true,
                ..Default::default()
            },
            work_done_progress_params: Default::default(),
        };

        let edits = formatting(params, &state).expect("Edits expected");
        assert!(edits.is_empty());
    }

    #[test]
    fn test_formatting_disabled_returns_no_edits() {
        let text = "Line 1   \n\n\n\nLine 2   ";
        let rel_path = Path::new("test.md");
        let doc = parse_document(text, rel_path);

        let mut state = SatzState {
            index: Index::build(vec![doc]),
            vault_root: Some(Path::new("").to_path_buf()),
            ..Default::default()
        };
        state.config.formatter.enabled = false;
        state.open_docs.insert(
            "file:///test.md".to_string(),
            crate::state::OpenDocument::new("file:///test.md", rel_path.to_path_buf(), text, 1),
        );

        let params = DocumentFormattingParams {
            text_document: TextDocumentIdentifier {
                uri: "file:///test.md".parse().unwrap(),
            },
            options: tower_lsp_server::ls_types::FormattingOptions {
                tab_size: 2,
                insert_spaces: true,
                ..Default::default()
            },
            work_done_progress_params: Default::default(),
        };

        let edits = formatting(params, &state).expect("Edits expected");
        assert!(
            edits.is_empty(),
            "disabled formatter must return no edits even for dirty content"
        );
    }
}
