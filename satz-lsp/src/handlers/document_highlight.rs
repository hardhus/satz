use satz_core::{ByteRange, DocId, Document, LinkKind, fold_key, slugify};
use tower_lsp_server::ls_types::{
    DocumentHighlight, DocumentHighlightKind, DocumentHighlightParams,
};

use crate::convert::{byte_range_to_lsp, lsp_pos_to_satz};
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

    let satz_pos = lsp_pos_to_satz(pos);
    let byte_offset = doc.line_index.position_to_byte(satz_pos);

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
            for link in &doc.links {
                if state.link_target_doc(doc, link) == Some(target_doc)
                    && link
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
            for link in &doc.links {
                if state.link_target_doc(doc, link) == Some(target_doc)
                    && link
                        .target_block
                        .as_deref()
                        .is_some_and(|b| b.eq_ignore_ascii_case(id))
                {
                    push(link.range, DocumentHighlightKind::READ);
                }
            }
        }
        HighlightTarget::Doc(ref target_doc) => {
            for link in &doc.links {
                if state.link_target_doc(doc, link) == Some(target_doc) {
                    push(link.range, DocumentHighlightKind::READ);
                }
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

#[cfg(test)]
#[allow(clippy::field_reassign_with_default)]
mod behavior_tests {
    use super::*;
    use satz_core::{Index, parse_document};
    use std::path::Path;
    use tower_lsp_server::ls_types::{
        Position, TextDocumentIdentifier, TextDocumentPositionParams,
    };

    /// `(line, start col, end col, is_write)` of every highlight at `at` in `open`, sorted.
    fn highlights(
        files: &[(&str, &str)],
        open: &str,
        at: (u32, u32),
    ) -> Option<Vec<(u32, u32, u32, bool)>> {
        let root = if cfg!(windows) {
            Path::new("C:\\vault").to_path_buf()
        } else {
            Path::new("/vault").to_path_buf()
        };
        let mut state = SatzState::default();
        state.index = Index::build(
            files
                .iter()
                .map(|(p, t)| parse_document(t, Path::new(p)))
                .collect(),
        );
        state.vault_root = Some(root.clone());
        let text = files.iter().find(|(p, _)| *p == open).unwrap().1;
        let uri = crate::convert::path_to_uri(&root.join(open))
            .unwrap()
            .as_str()
            .to_string();
        state.open_docs.insert(
            uri.clone(),
            crate::state::OpenDocument::new(&uri, root.join(open), text, 1),
        );
        let params = DocumentHighlightParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: uri.parse().unwrap(),
                },
                position: Position::new(at.0, at.1),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };
        let mut found: Vec<_> = document_highlight(params, &state)?
            .into_iter()
            .map(|h| {
                (
                    h.range.start.line,
                    h.range.start.character,
                    h.range.end.character,
                    h.kind == Some(DocumentHighlightKind::WRITE),
                )
            })
            .collect();
        found.sort();
        Some(found)
    }

    fn one(text: &str, at: (u32, u32)) -> Option<Vec<(u32, u32, u32, bool)>> {
        highlights(&[("a.md", text)], "a.md", at)
    }

    #[test]
    fn a_tag_in_a_heading_is_a_tag_not_the_heading() {
        let text = "# Title #tag\n\ntext #tag and #other\n";
        // On `#tag` in the heading: both `#tag`s, not the heading section.
        assert_eq!(
            one(text, (0, 9)),
            Some(vec![(0, 8, 12, false), (2, 5, 9, false)])
        );
        // On the heading text: the heading itself.
        let on_text = one(text, (0, 3)).unwrap();
        assert_eq!(on_text.len(), 1);
        assert_eq!((on_text[0].0, on_text[0].1, on_text[0].3), (0, 0, true));
    }

    #[test]
    fn links_to_a_missing_note_highlight_each_other_only() {
        let text = "[[ghost]] and [[Ghost]] and [[other]] and [t](ghost.md)\n";
        assert_eq!(
            one(text, (0, 3)),
            Some(vec![(0, 0, 9, false), (0, 14, 23, false)])
        );
        // A different broken target is its own group.
        assert_eq!(one(text, (0, 31)), Some(vec![(0, 28, 37, false)]));
    }

    #[test]
    fn a_broken_heading_reference_groups_by_note_and_heading() {
        let files = [
            ("a.md", "[[b#Yok]] [[b#yok]] [[b#Other]] [[b]]\n"),
            ("b.md", "# B\n"),
        ];
        assert_eq!(
            highlights(&files, "a.md", (0, 3)),
            Some(vec![(0, 0, 9, false), (0, 10, 19, false)])
        );
    }

    #[test]
    fn a_footnote_reference_and_its_definition_highlight_together() {
        let text = "One[^a] and two[^A] and [^b].\n\n[^a]: first note\n[^b]: second\n";
        // On a reference: every reference with that label (any case) and the definition label.
        assert_eq!(
            one(text, (0, 4)),
            Some(vec![(0, 3, 7, false), (0, 15, 19, false), (2, 0, 4, true)])
        );
        // On the definition label: the same set.
        assert_eq!(
            one(text, (2, 1)),
            Some(vec![(0, 3, 7, false), (0, 15, 19, false), (2, 0, 4, true)])
        );
        // Text of the definition is not the label.
        assert_eq!(one(text, (2, 10)), None);
    }

    #[test]
    fn a_footnote_without_a_definition_still_groups_its_references() {
        let text = "x[^gone] y[^gone] z[^other]\n";
        assert_eq!(
            one(text, (0, 3)),
            Some(vec![(0, 1, 8, false), (0, 10, 17, false)])
        );
    }

    #[test]
    fn a_link_without_a_target_highlights_nothing() {
        assert_eq!(one("[t]() and [t]()\n", (0, 1)), None);
        assert_eq!(one("[[]] text\n", (0, 2)), None);
        assert_eq!(one("plain text\n", (0, 3)), None);
    }

    #[test]
    fn external_links_highlight_nothing() {
        assert_eq!(
            one(
                "[x](https://example.com) [y](https://example.com)\n",
                (0, 1)
            ),
            None
        );
    }

    #[test]
    fn relative_markdown_links_use_the_notes_own_folder() {
        let files = [
            ("sub/a.md", "[t](b.md) [u](../b.md) [v](./b.md)\n"),
            ("sub/b.md", "# sub b\n"),
            ("b.md", "# root b\n"),
        ];
        // `b.md` and `./b.md` are sub/b.md; `../b.md` is the root note.
        assert_eq!(
            highlights(&files, "sub/a.md", (0, 1)),
            Some(vec![(0, 0, 9, false), (0, 23, 34, false)])
        );
        assert_eq!(
            highlights(&files, "sub/a.md", (0, 12)),
            Some(vec![(0, 10, 22, false)])
        );
    }

    #[test]
    fn block_ids_match_ignoring_case_and_positions_past_the_end_are_safe() {
        let text = "para ^abc\n\nsee [[a#^ABC]]\n";
        assert_eq!(
            one(text, (2, 6)),
            Some(vec![(0, 5, 9, true), (2, 4, 14, false)])
        );
        assert_eq!(one(text, (99, 0)), None);
        assert_eq!(one("", (0, 0)), None);
    }

    #[test]
    fn a_link_inside_a_link_label_highlights_as_the_inner_link() {
        let text = "[see [[inner]]](outer.md) and [[inner]]
";
        assert_eq!(
            one(text, (0, 8)),
            Some(vec![(0, 5, 14, false), (0, 30, 39, false)])
        );
        assert_eq!(one(text, (0, 2)), Some(vec![(0, 0, 25, false)]));
        assert_eq!(one(text, (0, 20)), Some(vec![(0, 0, 25, false)]));
    }

    #[test]
    fn utf16_positions_and_crlf_are_respected() {
        let text = "🦀 [[gizli]] ve [[gizli]]\r\n";
        assert_eq!(
            one(text, (0, 5)),
            Some(vec![(0, 3, 12, false), (0, 16, 25, false)])
        );
    }
}
