use satz_core::{DocId, Document, fold_key, slugify};
use tower_lsp_server::ls_types::{Location, ReferenceParams};

use crate::convert::byte_range_to_lsp;
use crate::state::{SatzState, SelfLinks};

#[derive(Debug, Clone, PartialEq, Eq)]
enum CursorTarget {
    Tag(String),
    /// `index` is the heading it resolves to (the first match; duplicates own no links); `None`
    /// for a reference to a heading that does not exist.
    Heading {
        doc: DocId,
        slug: String,
        index: Option<usize>,
    },
    Block {
        doc: DocId,
        id: String,
    },
    Doc(DocId),
}

fn cursor_target(doc: &Document, off: usize, state: &SatzState) -> Option<CursorTarget> {
    let index = &state.index;
    // Priority order: link > block > heading > tag > document
    if let Some(l) = doc.link_at(off) {
        let target = state.link_target_doc(doc, l)?.clone();
        return Some(match (&l.target_block, &l.target_heading) {
            (Some(b), _) => CursorTarget::Block {
                doc: target,
                id: b.clone(),
            },
            (_, Some(h)) => CursorTarget::Heading {
                index: index.get_doc(&target).and_then(|d| d.resolve_heading(h)),
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

    if let Some(i) = doc.headings.iter().position(|h| h.range.contains(off)) {
        return Some(CursorTarget::Heading {
            doc: doc.id.clone(),
            slug: doc.headings[i].slug.clone(),
            index: Some(i),
        });
    }

    if let Some(t) = doc.tags.iter().find(|t| t.range.contains(off)) {
        return Some(CursorTarget::Tag(
            t.name.trim_start_matches('#').to_string(),
        ));
    }

    Some(CursorTarget::Doc(doc.id.clone()))
}

pub fn find_references(params: ReferenceParams, state: &SatzState) -> Option<Vec<Location>> {
    let uri = params.text_document_position.text_document.uri.as_str();
    let pos = params.text_document_position.position;
    tracing::debug!(uri, ?pos, "find_references");

    let (_, doc) = state.doc_for_uri(uri)?;

    let byte_offset = crate::convert::lsp_pos_to_byte(&doc.line_index, pos);

    let target = cursor_target(doc, byte_offset, state)?;
    let mut locations = Vec::new();
    // Where the target is declared (the heading, the block, the note's start), if it has a place.
    let mut declaration: Option<Location> = None;

    match target {
        CursorTarget::Tag(ref tag_name) => {
            let clean_query = fold_key(tag_name.trim_start_matches('#'));
            let prefix = format!("{}/", clean_query);

            for tagged_doc in state.index.docs_with_tag(tag_name) {
                let Some(u) = state.doc_uri(tagged_doc) else {
                    continue;
                };
                for t in &tagged_doc.tags {
                    let clean_t = fold_key(t.name.trim_start_matches('#'));
                    if clean_t == clean_query || clean_t.starts_with(&prefix) {
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
                && let Some(b) = target_doc.resolve_block(id).map(|i| &target_doc.blocks[i])
                && let Some(u) = state.doc_uri(target_doc)
            {
                let location = Location::new(u, byte_range_to_lsp(b.range, &target_doc.line_index));
                declaration = Some(location.clone());
                locations.push(location);
            }

            for src_doc in state.documents_linking_to(doc, SelfLinks::Include) {
                let Some(src_uri) = state.doc_uri(src_doc) else {
                    continue;
                };

                for link in state.links_to(src_doc, doc) {
                    if link
                        .target_block
                        .as_deref()
                        .is_some_and(|b| b.eq_ignore_ascii_case(id))
                    {
                        locations.push(Location::new(
                            src_uri.clone(),
                            byte_range_to_lsp(link.range, &src_doc.line_index),
                        ));
                    }
                }
            }
        }
        CursorTarget::Heading {
            ref doc,
            ref slug,
            index: heading_index,
        } => {
            let target_doc_opt = state.index.get_doc(doc);
            if let Some(target_doc) = target_doc_opt
                && let Some(h) = match heading_index {
                    Some(i) => target_doc.headings.get(i),
                    None => target_doc.headings.iter().find(|h| &h.slug == slug),
                }
                && let Some(u) = state.doc_uri(target_doc)
            {
                let location = Location::new(u, byte_range_to_lsp(h.range, &target_doc.line_index));
                declaration = Some(location.clone());
                locations.push(location);
            }

            for src_doc in state.documents_linking_to(doc, SelfLinks::Include) {
                let Some(src_uri) = state.doc_uri(src_doc) else {
                    continue;
                };

                for link in state.links_to(src_doc, doc) {
                    // A reference belongs to the first heading it matches; a later duplicate
                    // owns none. (An unresolved heading falls back to the slug.)
                    let owns_link = link.target_heading.as_deref().is_some_and(|th| {
                        match (heading_index, target_doc_opt) {
                            (Some(i), Some(target_doc)) => {
                                target_doc.resolve_heading(th) == Some(i)
                            }
                            _ => slugify(th) == *slug,
                        }
                    });
                    if owns_link {
                        locations.push(Location::new(
                            src_uri.clone(),
                            byte_range_to_lsp(link.range, &src_doc.line_index),
                        ));
                    }
                }
            }
        }
        CursorTarget::Doc(ref target_doc_id) => {
            if let Some(target_doc) = state.index.get_doc(target_doc_id)
                && let Some(u) = state.doc_uri(target_doc)
            {
                let range = if let Some(h) = target_doc.headings.first() {
                    byte_range_to_lsp(h.range, &target_doc.line_index)
                } else {
                    byte_range_to_lsp(satz_core::ByteRange::new(0, 0), &target_doc.line_index)
                };
                let location = Location::new(u, range);
                declaration = Some(location.clone());
                locations.push(location);
            }

            for src_doc in state.documents_linking_to(target_doc_id, SelfLinks::Include) {
                let Some(src_uri) = state.doc_uri(src_doc) else {
                    continue;
                };

                for link in state.links_to(src_doc, target_doc_id) {
                    locations.push(Location::new(
                        src_uri.clone(),
                        byte_range_to_lsp(link.range, &src_doc.line_index),
                    ));
                }
            }
        }
    }

    // Without the declaration, drop exactly that location -- not whatever is under the cursor (a
    // link there is a real reference). Locations compare by `Uri` value, not by spelling.
    if !params.context.include_declaration
        && let Some(declaration) = &declaration
    {
        locations.retain(|loc| loc != declaration);
    }

    crate::convert::sort_locations(&mut locations);
    locations.dedup();

    Some(locations)
}

#[cfg(test)]
mod tests;
