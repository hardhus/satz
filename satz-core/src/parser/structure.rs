use pulldown_cmark::{Event, HeadingLevel, MetadataBlockKind, Options, Parser, Tag, TagEnd};

use crate::model::footnote::FootnoteDef;
use crate::model::heading::Heading;
use crate::model::link::{Link, LinkKind};
use crate::model::range::ByteRange;
use crate::slug::slugify;

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
}

/// Parses the structural markdown components using `pulldown-cmark`.
///
/// Collects headings, standard markdown links, footnote definitions/references,
/// YAML frontmatter block, code spans (which inline scan will avoid), and GFM table byte
/// ranges (the formatter re-parses the raw text of each range itself, to preserve inline
/// markdown/wikilinks inside cells verbatim rather than reconstructing it from the AST).
pub fn parse_structure(source: &str) -> StructureOutput {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_YAML_STYLE_METADATA_BLOCKS);
    options.insert(Options::ENABLE_HEADING_ATTRIBUTES);
    options.insert(Options::ENABLE_FOOTNOTES);
    options.insert(Options::ENABLE_TABLES);

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

    let mut in_link = false;
    let mut link_start = 0usize;
    let mut link_dest = String::new();
    let mut link_text = String::new();

    let mut in_footnote_def = false;
    let mut footnote_def_label = String::new();
    let mut footnote_def_start = 0usize;

    let mut in_table = false;
    let mut table_start = 0usize;

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
            Event::Start(Tag::CodeBlock(_)) => {
                in_code_block = true;
                code_block_start = range.start;
            }
            Event::End(TagEnd::CodeBlock) => {
                if in_code_block {
                    in_code_block = false;
                    output
                        .code_spans
                        .push(ByteRange::new(code_block_start, range.end));
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
