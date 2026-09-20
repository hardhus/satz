//! `parse_document_owned` is `parse_document` for text the caller already owns: the same document,
//! without a second copy of the text.

use satz_core::{parse_document, parse_document_owned};
use std::path::Path;

fn same_document(text: &str) {
    let borrowed = parse_document(text, Path::new("n.md"));
    let owned = parse_document_owned(text.to_string(), Path::new("n.md"));
    assert_eq!(owned.id, borrowed.id);
    assert_eq!(owned.title, borrowed.title);
    assert_eq!(owned.content_hash, borrowed.content_hash);
    assert_eq!(owned.line_index, borrowed.line_index);
    assert_eq!(owned.headings, borrowed.headings);
    assert_eq!(owned.links, borrowed.links);
    assert_eq!(owned.tags, borrowed.tags);
    assert_eq!(owned.blocks, borrowed.blocks);
    assert_eq!(owned.frontmatter_range, borrowed.frontmatter_range);
    assert_eq!(owned.frontmatter_error, borrowed.frontmatter_error);
    assert_eq!(owned.line_index.source(), borrowed.line_index.source());
}

#[test]
fn owned_and_borrowed_parsing_agree_on_every_kind_of_text() {
    for text in [
        "",
        "# Title\n\nbody [[link#h]] #tag ^blk\n",
        "---\ntitle: T\ntags: [a, b]\n---\n\n# H\n",
        "---\ntitle: never closed\n\ntext\n",
        "\u{feff}---\ntitle: bom\n---\n# H\n",
        "\u{feff}",
        "a\r\nb\r\n[[x]]\r\n",
        "İstanbul 🦀 ünlü\n\n```rust\n[[not a link]]\n```\n",
        "[^1]: note\n\ntext[^1] and [^2]\n",
        "| a | b |\n|---|---|\n| 1 | 2 |\n",
    ] {
        same_document(text);
    }
}

#[test]
fn a_leading_byte_order_mark_is_not_part_of_the_kept_text() {
    let owned = parse_document_owned("\u{feff}# H\n".to_string(), Path::new("n.md"));
    assert_eq!(owned.line_index.source(), "# H\n");
    assert_eq!(owned.headings.len(), 1);
}

#[test]
fn a_big_owned_text_is_kept_byte_for_byte() {
    let text = format!("# Big\n\n{}", "some words [[x]] and more\n".repeat(200_000));
    assert!(text.len() > 5_000_000);
    let owned = parse_document_owned(text.clone(), Path::new("big.md"));
    assert_eq!(owned.line_index.source().len(), text.len());
    assert_eq!(owned.line_index.source(), text);
    assert_eq!(owned.links.len(), 200_000);
}
