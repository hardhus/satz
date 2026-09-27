// Test states are built field by field so each test shows exactly what it sets up.
#![allow(clippy::field_reassign_with_default)]

use super::*;
use satz_core::{Index, parse_document};
use std::path::Path;
use tower_lsp_server::ls_types::TextDocumentIdentifier;

fn symbols(text: &str) -> Option<Vec<DocumentSymbol>> {
    let rel = Path::new("a.md");
    let mut state = SatzState::default();
    state.index = Index::build(vec![parse_document(text, rel)]);
    state.open_docs.insert(
        "file:///a.md".to_string(),
        crate::state::OpenDocument::new("file:///a.md", rel.to_path_buf(), text, 1),
    );
    let params = DocumentSymbolParams {
        text_document: TextDocumentIdentifier {
            uri: "file:///a.md".parse().unwrap(),
        },
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
    };
    match document_symbol(params, &state)? {
        DocumentSymbolResponse::Nested(s) => Some(s),
        DocumentSymbolResponse::Flat(_) => panic!("nested symbols expected"),
    }
}

fn lines(s: &DocumentSymbol) -> (u32, u32) {
    (s.range.start.line, s.range.end.line)
}

fn contains(outer: &DocumentSymbol, inner: &DocumentSymbol) -> bool {
    let a = outer.range;
    let b = inner.range;
    (a.start.line, a.start.character) <= (b.start.line, b.start.character)
        && (a.end.line, a.end.character) >= (b.end.line, b.end.character)
}

fn check_tree(symbol: &DocumentSymbol) {
    let sel = symbol.selection_range;
    assert!(
        contains(
            symbol,
            &DocumentSymbol {
                range: sel,
                ..symbol.clone()
            }
        ),
        "selection_range must lie inside range for {}",
        symbol.name
    );
    for child in symbol.children.as_deref().unwrap_or_default() {
        assert!(
            contains(symbol, child),
            "{} must contain {}",
            symbol.name,
            child.name
        );
        check_tree(child);
    }
}

#[test]
fn a_heading_symbol_spans_its_whole_section_including_its_children() {
    let text = "# One\n\ntext\n\n## Sub\n\nmore\n\n# Two\n\nend\n";
    let tree = symbols(text).unwrap();
    assert_eq!(tree.len(), 2);
    assert_eq!(tree[0].name, "One");
    assert_eq!(
        lines(&tree[0]),
        (0, 6),
        "up to the last content line before `# Two`"
    );
    assert_eq!(tree[0].children.as_ref().unwrap().len(), 1);
    let sub = &tree[0].children.as_ref().unwrap()[0];
    assert_eq!((sub.name.as_str(), lines(sub)), ("Sub", (4, 6)));
    assert_eq!(lines(&tree[1]), (8, 10));
    // The selection is the heading line only.
    assert_eq!(tree[0].selection_range.start.line, 0);
    assert_eq!(tree[0].selection_range.end.line, 0);
    for symbol in &tree {
        check_tree(symbol);
    }
}

#[test]
fn the_last_section_ends_at_the_last_content_line() {
    let tree = symbols("# A\n\ntext\n\n\n").unwrap();
    assert_eq!(lines(&tree[0]), (0, 2));
    let tree = symbols("# A").unwrap();
    assert_eq!(lines(&tree[0]), (0, 0));
}

#[test]
fn skipped_levels_and_repeated_levels_nest_correctly() {
    let tree = symbols("# A\n\n### Deep\n\n## Mid\n\n# B\n").unwrap();
    assert_eq!(tree.len(), 2);
    let names: Vec<&str> = tree[0]
        .children
        .as_ref()
        .unwrap()
        .iter()
        .map(|c| c.name.as_str())
        .collect();
    assert_eq!(names, vec!["Deep", "Mid"]);
    for symbol in &tree {
        check_tree(symbol);
    }
}

#[test]
fn a_heading_without_text_still_gets_a_name() {
    let tree = symbols("#\n\ntext\n\n## \n").unwrap();
    assert_eq!(tree[0].name, "(empty heading)");
    assert!(!tree[0].children.as_ref().unwrap()[0].name.is_empty());
}

#[test]
fn setext_crlf_block_id_and_unicode_headings() {
    let tree = symbols("Title ^blk\r\n=====\r\n\r\ntext\r\n\r\n## Günün 🦀 Özeti\r\n").unwrap();
    assert_eq!(tree[0].name, "Title");
    assert_eq!(tree[0].selection_range.start.line, 0);
    let child = &tree[0].children.as_ref().unwrap()[0];
    assert_eq!(child.name, "Günün 🦀 Özeti");
    assert_eq!(child.range.start.line, 5);
    check_tree(&tree[0]);
}

#[test]
fn a_document_without_headings_has_no_symbols() {
    assert!(symbols("just text\n\nmore\n").is_none());
    assert!(symbols("").is_none());
}

#[test]
fn equal_headings_are_separate_symbols_with_their_own_ranges() {
    let symbols = symbols("# A\n\n## Same\n\ntext\n\n## Same\n\nmore\n").unwrap();
    assert_eq!(symbols.len(), 1);
    let children = symbols[0].children.as_ref().unwrap();
    let named: Vec<(&str, u32)> = children
        .iter()
        .map(|c| (c.name.as_str(), c.range.start.line))
        .collect();
    assert_eq!(named, vec![("Same", 2), ("Same", 6)]);
    assert_eq!(
        lines(&children[0]),
        (2, 4),
        "the section ends at its last content line"
    );
}
