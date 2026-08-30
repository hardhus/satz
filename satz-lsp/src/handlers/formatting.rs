use crate::state::SatzState;
use tower_lsp_server::ls_types::{DocumentFormattingParams, Position, Range, TextEdit};

/// Formats a document according to the vault's FormatterConfig.
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

    let li = satz_core::LineIndex::new(&original);
    let end = li.byte_to_position(original.len());

    Some(vec![TextEdit {
        range: Range::new(Position::new(0, 0), Position::new(end.line, end.character)),
        new_text: formatted,
    }])
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
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].new_text, "Line 1\n\nLine 2\n");
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
