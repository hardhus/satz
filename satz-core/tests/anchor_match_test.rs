//! `#Heading` and `#^block` references: what they match, and how fast.

use satz_core::{Index, LinkResolution, parse_document};
use std::path::Path;

/// How the first link of `a.md` resolves against `b.md` with the given text.
fn resolve(link: &str, b_text: &str) -> &'static str {
    let a = parse_document(&format!("{link}\n"), Path::new("a.md"));
    let b = parse_document(b_text, Path::new("b.md"));
    let index = Index::build(vec![a, b]);
    let doc = index
        .documents()
        .find(|d| d.path == Path::new("a.md"))
        .unwrap();
    match index.resolve_link_full(&doc.links[0], Some(doc)) {
        LinkResolution::Resolved {
            anchor: Some(_), ..
        } => "anchor",
        LinkResolution::Resolved { anchor: None, .. } => "doc",
        LinkResolution::AnchorMissing { .. } => "anchor-missing",
        LinkResolution::DocMissing => "doc-missing",
    }
}

#[test]
fn a_block_id_is_matched_ignoring_case() {
    assert_eq!(resolve("[[b#^abc]]", "text ^abc\n"), "anchor");
    assert_eq!(resolve("[[b#^ABC]]", "text ^abc\n"), "anchor");
    assert_eq!(resolve("[[b#^abc]]", "text ^AbC\n"), "anchor");
    assert_eq!(resolve("[[b#^abd]]", "text ^abc\n"), "anchor-missing");
    assert_eq!(resolve("[[b#^ab]]", "text ^abc\n"), "anchor-missing");
}

#[test]
fn heading_references_match_by_text_or_slug_in_any_case() {
    let b = "# İşlem Notları\n\ntext\n";
    for link in [
        "[[b#İşlem Notları]]",
        "[[b#işlem notları]]",
        "[[b#İŞLEM NOTLARI]]",
        "[[b#işlem-notları]]",
    ] {
        assert_eq!(resolve(link, b), "anchor", "{link}");
    }
    assert_eq!(resolve("[[b#işlem]]", b), "anchor-missing");
    assert_eq!(resolve("[[b#Yok]]", b), "anchor-missing");
}

#[test]
fn a_heading_without_letters_is_not_matched_by_another_such_reference() {
    // Both slugify to "", which must not count as "the same heading".
    assert_eq!(resolve("[[b#***]]", "# ?!\n"), "anchor-missing");
    assert_eq!(resolve("[[b#?!]]", "# ?!\n"), "anchor");
    assert_eq!(resolve("[[b#★]]", "# ★\n\ntext\n"), "anchor");
    assert_eq!(resolve("[[b#★]]", "# ☆\n"), "anchor-missing");
}

#[test]
fn thousands_of_headings_and_links_resolve_quickly() {
    let mut b = String::new();
    let mut a = String::new();
    for i in 0..2000 {
        b.push_str(&format!("## Bölüm {i}\n\ntext ^blk{i}\n\n"));
        a.push_str(&format!("[[b#Bölüm {i}]] [[b#^BLK{i}]]\n"));
    }
    let index = Index::build(vec![
        parse_document(&a, Path::new("a.md")),
        parse_document(&b, Path::new("b.md")),
    ]);
    let doc = index
        .documents()
        .find(|d| d.path == Path::new("a.md"))
        .unwrap();
    let start = std::time::Instant::now();
    let resolved = doc
        .links
        .iter()
        .filter(|l| {
            matches!(
                index.resolve_link_full(l, Some(doc)),
                LinkResolution::Resolved {
                    anchor: Some(_),
                    ..
                }
            )
        })
        .count();
    assert_eq!(resolved, 4000);
    assert!(
        start.elapsed() < std::time::Duration::from_secs(5),
        "{:?}",
        start.elapsed()
    );
}
