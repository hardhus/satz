use crate::state::SatzState;
use tower_lsp_server::ls_types::{CodeLens, CodeLensParams, Command, Position, Range};

/// Computes CodeLens entries for a document, displaying incoming backlink count.
pub fn code_lens(params: CodeLensParams, state: &SatzState) -> Option<Vec<CodeLens>> {
    if !state.config.lsp.codelens.enable {
        return None;
    }

    let uri = params.text_document.uri.as_str();
    let open_doc = state.open_docs.get(uri)?;
    let rel_path = match &state.vault_root {
        Some(root) => open_doc.path.strip_prefix(root).unwrap_or(&open_doc.path),
        None => &open_doc.path,
    };
    let rel_path_str = rel_path.to_string_lossy().replace('\\', "/");
    let doc_id = satz_core::DocId::new(&rel_path_str);

    let count = state.index.backlinks_of(&doc_id).count();
    let title = match count {
        0 => "0 backlinks".to_string(),
        1 => "1 backlink".to_string(),
        n => format!("{} backlinks", n),
    };

    Some(vec![CodeLens {
        range: Range::new(Position::new(0, 0), Position::new(0, 0)),
        command: Some(Command {
            title,
            command: "satz.showBacklinks".to_string(),
            arguments: None,
        }),
        data: None,
    }])
}

#[cfg(test)]
mod tests {
    use super::*;
    use satz_core::{Index, VaultConfig, parse_document};
    use std::path::Path;
    use tower_lsp_server::ls_types::TextDocumentIdentifier;

    #[test]
    fn test_codelens_disabled_by_default() {
        let doc_a = parse_document("# Doc A\n\nContent", Path::new("doc-a.md"));
        let state = SatzState {
            index: Index::build(vec![doc_a]),
            vault_root: Some(Path::new("").to_path_buf()),
            ..Default::default()
        };

        let params = CodeLensParams {
            text_document: TextDocumentIdentifier {
                uri: "file:///doc-a.md".parse().unwrap(),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };

        assert!(code_lens(params, &state).is_none());
    }

    #[test]
    fn test_codelens_enabled_with_backlinks() {
        let doc_a = parse_document("# Doc A", Path::new("doc-a.md"));
        let doc_b = parse_document("# Doc B\n\n[[doc-a]]", Path::new("doc-b.md"));
        let doc_c = parse_document("# Doc C\n\n[[doc-a]]", Path::new("doc-c.md"));

        let mut config = VaultConfig::default();
        config.lsp.codelens.enable = true;

        let uri_str = "file:///doc-a.md";
        let rel_a = Path::new("doc-a.md");

        let mut state = SatzState {
            index: Index::build(vec![doc_a, doc_b, doc_c]),
            vault_root: Some(Path::new("").to_path_buf()),
            config,
            ..Default::default()
        };
        state.open_docs.insert(
            uri_str.to_string(),
            crate::state::OpenDocument::new(uri_str, rel_a.to_path_buf(), "# Doc A", 1),
        );

        let params = CodeLensParams {
            text_document: TextDocumentIdentifier {
                uri: uri_str.parse().unwrap(),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };

        let result = code_lens(params, &state).expect("CodeLens expected");
        assert_eq!(result.len(), 1);
        let lens = &result[0];
        assert_eq!(lens.range.start.line, 0);
        let cmd = lens.command.as_ref().unwrap();
        assert_eq!(cmd.title, "2 backlinks");
    }
}
