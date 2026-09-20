//! Text that is spelled with combining marks (NFD, what macOS file systems hand out) means the same
//! as the precomposed spelling (NFC): links, headings, tags and file names match either way.

use satz_core::{Index, LinkResolution, fold_key, parse_document, slugify};
use std::path::Path;

/// (NFC, NFD) spellings of the same word.
const PAIRS: &[(&str, &str)] = &[
    ("dünya", "du\u{308}nya"),
    ("şehir", "s\u{327}ehir"),
    ("çığ", "c\u{327}ığ"),
    ("Ünlü", "U\u{308}nlu\u{308}"),
    ("ÖĞRENCİ", "O\u{308}G\u{306}RENCI\u{307}"),
    ("İstanbul", "I\u{307}stanbul"),
    ("café", "cafe\u{301}"),
    ("한글", "\u{1112}\u{1161}\u{11ab}\u{1100}\u{1173}\u{11af}"),
    ("ǆ", "ǆ"),
    ("Å", "A\u{30a}"),
];

#[test]
fn both_spellings_have_one_slug() {
    for (nfc, nfd) in PAIRS {
        assert_eq!(slugify(nfc), slugify(nfd), "{nfc:?} vs {nfd:?}");
        assert!(!slugify(nfd).contains('\u{308}'), "{nfd:?}");
    }
    assert_eq!(slugify("du\u{308}nya"), "dünya");
    assert_eq!(
        slugify("Merhaba Du\u{308}nya! (2024)"),
        "merhaba-dünya-2024"
    );
}

#[test]
fn both_spellings_have_one_folded_key() {
    for (nfc, nfd) in PAIRS {
        assert_eq!(fold_key(nfc), fold_key(nfd), "{nfc:?} vs {nfd:?}");
    }
    assert_eq!(fold_key("du\u{308}nya"), "dünya");
    // A dotted capital I written as I + combining dot folds like the precomposed one.
    assert_eq!(fold_key("I\u{307}stanbul"), fold_key("İstanbul"));
    assert_eq!(fold_key("I\u{307}stanbul"), "istanbul");
}

#[test]
fn a_combining_mark_alone_or_at_an_edge_does_not_break_anything() {
    assert_eq!(slugify("\u{308}"), "");
    assert_eq!(fold_key("\u{308}"), "\u{308}");
    assert_eq!(
        slugify("e\u{301}\u{301}\u{301}"),
        slugify("é\u{301}\u{301}")
    );
    assert_eq!(slugify("\u{200d}\u{200d}"), "");
    assert_eq!(fold_key("a\u{308}\u{308} b"), fold_key("ä\u{308} b"));
}

#[test]
fn plain_ascii_and_already_composed_text_is_unchanged() {
    assert_eq!(slugify("Hello, World 2"), "hello-world-2");
    assert_eq!(fold_key("  Hello   World "), "hello world");
    assert_eq!(slugify("dünya"), "dünya");
}

#[test]
fn a_very_long_decomposed_string_is_folded_quickly() {
    let text = "u\u{308}".repeat(100_000);
    let start = std::time::Instant::now();
    let key = fold_key(&text);
    let slug = slugify(&text);
    assert_eq!(key.chars().count(), 100_000);
    assert_eq!(slug, "ü".repeat(100_000));
    assert!(
        start.elapsed() < std::time::Duration::from_millis(500),
        "{:?}",
        start.elapsed()
    );
}

fn index_of(files: &[(&str, &str)]) -> Index {
    Index::build(
        files
            .iter()
            .map(|(path, text)| parse_document(text, Path::new(path)))
            .collect(),
    )
}

#[test]
fn a_composed_heading_link_finds_a_decomposed_heading_and_the_other_way_round() {
    let index = index_of(&[
        ("a.md", "# Not\n\n## Du\u{308}nya\n"),
        ("b.md", "# B\n\n[[a#Dünya]]\n\n## Şehir\n"),
        ("c.md", "# C\n\n[[b#S\u{327}ehir]]\n"),
    ]);
    for (from, link_idx) in [("b.md", 0), ("c.md", 0)] {
        let doc = index.get_doc_by_path(Path::new(from)).unwrap();
        let resolution = index.resolve_link_full(&doc.links[link_idx], Some(doc));
        assert!(
            matches!(
                resolution,
                LinkResolution::Resolved {
                    anchor: Some(_),
                    ..
                }
            ),
            "{from}: {resolution:?}"
        );
    }
}

#[test]
fn a_decomposed_file_name_or_title_or_tag_is_found_by_the_composed_spelling() {
    let index = index_of(&[
        ("du\u{308}nya.md", "# X\n"),
        ("y.md", "---\ntitle: Şehir\ntags: [öğrenci]\n---\n# Y\n"),
        (
            "z.md",
            "---\ntitle: C\u{327}ig\u{306}\ntags: [o\u{308}g\u{306}renci]\n---\n# Z\n",
        ),
    ]);
    assert_eq!(
        index.resolve_link("dünya").map(|i| i.as_str().to_string()),
        Some("du\u{308}nya.md".to_string())
    );
    assert!(index.resolve_link("DÜNYA").is_some());
    assert!(index.resolve_link("s\u{327}ehir").is_some());
    assert!(index.resolve_link("Çiğ").is_some());
    assert_eq!(index.docs_with_tag("öğrenci").count(), 2);
    assert_eq!(index.docs_with_tag("o\u{308}g\u{306}renci").count(), 2);
}

#[test]
fn a_link_to_a_decomposed_note_gives_it_a_backlink() {
    let index = index_of(&[("a.md", "# A\n\n[[dünya]]\n"), ("du\u{308}nya.md", "# D\n")]);
    let target = index.resolve_link("dünya").unwrap().clone();
    assert_eq!(index.backlinks_of(&target).count(), 1);
    assert_eq!(index.orphan_docs().count(), 1, "only a.md is an orphan");
}
