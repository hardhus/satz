pub mod frontmatter;
pub mod inline_scan;
pub mod structure;

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::Path;

use crate::model::document::{DocId, Document};
use crate::model::footnote::FootnoteTable;
use crate::model::link::Link;
use crate::model::range::ByteRange;
use crate::model::tag::Tag;
use crate::text::LineIndex;

fn locate_fm_tag(source: &str, fm: ByteRange, name: &str, from: usize) -> Option<ByteRange> {
    let hay = &source[fm.start..fm.end];
    let key_at = hay
        .find("\ntags:")
        .or_else(|| hay.find("\ntag:"))
        .map(|i| i + 1)
        .or_else(|| {
            if hay.starts_with("tags:") || hay.starts_with("tag:") {
                Some(0)
            } else {
                None
            }
        })
        .unwrap_or(0);
    let mut at = from.max(key_at);
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
    // `[x](#anchor)` and `<a href="#x">` contain anchors, not tags.
    tags.extend(inline.tags.into_iter().filter(|t| {
        !structure
            .non_text_spans
            .iter()
            .any(|s| s.overlaps(&t.range))
    }));

    // Combine all links: markdown, wikilinks, and footnote references
    let mut links = structure.std_links;
    links.extend(inline.wiki_links);
    links.extend(structure.footnote_refs);
    links.sort_by_key(|link| link.range.start);

    let footnotes = FootnoteTable {
        definitions: structure.footnote_defs,
    };

    // A footnote candidate is "broken" iff its label has no matching definition -- this also
    // correctly excludes every already-resolved reference (already counted above via
    // `structure.footnote_refs`) and each definition's own `[^label]:` marker occurrence, since
    // both trivially have a matching definition by construction. Labels match
    // case-insensitively, exactly as pulldown-cmark resolves them (`FootnoteTable::find_def`).
    let broken_footnote_refs: Vec<Link> = inline
        .footnote_candidates
        .into_iter()
        .filter(|candidate| {
            let label = candidate.display.as_deref().unwrap_or("");
            footnotes.find_def(label).is_none()
        })
        .collect();

    let title = Document::resolve_title(&frontmatter, &structure.headings, path);
    let id = DocId(path.to_string_lossy().replace('\\', "/"));

    Document {
        id,
        path: path.to_path_buf(),
        title,
        frontmatter,
        frontmatter_range: structure.frontmatter_range,
        headings: structure.headings,
        links,
        tags,
        footnotes,
        broken_footnote_refs,
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
        assert!(doc.broken_footnote_refs.is_empty());
    }

    #[test]
    fn test_broken_footnote_ref_detected() {
        let md = "Ref one [^ok] and ref two [^missing].\n\n[^ok]: Defined.\n";
        let doc = parse_document(md, Path::new("notes/test.md"));

        assert_eq!(doc.broken_footnote_refs.len(), 1);
        assert_eq!(
            doc.broken_footnote_refs[0].display.as_deref(),
            Some("missing")
        );
        // The resolved one stays in `links` as usual, not duplicated into `broken_footnote_refs`.
        assert!(doc.links.iter().any(|l| l.kind == LinkKind::Footnote));
    }

    #[test]
    fn footnote_label_matching_is_case_insensitive() {
        // pulldown-cmark resolves footnote labels case-insensitively (including non-ASCII), so a
        // reference `[^A]` DOES have its `[^a]:` definition -- it must not also be "broken".
        for md in ["Ref[^A]\n\n[^a]: x\n", "Ref[^Ü]\n\n[^ü]: x\n"] {
            let doc = parse_document(md, Path::new("notes/test.md"));
            assert!(
                doc.links.iter().any(|l| l.kind == LinkKind::Footnote),
                "pulldown should resolve the reference in {md:?}"
            );
            assert!(
                doc.broken_footnote_refs.is_empty(),
                "case-differing label wrongly reported broken in {md:?}: {:?}",
                doc.broken_footnote_refs
            );
        }
    }

    #[test]
    fn footnote_label_with_space_is_valid_when_defined() {
        // pulldown-cmark accepts spaces inside a footnote label, so `[^a b]` must not be
        // reported broken when `[^a b]: ...` exists; an undefined one still is.
        let doc = parse_document("Ref[^a b]\n\n[^a b]: x\n", Path::new("notes/test.md"));
        assert!(doc.broken_footnote_refs.is_empty());

        let doc = parse_document("Ref[^no def]\n", Path::new("notes/test.md"));
        assert_eq!(doc.broken_footnote_refs.len(), 1);
    }

    #[test]
    fn bracket_in_footnote_label_is_never_a_footnote() {
        // pulldown-cmark does not treat `[^a[b]` as a footnote even when "defined", so it must
        // not surface as a broken reference (and the definition marker must not either).
        let doc = parse_document("Ref[^a[b]\n\n[^a[b]: x\n", Path::new("notes/test.md"));
        assert!(
            doc.broken_footnote_refs.is_empty(),
            "{:?}",
            doc.broken_footnote_refs
        );
    }

    #[test]
    fn test_broken_footnote_ref_ignored_inside_code_span() {
        let md = "See `[^fake]` for syntax, but [^real] is undefined too.";
        let doc = parse_document(md, Path::new("notes/test.md"));

        assert_eq!(doc.broken_footnote_refs.len(), 1);
        assert_eq!(doc.broken_footnote_refs[0].display.as_deref(), Some("real"));
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

    #[test]
    fn test_fm_tag_range_not_confused_by_title() {
        let md = "---\ntitle: rust rehberi\ntags: [rust]\n---\n# Content";
        let doc = parse_document(md, Path::new("doc.md"));
        assert_eq!(doc.tags.len(), 1);
        let tag = &doc.tags[0];
        assert_eq!(&md[tag.range.start..tag.range.end], "rust");
        let tags_key_pos = md.find("tags:").unwrap();
        assert!(tag.range.start > tags_key_pos);
    }

    fn body_tags(md: &str) -> Vec<String> {
        parse_document(md, Path::new("doc.md"))
            .tags
            .into_iter()
            .map(|t| t.name)
            .collect()
    }

    #[test]
    fn a_hash_inside_a_link_destination_is_an_anchor_not_a_tag() {
        for md in [
            "See [jump](#heading) here",
            "See [jump](doc.md#heading) here",
            "See [jump]( #heading ) here",
            "See [a](x_(b)#c) here",
            "Türkçe ünlü [git](#başlık) şimdi",
            "line\r\nSee [jump](#heading)\r\n",
            "![img](pic.png#frag)",
        ] {
            assert!(body_tags(md).is_empty(), "{md:?} -> {:?}", body_tags(md));
        }
    }

    #[test]
    fn a_hash_inside_html_is_not_a_tag() {
        for md in [
            "<a href=\"#x\">go</a>",
            "text <span id=\"#y\">z</span> text",
            "<div id=\"#y\">\nbody\n</div>\n",
            "<!-- #hidden -->",
        ] {
            assert!(body_tags(md).is_empty(), "{md:?} -> {:?}", body_tags(md));
        }
    }

    #[test]
    fn real_tags_next_to_links_and_html_are_kept() {
        // Link TEXT is ordinary text.
        assert_eq!(body_tags("[#real](x.md)"), vec!["real"]);
        assert_eq!(body_tags("[a #real b](x.md)"), vec!["real"]);
        // Text around a link, and the plain-text `(#tag)` form, stay tags (as in Obsidian).
        assert_eq!(body_tags("[a](#anchor) and #after"), vec!["after"]);
        assert_eq!(body_tags("#before and [a](#anchor)"), vec!["before"]);
        assert_eq!(body_tags("colour (#fff) here"), vec!["fff"]);
        assert_eq!(body_tags("<b>bold</b> #kept"), vec!["kept"]);
        assert_eq!(body_tags("<!-- c -->\n\n#kept"), vec!["kept"]);
    }
}
