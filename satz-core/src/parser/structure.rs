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
    pub ordered: bool,
    pub ordinal: u64,
}

/// A GFM task-list checkbox (`[ ]`/`[x]`), with `range` covering exactly the bracketed marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TaskMarkerSpan {
    pub range: ByteRange,
    pub checked: bool,
}

#[derive(Default)]
struct ListCtx {
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
    pub task_markers: Vec<TaskMarkerSpan>,
    /// Only outermost blockquotes — a nested `> >` blockquote is *not* also recorded separately,
    /// since the formatter re-scans the outer span's raw lines itself to normalize every nesting
    /// level's `>` marker in one pass (recording both would double-process the shared lines).
    pub blockquote_spans: Vec<ByteRange>,
    /// Only fenced code blocks (`CodeBlockKind::Fenced`) — indented code blocks have no fence
    /// delimiter to restyle.
    pub code_fence_spans: Vec<ByteRange>,
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
                list_stack.push(ListCtx {
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
                    let trimmed_text = heading_text.trim().to_string();
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
                    if code_block_is_fenced {
                        output.code_fence_spans.push(full_range);
                    }
                }
            }

            // --- Inline Code ---
            Event::Code(s) => {
                if in_heading {
                    heading_text.push_str(&s);
                } else if in_link {
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
                } else if in_heading {
                    heading_text.push_str(&s);
                } else if in_link {
                    link_text.push_str(&s);
                }
            }

            _ => {}
        }
    }

    output
}

fn parse_link_dest(dest: &str) -> (String, Option<String>) {
    if let Some((doc, heading)) = dest.split_once('#') {
        (doc.to_string(), Some(heading.to_string()))
    } else {
        (dest.to_string(), None)
    }
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
}
