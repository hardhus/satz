use tower_lsp_server::ls_types::{DocumentLink, DocumentLinkParams, Uri};

use crate::convert::byte_range_to_lsp;
use crate::state::SatzState;
use satz_core::model::LinkKind;

pub fn document_link(params: DocumentLinkParams, state: &SatzState) -> Option<Vec<DocumentLink>> {
    let uri = params.text_document.uri.as_str();
    tracing::trace!(uri, "document_link");

    let (_, doc) = state.doc_for_uri(uri)?;

    let mut links = Vec::new();

    for l in &doc.links {
        let range = byte_range_to_lsp(l.range, &doc.line_index);

        if satz_core::model::link::is_external_target(&l.target_doc) {
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
            match state.resolve(l, doc) {
                satz_core::LinkResolution::Resolved {
                    doc: target_doc, ..
                }
                | satz_core::LinkResolution::AnchorMissing { doc: target_doc } => {
                    if let Some(url) = state.doc_uri(target_doc) {
                        links.push(DocumentLink {
                            range,
                            target: Some(url),
                            tooltip: Some(format!("Go to {}", target_doc.title)),
                            data: None,
                        });
                    }
                }
                satz_core::LinkResolution::DocMissing => {}
            }
        }
    }

    Some(links)
}

#[cfg(test)]
mod tests;
