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

fn locate_fm_tag(source: &str, fm: ByteRange, name: &str, from: usize) -> Option<ByteRange> {
    let hay = &source[fm.start..fm.end];
    let mut at = from;
    while let Some(idx) = hay[at..].find(name) {
        let s = at + idx;
        let e = s + name.len();
        let prev_ok = s == 0
            || !hay[..s]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_alphanumeric() || c == '-' || c == '/');
        let next_ok = hay[e..]
            .chars()
            .next()
            .is_none_or(|c| !c.is_alphanumeric() && c != '-' && c != '/');
        if prev_ok && next_ok {
            return Some(ByteRange::new(fm.start + s, fm.start + e));
        }
        at = e;
    }
    None
}

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

    let mut code_spans = structure.code_spans.clone();
    if let Some(fm_range) = structure.frontmatter_range {
        code_spans.push(fm_range);
    }
    code_spans.sort_unstable_by_key(|s| s.start);
    let inline = inline_scan::scan_inline(source, &code_spans);

    // Frontmatter tags + body tags
    let mut tags: Vec<Tag> = Vec::new();
    if let Some(fm_range) = structure.frontmatter_range {
        let mut fm_cursor = 0usize;
        for t in &frontmatter.tags {
            let clean_name = t.trim_start_matches('#');
            if let Some(range) = locate_fm_tag(source, fm_range, clean_name, fm_cursor) {
                fm_cursor = range.end.saturating_sub(fm_range.start);
                tags.push(Tag::new(t.clone(), range));
            } else {
                tags.push(Tag::new(
                    t.clone(),
                    ByteRange::new(fm_range.start, fm_range.start),
                ));
            }
        }
    }
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
        blocks: inline.blocks,
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

    #[test]
    fn test_frontmatter_tag_range() {
        let md = "---\ntitle: Foo\ntags: [rust, yazilim/araclar]\n---\n# Foo";
        let doc = parse_document(md, Path::new("foo.md"));
        let rust_tag = doc.tags.iter().find(|t| t.name == "rust").unwrap();
        assert_eq!(&md[rust_tag.range.start..rust_tag.range.end], "rust");
        let yazilim_tag = doc
            .tags
            .iter()
            .find(|t| t.name == "yazilim/araclar")
            .unwrap();
        assert_eq!(
            &md[yazilim_tag.range.start..yazilim_tag.range.end],
            "yazilim/araclar"
        );
    }
}
