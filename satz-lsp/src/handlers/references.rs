use std::path::Path;

use satz_core::{DocId, Document, Index, fold_key, slugify};
use tower_lsp_server::ls_types::{Location, ReferenceParams, Uri};

use crate::convert::{byte_range_to_lsp, lsp_pos_to_satz, path_to_uri};
use crate::state::SatzState;

#[derive(Debug, Clone, PartialEq, Eq)]
enum CursorTarget {
    Tag(String),
    Heading { doc: DocId, slug: String },
    Block { doc: DocId, id: String },
    Doc(DocId),
}

fn cursor_target(doc: &Document, off: usize, index: &Index) -> Option<CursorTarget> {
    // Priority order: link > block > heading > tag > document
    if let Some(l) = doc.links.iter().find(|l| l.range.contains(off)) {
        let target = if l.target_doc.is_empty() {
            doc.id.clone()
        } else {
            index.resolve_link(&l.target_doc)?.clone()
        };
        return Some(match (&l.target_block, &l.target_heading) {
            (Some(b), _) => CursorTarget::Block {
                doc: target,
                id: b.clone(),
            },
            (_, Some(h)) => CursorTarget::Heading {
                doc: target,
                slug: slugify(h),
            },
            _ => CursorTarget::Doc(target),
        });
    }

    if let Some(b) = doc.blocks.iter().find(|b| b.range.contains(off)) {
        return Some(CursorTarget::Block {
            doc: doc.id.clone(),
            id: b.id.clone(),
        });
    }

    if let Some(h) = doc.headings.iter().find(|h| h.range.contains(off)) {
        return Some(CursorTarget::Heading {
            doc: doc.id.clone(),
            slug: h.slug.clone(),
        });
    }

    if let Some(t) = doc.tags.iter().find(|t| t.range.contains(off)) {
        return Some(CursorTarget::Tag(
            t.name.trim_start_matches('#').to_string(),
        ));
    }

    Some(CursorTarget::Doc(doc.id.clone()))
}

fn doc_uri(doc: &Document, vault_root: Option<&Path>) -> Option<Uri> {
    let doc_path = match vault_root {
        Some(root) if !doc.path.is_absolute() => root.join(&doc.path),
        _ => doc.path.clone(),
    };
    path_to_uri(&doc_path)
}

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

    let target = cursor_target(doc, byte_offset, &state.index)?;
    let mut locations = Vec::new();

    match target {
        CursorTarget::Tag(ref tag_name) => {
            for tagged_doc in state.index.docs_with_tag(tag_name) {
                let Some(u) = doc_uri(tagged_doc, state.vault_root.as_deref()) else {
                    continue;
                };
                for t in &tagged_doc.tags {
                    if fold_key(t.name.trim_start_matches('#')) == fold_key(tag_name) {
                        locations.push(Location::new(
                            u.clone(),
                            byte_range_to_lsp(t.range, &tagged_doc.line_index),
                        ));
                    }
                }
            }
        }
        CursorTarget::Block { ref doc, ref id } => {
            if let Some(target_doc) = state.index.get_doc(doc)
                && let Some(b) = target_doc.blocks.iter().find(|b| &b.id == id)
                && let Some(u) = doc_uri(target_doc, state.vault_root.as_deref())
            {
                locations.push(Location::new(
                    u,
                    byte_range_to_lsp(b.range, &target_doc.line_index),
                ));
            }

            let mut candidate_ids: Vec<DocId> = state.index.backlinks_of(doc).cloned().collect();
            if !candidate_ids.contains(doc) {
                candidate_ids.push(doc.clone());
            }

            for src_id in &candidate_ids {
                if let Some(src_doc) = state.index.get_doc(src_id) {
                    let Some(src_uri) = doc_uri(src_doc, state.vault_root.as_deref()) else {
                        continue;
                    };

                    for link in &src_doc.links {
                        let resolves_to_target = if link.target_doc.is_empty() {
                            src_id == doc
                        } else {
                            state.index.resolve_link(&link.target_doc) == Some(doc)
                        };

                        if resolves_to_target && link.target_block.as_deref() == Some(id.as_str()) {
                            locations.push(Location::new(
                                src_uri.clone(),
                                byte_range_to_lsp(link.range, &src_doc.line_index),
                            ));
                        }
                    }
                }
            }
        }
        CursorTarget::Heading { ref doc, ref slug } => {
            if let Some(target_doc) = state.index.get_doc(doc)
                && let Some(h) = target_doc.headings.iter().find(|h| &h.slug == slug)
                && let Some(u) = doc_uri(target_doc, state.vault_root.as_deref())
            {
                locations.push(Location::new(
                    u,
                    byte_range_to_lsp(h.range, &target_doc.line_index),
                ));
            }

            let mut candidate_ids: Vec<DocId> = state.index.backlinks_of(doc).cloned().collect();
            if !candidate_ids.contains(doc) {
                candidate_ids.push(doc.clone());
            }

            for src_id in &candidate_ids {
                if let Some(src_doc) = state.index.get_doc(src_id) {
                    let Some(src_uri) = doc_uri(src_doc, state.vault_root.as_deref()) else {
                        continue;
                    };

                    for link in &src_doc.links {
                        let resolves_to_target = if link.target_doc.is_empty() {
                            src_id == doc
                        } else {
                            state.index.resolve_link(&link.target_doc) == Some(doc)
                        };

                        if resolves_to_target
                            && link.target_heading.as_deref().map(slugify).as_deref()
                                == Some(slug.as_str())
                        {
                            locations.push(Location::new(
                                src_uri.clone(),
                                byte_range_to_lsp(link.range, &src_doc.line_index),
                            ));
                        }
                    }
                }
            }
        }
        CursorTarget::Doc(ref target_doc_id) => {
            if let Some(target_doc) = state.index.get_doc(target_doc_id)
                && let Some(u) = doc_uri(target_doc, state.vault_root.as_deref())
            {
                let range = if let Some(h) = target_doc.headings.first() {
                    byte_range_to_lsp(h.range, &target_doc.line_index)
                } else {
                    byte_range_to_lsp(satz_core::ByteRange::new(0, 0), &target_doc.line_index)
                };
                locations.push(Location::new(u, range));
            }

            let mut candidate_ids: Vec<DocId> =
                state.index.backlinks_of(target_doc_id).cloned().collect();
            if !candidate_ids.contains(target_doc_id) {
                candidate_ids.push(target_doc_id.clone());
            }

            for src_id in &candidate_ids {
                if let Some(src_doc) = state.index.get_doc(src_id) {
                    let Some(src_uri) = doc_uri(src_doc, state.vault_root.as_deref()) else {
                        continue;
                    };

                    for link in &src_doc.links {
                        let resolves_to_target = if link.target_doc.is_empty() {
                            src_id == target_doc_id
                        } else {
                            state.index.resolve_link(&link.target_doc) == Some(target_doc_id)
                        };

                        if resolves_to_target {
                            locations.push(Location::new(
                                src_uri.clone(),
                                byte_range_to_lsp(link.range, &src_doc.line_index),
                            ));
                        }
                    }
                }
            }
        }
    }

    if !params.context.include_declaration {
        locations.retain(|loc| {
            !(loc.uri.as_str() == uri
                && loc.range.start.line <= pos.line
                && loc.range.end.line >= pos.line
                && (loc.range.start.line < pos.line || loc.range.start.character <= pos.character)
                && (loc.range.end.line > pos.line || loc.range.end.character >= pos.character))
        });
    }

    locations.sort_by(|a, b| {
        (a.uri.as_str(), a.range.start.line, a.range.start.character).cmp(&(
            b.uri.as_str(),
            b.range.start.line,
            b.range.start.character,
        ))
    });
    locations.dedup();

    Some(locations)
}

#[cfg(test)]
#[allow(unused_variables)]
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use std::path::Path;

    use satz_core::{Index, parse_document};
    use tower_lsp_server::ls_types::{
        Position, ReferenceContext, TextDocumentIdentifier, TextDocumentPositionParams,
    };

    use super::*;

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

    #[test]
    fn test_find_block_references() {
        let abs_lsp = if cfg!(windows) {
            Path::new("C:\\LSP.md")
        } else {
            Path::new("/LSP.md")
        };
        let abs_daily = if cfg!(windows) {
            Path::new("C:\\daily.md")
        } else {
            Path::new("/daily.md")
        };

        let rel_lsp = Path::new("LSP.md");
        let rel_daily = Path::new("daily.md");

        let doc_lsp = parse_document(
            "# LSP\n\nArchitecture definition here ^mimari-tanim",
            rel_lsp,
        );
        let doc_daily = parse_document("# Daily\n\nSee [[LSP#^mimari-tanim]] for info.", rel_daily);

        let mut state = SatzState::default();
        state.index = Index::build(vec![doc_lsp, doc_daily]);
        state.vault_root = Some(if cfg!(windows) {
            Path::new("C:\\").to_path_buf()
        } else {
            Path::new("/").to_path_buf()
        });

        let uri_daily_str = if cfg!(windows) {
            "file:///C:/daily.md"
        } else {
            "file:///daily.md"
        };

        state.open_docs.insert(
            uri_daily_str.to_string(),
            crate::state::OpenDocument::new(
                uri_daily_str,
                abs_daily.to_path_buf(),
                "# Daily\n\nSee [[LSP#^mimari-tanim]] for info.",
                1,
            ),
        );

        let params = ReferenceParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: uri_daily_str.parse().unwrap(),
                },
                position: Position::new(2, 10), // on [[LSP#^mimari-tanim]]
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: ReferenceContext {
                include_declaration: true,
            },
        };

        let refs = find_references(params, &state).expect("References expected");
        // 1 block definition in LSP.md + 1 link in daily.md = 2
        assert_eq!(refs.len(), 2);
    }
}
