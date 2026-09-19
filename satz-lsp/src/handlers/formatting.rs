use crate::convert::line_edits_to_text_edits;
use crate::state::SatzState;
use satz_core::formatter::diff::line_diff;
use tower_lsp_server::ls_types::{DocumentFormattingParams, TextEdit};

/// Formats a document according to the vault's FormatterConfig, returning minimal line-range
/// `TextEdit`s (via a line-based diff) rather than one edit replacing the whole document — this
/// keeps the editor's undo history and the LSP payload proportional to what actually changed.
pub fn formatting(params: DocumentFormattingParams, state: &SatzState) -> Option<Vec<TextEdit>> {
    if !state.formatting_allowed() {
        return Some(vec![]);
    }

    let uri = params.text_document.uri.as_str();
    tracing::debug!(uri, "formatting");
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

    fn dirty_state() -> (SatzState, DocumentFormattingParams) {
        let text = "Line 1   \n\n\n\nLine 2   ";
        let rel_path = Path::new("test.md");
        let mut state = SatzState {
            index: Index::build(vec![parse_document(text, rel_path)]),
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
        (state, params)
    }

    #[test]
    fn formatting_is_off_while_the_config_is_invalid_and_back_on_once_fixed() {
        let (mut state, params) = dirty_state();

        // Control: a valid config formats the dirty document (so "no edits" below is meaningful).
        let edits = formatting(params.clone(), &state).expect("Some");
        assert!(
            !edits.is_empty(),
            "control: dirty text should produce edits"
        );

        state.config_error = Some("invalid .satz.toml: line 1".to_string());
        let edits = formatting(params.clone(), &state).expect("Some");
        assert!(
            edits.is_empty(),
            "an unusable config must not format with defaults: {edits:?}"
        );

        state.config_error = None;
        let edits = formatting(params, &state).expect("Some");
        assert!(!edits.is_empty(), "fixing the config re-enables formatting");
    }

    fn state_and_params_for(text: &str) -> (SatzState, DocumentFormattingParams) {
        let rel_path = Path::new("test.md");
        let mut state = SatzState {
            index: Index::build(vec![parse_document(text, rel_path)]),
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
        (state, params)
    }

    #[test]
    fn a_clean_crlf_document_gets_no_edits() {
        let text = "# Title\r\n\r\nClean paragraph.\r\n\r\n- a\r\n- b\r\n";
        let (state, params) = state_and_params_for(text);
        let edits = formatting(params, &state).expect("Some");
        assert!(
            edits.is_empty(),
            "CRLF must not count as a change: {edits:?}"
        );
    }

    #[test]
    fn a_dirty_crlf_document_is_formatted_and_stays_entirely_crlf() {
        let text = "# Title\r\n\r\nLine with   trailing   \r\n\r\n\r\n\r\nNext.\r\n";
        let (state, params) = state_and_params_for(text);

        let edits = formatting(params, &state).expect("Some");
        assert!(!edits.is_empty(), "the dirty document must produce edits");

        let result = crate::convert::apply_text_edits(text, &edits);
        assert_eq!(
            result,
            "# Title\r\n\r\nLine with   trailing\r\n\r\nNext.\r\n"
        );
    }

    #[test]
    fn crlf_edits_leave_untouched_lines_alone() {
        // Only the middle line is dirty; the edits must not rewrite the clean CRLF lines.
        let text = "clean one\r\ndirty   \r\nclean two\r\n";
        let (state, params) = state_and_params_for(text);

        let edits = formatting(params, &state).expect("Some");

        assert_eq!(edits.len(), 1, "{edits:?}");
        assert_eq!(edits[0].range.start.line, 1);
        assert_eq!(edits[0].range.end.line, 2);
        assert_eq!(
            crate::convert::apply_text_edits(text, &edits),
            "clean one\r\ndirty\r\nclean two\r\n"
        );
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
