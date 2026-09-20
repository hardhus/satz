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

/// Where the values of the frontmatter's `tags:` / `tag:` keys are written, so each tag can be found
/// inside its own key's value and nowhere else.
struct TagKeyRegion {
    /// The whole key line (what a tag points at when its spelling cannot be found).
    key_line: ByteRange,
    /// End of the region: the next top-level key or the closing fence, absolute.
    end: usize,
    /// Where the next search in this region starts, absolute.
    cursor: usize,
}

/// A line that starts a top-level YAML key (`name:` at column 0), as opposed to an indented
/// continuation, a `- item`, a comment or a fence.
fn is_top_level_key(line: &str) -> bool {
    let Some(first) = line.chars().next() else {
        return false;
    };
    if first.is_whitespace()
        || first == '#'
        || first == '-' && (line.len() == 1 || line[1..].starts_with([' ', '\t']))
    {
        return false;
    }
    line.find(':').is_some_and(|i| {
        let after = &line[i + 1..];
        after.is_empty() || after.starts_with([' ', '\t'])
    })
}

fn tag_key_regions(source: &str, fm: ByteRange) -> Vec<TagKeyRegion> {
    let block = &source[fm.start..fm.end];
    // (absolute start, line without its line ending) for every line of the block.
    let mut lines: Vec<(usize, &str)> = Vec::new();
    let mut offset = 0;
    for raw in block.split_inclusive('\n') {
        lines.push((fm.start + offset, raw.trim_end_matches(['\n', '\r'])));
        offset += raw.len();
    }
    // The opening and closing `---` fences are not part of any value.
    let content_end = match lines.last() {
        Some((start, text)) if lines.len() > 1 && text.trim() == "---" => *start,
        _ => fm.end,
    };
    let mut regions = Vec::new();
    for (i, (start, text)) in lines.iter().enumerate() {
        if *start >= content_end || !is_top_level_key(text) {
            continue;
        }
        let key = text.split(':').next().unwrap_or("");
        if key != "tags" && key != "tag" {
            continue;
        }
        let end = lines[i + 1..]
            .iter()
            .find(|(s, t)| *s >= content_end || is_top_level_key(t))
            .map_or(content_end, |(s, _)| *s)
            .min(content_end);
        let value_start = start + key.len() + 1;
        regions.push(TagKeyRegion {
            key_line: ByteRange::new(*start, start + text.len()),
            end,
            cursor: value_start,
        });
    }
    regions
}

/// The byte range of `name` written in one of the tag keys' values: never in the key itself, in a
/// comment or in another key, and in order (a repeated tag gets its next occurrence).
fn locate_fm_tag(source: &str, regions: &mut [TagKeyRegion], name: &str) -> Option<ByteRange> {
    if name.is_empty() {
        return None;
    }
    for region in regions.iter_mut() {
        let mut at = region.cursor;
        while at < region.end {
            let Some(idx) = source[at..region.end].find(name) else {
                break;
            };
            let s = at + idx;
            let e = s + name.len();
            // A `# comment` line inside the value is not a tag list.
            let line_start = source[..s].rfind('\n').map_or(0, |i| i + 1);
            let in_comment = source[line_start..s].trim_start().starts_with('#')
                || source[line_start..s].contains(" #");
            let prev_ok = !source[..s]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_alphanumeric() || c == '-' || c == '/');
            let next_ok = source[e..]
                .chars()
                .next()
                .is_none_or(|c| !c.is_alphanumeric() && c != '-' && c != '/');
            if prev_ok && next_ok && !in_comment {
                region.cursor = e;
                return Some(ByteRange::new(s, e));
            }
            at = e;
        }
    }
    None
}

/// The hash of a document's text that `Document::content_hash` holds (the text without a leading
/// byte order mark, as `parse_document` sees it). Equal for equal text, so it says whether a
/// buffer is still the text something was computed from.
pub fn content_hash(source: &str) -> u64 {
    let source = source.strip_prefix('\u{feff}').unwrap_or(source);
    let mut hasher = DefaultHasher::new();
    source.hash(&mut hasher);
    hasher.finish()
}

/// `parse_document` for text the caller owns: the text moves into the document instead of being copied.
pub fn parse_document_owned(mut source: String, path: &Path) -> Document {
    // A leading UTF-8 byte order mark is a property of the file, not content: it must not stop the
    // frontmatter fence from being recognised or shift the first line.
    if source.starts_with('\u{feff}') {
        source.drain(..'\u{feff}'.len_utf8());
    }
    parse_prepared(source, path)
}

/// Parses a single Markdown source text into a complete `Document`.
///
/// This is the primary single-file entry point in `satz-core`.
/// It extracts frontmatter, headings, links (standard and wikilinks),
/// tags (frontmatter and body), footnotes, and builds a UTF-16 safe `LineIndex`.
///
/// Never panics; if frontmatter has YAML syntax errors, it falls back to empty frontmatter.
pub fn parse_document(source: &str, path: &Path) -> Document {
    parse_document_owned(source.to_string(), path)
}

/// The parse itself, of text that has no byte order mark any more. The text ends up in the
/// document's `LineIndex`, moved there once everything has been read from it.
fn parse_prepared(text: String, path: &Path) -> Document {
    let source = text.as_str();
    let content_hash = content_hash(source);

    let mut structure = structure::parse_structure(source);

    // A block that cannot be read is treated as empty (nothing of it is used) but the reason is kept,
    // so it can be shown to the user instead of silently dropping their title, aliases and tags.
    let (frontmatter, frontmatter_error) = match structure.frontmatter_yaml.as_deref() {
        Some(yaml) => match frontmatter::parse_frontmatter(yaml) {
            Ok(fm) => (fm, None),
            Err(e) => (Default::default(), Some(e.to_string())),
        },
        None => (Default::default(), None),
    };

    let mut code_spans = std::mem::take(&mut structure.code_spans);
    if let Some(fm_range) = structure.frontmatter_range {
        code_spans.push(fm_range);
    }
    code_spans.sort_unstable_by_key(|s| s.start);
    let inline = inline_scan::scan_inline(source, &code_spans);

    // Frontmatter tags + body tags
    let mut tags: Vec<Tag> = Vec::new();
    if let Some(fm_range) = structure.frontmatter_range {
        let mut regions = tag_key_regions(source, fm_range);
        // A tag whose spelling cannot be found (an escaped or otherwise normalised value) points at
        // its key's line, never at an empty range.
        let fallback = regions.first().map_or_else(
            || {
                let end = source[fm_range.start..fm_range.end]
                    .find('\n')
                    .map_or(fm_range.end, |n| fm_range.start + n);
                ByteRange::new(
                    fm_range.start,
                    end.max(fm_range.start + 1).min(fm_range.end),
                )
            },
            |r| r.key_line,
        );
        for t in &frontmatter.tags {
            let clean_name = t.trim_start_matches('#');
            let range = locate_fm_tag(source, &mut regions, clean_name).unwrap_or(fallback);
            tags.push(Tag::new(t.clone(), range));
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
        frontmatter_error,
        headings: structure.headings,
        links,
        tags,
        footnotes,
        broken_footnote_refs,
        blocks: inline.blocks,
        line_index: LineIndex::from_string(text),
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

    // ---- a broken frontmatter block is reported, not silently dropped ----

    fn frontmatter_error(md: &str) -> Option<String> {
        parse_document(md, Path::new("a.md")).frontmatter_error
    }

    #[test]
    fn valid_absent_or_empty_frontmatter_has_no_error() {
        for md in [
            "---\ntitle: T\ntags: [a]\n---\n# H\n",
            "# No frontmatter\n",
            "---\n---\n# Empty\n",
            "---\n\n---\n# Blank\n",
            "---\ntitle: T\n# never closed\n",
            "text\n\n---\nnot: frontmatter\n---\n",
            "---\r\ntitle: T\r\n---\r\n# H\r\n",
            "",
        ] {
            assert_eq!(frontmatter_error(md), None, "{md:?}");
        }
    }

    #[test]
    fn invalid_yaml_is_recorded_and_its_fields_are_ignored() {
        let md = "---\ntitle: Foo: bar\ntags: [a]\n---\n# Real title\n";
        let doc = parse_document(md, Path::new("a.md"));
        let error = doc.frontmatter_error.expect("an error message");
        assert!(error.to_lowercase().contains("yaml"), "{error}");
        // Behaviour is unchanged: nothing of the broken block is used.
        assert_eq!(doc.title, "Real title");
        assert!(doc.tags.is_empty());
        assert!(doc.frontmatter.aliases.is_empty());
        assert!(doc.frontmatter_range.is_some());
    }

    #[test]
    fn a_frontmatter_that_is_not_a_mapping_is_an_error_too() {
        let error = frontmatter_error("---\n- a\n- b\n---\n# H\n").expect("an error message");
        assert!(error.to_lowercase().contains("mapping"), "{error}");
        assert!(frontmatter_error("---\njust text\n---\n# H\n").is_some());
    }

    #[test]
    fn a_broken_block_with_crlf_is_reported_as_well() {
        assert!(frontmatter_error("---\r\ntitle: Foo: bar\r\n---\r\n# H\r\n").is_some());
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
