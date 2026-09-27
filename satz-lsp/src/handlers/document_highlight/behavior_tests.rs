// Test states are built field by field so each test shows exactly what it sets up.
#![allow(clippy::field_reassign_with_default)]

use super::*;
use satz_core::{Index, parse_document};
use std::path::Path;
use tower_lsp_server::ls_types::{Position, TextDocumentIdentifier, TextDocumentPositionParams};

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
    state.set_vault_root(Some(root.clone()));
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
fn a_frontmatter_tag_is_highlighted_where_it_is_written() {
    let text = "---
title: rust notes
tags: [rust]
---
body #rust
";
    // From the body tag: the tag in the frontmatter list (not the `title`) and the body tag.
    assert_eq!(
        one(text, (4, 7)),
        Some(vec![(2, 7, 11, false), (4, 5, 10, false)])
    );
    // From the frontmatter tag itself: the same two.
    assert_eq!(
        one(text, (2, 8)),
        Some(vec![(2, 7, 11, false), (4, 5, 10, false)])
    );
}

#[test]
fn utf16_positions_and_crlf_are_respected() {
    let text = "🦀 [[gizli]] ve [[gizli]]\r\n";
    assert_eq!(
        one(text, (0, 5)),
        Some(vec![(0, 3, 12, false), (0, 16, 25, false)])
    );
}
