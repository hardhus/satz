use satz_core::{ByteRange, DocId, Document, LinkKind, fold_key, slugify};
use tower_lsp_server::ls_types::{
    DocumentHighlight, DocumentHighlightKind, DocumentHighlightParams,
};

use crate::convert::byte_range_to_lsp;
use crate::state::SatzState;

#[derive(Debug, Clone, PartialEq, Eq)]
enum HighlightTarget {
    Tag(String),
    Heading {
        doc: DocId,
        slug: String,
    },
    Block {
        doc: DocId,
        id: String,
    },
    Doc(DocId),
    /// A link whose note does not exist: it is grouped with the other links to the same thing.
    Broken(String),
    /// A footnote label (lowercased), defined or not.
    Footnote(String),
}

/// What a link to a missing note is grouped by: the folded note name plus its anchor.
fn broken_key(link: &satz_core::Link) -> String {
    format!(
        "{}#{}#{}",
        fold_key(&link.target_doc),
        link.target_heading
            .as_deref()
            .map(slugify)
            .unwrap_or_default(),
        link.target_block
            .as_deref()
            .unwrap_or_default()
            .to_ascii_lowercase()
    )
}

/// True for a link that has something to point at (external links and empty `[t]()` do not).
fn has_target(link: &satz_core::Link) -> bool {
    !satz_core::model::link::is_external_target(&link.target_doc)
        && (!link.target_doc.is_empty()
            || link.target_heading.is_some()
            || link.target_block.is_some())
}

/// The byte range of a footnote definition's `[^label]` token (not its body).
fn footnote_label_range(doc: &Document, start: usize) -> ByteRange {
    let source = doc.line_index.source();
    let end = source[start..].find(']').map_or(start, |n| start + n + 1);
    ByteRange::new(start, end)
}

/// What the cursor is on. Several things can cover the offset (a tag inside a heading, a link
/// inside a link label); the narrowest one wins.
fn cursor_target(doc: &Document, off: usize, state: &SatzState) -> Option<HighlightTarget> {
    let mut best: Option<(usize, HighlightTarget)> = None;
    let mut offer = |range: ByteRange, target: HighlightTarget| {
        let width = range.end - range.start;
        if range.contains(off) && best.as_ref().is_none_or(|(w, _)| width < *w) {
            best = Some((width, target));
        }
    };

    for link in &doc.links {
        if link.kind == LinkKind::Footnote {
            if let Some(label) = &link.display {
                offer(link.range, HighlightTarget::Footnote(label.to_lowercase()));
            }
            continue;
        }
        if !has_target(link) {
            continue;
        }
        let target = match state.link_target_doc(doc, link).cloned() {
            Some(target) => match (&link.target_block, &link.target_heading) {
                (Some(b), _) => HighlightTarget::Block {
                    doc: target,
                    id: b.clone(),
                },
                (_, Some(h)) => HighlightTarget::Heading {
                    doc: target,
                    slug: slugify(h),
                },
                _ => HighlightTarget::Doc(target),
            },
            None => HighlightTarget::Broken(broken_key(link)),
        };
        offer(link.range, target);
    }
    for link in &doc.broken_footnote_refs {
        if let Some(label) = &link.display {
            offer(link.range, HighlightTarget::Footnote(label.to_lowercase()));
        }
    }
    for def in &doc.footnotes.definitions {
        offer(
            footnote_label_range(doc, def.range.start),
            HighlightTarget::Footnote(def.label.to_lowercase()),
        );
    }
    for b in &doc.blocks {
        offer(
            b.range,
            HighlightTarget::Block {
                doc: doc.id.clone(),
                id: b.id.clone(),
            },
        );
    }
    for h in &doc.headings {
        offer(
            h.range,
            HighlightTarget::Heading {
                doc: doc.id.clone(),
                slug: h.slug.clone(),
            },
        );
    }
    for t in &doc.tags {
        offer(
            t.range,
            HighlightTarget::Tag(t.name.trim_start_matches('#').to_string()),
        );
    }

    best.map(|(_, target)| target)
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
    tracing::debug!(uri, "document_highlight");
    let pos = params.text_document_position_params.position;

    let (_, doc) = state.doc_for_uri(uri)?;

    let byte_offset = crate::convert::lsp_pos_to_byte(&doc.line_index, pos);

    let target = cursor_target(doc, byte_offset, state)?;
    let mut highlights = Vec::new();
    let mut push = |range: ByteRange, kind: DocumentHighlightKind| {
        highlights.push(DocumentHighlight {
            range: byte_range_to_lsp(range, &doc.line_index),
            kind: Some(kind),
        });
    };

    match target {
        HighlightTarget::Tag(ref tag_name) => {
            let clean_query = fold_key(tag_name.trim_start_matches('#'));
            let prefix = format!("{}/", clean_query);

            for t in &doc.tags {
                let clean_t = fold_key(t.name.trim_start_matches('#'));
                if clean_t == clean_query || clean_t.starts_with(&prefix) {
                    push(t.range, DocumentHighlightKind::TEXT);
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
                        push(h.range, DocumentHighlightKind::WRITE);
                    }
                }
            }
            // Highlight any links in this document pointing to this heading
            for link in state.links_to(doc, target_doc) {
                if link
                    .target_heading
                    .as_deref()
                    .is_some_and(|th| slugify(th) == *slug)
                {
                    push(link.range, DocumentHighlightKind::READ);
                }
            }
        }
        HighlightTarget::Block {
            doc: ref target_doc,
            ref id,
        } => {
            // If target doc is current doc, highlight the block definition
            if target_doc == &doc.id
                && let Some(b) = doc.resolve_block(id).map(|i| &doc.blocks[i])
            {
                push(b.range, DocumentHighlightKind::WRITE);
            }
            // Highlight any links in this document pointing to this block
            for link in state.links_to(doc, target_doc) {
                if link
                    .target_block
                    .as_deref()
                    .is_some_and(|b| b.eq_ignore_ascii_case(id))
                {
                    push(link.range, DocumentHighlightKind::READ);
                }
            }
        }
        HighlightTarget::Doc(ref target_doc) => {
            for link in state.links_to(doc, target_doc) {
                push(link.range, DocumentHighlightKind::READ);
            }
        }
        HighlightTarget::Broken(ref key) => {
            for link in &doc.links {
                if has_target(link)
                    && link.kind != LinkKind::Footnote
                    && state.link_target_doc(doc, link).is_none()
                    && broken_key(link) == *key
                {
                    push(link.range, DocumentHighlightKind::READ);
                }
            }
        }
        HighlightTarget::Footnote(ref label) => {
            for link in doc.links.iter().chain(&doc.broken_footnote_refs) {
                if link.kind == LinkKind::Footnote
                    && link
                        .display
                        .as_ref()
                        .is_some_and(|l| l.to_lowercase() == *label)
                {
                    push(link.range, DocumentHighlightKind::READ);
                }
            }
            for def in &doc.footnotes.definitions {
                if def.label.to_lowercase() == *label {
                    push(
                        footnote_label_range(doc, def.range.start),
                        DocumentHighlightKind::WRITE,
                    );
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
mod tests;

#[cfg(test)]
mod behavior_tests;
