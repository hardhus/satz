use pulldown_cmark::{
    CodeBlockKind, Event, HeadingLevel, MetadataBlockKind, Options, Parser, Tag, TagEnd,
};

use crate::model::footnote::FootnoteDef;
use crate::model::heading::Heading;
use crate::model::link::{Link, LinkKind};
use crate::model::range::ByteRange;
use crate::slug::slugify;

/// Whether an inline emphasis span is single-delimiter (`*x*`/`_x_`) or double (`**x**`/`__x__`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmphasisKind {
    Italic,
    Bold,
}

/// An `Emphasis`/`Strong` span, including its opening/closing delimiter bytes in `range`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EmphasisSpan {
    pub kind: EmphasisKind,
    pub range: ByteRange,
}

/// A list item's own range (starting exactly at its marker's first byte — indentation before it
/// belongs to the parent item/list, never to this item) plus its resolved position within its
/// own sibling list (siblings only; a nested sub-list has its own independent numbering).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ListItemSpan {
    pub range: ByteRange,
    /// Index into `StructureOutput::list_spans` of the list this item belongs to.
    pub list_id: usize,
    pub ordered: bool,
    pub ordinal: u64,
}

/// A whole list: its byte range, whether it is ordered, and how deeply it is nested (0 = top level).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ListSpan {
    pub range: ByteRange,
    pub ordered: bool,
    pub depth: usize,
}

/// A GFM task-list checkbox (`[ ]`/`[x]`), with `range` covering exactly the bracketed marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TaskMarkerSpan {
    pub range: ByteRange,
    pub checked: bool,
}

#[derive(Default)]
struct ListCtx {
    id: usize,
    ordered: bool,
    next_ordinal: u64,
}

#[derive(Debug, Default)]
pub struct StructureOutput {
    pub frontmatter_yaml: Option<String>,
    pub frontmatter_range: Option<ByteRange>,
    pub headings: Vec<Heading>,
    pub std_links: Vec<Link>,
    pub footnote_defs: Vec<FootnoteDef>,
    pub footnote_refs: Vec<Link>,
    pub code_spans: Vec<ByteRange>,
    pub table_spans: Vec<ByteRange>,
    pub emphasis_spans: Vec<EmphasisSpan>,
    pub rule_spans: Vec<ByteRange>,
    pub list_items: Vec<ListItemSpan>,
    /// Every list (also nested ones), in source order; `ListItemSpan::list_id` indexes this.
    pub list_spans: Vec<ListSpan>,
    pub task_markers: Vec<TaskMarkerSpan>,
    /// Only outermost blockquotes — a nested `> >` blockquote is *not* also recorded separately,
    /// since the formatter re-scans the outer span's raw lines itself to normalize every nesting
    /// level's `>` marker in one pass (recording both would double-process the shared lines).
    pub blockquote_spans: Vec<ByteRange>,
    /// Only fenced code blocks (`CodeBlockKind::Fenced`) — indented code blocks have no fence
    /// delimiter to restyle.
    pub code_fence_spans: Vec<ByteRange>,
    /// Only top-level paragraphs — not inside a list item, blockquote, or table — since wrapping
    /// those would need indentation-aware continuation lines the wrap pass doesn't attempt yet.
    pub paragraph_spans: Vec<ByteRange>,
    /// Every code BLOCK, fenced or indented (never inline code spans). Whitespace and blank
    /// lines inside are content, so line-oriented passes must leave their lines alone.
    pub code_block_spans: Vec<ByteRange>,
    /// Every HTML block (`<div>`, `<pre>`, `<!-- -->`, ...), whose raw text is passed through to
    /// the output verbatim.
    pub html_block_spans: Vec<ByteRange>,
    /// Regions that look like text to a raw scan but are markup: an inline link's or image's
    /// `(destination)` and raw HTML. A `#anchor` in there is not a tag.
    pub non_text_spans: Vec<ByteRange>,
    /// Every hard line break: `text` + two or more spaces + newline, or `text` + newline. The
    /// trailing spaces of the first form are content, not whitespace to trim.
    pub hard_break_spans: Vec<ByteRange>,
}

/// The `(destination ...)` part of an inline link/image whose whole source range is `range`,
/// i.e. from the `(` after the last `](`. `None` for links without one (reference style,
/// autolinks), whose destination is not inside the range.
fn destination_span(source: &str, range: std::ops::Range<usize>) -> Option<ByteRange> {
    let text = source.get(range.clone())?;
    let idx = text.rfind("](")?;
    text.ends_with(')')
        .then(|| ByteRange::new(range.start + idx + 1, range.end))
}

/// Parses the structural markdown components using `pulldown-cmark`.
///
/// Collects headings, standard markdown links, footnote definitions/references, YAML
/// frontmatter block, code spans (which inline scan will avoid), GFM table byte ranges (the
/// formatter re-parses the raw text of each range itself, to preserve inline markdown/wikilinks
/// inside cells verbatim rather than reconstructing it from the AST), emphasis/strong spans,
/// thematic-break (`Rule`) spans, list item spans (with sibling-relative ordinal already
/// resolved), task-list checkbox spans, outermost blockquote spans, and fenced code block spans.
pub fn parse_structure(source: &str) -> StructureOutput {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_YAML_STYLE_METADATA_BLOCKS);
    options.insert(Options::ENABLE_HEADING_ATTRIBUTES);
    options.insert(Options::ENABLE_FOOTNOTES);
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_TASKLISTS);

    let parser = Parser::new_ext(source, options);
    let mut output = StructureOutput::default();

    // State trackers
    let mut in_metadata = false;
    let mut metadata_start = 0usize;
    let mut metadata_text = String::new();

    let mut in_heading = false;
    let mut heading_level = 1u8;
    let mut heading_start = 0usize;
    let mut heading_text = String::new();

    let mut in_code_block = false;
    let mut code_block_start = 0usize;
    let mut code_block_is_fenced = false;

    let mut in_link = false;
    let mut link_start = 0usize;
    let mut link_dest = String::new();
    let mut link_text = String::new();

    let mut in_footnote_def = false;
    let mut footnote_def_label = String::new();
    let mut footnote_def_start = 0usize;

    let mut in_table = false;
    let mut table_start = 0usize;

    let mut list_stack: Vec<ListCtx> = Vec::new();

    let mut blockquote_depth: usize = 0;
    let mut blockquote_start = 0usize;

    let mut paragraph_start = 0usize;
    let mut html_block_start = 0usize;

    for (event, range) in parser.into_offset_iter() {
        match event {
            // --- GFM Tables ---
            // Only the outer block range is captured; the formatter re-derives cell text and
            // alignment from the raw source itself rather than the table's inner cell events.
            Event::Start(Tag::Table(_)) => {
                in_table = true;
                table_start = range.start;
            }
            Event::End(TagEnd::Table) => {
                if in_table {
                    in_table = false;
                    output
                        .table_spans
                        .push(ByteRange::new(table_start, range.end));
                }
            }

            // --- Top-level paragraphs (for the wrap pass) ---
            // Only recorded when not nested inside a list item, blockquote, or table -- wrapping
            // those needs indentation-aware continuation lines this pass doesn't attempt yet.
            Event::Start(Tag::Paragraph) => {
                paragraph_start = range.start;
            }
            Event::End(TagEnd::Paragraph) => {
                if list_stack.is_empty() && blockquote_depth == 0 && !in_table && !in_footnote_def {
                    output
                        .paragraph_spans
                        .push(ByteRange::new(paragraph_start, range.end));
                }
            }

            // --- Emphasis / Strong ---
            // Start's own reported range already covers the whole span including both
            // delimiters (verified against pulldown-cmark's offset iterator), so no End-event
            // bookkeeping is needed here.
            Event::Start(Tag::Emphasis) => {
                output.emphasis_spans.push(EmphasisSpan {
                    kind: EmphasisKind::Italic,
                    range: ByteRange::new(range.start, range.end),
                });
            }
            Event::Start(Tag::Strong) => {
                output.emphasis_spans.push(EmphasisSpan {
                    kind: EmphasisKind::Bold,
                    range: ByteRange::new(range.start, range.end),
                });
            }

            // --- Thematic break ---
            Event::Rule => {
                output
                    .rule_spans
                    .push(ByteRange::new(range.start, range.end));
            }

            // --- Lists ---
            Event::Start(Tag::List(start_number)) => {
                output.list_spans.push(ListSpan {
                    range: ByteRange::new(range.start, range.end),
                    ordered: start_number.is_some(),
                    depth: list_stack.len(),
                });
                list_stack.push(ListCtx {
                    id: output.list_spans.len() - 1,
                    ordered: start_number.is_some(),
                    next_ordinal: start_number.unwrap_or(1),
                });
            }
            Event::End(TagEnd::List(_)) => {
                list_stack.pop();
            }
            Event::Start(Tag::Item) => {
                if let Some(ctx) = list_stack.last_mut() {
                    output.list_items.push(ListItemSpan {
                        range: ByteRange::new(range.start, range.end),
                        list_id: ctx.id,
                        ordered: ctx.ordered,
                        ordinal: ctx.next_ordinal,
                    });
                    ctx.next_ordinal += 1;
                }
            }
            Event::TaskListMarker(checked) => {
                output.task_markers.push(TaskMarkerSpan {
                    range: ByteRange::new(range.start, range.end),
                    checked,
                });
            }

            // --- Blockquotes (outermost only, see `blockquote_spans` doc comment) ---
            Event::Start(Tag::BlockQuote(_)) => {
                if blockquote_depth == 0 {
                    blockquote_start = range.start;
                }
                blockquote_depth += 1;
            }
            Event::End(TagEnd::BlockQuote(_)) => {
                blockquote_depth = blockquote_depth.saturating_sub(1);
                if blockquote_depth == 0 {
                    output
                        .blockquote_spans
                        .push(ByteRange::new(blockquote_start, range.end));
                }
            }

            // --- Frontmatter / MetadataBlock ---
            Event::Start(Tag::MetadataBlock(MetadataBlockKind::YamlStyle)) => {
                in_metadata = true;
                metadata_start = range.start;
                metadata_text.clear();
            }
            Event::End(TagEnd::MetadataBlock(MetadataBlockKind::YamlStyle)) => {
                in_metadata = false;
                output.frontmatter_yaml = Some(metadata_text.clone());
                output.frontmatter_range = Some(ByteRange::new(metadata_start, range.end));
            }

            // --- Headings ---
            Event::Start(Tag::Heading { level, .. }) => {
                in_heading = true;
                heading_level = match level {
                    HeadingLevel::H1 => 1,
                    HeadingLevel::H2 => 2,
                    HeadingLevel::H3 => 3,
                    HeadingLevel::H4 => 4,
                    HeadingLevel::H5 => 5,
                    HeadingLevel::H6 => 6,
                };
                heading_start = range.start;
                heading_text.clear();
            }
            Event::End(TagEnd::Heading(_)) => {
                if in_heading {
                    in_heading = false;
                    // A trailing ` ^block-id` labels the heading; it is not part of its name.
                    let trimmed_text = Heading::split_block_id(heading_text.trim()).0.to_string();
                    let slug = slugify(&trimmed_text);
                    output.headings.push(Heading::new(
                        heading_level,
                        trimmed_text,
                        slug,
                        ByteRange::new(heading_start, range.end),
                    ));
                }
            }

            // --- Code Blocks ---
            Event::Start(Tag::CodeBlock(ref kind)) => {
                in_code_block = true;
                code_block_start = range.start;
                code_block_is_fenced = matches!(kind, CodeBlockKind::Fenced(_));
            }
            Event::End(TagEnd::CodeBlock) => {
                if in_code_block {
                    in_code_block = false;
                    let full_range = ByteRange::new(code_block_start, range.end);
                    output.code_spans.push(full_range);
                    output.code_block_spans.push(full_range);
                    if code_block_is_fenced {
                        output.code_fence_spans.push(full_range);
                    }
                }
            }

            // --- HTML blocks (raw text passed through verbatim) ---
            Event::Start(Tag::HtmlBlock) => {
                html_block_start = range.start;
            }
            Event::End(TagEnd::HtmlBlock) => {
                output
                    .html_block_spans
                    .push(ByteRange::new(html_block_start, range.end));
            }

            // --- Inline Code ---
            Event::Code(s) => {
                if in_heading {
                    heading_text.push_str(&s);
                }
                if in_link {
                    link_text.push_str(&s);
                }
                output
                    .code_spans
                    .push(ByteRange::new(range.start, range.end));
            }

            // --- Standard Markdown Links ---
            Event::Start(Tag::Link { dest_url, .. }) => {
                in_link = true;
                link_start = range.start;
                link_dest = dest_url.to_string();
                link_text.clear();
            }
            Event::End(TagEnd::Link) => {
                if in_link {
                    in_link = false;
                    if let Some(span) = destination_span(source, range.clone()) {
                        output.non_text_spans.push(span);
                    }
                    let (target_doc, target_heading) = parse_link_dest(&link_dest);
                    let display = if link_text.is_empty() {
                        None
                    } else {
                        Some(link_text.clone())
                    };

                    output.std_links.push(Link::new(
                        LinkKind::Markdown,
                        target_doc,
                        target_heading,
                        None,
                        display,
                        ByteRange::new(link_start, range.end),
                    ));
                }
            }

            // --- Images: the `(dest)` part is not text either ---
            Event::End(TagEnd::Image) => {
                if let Some(span) = destination_span(source, range) {
                    output.non_text_spans.push(span);
                }
            }

            Event::HardBreak => {
                output
                    .hard_break_spans
                    .push(ByteRange::new(range.start, range.end));
                if in_heading {
                    heading_text.push(' ');
                }
                if in_link {
                    link_text.push(' ');
                }
            }

            // --- Raw HTML (attribute values are not text) ---
            Event::Html(_) | Event::InlineHtml(_) => {
                output
                    .non_text_spans
                    .push(ByteRange::new(range.start, range.end));
            }

            // --- Footnotes ---
            Event::FootnoteReference(label) => {
                output.footnote_refs.push(Link::new(
                    LinkKind::Footnote,
                    String::new(),
                    None,
                    None,
                    Some(label.to_string()),
                    ByteRange::new(range.start, range.end),
                ));
            }
            Event::Start(Tag::FootnoteDefinition(label)) => {
                in_footnote_def = true;
                footnote_def_label = label.to_string();
                footnote_def_start = range.start;
            }
            Event::End(TagEnd::FootnoteDefinition) => {
                if in_footnote_def {
                    in_footnote_def = false;
                    output.footnote_defs.push(FootnoteDef::new(
                        footnote_def_label.clone(),
                        ByteRange::new(footnote_def_start, range.end),
                    ));
                }
            }

            // --- Text accumulator ---
            Event::Text(s) => {
                if in_metadata {
                    metadata_text.push_str(&s);
                } else {
                    // A link inside a heading is part of both texts.
                    if in_heading {
                        heading_text.push_str(&s);
                    }
                    if in_link {
                        link_text.push_str(&s);
                    }
                }
            }

            // A line break inside a heading (setext) or a link reads as a space.
            Event::SoftBreak => {
                if in_heading {
                    heading_text.push(' ');
                }
                if in_link {
                    link_text.push(' ');
                }
            }

            _ => {}
        }
    }

    output
}

fn parse_link_dest(dest: &str) -> (String, Option<String>) {
    // An external URL is one opaque target: its `#fragment` is not a note heading.
    if crate::model::link::is_external_target(dest) {
        return (dest.to_string(), None);
    }
    // A note target is a path: `my%20note.md` names the file `my note.md`.
    if let Some((doc, heading)) = dest.split_once('#') {
        (percent_decode(doc), Some(percent_decode(heading)))
    } else {
        (percent_decode(dest), None)
    }
}

/// Decodes `%XX` escapes (UTF-8). Text with a malformed escape, or one that would not be valid
/// UTF-8 once decoded, is returned as written.
fn percent_decode(text: &str) -> String {
    if !text.contains('%') {
        return text.to_string();
    }
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(hi), Some(lo)) = (
                (bytes[i + 1] as char).to_digit(16),
                (bytes[i + 2] as char).to_digit(16),
            )
        {
            out.push((hi * 16 + lo) as u8);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap_or_else(|_| text.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_structure_headings() {
        let md = "# Title\n\nSome text.\n\n## Sub Title `code`\n";
        let structure = parse_structure(md);
        assert_eq!(structure.headings.len(), 2);
        assert_eq!(structure.headings[0].level, 1);
        assert_eq!(structure.headings[0].text, "Title");
        assert_eq!(structure.headings[0].slug, "title");

        assert_eq!(structure.headings[1].level, 2);
        assert_eq!(structure.headings[1].text, "Sub Title code");
        assert_eq!(structure.headings[1].slug, "sub-title-code");
    }

    #[test]
    fn test_structure_code_spans() {
        let md = "Here is `inline` and:\n```rust\nlet x = 1;\n```\n";
        let structure = parse_structure(md);
        assert_eq!(structure.code_spans.len(), 2);
    }

    #[test]
    fn test_structure_table_span() {
        let md = "Intro paragraph.\n\n| A | B |\n| --- | ---: |\n| 1 | 2 |\n\nAfter.\n";
        let structure = parse_structure(md);
        assert_eq!(structure.table_spans.len(), 1);
        let span = structure.table_spans[0];
        let table_text = &md[span.start..span.end];
        assert!(table_text.starts_with("| A | B |"));
        assert!(table_text.trim_end().ends_with("| 1 | 2 |"));
    }

    #[test]
    fn test_structure_emphasis_and_strong_spans() {
        let md = "half*emph* and half**strong** and _also_ and __also_too__\n";
        let structure = parse_structure(md);
        assert_eq!(structure.emphasis_spans.len(), 4);
        assert_eq!(structure.emphasis_spans[0].kind, EmphasisKind::Italic);
        assert_eq!(
            &md[structure.emphasis_spans[0].range.start..structure.emphasis_spans[0].range.end],
            "*emph*"
        );
        assert_eq!(structure.emphasis_spans[1].kind, EmphasisKind::Bold);
        assert_eq!(
            &md[structure.emphasis_spans[1].range.start..structure.emphasis_spans[1].range.end],
            "**strong**"
        );
        assert_eq!(structure.emphasis_spans[2].kind, EmphasisKind::Italic);
        assert_eq!(
            &md[structure.emphasis_spans[2].range.start..structure.emphasis_spans[2].range.end],
            "_also_"
        );
        assert_eq!(structure.emphasis_spans[3].kind, EmphasisKind::Bold);
        assert_eq!(
            &md[structure.emphasis_spans[3].range.start..structure.emphasis_spans[3].range.end],
            "__also_too__"
        );
    }

    #[test]
    fn test_structure_rule_span() {
        let md = "para\n\n---\n\npara2\n";
        let structure = parse_structure(md);
        assert_eq!(structure.rule_spans.len(), 1);
        let span = structure.rule_spans[0];
        assert_eq!(&md[span.start..span.end], "---\n");
    }

    #[test]
    fn test_structure_list_items_sibling_ordinals_and_nesting() {
        // Nested ordered-inside-unordered: nested list must get its own independent ordinal
        // sequence, and the parent's next sibling must resume from where the parent left off
        // (not be affected by however many items the nested list had).
        let md = "- a\n  1. n1\n  2. n2\n  3. n3\n- b\n- c\n";
        let structure = parse_structure(md);
        assert_eq!(structure.list_items.len(), 6);

        assert!(!structure.list_items[0].ordered); // "a"
        assert_eq!(structure.list_items[0].ordinal, 1);

        assert!(structure.list_items[1].ordered); // "n1"
        assert_eq!(structure.list_items[1].ordinal, 1);
        assert!(structure.list_items[2].ordered); // "n2"
        assert_eq!(structure.list_items[2].ordinal, 2);
        assert!(structure.list_items[3].ordered); // "n3"
        assert_eq!(structure.list_items[3].ordinal, 3);

        assert!(!structure.list_items[4].ordered); // "b" — resumes parent's own count
        assert_eq!(structure.list_items[4].ordinal, 2);
        assert!(!structure.list_items[5].ordered); // "c"
        assert_eq!(structure.list_items[5].ordinal, 3);
    }

    #[test]
    fn test_structure_ordered_list_custom_start_number() {
        let md = "5. five\n6. six\n";
        let structure = parse_structure(md);
        assert_eq!(structure.list_items[0].ordinal, 5);
        assert_eq!(structure.list_items[1].ordinal, 6);
    }

    #[test]
    fn test_structure_task_markers() {
        let md = "- [ ] todo\n- [x] done\n";
        let structure = parse_structure(md);
        assert_eq!(structure.task_markers.len(), 2);
        assert!(!structure.task_markers[0].checked);
        assert!(structure.task_markers[1].checked);
        assert_eq!(
            &md[structure.task_markers[0].range.start..structure.task_markers[0].range.end],
            "[ ]"
        );
    }

    #[test]
    fn test_structure_blockquote_span_outermost_only() {
        let md = "> outer\n> > inner\n";
        let structure = parse_structure(md);
        // Only ONE span recorded (the outermost), even though the quote is 2 levels deep.
        assert_eq!(structure.blockquote_spans.len(), 1);
        let span = structure.blockquote_spans[0];
        assert_eq!(&md[span.start..span.end], md);
    }

    #[test]
    fn test_structure_code_fence_span_excludes_indented() {
        let md = "```rust\nlet x = 1;\n```\n\n    indented code\n";
        let structure = parse_structure(md);
        assert_eq!(structure.code_fence_spans.len(), 1);
        // Indented code block is still tracked as a generic code_span (2 total: fenced + indented)
        // but must NOT appear in code_fence_spans (nothing to restyle there).
        assert_eq!(structure.code_spans.len(), 2);
    }

    #[test]
    fn test_structure_frontmatter_fence_is_not_a_rule() {
        // The YAML frontmatter's own "---" delimiters must never also surface as a thematic
        // break Rule — otherwise HR normalization would corrupt the frontmatter fence.
        let md = "---\ntitle: X\n---\n\n# H\n\nbody\n\n---\n\nafter\n";
        let structure = parse_structure(md);
        assert_eq!(structure.rule_spans.len(), 1);
        assert!(structure.frontmatter_range.is_some());
    }

    fn texts<'a>(md: &'a str, spans: &[ByteRange]) -> Vec<&'a str> {
        spans.iter().map(|r| md[r.start..r.end].trim()).collect()
    }

    #[test]
    fn code_block_spans_cover_fenced_and_indented_blocks_but_not_inline_code() {
        let md = "text `inline`\n\n```\nfenced\n```\n\n    indented one\n    indented two\n\nend\n";
        let out = parse_structure(md);
        assert_eq!(
            texts(md, &out.code_block_spans),
            vec!["```\nfenced\n```", "indented one\n    indented two"],
        );
        // Inline code stays out of the block list (but is still a code span).
        assert!(out.code_spans.len() >= 3);
    }

    #[test]
    fn code_block_spans_include_blocks_nested_in_quotes_and_lists() {
        let md = "> ```\n> in quote\n> ```\n\n- item\n\n  ```\n  in list\n  ```\n";
        let out = parse_structure(md);
        assert_eq!(
            out.code_block_spans.len(),
            2,
            "{:?}",
            texts(md, &out.code_block_spans)
        );
    }

    #[test]
    fn a_document_without_code_blocks_has_no_code_block_spans() {
        let out = parse_structure("just `inline` code\n");
        assert!(out.code_block_spans.is_empty());
    }

    #[test]
    fn html_block_spans_cover_block_html_only() {
        let md = "<div>\nx\n\ny\n</div>\n\npara <b>inline</b> html\n\n<!-- comment -->\n\n<pre>\na\n\n\nb\n</pre>\n";
        let out = parse_structure(md);
        let t = texts(md, &out.html_block_spans);
        assert!(t.iter().any(|s| s.starts_with("<div>")), "{t:?}");
        assert!(t.iter().any(|s| s.starts_with("<!-- comment -->")), "{t:?}");
        assert!(
            t.iter()
                .any(|s| s.starts_with("<pre>") && s.ends_with("</pre>")),
            "{t:?}"
        );
        assert!(
            t.iter().all(|s| !s.contains("para")),
            "inline html is not a block: {t:?}"
        );
    }

    #[test]
    fn test_structure_paragraph_spans_exclude_footnote_definitions() {
        let md = "Top paragraph.[^1]\n\n[^1]: Footnote body.\n\n    Second footnote paragraph.\n";
        let out = parse_structure(md);
        let spans: Vec<&str> = out
            .paragraph_spans
            .iter()
            .map(|r| md[r.start..r.end].trim())
            .collect();
        assert_eq!(spans, vec!["Top paragraph.[^1]"]);
    }

    #[test]
    fn test_structure_paragraph_spans_top_level_only() {
        // Two list items separated by a blank line force a "loose" list, so pulldown-cmark
        // wraps each item's own text in a Paragraph event too -- exactly the case the
        // list_stack/blockquote_depth guard needs to exclude.
        let md = "Top paragraph one.\n\nTop paragraph two.\n\n\
                  - loose item one\n\n- loose item two\n\n\
                  > quoted text\n\n\
                  | a | b |\n| - | - |\n| 1 | 2 |\n";
        let structure = parse_structure(md);
        assert_eq!(
            structure.paragraph_spans.len(),
            2,
            "list-item and blockquote paragraphs must be excluded, only top-level ones kept"
        );
        assert_eq!(
            md[structure.paragraph_spans[0].start..structure.paragraph_spans[0].end].trim(),
            "Top paragraph one."
        );
        assert_eq!(
            md[structure.paragraph_spans[1].start..structure.paragraph_spans[1].end].trim(),
            "Top paragraph two."
        );
    }

    #[test]
    fn test_structure_std_link() {
        let md = "See [My Note](notes/intro.md#overview) here.";
        let structure = parse_structure(md);
        assert_eq!(structure.std_links.len(), 1);
        let link = &structure.std_links[0];
        assert_eq!(link.kind, LinkKind::Markdown);
        assert_eq!(link.target_doc, "notes/intro.md");
        assert_eq!(link.target_heading.as_deref(), Some("overview"));
        assert_eq!(link.display.as_deref(), Some("My Note"));
    }

    #[test]
    fn test_structure_footnotes() {
        let md = "Reference[^1].\n\n[^1]: Note text.\n";
        let structure = parse_structure(md);
        assert_eq!(structure.footnote_refs.len(), 1);
        assert_eq!(structure.footnote_defs.len(), 1);
        assert_eq!(structure.footnote_defs[0].label, "1");
    }

    // ---- links inside headings and line breaks in headings/links ----

    fn heading_texts(md: &str) -> Vec<(String, String)> {
        parse_structure(md)
            .headings
            .into_iter()
            .map(|h| (h.text, h.slug))
            .collect()
    }

    fn link_displays(md: &str) -> Vec<Option<String>> {
        parse_structure(md)
            .std_links
            .into_iter()
            .map(|l| l.display)
            .collect()
    }

    #[test]
    fn a_link_inside_a_heading_keeps_its_display_text() {
        assert_eq!(link_displays("# [Foo](x.md)\n"), vec![Some("Foo".into())]);
        assert_eq!(
            link_displays("# See [Foo](x.md) now\n"),
            vec![Some("Foo".into())]
        );
        assert_eq!(
            link_displays("## [a](x.md) and [b](y.md)\n"),
            vec![Some("a".into()), Some("b".into())]
        );
        assert_eq!(
            link_displays("# [`code`](x.md)\n"),
            vec![Some("code".into())]
        );
        assert_eq!(
            link_displays("# [**bold** t](x.md)\n"),
            vec![Some("bold t".into())]
        );
        // Outside headings nothing changes.
        assert_eq!(
            link_displays("text [Foo](x.md)\n"),
            vec![Some("Foo".into())]
        );
        assert_eq!(link_displays("[](x.md)\n"), vec![None]);
    }

    #[test]
    fn the_heading_text_still_reads_through_its_links() {
        assert_eq!(
            heading_texts("# See [Foo](x.md) now\n"),
            vec![("See Foo now".to_string(), "see-foo-now".to_string())]
        );
        assert_eq!(
            heading_texts("# [Foo](x.md)\n"),
            vec![("Foo".to_string(), "foo".to_string())]
        );
    }

    fn link_targets(md: &str) -> Vec<(String, Option<String>)> {
        parse_structure(md)
            .std_links
            .into_iter()
            .map(|l| (l.target_doc, l.target_heading))
            .collect()
    }

    #[test]
    fn percent_encoded_note_targets_are_decoded() {
        assert_eq!(
            link_targets("[t](my%20note.md)\n"),
            vec![("my note.md".to_string(), None)]
        );
        assert_eq!(
            link_targets("[t](a%C3%BCn.md#Big%20Head)\n"),
            vec![("aün.md".to_string(), Some("Big Head".to_string()))]
        );
        assert_eq!(
            link_targets("[t](sub%20dir/n%C3%B6t.md)\n"),
            vec![("sub dir/nöt.md".to_string(), None)]
        );
    }

    #[test]
    fn malformed_escapes_and_plus_signs_are_left_alone() {
        for (md, target) in [
            ("[t](a%zz.md)\n", "a%zz.md"),
            ("[t](a%2)\n", "a%2"),
            ("[t](a%)\n", "a%"),
            ("[t](a+b.md)\n", "a+b.md"),
            // Not valid UTF-8 once decoded: keep the original text.
            ("[t](a%FF.md)\n", "a%FF.md"),
        ] {
            assert_eq!(link_targets(md), vec![(target.to_string(), None)], "{md:?}");
        }
    }

    #[test]
    fn external_urls_stay_exactly_as_written() {
        assert_eq!(
            link_targets("[t](https://a.b/c%20d#x%20y)\n"),
            vec![("https://a.b/c%20d#x%20y".to_string(), None)]
        );
        assert_eq!(
            link_targets("[t](mailto:a%40b.c)\n"),
            vec![("mailto:a%40b.c".to_string(), None)]
        );
    }

    #[test]
    fn a_line_break_inside_a_heading_or_link_is_a_space() {
        assert_eq!(
            heading_texts("Foo\nbar\n===\n"),
            vec![("Foo bar".to_string(), "foo-bar".to_string())]
        );
        assert_eq!(
            heading_texts("Foo\r\nbar\r\n---\r\n"),
            vec![("Foo bar".to_string(), "foo-bar".to_string())]
        );
        assert_eq!(
            heading_texts("one\ntwo\nthree\n===\n"),
            vec![("one two three".to_string(), "one-two-three".to_string())]
        );
        assert_eq!(link_displays("[a\nb](x.md)\n"), vec![Some("a b".into())]);
        assert_eq!(link_displays("[a\r\nb](x.md)\n"), vec![Some("a b".into())]);
        // A single-line ATX heading is unchanged.
        assert_eq!(
            heading_texts("# Plain title\n"),
            vec![("Plain title".to_string(), "plain-title".to_string())]
        );
    }
}
