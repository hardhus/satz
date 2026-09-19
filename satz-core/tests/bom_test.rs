//! Files saved with a UTF-8 byte order mark (common with Windows editors) must behave exactly like
//! the same text without one: the mark is not content.

use satz_core::config::FormatterConfig;
use satz_core::formatter::format_document;
use satz_core::{parse_document, walk_vault};
use std::path::Path;

const BOM: &str = "\u{feff}";

const NOTE: &str = "---\ntitle: Bom Note\naliases: [bn]\ntags: [alpha]\n---\n\n# Heading\n\nText with [[other]] and #beta.\n";

#[test]
fn a_bom_is_not_part_of_the_parsed_document() {
    let plain = parse_document(NOTE, Path::new("n.md"));
    let with_bom = parse_document(&format!("{BOM}{NOTE}"), Path::new("n.md"));
    assert_eq!(with_bom, plain);
    assert_eq!(with_bom.title, "Bom Note");
    assert_eq!(with_bom.frontmatter.aliases, vec!["bn"]);
    assert!(with_bom.tags.iter().any(|t| t.name == "alpha"));
    assert!(with_bom.frontmatter_range.is_some());
    assert!(!with_bom.line_index.source().starts_with(BOM));
}

#[test]
fn only_a_leading_bom_is_stripped() {
    let doc = parse_document(&format!("text {BOM} inside\n"), Path::new("n.md"));
    assert!(doc.line_index.source().contains(BOM));
    let doubled = parse_document(&format!("{BOM}{BOM}# T\n"), Path::new("n.md"));
    assert!(
        doubled.line_index.source().starts_with(BOM),
        "one mark only"
    );
    let empty = parse_document(BOM, Path::new("n.md"));
    assert_eq!(empty.line_index.source(), "");
}

#[test]
fn a_bom_file_on_disk_is_indexed_with_its_frontmatter() {
    let dir = std::env::temp_dir().join(format!("satz-bom-walk-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("n.md"), format!("{BOM}{NOTE}")).unwrap();
    std::fs::write(
        dir.join("crlf.md"),
        format!("{BOM}---\r\ntags: [zed]\r\n---\r\n# C\r\n"),
    )
    .unwrap();

    let docs = walk_vault(&dir).unwrap();
    let by_name = |name: &str| docs.iter().find(|d| d.path.ends_with(name)).unwrap();
    let n = by_name("n.md");
    assert_eq!(n.title, "Bom Note");
    assert!(n.tags.iter().any(|t| t.name == "alpha"));
    assert!(by_name("crlf.md").tags.iter().any(|t| t.name == "zed"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn formatting_keeps_the_bom_and_formats_like_the_bom_free_text() {
    let cfg = FormatterConfig::default();
    let dirty = "---\ntitle: T   \n---\n\n\n\n# H  \n\ntext  \n";
    let plain = format_document(dirty, &cfg);
    let with_bom = format_document(&format!("{BOM}{dirty}"), &cfg);
    assert_eq!(with_bom, format!("{BOM}{plain}"));
    // Idempotent, and a clean BOM file is left as it is.
    assert_eq!(format_document(&with_bom, &cfg), with_bom);
    let clean = format!("{BOM}# T\n");
    assert_eq!(format_document(&clean, &cfg), clean);
}

#[test]
fn formatting_a_bom_file_with_crlf_keeps_both() {
    let cfg = FormatterConfig::default();
    let out = format_document(&format!("{BOM}# T  \r\n\r\ntext\r\n"), &cfg);
    assert_eq!(out, format!("{BOM}# T\r\n\r\ntext\r\n"));
}

#[test]
fn a_bom_only_file_stays_a_bom_file() {
    let cfg = FormatterConfig::default();
    assert!(format_document(BOM, &cfg).starts_with(BOM));
}

#[test]
fn a_bom_does_not_stop_the_frontmatter_from_being_protected() {
    let cfg = FormatterConfig::default();
    let src = format!("{BOM}---\nrelated: \"[[  a  ]]\"\n---\n\n# T\n\n[[  b  ]]\n");
    let out = format_document(&src, &cfg);
    // Links in the frontmatter are left alone, links in the body are normalized.
    assert_eq!(
        out,
        format!("{BOM}---\nrelated: \"[[  a  ]]\"\n---\n\n# T\n\n[[b]]\n")
    );
}
