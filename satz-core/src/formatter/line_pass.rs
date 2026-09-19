use crate::config::FormatterConfig;
use crate::model::ByteRange;
use crate::parser::structure::parse_structure;

/// Line-based formatting pass: trailing whitespace, blank-line and heading spacing normalization,
/// wikilink whitespace trimming, final newline. Frontmatter, code blocks (fenced or indented) and
/// HTML blocks are copied through untouched.
///
/// Which lines are "code" or "HTML" comes from the real Markdown parser (`parse_structure`), not
/// from scanning lines for fence characters: fence length, nesting, indentation, blockquote/list
/// containers and unterminated blocks are all decided by the parser, so the two passes below can
/// never disagree with each other or with how the document renders.
pub fn run(source: &str, config: &FormatterConfig) -> String {
    layout(&super::links::normalize(source, config), config)
}

/// `run` without the wikilink step (the formatter pipeline has already done it, before tables and
/// wrapping).
pub(crate) fn layout(source: &str, config: &FormatterConfig) -> String {
    if source.is_empty() {
        return if config.final_newline {
            "\n".to_string()
        } else {
            String::new()
        };
    }

    let structure = parse_structure(source);
    let raw_lines: Vec<&str> = source.lines().collect();
    let protected = protected_lines(source, &raw_lines, &structure);

    // 1. First pass: frontmatter, protected regions verbatim, trailing whitespace elsewhere.
    // Each entry is (text, is_protected).
    let mut lines: Vec<(String, bool)> = Vec::new();
    let mut in_frontmatter = false;

    for (i, raw_line) in raw_lines.iter().enumerate() {
        let trimmed_end = raw_line.trim_end();

        // Check frontmatter boundary
        if i == 0 && (trimmed_end == "---" || trimmed_end == "+++") {
            in_frontmatter = true;
            lines.push((trimmed_end.to_string(), false));
            continue;
        }

        if in_frontmatter {
            if trimmed_end == "---" || trimmed_end == "+++" {
                in_frontmatter = false;
            }
            lines.push((trimmed_end.to_string(), false));
            continue;
        }

        if protected[i] {
            lines.push((raw_line.to_string(), true));
        } else {
            lines.push((trimmed_end.to_string(), false));
        }
    }

    // 2. Second pass: blank lines and heading spacing, outside frontmatter and protected regions.
    let mut result_lines: Vec<(String, bool)> = Vec::new();
    let mut consecutive_blanks = 0;
    let mut in_fm = false;

    for (i, (line, is_protected)) in lines.iter().enumerate() {
        if i == 0 && (line == "---" || line == "+++") {
            in_fm = true;
            result_lines.push((line.clone(), false));
            continue;
        }
        if in_fm {
            if line == "---" || line == "+++" {
                in_fm = false;
            }
            result_lines.push((line.clone(), false));
            continue;
        }

        if *is_protected {
            result_lines.push((line.clone(), true));
            consecutive_blanks = 0;
            continue;
        }

        let is_heading = is_atx_heading(line);
        let is_blank = line.trim().is_empty();

        if is_heading {
            // Ensure configured blank lines before heading (if not at very start of content)
            let needed_before = config.blank_lines_around_headings as usize;
            if !result_lines.is_empty() {
                // Remove existing trailing blanks (never a protected line: those are content)
                while let Some((last, last_protected)) = result_lines.last() {
                    if !*last_protected && last.trim().is_empty() {
                        result_lines.pop();
                    } else {
                        break;
                    }
                }
                // Don't add blank lines if previous was frontmatter closing or empty doc
                let after_fm = result_lines.last().map(|(s, _)| s.as_str()) == Some("---");
                let target_blanks = if after_fm { 1 } else { needed_before.max(1) };
                for _ in 0..target_blanks {
                    result_lines.push((String::new(), false));
                }
            }

            result_lines.push((line.clone(), false));
            consecutive_blanks = 0;
            continue;
        }

        if is_blank {
            consecutive_blanks += 1;
            if consecutive_blanks <= 1 {
                result_lines.push((String::new(), false));
            }
        } else {
            consecutive_blanks = 0;
            result_lines.push((line.clone(), false));
        }
    }

    // 3. Join with newlines and apply final newline rule
    let last_is_protected = result_lines.last().is_some_and(|(_, p)| *p);
    let mut formatted = result_lines
        .into_iter()
        .map(|(line, _)| line)
        .collect::<Vec<_>>()
        .join("\n");

    // Clean any trailing whitespace / extra newlines at EOF. A last line that is code or HTML
    // keeps its own trailing spaces (they are content); only the newlines after it go.
    if last_is_protected {
        formatted.truncate(formatted.trim_end_matches('\n').len());
    } else {
        formatted = formatted.trim_end().to_string();
    }

    if config.final_newline && !formatted.is_empty() {
        formatted.push('\n');
    }

    formatted
}

/// For each line, whether it lies inside a code block or an HTML block and so must be preserved
/// exactly (trailing whitespace, blank lines and all).
fn protected_lines(
    source: &str,
    lines: &[&str],
    structure: &crate::parser::structure::StructureOutput,
) -> Vec<bool> {
    let mut spans: Vec<ByteRange> = structure
        .code_block_spans
        .iter()
        .chain(structure.html_block_spans.iter())
        .copied()
        .collect();
    spans.sort_unstable_by_key(|s| s.start);

    // Byte offset at which each line starts.
    let mut line_start = 0usize;
    let mut starts = Vec::with_capacity(lines.len());
    for line in lines {
        starts.push(line_start);
        line_start += line.len();
        // Skip this line's terminator ("\n" or "\r\n"); the last line may have none.
        let rest = &source[line_start.min(source.len())..];
        if rest.starts_with("\r\n") {
            line_start += 2;
        } else if rest.starts_with('\n') {
            line_start += 1;
        }
    }

    let mut result = vec![false; lines.len()];
    let mut si = 0usize;
    for (i, line) in lines.iter().enumerate() {
        let start = starts[i];
        let end = start + line.len();
        // Drop spans that ended before this line starts (lines only move forward).
        while si < spans.len() && spans[si].end <= start {
            si += 1;
        }
        // A span may begin mid-line (a fence after indentation or a `>` marker), so a line is
        // inside the block when the block starts at or before the line's end and hasn't ended.
        let mut sj = si;
        while sj < spans.len() && spans[sj].start <= end {
            if spans[sj].end > start {
                result[i] = true;
                break;
            }
            sj += 1;
        }
    }
    result
}

/// An ATX heading: 1-6 `#` followed by a space, indented by at most 3 spaces (4+ is code, or a
/// continuation line of the paragraph above).
fn is_atx_heading(line: &str) -> bool {
    let indent = line.len() - line.trim_start_matches(' ').len();
    if indent > 3 {
        return false;
    }
    let bytes = &line.as_bytes()[indent..];
    let mut level = 0;
    while level < bytes.len() && bytes[level] == b'#' {
        level += 1;
    }
    (1..=6).contains(&level) && bytes.get(level) == Some(&b' ')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_trailing_whitespace_removal() {
        let input = "Line 1   \nLine 2\t\t\nLine 3";
        let config = FormatterConfig::default();
        let formatted = run(input, &config);
        assert_eq!(formatted, "Line 1\nLine 2\nLine 3\n");
    }

    #[test]
    fn test_consecutive_blank_lines_collapsed() {
        let input = "Line 1\n\n\n\n\nLine 2";
        let config = FormatterConfig::default();
        let formatted = run(input, &config);
        assert_eq!(formatted, "Line 1\n\nLine 2\n");
    }

    #[test]
    fn test_link_normalization() {
        let input = "See [[  note a  ]] and [[  note b  |  alias b  ]].";
        let config = FormatterConfig::default();
        let formatted = run(input, &config);
        assert_eq!(formatted, "See [[note a]] and [[note b|alias b]].\n");
    }

    #[test]
    fn test_code_block_preserved_verbatim() {
        let input = "```rust\nlet x = 1;   \n\n\nlet y = 2;\n```";
        let config = FormatterConfig::default();
        let formatted = run(input, &config);
        assert_eq!(formatted, "```rust\nlet x = 1;   \n\n\nlet y = 2;\n```\n");
    }

    #[test]
    fn test_heading_spacing() {
        let input = "# Heading 1\nContent\n\n\n\n## Heading 2\nContent 2";
        let config = FormatterConfig::default();
        let formatted = run(input, &config);
        assert_eq!(
            formatted,
            "# Heading 1\nContent\n\n## Heading 2\nContent 2\n"
        );
    }

    #[test]
    fn test_frontmatter_preserved() {
        let input = "---\ntitle: Note Title\ntags: [a, b]\n---\n\n\n# Heading\nContent";
        let config = FormatterConfig::default();
        let formatted = run(input, &config);
        assert_eq!(
            formatted,
            "---\ntitle: Note Title\ntags: [a, b]\n---\n\n# Heading\nContent\n"
        );
    }

    #[test]
    fn test_formatter_idempotence() {
        let input = "# Heading\n\nText with [[  link  ]] and code:\n\n```\nfoo\n```\n";
        let config = FormatterConfig::default();
        let pass1 = run(input, &config);
        let pass2 = run(&pass1, &config);
        assert_eq!(pass1, pass2);
    }

    fn fmt(input: &str) -> String {
        run(input, &FormatterConfig::default())
    }

    #[test]
    fn code_blocks_are_untouched_whatever_their_fence_shape() {
        // Each of these is already "clean" except for whitespace that is CONTENT inside code.
        for input in [
            // an inner ``` inside a 4-backtick block must not end it
            "````\n```\ninner   \n```\n````\n",
            // a different fence character inside a block is plain content
            "```\n~~~\n\n\n\ntext   \n```\n",
            "~~~\n```\n\n\n\ntext   \n~~~\n",
            // a longer closing fence
            "```\ncode   \n`````\n",
            // indented closing fence
            "```\ncode   \n  ```\n",
            // info string, blank lines and trailing spaces inside
            "```rust\nlet x = 1;   \n\n\n\nlet y = 2;   \n```\n",
            // two blocks back to back
            "```\na   \n```\n\n```\nb   \n```\n",
        ] {
            assert_eq!(fmt(input), input, "input {input:?}");
        }
    }

    #[test]
    fn a_code_line_after_a_shorter_run_is_still_code() {
        // "````" is closed only by 4+ backticks, so the "```" line is content and everything up
        // to EOF is inside the block.
        let input = "````\ncode   \n```\nmore   \n\n\nstill code   \n";
        assert_eq!(fmt(input), input);
    }

    #[test]
    fn an_unterminated_fence_protects_everything_to_the_end_of_the_file() {
        assert_eq!(
            fmt("```\ncode   \n\n\nmore   \n"),
            "```\ncode   \n\n\nmore   \n"
        );
        // Even without a final newline, the last code line keeps its trailing spaces.
        assert_eq!(fmt("```\ncode   \nmore   "), "```\ncode   \nmore   \n");
    }

    #[test]
    fn indented_code_is_verbatim_and_its_hash_lines_are_not_headings() {
        let input = "Para\n\n    # not a heading, this is code\n    trailing   \n\n\n    more   \n\nAfter\n";
        assert_eq!(fmt(input), input);
    }

    #[test]
    fn an_indented_hash_line_inside_a_paragraph_is_text_not_a_heading() {
        // 4+ leading spaces can't start an ATX heading, and indented code can't interrupt a
        // paragraph, so this is a lazy continuation line: no blank line may be inserted.
        assert_eq!(fmt("Para\n    # x\n"), "Para\n    # x\n");
    }

    #[test]
    fn atx_headings_allow_up_to_three_spaces_of_indentation() {
        assert_eq!(fmt("text\n   ## H\n"), "text\n\n   ## H\n");
        assert_eq!(fmt("text\n  ## H\n"), "text\n\n  ## H\n");
        assert_eq!(fmt("text\n    ## H\n"), "text\n    ## H\n");
        // Not a heading at all: no space after the #s.
        assert_eq!(fmt("text\n#tag\n"), "text\n#tag\n");
        assert_eq!(fmt("text\n####### seven\n"), "text\n####### seven\n");
    }

    #[test]
    fn html_blocks_keep_their_blank_lines_and_trailing_spaces() {
        for input in [
            "<pre>\nline one\n\n\nline two   \n</pre>\n",
            "<!-- comment\n\n\nstill comment   -->\n",
            "<div>\ntext   \n</div>\n",
            "<script>\nlet a = 1;   \n\n\nlet b = 2;\n</script>\n",
        ] {
            assert_eq!(fmt(input), input, "input {input:?}");
        }
    }

    #[test]
    fn blank_lines_collapse_outside_protected_regions_only() {
        let input = "a\n\n\n\nb\n\n```\nx\n\n\ny\n```\n\n\n\nc\n";
        assert_eq!(fmt(input), "a\n\nb\n\n```\nx\n\n\ny\n```\n\nc\n");
    }

    #[test]
    fn wikilinks_in_inline_code_are_left_alone_but_others_are_normalised() {
        assert_eq!(
            fmt("`[[ a ]]` and [[ b ]] and `x` [[ c | d ]]\n"),
            "`[[ a ]]` and [[b]] and `x` [[c|d]]\n"
        );
        // Inside fenced and indented code too.
        assert_eq!(fmt("```\n[[ a ]]\n```\n"), "```\n[[ a ]]\n```\n");
        assert_eq!(fmt("para\n\n    [[ a ]]\n"), "para\n\n    [[ a ]]\n");
        // Embeds.
        assert_eq!(fmt("![[ a | b ]]\n"), "![[a|b]]\n");
        // Several on one line, adjacent, and with heading/block parts.
        assert_eq!(fmt("[[ a ]][[ b ]]\n"), "[[a]][[b]]\n");
        assert_eq!(
            fmt("[[ a # h ]] [[ a #^ b | c ]]\n"),
            "[[a # h]] [[a #^ b|c]]\n"
        );
        // An unclosed opener is left alone.
        assert_eq!(fmt("[[ a and text\n"), "[[ a and text\n");
    }

    #[test]
    fn frontmatter_is_never_link_normalised_or_reflowed() {
        let input = "---\ntitle: \"[[ x ]]\"   \n\n\ntags: [a]\n---\n\n[[ y ]]\n";
        assert_eq!(
            fmt(input),
            "---\ntitle: \"[[ x ]]\"\n\n\ntags: [a]\n---\n\n[[y]]\n"
        );
    }

    #[test]
    fn link_normalisation_can_be_switched_off() {
        let config = FormatterConfig {
            normalize_links: false,
            ..FormatterConfig::default()
        };
        assert_eq!(run("[[ a ]]\n", &config), "[[ a ]]\n");
    }

    #[test]
    fn every_case_above_is_idempotent() {
        for input in [
            "````\n```\ninner   \n```\n````\n",
            "Para\n\n    # code   \n\n\n    more\n\nAfter\n",
            "<pre>\na\n\n\nb   \n</pre>\n\n\n\nafter   \n",
            "`[[ a ]]` [[ b ]]\n\n\n\n## H\ntext",
        ] {
            let once = fmt(input);
            assert_eq!(fmt(&once), once, "input {input:?}");
        }
    }
}
