//! Parsing edge cases: CRLF and unclosed frontmatter, headings with special characters.

use satz_core::{Index, LinkResolution, parse_document, slugify};
use std::path::Path;

fn doc(text: &str) -> satz_core::Document {
    parse_document(text, Path::new("n.md"))
}

#[test]
fn crlf_frontmatter_gives_the_same_facts_as_lf() {
    let lf = "---\ntitle: T\naliases: [x, y]\ntags: [a, b]\n---\n# H\n\ntext #body [[n]]\n";
    let crlf = lf.replace('\n', "\r\n");
    let (a, b) = (doc(lf), doc(&crlf));
    assert_eq!(a.title, b.title);
    assert_eq!(a.frontmatter.aliases, b.frontmatter.aliases);
    assert_eq!(a.frontmatter.tags, b.frontmatter.tags);
    assert!(b.frontmatter_range.is_some());
    assert_eq!(b.frontmatter_error, None);
    let names = |d: &satz_core::Document| -> Vec<String> {
        d.tags.iter().map(|t| t.name.clone()).collect()
    };
    assert_eq!(names(&a), names(&b));
    assert_eq!(a.headings.len(), b.headings.len());
    assert_eq!(b.headings[0].text, "H", "no stray carriage return");
    assert_eq!(a.links.len(), b.links.len());
    // Line numbers agree between the two spellings.
    let line = |d: &satz_core::Document, at: usize| d.line_index.byte_to_position(at).line;
    assert_eq!(
        line(&a, a.headings[0].range.start),
        line(&b, b.headings[0].range.start)
    );
}

#[test]
fn an_empty_frontmatter_block_is_read_the_same_with_lf_and_crlf() {
    // Fences with nothing between them are thematic breaks (as in CommonMark), not an empty block.
    // Line endings make no difference either way.
    for lf in [
        "---\n---\n# H\n",
        "---\n\n---\ntext\n",
        "---\ntitle: T\n---\n",
    ] {
        let (a, b) = (doc(lf), doc(&lf.replace('\n', "\r\n")));
        assert_eq!(
            a.frontmatter_range.is_some(),
            b.frontmatter_range.is_some(),
            "{lf:?}"
        );
        assert_eq!(a.frontmatter_error, b.frontmatter_error, "{lf:?}");
        assert_eq!(a.title, b.title, "{lf:?}");
    }
    assert!(doc("---\n\n---\ntext\n").frontmatter_range.is_none());
    assert!(doc("---\n---\ntext\n").frontmatter_range.is_none());
    assert!(doc("---\n#\n---\ntext\n").frontmatter_range.is_some());
}

#[test]
fn unclosed_frontmatter_is_not_frontmatter_and_does_not_swallow_the_note() {
    let src = "---\ntitle: T\ntags: [a]\n# Heading\ntext #body [[other]]\n";
    let d = doc(src);
    assert!(d.frontmatter_range.is_none());
    assert_eq!(d.frontmatter_error, None);
    assert!(d.frontmatter.tags.is_empty() && d.frontmatter.aliases.is_empty());
    assert_eq!(
        d.title, "Heading",
        "the title comes from the heading, not the `title:` line"
    );
    assert_eq!(d.headings.len(), 1);
    assert_eq!(
        d.tags.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
        vec!["body"]
    );
    assert_eq!(d.links.len(), 1);
    // The same with CRLF and with only the opening fence.
    let crlf = doc(&src.replace('\n', "\r\n"));
    assert!(crlf.frontmatter_range.is_none());
    assert_eq!(crlf.headings.len(), 1);
    assert!(doc("---\n").frontmatter_range.is_none());
    assert!(doc("---").frontmatter_range.is_none());
}

#[test]
fn a_closing_fence_at_the_very_end_of_the_file_counts() {
    for src in [
        "---\ntitle: T\n---",
        "---\ntitle: T\n---\n",
        "---\r\ntitle: T\r\n---",
    ] {
        let d = doc(src);
        assert!(d.frontmatter_range.is_some(), "{src:?}");
        assert_eq!(d.title, "T", "{src:?}");
    }
    assert!(doc("").frontmatter_range.is_none());
}

const SPECIAL: &str = "C++ & Rust: \"a/b\" (2024)!";

#[test]
fn a_heading_with_special_characters_keeps_its_text_and_gets_a_stable_slug() {
    let d = doc(&format!("# {SPECIAL}\n\ntext\n"));
    assert_eq!(d.headings[0].text, SPECIAL);
    assert_eq!(d.headings[0].slug, "c-rust-a-b-2024");
    assert_eq!(slugify(SPECIAL), "c-rust-a-b-2024");
    let h = &d.headings[0];
    for reference in [
        SPECIAL,
        "c-rust-a-b-2024",
        "c++ & rust: \"a/b\" (2024)!",
        "C-RUST-A-B-2024",
    ] {
        assert!(h.matches(reference), "{reference:?}");
    }
    for reference in ["C++ & Rust", "c-rust-a-b-2025", "", "   "] {
        assert!(!h.matches(reference), "{reference:?}");
    }
}

#[test]
fn a_link_to_a_special_heading_resolves_by_text_and_by_slug() {
    let target = format!("# Top\n\n## {SPECIAL}\n\ntext ^blk\n");
    for link in [
        format!("[[n#{SPECIAL}]]"),
        "[[n#c-rust-a-b-2024]]".to_string(),
        "[[n#C++ & RUST: \"A/B\" (2024)!]]".to_string(),
    ] {
        let a = parse_document(&format!("{link}\n"), Path::new("a.md"));
        let b = parse_document(&target, Path::new("n.md"));
        let index = Index::build(vec![a, b]);
        let a = index
            .documents()
            .find(|d| d.path == Path::new("a.md"))
            .unwrap();
        assert!(
            matches!(
                index.resolve_link_full(&a.links[0], Some(a)),
                LinkResolution::Resolved {
                    anchor: Some(_),
                    ..
                }
            ),
            "{link}"
        );
    }
}

#[test]
fn unusual_headings_never_panic_and_match_their_own_text() {
    for text in [
        "# 🦀 Rust 🦀",
        "# [[wiki]] link in heading",
        "# <b>bold</b> html",
        "# `code` in heading",
        "# ***",
        "# ---",
        "#",
        "# İşlem Ğ Ü",
        "Setext with ^blk\n===",
        "# ünïcödé\u{0301} combining",
        &format!("# {}", "long ".repeat(500)),
    ] {
        let d = doc(&format!("{text}\n\nbody\n"));
        assert_eq!(d.headings.len(), 1, "{text:?}");
        let h = &d.headings[0];
        if !h.text.trim().is_empty() {
            assert!(
                h.matches(&h.text),
                "{text:?} does not match its own text {:?}",
                h.text
            );
        }
        assert!(h.range.end <= text.len() + 2);
    }
}

#[test]
fn a_heading_without_text_and_a_document_of_only_frontmatter_are_fine() {
    assert_eq!(doc("#\n").headings.len(), 1);
    let only = doc("---\ntitle: T\n---\n");
    assert_eq!(only.title, "T");
    assert!(only.headings.is_empty() && only.links.is_empty());
    let empty = doc("");
    assert_eq!(empty.title, "n");
    assert!(empty.headings.is_empty());
}

#[test]
fn content_hash_is_the_hash_a_parsed_document_carries() {
    for text in [
        "",
        "# A\n",
        "---\ntitle: T\n---\nbody\n",
        "Ünal 🦀\r\nx\r\n",
        "\u{feff}# with bom\n",
    ] {
        assert_eq!(
            satz_core::content_hash(text),
            doc(text).content_hash,
            "{text:?}"
        );
    }
    assert_eq!(
        satz_core::content_hash("same"),
        satz_core::content_hash("same")
    );
    assert_ne!(satz_core::content_hash("a"), satz_core::content_hash("b"));
    // A byte order mark is not content.
    assert_eq!(
        satz_core::content_hash("\u{feff}# x\n"),
        satz_core::content_hash("# x\n")
    );
}
