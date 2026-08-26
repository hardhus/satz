#![allow(clippy::collapsible_if)]

use tower_lsp_server::ls_types::{DocumentLink, DocumentLinkParams, Uri};

use crate::convert::{byte_range_to_lsp, path_to_uri};
use crate::state::SatzState;
use satz_core::model::LinkKind;

pub fn document_link(params: DocumentLinkParams, state: &SatzState) -> Option<Vec<DocumentLink>> {
    let uri = params.text_document.uri.as_str();

    let open_doc = state.open_docs.get(uri)?;
    let rel_path = match &state.vault_root {
        Some(root) => open_doc.path.strip_prefix(root).unwrap_or(&open_doc.path),
        None => &open_doc.path,
    };
    let rel_path_str = rel_path.to_string_lossy().replace('\\', "/");
    let doc_id = satz_core::DocId::new(&rel_path_str);
    let doc = state.index.get_doc(&doc_id)?;

    let mut links = Vec::new();

    for l in &doc.links {
        let range = byte_range_to_lsp(l.range, &doc.line_index);

        if l.target_doc.starts_with("http://") || l.target_doc.starts_with("https://") {
            if let Ok(url) = l.target_doc.parse::<Uri>() {
                links.push(DocumentLink {
                    range,
                    target: Some(url),
                    tooltip: Some("Open external link".to_string()),
                    data: None,
                });
            }
            continue;
        }

        if matches!(
            l.kind,
            LinkKind::WikiLink | LinkKind::Embed | LinkKind::Markdown
        ) {
            let target_id = if l.target_doc.is_empty() {
                &doc_id
            } else if let Some(resolved) = state.index.resolve_link(&l.target_doc) {
                resolved
            } else {
                continue;
            };

            if let Some(target_doc) = state.index.get_doc(target_id) {
                let target_path = match &state.vault_root {
                    Some(root) if !target_doc.path.is_absolute() => root.join(&target_doc.path),
                    _ => target_doc.path.clone(),
                };

                if let Some(url) = path_to_uri(&target_path) {
                    links.push(DocumentLink {
                        range,
                        target: Some(url),
                        tooltip: Some(format!("Go to {}", target_doc.title)),
                        data: None,
                    });
                }
            }
        }
    }

    Some(links)
}

#[cfg(test)]
#[allow(unused_variables)]
#[allow(clippy::field_reassign_with_default)]
mod tests {
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
        let abs_b = if cfg!(windows) {
            Path::new("C:\\doc-b.md")
        } else {
            Path::new("/doc-b.md")
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
        state.vault_root = Some(if cfg!(windows) {
            Path::new("C:\\").to_path_buf()
        } else {
            Path::new("/").to_path_buf()
        });

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
}
