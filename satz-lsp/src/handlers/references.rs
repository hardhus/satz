#![allow(clippy::collapsible_if)]

use crate::convert::{byte_range_to_lsp, lsp_pos_to_satz, path_to_uri};
use crate::state::SatzState;
use tower_lsp_server::ls_types::{Location, ReferenceParams};

pub fn find_references(params: ReferenceParams, state: &SatzState) -> Option<Vec<Location>> {
    let uri = params.text_document_position.text_document.uri.as_str();
    let pos = params.text_document_position.position;

    let open_doc = state.open_docs.get(uri)?;

    let rel_path = match &state.vault_root {
        Some(root) => open_doc.path.strip_prefix(root).unwrap_or(&open_doc.path),
        None => &open_doc.path,
    };
    let rel_path_str = rel_path.to_string_lossy().replace('\\', "/");
    let doc_id = satz_core::DocId::new(&rel_path_str);
    let doc = state.index.get_doc(&doc_id)?;

    let satz_pos = lsp_pos_to_satz(pos);
    let byte_offset = doc.line_index.position_to_byte(satz_pos);

    // Check if cursor is on a heading
    let target_heading = doc.headings.iter().find(|h| h.range.contains(byte_offset));

    let mut locations = Vec::new();
    let backlinks = state.index.backlinks_of(&doc_id);

    for src_id in backlinks {
        if let Some(src_doc) = state.index.get_doc(src_id) {
            // Find links in src_doc that point to doc_id
            for link in &src_doc.links {
                if link.target_doc.is_empty() {
                    // Intra-document links inside doc itself
                    if src_id == &doc_id {
                        if let Some(h) = target_heading {
                            if link.target_heading.as_deref() == Some(&h.slug)
                                || link.target_heading.as_deref() == Some(&h.text)
                            {
                                let src_path = match &state.vault_root {
                                    Some(root) if !src_doc.path.is_absolute() => {
                                        root.join(&src_doc.path)
                                    }
                                    _ => src_doc.path.clone(),
                                };
                                if let Some(url) = path_to_uri(&src_path) {
                                    locations.push(Location::new(
                                        url,
                                        byte_range_to_lsp(link.range, &src_doc.line_index),
                                    ));
                                }
                            }
                        }
                    }
                    continue;
                }

                // If link points to doc_id
                if state.index.resolve_link(&link.target_doc) == Some(&doc_id) {
                    let matches = if let Some(h) = target_heading {
                        link.target_heading.as_deref() == Some(&h.slug)
                            || link.target_heading.as_deref() == Some(&h.text)
                    } else {
                        // Just point to the document itself, include all links to the document
                        true
                    };

                    if matches {
                        let src_path = match &state.vault_root {
                            Some(root) if !src_doc.path.is_absolute() => root.join(&src_doc.path),
                            _ => src_doc.path.clone(),
                        };
                        if let Some(url) = path_to_uri(&src_path) {
                            locations.push(Location::new(
                                url,
                                byte_range_to_lsp(link.range, &src_doc.line_index),
                            ));
                        }
                    }
                }
            }
        }
    }

    // Include self? The LSP spec says if include_declaration is true, include the target itself.
    // We can just add the heading definition or doc start itself.
    if params.context.include_declaration {
        if let Some(h) = target_heading {
            let doc_path = match &state.vault_root {
                Some(root) if !doc.path.is_absolute() => root.join(&doc.path),
                _ => doc.path.clone(),
            };
            if let Some(url) = path_to_uri(&doc_path) {
                locations.push(Location::new(
                    url,
                    byte_range_to_lsp(h.range, &doc.line_index),
                ));
            }
        }
    }

    Some(locations)
}
