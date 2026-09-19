#![allow(clippy::field_reassign_with_default)]
//! `mailto:`/`tel:`/`obsidian:` links are external: no "missing note" diagnostics, no inlay
//! hints, no semantic-token colouring as a note link, and the document link opens the URL.

use satz_core::{Index, parse_document};
use satz_lsp::handlers::diagnostics::compute_diagnostics;
use satz_lsp::handlers::document_link::document_link;
use satz_lsp::handlers::inlay_hint::inlay_hint;
use satz_lsp::state::{OpenDocument, SatzState};
use std::path::{Path, PathBuf};
use tower_lsp_server::ls_types::*;

const BODY: &str = "[mail](mailto:a@b.c) [tel](tel:+905551112233) [app](obsidian://open?vault=v) [web](https://a.b/c#frag)\n";

fn state() -> (SatzState, String) {
    let root = PathBuf::from(if cfg!(windows) { "C:\\vault" } else { "/vault" });
    let mut state = SatzState::default();
    state.index = Index::build(vec![parse_document(BODY, Path::new("a.md"))]);
    state.vault_root = Some(root.clone());
    let uri = satz_lsp::convert::path_to_uri(&root.join("a.md"))
        .unwrap()
        .as_str()
        .to_string();
    state.open_docs.insert(
        uri.clone(),
        OpenDocument::new(&uri, root.join("a.md"), BODY, 1),
    );
    (state, uri)
}

#[test]
fn no_diagnostics_for_external_links() {
    let (state, _) = state();
    let doc = state.index.get_doc(&satz_core::DocId::new("a.md")).unwrap();
    let diags = compute_diagnostics(doc, &state.index, &state.config);
    assert!(
        !diags
            .iter()
            .any(|d| d.message.to_lowercase().contains("not found")
                || d.message.to_lowercase().contains("broken")
                || d.message.to_lowercase().contains("missing")),
        "{diags:?}"
    );
}

#[test]
fn document_links_open_external_urls_unchanged() {
    let (state, uri) = state();
    let links = document_link(
        DocumentLinkParams {
            text_document: TextDocumentIdentifier {
                uri: uri.parse().unwrap(),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        },
        &state,
    )
    .unwrap();
    let targets: Vec<String> = links
        .iter()
        .map(|l| l.target.as_ref().unwrap().as_str().to_string())
        .collect();
    assert_eq!(
        targets,
        vec![
            "mailto:a@b.c",
            "tel:+905551112233",
            "obsidian://open?vault=v",
            "https://a.b/c#frag"
        ]
    );
}

#[test]
fn no_inlay_hints_for_external_links() {
    let (mut state, uri) = state();
    state.config.lsp.inlay_hints.enable = true;
    let hints = inlay_hint(
        InlayHintParams {
            work_done_progress_params: Default::default(),
            text_document: TextDocumentIdentifier {
                uri: uri.parse().unwrap(),
            },
            range: Range::new(Position::new(0, 0), Position::new(1, 0)),
        },
        &state,
    );
    assert!(hints.is_none_or(|h| h.is_empty()));
}
