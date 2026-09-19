//! `Document::link_at`: the link under a byte offset, innermost first.

use satz_core::{Document, parse_document};
use std::path::Path;

fn doc(text: &str) -> Document {
    parse_document(text, Path::new("a.md"))
}

fn target_at(doc: &Document, needle: &str, nth_char: usize) -> Option<String> {
    let start = doc.line_index.source().find(needle).unwrap();
    doc.link_at(start + nth_char).map(|l| l.target_doc.clone())
}

#[test]
fn a_single_link_is_found_across_its_whole_range_and_nowhere_else() {
    let d = doc("see [[note]] here\n");
    assert_eq!(target_at(&d, "[[note]]", 0), Some("note".into()));
    assert_eq!(target_at(&d, "[[note]]", 7), Some("note".into()));
    assert_eq!(target_at(&d, "[[note]]", 8), None, "end is exclusive");
    assert_eq!(target_at(&d, "see", 0), None);
    assert_eq!(d.link_at(10_000), None);
}

#[test]
fn adjacent_links_do_not_bleed_into_each_other() {
    let d = doc("[[a]][[b]] [x](c.md)[y](d.md)\n");
    assert_eq!(target_at(&d, "[[a]]", 4), Some("a".into()));
    assert_eq!(target_at(&d, "[[b]]", 0), Some("b".into()));
    assert_eq!(target_at(&d, "[x](c.md)", 8), Some("c.md".into()));
    assert_eq!(target_at(&d, "[y](d.md)", 0), Some("d.md".into()));
}

#[test]
fn the_innermost_of_nested_links_wins() {
    let d = doc("[see [[inner]]](outer.md)\n");
    let inner = target_at(&d, "[[inner]]", 3);
    let outer = target_at(&d, "](outer.md)", 3);
    assert_eq!(inner.as_deref(), Some("inner"), "{:?}", d.links);
    assert_eq!(outer.as_deref(), Some("outer.md"), "{:?}", d.links);
}
