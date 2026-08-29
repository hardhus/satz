#![allow(clippy::collapsible_if)]

use crate::convert::{byte_range_to_lsp, lsp_pos_to_satz, path_to_uri};
use crate::state::SatzState;
use tower_lsp_server::ls_types::{Location, ReferenceParams};

pub fn find_references(params: ReferenceParams, state: &SatzState) -> Option<Vec<Location>> {
    let uri = params.text_document_position.text_document.uri.as_str();
    let pos = params.text_document_position.position;

    let open_doc = state.open_docs.get(uri)?;

    let rel_path =
        crate::state::SatzState::get_rel_path(&open_doc.path, state.vault_root.as_deref());
    let rel_path_str = rel_path.to_string_lossy().replace('\\', "/");
    let doc_id = satz_core::DocId::new(&rel_path_str);
    let doc = state.index.get_doc(&doc_id)?;

    let satz_pos = lsp_pos_to_satz(pos);
    let byte_offset = doc.line_index.position_to_byte(satz_pos);

    // 1. Check if cursor is on a Tag -> return all occurrences of this tag across the vault
    if let Some(target_tag) = doc.tags.iter().find(|t| t.range.contains(byte_offset)) {
        let tag_clean = target_tag.name.trim_start_matches('#');
        let mut locations = Vec::new();

        for tagged_doc in state.index.docs_with_tag(tag_clean) {
            let doc_path = match &state.vault_root {
                Some(root) if !tagged_doc.path.is_absolute() => root.join(&tagged_doc.path),
                _ => tagged_doc.path.clone(),
            };
            if let Some(url) = path_to_uri(&doc_path) {
                for t in &tagged_doc.tags {
                    if t.name
                        .trim_start_matches('#')
                        .eq_ignore_ascii_case(tag_clean)
                    {
                        locations.push(Location::new(
                            url.clone(),
                            byte_range_to_lsp(t.range, &tagged_doc.line_index),
                        ));
                    }
                }
            }
        }

        return Some(locations);
    }

    // 2. Check if cursor is on a heading
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
                            if link
                                .target_heading
                                .as_deref()
                                .is_some_and(|th| h.matches(th))
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
                        link.target_heading
                            .as_deref()
                            .is_some_and(|th| h.matches(th))
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

#[cfg(test)]
#[allow(unused_variables)]
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use super::*;
    use satz_core::{Index, parse_document};
    use std::path::Path;
    use tower_lsp_server::ls_types::{
        Position, ReferenceContext, TextDocumentIdentifier, TextDocumentPositionParams,
    };

    #[test]
    fn test_find_tag_references() {
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

        let doc_a = parse_document("# Doc A\n\nSome text with #rust tag.", rel_a);
        let doc_b = parse_document(
            "---\ntags: [rust]\n---\n# Doc B\n\nAlso has #rust tag.",
            rel_b,
        );

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
                "# Doc A\n\nSome text with #rust tag.",
                1,
            ),
        );

        let params = ReferenceParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: uri_a_str.parse().unwrap(),
                },
                position: Position::new(2, 16), // on "#rust"
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: ReferenceContext {
                include_declaration: true,
            },
        };

        let refs = find_references(params, &state).expect("References expected");
        // doc_a has 1 #rust tag, doc_b has 2 tags (frontmatter + body) -> total 3
        assert_eq!(refs.len(), 3);
    }
}
