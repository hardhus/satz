use std::path::Path;

use satz_core::model::LinkKind;
use satz_core::parser::parse_document;

#[test]
fn test_daily_note_parsing() {
    let source = include_str!("fixtures/daily_note.md");
    let doc = parse_document(source, Path::new("daily/2024-01-15.md"));

    assert_eq!(doc.title, "2024-01-15");
    assert_eq!(doc.id.as_str(), "daily/2024-01-15.md");
    assert_eq!(doc.frontmatter.tags, vec!["daily", "log"]);
    assert_eq!(doc.headings.len(), 2);
    assert_eq!(doc.headings[0].text, "Günlük");
    assert_eq!(doc.headings[1].text, "Notlar");

    // Wikilinks & Std links
    assert!(
        doc.links
            .iter()
            .any(|l| l.kind == LinkKind::WikiLink && l.target_doc == "proje-alfa")
    );
    assert!(doc.links.iter().any(|l| l.kind == LinkKind::WikiLink
        && l.target_doc == "proje-alfa"
        && l.target_heading.as_deref() == Some("mimari")));
    assert!(
        doc.links
            .iter()
            .any(|l| l.kind == LinkKind::Markdown && l.target_doc == "kaynaklar/makale.md")
    );
    assert!(doc.links.iter().any(|l| l.kind == LinkKind::Footnote));

    // Tags
    let tag_names: Vec<&str> = doc.tags.iter().map(|t| t.name.as_str()).collect();
    assert!(tag_names.contains(&"daily"));
    assert!(tag_names.contains(&"log"));
    assert!(tag_names.contains(&"fikir"));

    // Footnotes
    assert_eq!(doc.footnotes.definitions.len(), 1);
    assert_eq!(doc.footnotes.definitions[0].label, "1");

    // Snapshot
    insta::assert_yaml_snapshot!("daily_note", doc);
}

#[test]
fn test_book_note_parsing() {
    let source = include_str!("fixtures/book_note.md");
    let doc = parse_document(source, Path::new("books/tractatus.md"));

    assert_eq!(doc.title, "Tractatus Logico-Philosophicus");
    assert_eq!(doc.frontmatter.aliases, vec!["TLP", "Tractatus"]);
    assert_eq!(
        doc.frontmatter.extra.get("author").unwrap(),
        "Ludwig Wittgenstein"
    );
    assert_eq!(doc.frontmatter.extra.get("year").unwrap(), 1921);
    assert_eq!(doc.frontmatter.extra.get("status").unwrap(), "reading");

    assert_eq!(doc.headings.len(), 4);

    // Links & Embeds
    assert!(
        doc.links
            .iter()
            .any(|l| l.kind == LinkKind::Embed && l.target_doc == "wittgenstein-portre")
    );
    assert!(doc.links.iter().any(|l| l.kind == LinkKind::WikiLink
        && l.target_doc == "vienna-circle"
        && l.display.as_deref() == Some("Viyana Çevresi")));
    assert!(doc.links.iter().any(|l| l.kind == LinkKind::WikiLink
        && l.target_doc == "russell"
        && l.target_heading.as_deref() == Some("mantıkçı-atomculuk")));
    assert!(
        doc.links
            .iter()
            .any(|l| l.kind == LinkKind::Markdown && l.target_doc == "https://plato.stanford.edu")
    );

    // Tags
    let tag_names: Vec<&str> = doc.tags.iter().map(|t| t.name.as_str()).collect();
    assert!(tag_names.contains(&"felsefe"));
    assert!(tag_names.contains(&"wittgenstein"));
    assert!(tag_names.contains(&"mantık"));
    assert!(tag_names.contains(&"alıntı"));

    // Snapshot
    insta::assert_yaml_snapshot!("book_note", doc);
}

#[test]
fn test_tractatus_style_parsing() {
    let source = include_str!("fixtures/tractatus_style.md");
    let doc = parse_document(source, Path::new("tractatus/1.1.md"));

    assert_eq!(doc.title, "1.1 — Dünya olgular bütünüdür");
    assert_eq!(doc.frontmatter.aliases, vec!["1.1"]);

    assert_eq!(doc.headings.len(), 2);
    assert_eq!(doc.headings[0].text, "1.1 — Dünya olgular bütünüdür");
    assert_eq!(doc.headings[1].text, "1.11");

    assert!(doc.links.iter().any(|l| l.target_doc == "1"));
    assert!(doc.links.iter().any(|l| l.target_doc == "1.11"));

    let tag_names: Vec<&str> = doc.tags.iter().map(|t| t.name.as_str()).collect();
    assert!(tag_names.contains(&"tractatus"));
    assert!(tag_names.contains(&"ontoloji"));
    assert!(tag_names.contains(&"tractatus/temel"));

    // Snapshot
    insta::assert_yaml_snapshot!("tractatus_style", doc);
}

#[test]
fn test_edge_case_parsing() {
    let source = include_str!("fixtures/edge_case.md");
    let doc = parse_document(source, Path::new("tests/edge_case.md"));

    assert_eq!(doc.title, "Edge Case Testi ığüşçö");
    assert_eq!(doc.frontmatter.aliases, vec!["tekil-alias"]);
    assert_eq!(doc.frontmatter.extra.get("custom_field").unwrap(), 42);

    // Verify code block links are NOT present
    assert!(!doc.links.iter().any(|l| l.target_doc == "fake-link"));
    assert!(!doc.links.iter().any(|l| l.target_doc == "also-not-a-link"));
    assert!(!doc.links.iter().any(|l| l.target_doc == "inline-code-link"));

    // Verify code block tags are NOT present
    let tag_names: Vec<&str> = doc.tags.iter().map(|t| t.name.as_str()).collect();
    assert!(!tag_names.contains(&"not-a-tag"));
    assert!(!tag_names.contains(&"inline-tag"));

    // Real links and tags MUST be present
    assert!(doc.links.iter().any(|l| l.target_doc == "gerçek-link"));
    assert!(
        doc.links.iter().any(|l| l.target_doc == "türkçe-not"
            && l.target_heading.as_deref() == Some("bölüm-başlığı"))
    );
    assert!(
        doc.links
            .iter()
            .any(|l| l.kind == LinkKind::Embed && l.target_doc == "embed-dosya")
    );
    assert!(
        doc.links
            .iter()
            .any(|l| l.target_doc == "dosya" && l.target_block.as_deref() == Some("block-ref-123"))
    );

    assert!(tag_names.contains(&"gerçek-tag"));
    assert!(tag_names.contains(&"unicode-test"));

    // LineIndex verification on unicode characters
    let pos_turkce = doc
        .line_index
        .byte_to_position(source.find("İğneyle").unwrap());
    assert!(pos_turkce.line > 0);

    // Footnotes
    assert_eq!(doc.footnotes.definitions.len(), 2);
    assert_eq!(doc.footnotes.definitions[0].label, "dipnot1");
    assert_eq!(doc.footnotes.definitions[1].label, "dipnot2");

    // Snapshot
    insta::assert_yaml_snapshot!("edge_case", doc);
}
