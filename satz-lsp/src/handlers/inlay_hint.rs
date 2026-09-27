use crate::convert::byte_range_to_lsp;
use crate::state::SatzState;
use satz_core::LinkKind;
use tower_lsp_server::ls_types::{InlayHint, InlayHintKind, InlayHintLabel, InlayHintParams};

/// Whether `position` lies in `range` (both ends inclusive).
fn in_range(
    position: tower_lsp_server::ls_types::Position,
    range: tower_lsp_server::ls_types::Range,
) -> bool {
    (range.start.line, range.start.character) <= (position.line, position.character)
        && (position.line, position.character) <= (range.end.line, range.end.character)
}

/// Computes InlayHint entries displaying target note metadata right next to links.
pub fn inlay_hint(params: InlayHintParams, state: &SatzState) -> Option<Vec<InlayHint>> {
    if !state.config.lsp.inlay_hints.enable {
        return None;
    }

    let uri = params.text_document.uri.as_str();
    tracing::debug!(uri, "inlay_hint");
    let (_, doc) = state.doc_for_uri(uri)?;

    let mut hints: Vec<InlayHint> = Vec::new();

    for link in &doc.links {
        match link.kind {
            LinkKind::WikiLink | LinkKind::Embed | LinkKind::Markdown => {
                if satz_core::model::link::is_external_target(&link.target_doc) {
                    continue;
                }
                // A link inside this same note (`[[#Heading]]`) only needs a hint when its anchor
                // is missing; there is nothing to say about the note it already is.
                let same_note = link.target_doc.is_empty();
                if same_note && link.target_heading.is_none() && link.target_block.is_none() {
                    continue;
                }

                let range = byte_range_to_lsp(link.range, &doc.line_index);
                let position = range.end;
                if !in_range(position, params.range) {
                    continue;
                }

                let resolution = crate::handlers::diagnostics::as_the_user_sees_it(
                    link,
                    state.resolve(link, doc),
                );
                if same_note
                    && !matches!(resolution, satz_core::LinkResolution::AnchorMissing { .. })
                {
                    continue;
                }

                let label_text = match resolution {
                    satz_core::LinkResolution::AnchorMissing { .. } => {
                        if link.target_block.is_some() {
                            " ⚠ block not found".to_string()
                        } else {
                            " ⚠ heading not found".to_string()
                        }
                    }
                    satz_core::LinkResolution::Resolved {
                        doc: target_doc, ..
                    } => {
                        if !target_doc.tags.is_empty() {
                            let tag_str: Vec<String> = target_doc
                                .tags
                                .iter()
                                .take(3)
                                .map(|t| format!("#{}", t.name.trim_start_matches('#')))
                                .collect();
                            format!(" {}", tag_str.join(" "))
                        } else {
                            format!(" ({})", target_doc.title)
                        }
                    }
                    satz_core::LinkResolution::DocMissing => " ⚠ not found".to_string(),
                };

                hints.push(InlayHint {
                    position,
                    label: InlayHintLabel::String(label_text),
                    kind: Some(InlayHintKind::TYPE),
                    text_edits: None,
                    tooltip: None,
                    padding_left: Some(true),
                    padding_right: None,
                    data: None,
                });
            }
            LinkKind::Footnote => {}
        }
    }

    if hints.is_empty() { None } else { Some(hints) }
}

#[cfg(test)]
mod tests;
