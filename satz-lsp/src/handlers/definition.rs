use crate::convert::byte_range_to_lsp;
use crate::state::SatzState;
use satz_core::LinkKind;
use tower_lsp_server::ls_types::{GotoDefinitionParams, GotoDefinitionResponse, Location, Range};

pub fn goto_definition(
    params: GotoDefinitionParams,
    state: &SatzState,
) -> Option<GotoDefinitionResponse> {
    let uri = params
        .text_document_position_params
        .text_document
        .uri
        .as_str();
    let pos = params.text_document_position_params.position;
    tracing::debug!(uri, ?pos, "goto_definition");

    // Get the current document
    let (_, doc) = state.doc_for_uri(uri)?;

    // Convert position to byte offset
    let byte_offset = crate::convert::lsp_pos_to_byte(&doc.line_index, pos);

    // Find the link under the cursor
    let link = doc.link_at(byte_offset)?;

    // Special case for footnotes: jump to definition in the SAME document
    if link.kind == LinkKind::Footnote
        && let Some(label) = &link.display
        && let Some(def) = doc.footnotes.find_def(label)
    {
        let range = byte_range_to_lsp(def.range, &doc.line_index);
        let url = state.doc_uri(doc)?;
        return Some(GotoDefinitionResponse::Scalar(Location::new(url, range)));
    }

    let resolution =
        state
            .index
            .resolve_link_full_with_config(link, Some(doc), Some(&state.config));
    // Deliberately not logging `resolution` itself: it borrows the full target
    // `Document` and its derived `Debug` would dump the whole parsed document
    // (content, headings, links, ...) on every `gd` call at debug level.
    let outcome = match &resolution {
        satz_core::LinkResolution::Resolved { doc, .. } => format!("Resolved({:?})", doc.id),
        satz_core::LinkResolution::AnchorMissing { doc } => format!("AnchorMissing({:?})", doc.id),
        satz_core::LinkResolution::DocMissing => "DocMissing".to_string(),
    };
    tracing::debug!(
        target_doc = %link.target_doc,
        target_heading = ?link.target_heading,
        outcome,
        "goto_definition: link resolution"
    );

    match resolution {
        satz_core::LinkResolution::Resolved {
            doc: target_doc,
            anchor,
        } => {
            let target_uri = state.doc_uri(target_doc)?;
            let target_range = if let Some(r) = anchor {
                byte_range_to_lsp(r, &target_doc.line_index)
            } else {
                Range {
                    start: tower_lsp_server::ls_types::Position::new(0, 0),
                    end: tower_lsp_server::ls_types::Position::new(0, 0),
                }
            };
            Some(GotoDefinitionResponse::Scalar(Location::new(
                target_uri,
                target_range,
            )))
        }
        satz_core::LinkResolution::AnchorMissing { doc: target_doc } => {
            let target_uri = state.doc_uri(target_doc)?;
            Some(GotoDefinitionResponse::Scalar(Location::new(
                target_uri,
                Range {
                    start: tower_lsp_server::ls_types::Position::new(0, 0),
                    end: tower_lsp_server::ls_types::Position::new(0, 0),
                },
            )))
        }
        satz_core::LinkResolution::DocMissing => None,
    }
}

#[cfg(test)]
mod tests;
