// Test states are built field by field so each test shows exactly what it sets up.
#![allow(clippy::field_reassign_with_default)]

use super::*;
use satz_core::{Index, parse_document};
use std::path::Path;
use tower_lsp_server::ls_types::TextDocumentIdentifier;

#[test]
fn test_document_link_extraction() {
    let abs_a = if cfg!(windows) {
        Path::new("C:\\doc-a.md")
    } else {
        Path::new("/doc-a.md")
    };

    let rel_a = Path::new("doc-a.md");
    let rel_b = Path::new("doc-b.md");

    let doc_a = parse_document(
        "# Doc A\n\n[[doc-b]] and [rust](https://rust-lang.org)",
        rel_a,
    );
    let doc_b = parse_document("# Doc B", rel_b);

    let mut state = SatzState::default();
    state.index = Index::build(vec![doc_a, doc_b]);
    state.set_vault_root(Some(if cfg!(windows) {
        Path::new("C:\\").to_path_buf()
    } else {
        Path::new("/").to_path_buf()
    }));

    let uri_a_str = if cfg!(windows) {
        "file:///C:/doc-a.md"
    } else {
        "file:///doc-a.md"
    };

    state.open_docs.insert(
        uri_a_str.to_string(),
        crate::state::OpenDocument::new(
            uri_a_str,
            abs_a.to_path_buf(),
            "# Doc A\n\n[[doc-b]] and [rust](https://rust-lang.org)",
            1,
        ),
    );

    let params = DocumentLinkParams {
        text_document: TextDocumentIdentifier {
            uri: uri_a_str.parse().unwrap(),
        },
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
    };

    let result = document_link(params, &state).expect("Links expected");
    assert_eq!(result.len(), 2);
}
