pub mod frontmatter;
pub mod inline_scan;
pub mod structure;

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::Path;

use crate::model::document::{DocId, Document};
use crate::model::footnote::FootnoteTable;
use crate::model::range::ByteRange;
use crate::model::tag::Tag;
use crate::text::LineIndex;

/// Parses a single Markdown source text into a complete `Document`.
///
/// This is the primary single-file entry point in `satz-core`.
/// It extracts frontmatter, headings, links (standard and wikilinks),
/// tags (frontmatter and body), footnotes, and builds a UTF-16 safe `LineIndex`.
///
/// Never panics; if frontmatter has YAML syntax errors, it falls back to empty frontmatter.
pub fn parse_document(source: &str, path: &Path) -> Document {
    let line_index = LineIndex::new(source);

    let mut hasher = DefaultHasher::new();
    source.hash(&mut hasher);
    let content_hash = hasher.finish();

    let structure = structure::parse_structure(source);

    let frontmatter = structure
        .frontmatter_yaml
        .as_deref()
        .and_then(|y| frontmatter::parse_frontmatter(y).ok())
        .unwrap_or_default();

    let inline = inline_scan::scan_inline(source, &structure.code_spans);

    // Frontmatter tags + body tags
    let fm_range = structure.frontmatter_range.unwrap_or(ByteRange::new(0, 0));
    let mut tags: Vec<Tag> = frontmatter
        .tags
        .iter()
        .map(|t| Tag::new(t.clone(), fm_range))
        .collect();
    tags.extend(inline.tags);

    // Combine all links: markdown, wikilinks, and footnote references
    let mut links = structure.std_links;
    links.extend(inline.wiki_links);
    links.extend(structure.footnote_refs);
    links.sort_by_key(|link| link.range.start);

    let title = Document::resolve_title(&frontmatter, &structure.headings, path);
    let id = DocId(path.to_string_lossy().replace('\\', "/"));

    Document {
        id,
        path: path.to_path_buf(),
        title,
        frontmatter,
        headings: structure.headings,
        links,
        tags,
        footnotes: FootnoteTable {
            definitions: structure.footnote_defs,
        },
        line_index,
        content_hash,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::link::LinkKind;

    #[test]
    fn test_parse_document_integration() {
        let md = r#"---
title: "Test Note"
tags: [rust, lsp]
aliases: [tnote]
---

# Test Note

This is a note linking to [[other-note#section]] and [Website](https://example.com).

Here is a tag: #syntax and a footnote[^1].

```rust
// In code block: [[not-a-link]] and #not-a-tag
```

[^1]: Footnote content.
"#;
        let path = Path::new("notes/test.md");
        let doc = parse_document(md, path);

        assert_eq!(doc.title, "Test Note");
        assert_eq!(doc.id.as_str(), "notes/test.md");
        assert_eq!(doc.frontmatter.aliases, vec!["tnote"]);
        assert_eq!(doc.headings.len(), 1);
        assert_eq!(doc.headings[0].text, "Test Note");

        // Tags: rust, lsp from frontmatter, syntax from body
        let tag_names: Vec<&str> = doc.tags.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(tag_names, vec!["rust", "lsp", "syntax"]);

        // Links: [[other-note#section]], [Website](...), [^1]
        assert_eq!(doc.links.len(), 3);
        assert!(
            doc.links
                .iter()
                .any(|l| l.kind == LinkKind::WikiLink && l.target_doc == "other-note")
        );
        assert!(
            doc.links
                .iter()
                .any(|l| l.kind == LinkKind::Markdown && l.target_doc == "https://example.com")
        );
        assert!(doc.links.iter().any(|l| l.kind == LinkKind::Footnote));

        // Footnotes
        assert_eq!(doc.footnotes.definitions.len(), 1);
        assert_eq!(doc.footnotes.definitions[0].label, "1");
    }
}
