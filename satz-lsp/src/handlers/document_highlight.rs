use satz_core::{DocId, Document, Index, fold_key, slugify};
use tower_lsp_server::ls_types::{
    DocumentHighlight, DocumentHighlightKind, DocumentHighlightParams,
};

use crate::convert::{byte_range_to_lsp, lsp_pos_to_satz};
use crate::state::SatzState;

#[derive(Debug, Clone, PartialEq, Eq)]
enum HighlightTarget {
    Tag(String),
    Heading { doc: DocId, slug: String },
    Block { doc: DocId, id: String },
    Doc(DocId),
}

fn cursor_target(doc: &Document, off: usize, index: &Index) -> Option<HighlightTarget> {
    // Priority order: link > block > heading > tag
    if let Some(l) = doc.links.iter().find(|l| l.range.contains(off)) {
        let target = if l.target_doc.is_empty() {
            doc.id.clone()
        } else {
            index.resolve_link(&l.target_doc)?.clone()
        };
        return Some(match (&l.target_block, &l.target_heading) {
            (Some(b), _) => HighlightTarget::Block {
                doc: target,
                id: b.clone(),
            },
            (_, Some(h)) => HighlightTarget::Heading {
                doc: target,
                slug: slugify(h),
            },
            _ => HighlightTarget::Doc(target),
        });
    }

    if let Some(b) = doc.blocks.iter().find(|b| b.range.contains(off)) {
        return Some(HighlightTarget::Block {
            doc: doc.id.clone(),
            id: b.id.clone(),
        });
    }

    if let Some(h) = doc.headings.iter().find(|h| h.range.contains(off)) {
        return Some(HighlightTarget::Heading {
            doc: doc.id.clone(),
            slug: h.slug.clone(),
        });
    }

    if let Some(t) = doc.tags.iter().find(|t| t.range.contains(off)) {
        return Some(HighlightTarget::Tag(
            t.name.trim_start_matches('#').to_string(),
        ));
    }

    None
}

pub fn document_highlight(
    params: DocumentHighlightParams,
    state: &SatzState,
) -> Option<Vec<DocumentHighlight>> {
    let uri = params
        .text_document_position_params
        .text_document
        .uri
        .as_str();
    let pos = params.text_document_position_params.position;

    let open_doc = state.open_docs.get(uri)?;
    let rel_path =
        crate::state::SatzState::get_rel_path(&open_doc.path, state.vault_root.as_deref());
    let rel_path_str = rel_path.to_string_lossy().replace('\\', "/");
    let doc_id = satz_core::DocId::new(&rel_path_str);
    let doc = state.index.get_doc(&doc_id)?;

    let satz_pos = lsp_pos_to_satz(pos);
    let byte_offset = doc.line_index.position_to_byte(satz_pos);

    let target = cursor_target(doc, byte_offset, &state.index)?;
    let mut highlights = Vec::new();

    match target {
        HighlightTarget::Tag(ref tag_name) => {
            let clean_query = fold_key(tag_name.trim_start_matches('#'));
            let prefix = format!("{}/", clean_query);

            for t in &doc.tags {
                let clean_t = fold_key(t.name.trim_start_matches('#'));
                if clean_t == clean_query || clean_t.starts_with(&prefix) {
                    highlights.push(DocumentHighlight {
                        range: byte_range_to_lsp(t.range, &doc.line_index),
                        kind: Some(DocumentHighlightKind::TEXT),
                    });
                }
            }
        }
        HighlightTarget::Heading {
            doc: ref target_doc,
            ref slug,
        } => {
            // If target doc is current doc, highlight the heading definition
            if target_doc == &doc.id {
                for h in &doc.headings {
                    if &h.slug == slug {
                        highlights.push(DocumentHighlight {
                            range: byte_range_to_lsp(h.range, &doc.line_index),
                            kind: Some(DocumentHighlightKind::WRITE),
                        });
                    }
                }
            }
            // Highlight any links in this document pointing to this heading
            for link in &doc.links {
                let resolved_doc = if link.target_doc.is_empty() {
                    Some(&doc.id)
                } else {
                    state.index.resolve_link(&link.target_doc)
                };
                if resolved_doc == Some(target_doc)
                    && link
                        .target_heading
                        .as_deref()
                        .is_some_and(|th| slugify(th) == *slug)
                {
                    highlights.push(DocumentHighlight {
                        range: byte_range_to_lsp(link.range, &doc.line_index),
                        kind: Some(DocumentHighlightKind::READ),
                    });
                }
            }
        }
        HighlightTarget::Block {
            doc: ref target_doc,
            ref id,
        } => {
            // If target doc is current doc, highlight the block definition
            if target_doc == &doc.id
                && let Some(b) = doc.blocks.iter().find(|b| &b.id == id)
            {
                highlights.push(DocumentHighlight {
                    range: byte_range_to_lsp(b.range, &doc.line_index),
                    kind: Some(DocumentHighlightKind::WRITE),
                });
            }
            // Highlight any links in this document pointing to this block
            for link in &doc.links {
                let resolved_doc = if link.target_doc.is_empty() {
                    Some(&doc.id)
                } else {
                    state.index.resolve_link(&link.target_doc)
                };
                if resolved_doc == Some(target_doc)
                    && link.target_block.as_deref().is_some_and(|b| b == id)
                {
                    highlights.push(DocumentHighlight {
                        range: byte_range_to_lsp(link.range, &doc.line_index),
                        kind: Some(DocumentHighlightKind::READ),
                    });
                }
            }
        }
        HighlightTarget::Doc(ref target_doc) => {
            for link in &doc.links {
                if state.index.resolve_link(&link.target_doc) == Some(target_doc) {
                    highlights.push(DocumentHighlight {
                        range: byte_range_to_lsp(link.range, &doc.line_index),
                        kind: Some(DocumentHighlightKind::READ),
                    });
                }
            }
        }
    }

    if highlights.is_empty() {
        None
    } else {
        Some(highlights)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use satz_core::{Index, parse_document};
    use std::path::Path;
    use tower_lsp_server::ls_types::{
        Position, TextDocumentIdentifier, TextDocumentPositionParams,
    };

    #[test]
    fn test_document_highlight_tags() {
        let text = "#tag and #tag/sub and unrelated #other";
        let rel_path = Path::new("doc-a.md");
        let doc_a = parse_document(text, rel_path);

        let mut state = SatzState {
            index: Index::build(vec![doc_a]),
            vault_root: Some(Path::new("").to_path_buf()),
            ..Default::default()
        };
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
}
