use super::*;
use satz_core::{Index, parse_document};
use std::path::Path;
use tower_lsp_server::ls_types::{Position, TextDocumentIdentifier, TextDocumentPositionParams};

#[test]
fn test_document_highlight_tags() {
    let text = "#tag and #tag/sub and unrelated #other";
    let rel_path = Path::new("doc-a.md");
    let doc_a = parse_document(text, rel_path);

    let mut state = SatzState::default();
    state.index = Index::build(vec![doc_a]);
    state.set_vault_root(Some(Path::new("").to_path_buf()));
    state.open_docs.insert(
        "file:///doc-a.md".to_string(),
        crate::state::OpenDocument::new("file:///doc-a.md", rel_path.to_path_buf(), text, 1),
    );

    let params = DocumentHighlightParams {
        text_document_position_params: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier {
                uri: "file:///doc-a.md".parse().unwrap(),
            },
            position: Position::new(0, 1), // on `#tag`
        },
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
    };

    let res = document_highlight(params, &state).expect("Highlights expected");
    assert_eq!(res.len(), 2, "Expected #tag and #tag/sub to be highlighted");
}
