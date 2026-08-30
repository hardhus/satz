use crate::config::MiscConfig;
use crate::model::ByteRange;

/// Computes splice replacements for thematic breaks (`---`/`***`/`___`), fenced code block
/// delimiters, and blockquote `>` marker spacing.
pub fn replacements(
    source: &str,
    rule_spans: &[ByteRange],
    code_fence_spans: &[ByteRange],
    blockquote_spans: &[ByteRange],
    config: &MiscConfig,
) -> Vec<(ByteRange, String)> {
    let mut out = Vec::new();

    let hr_style = normalize_hr_style(&config.hr_style);
    for span in rule_spans {
        // A Rule's own range always ends right after its single trailing '\n' (verified against
        // pulldown-cmark's offset iterator), so the replacement must include one too.
        out.push((*span, format!("{hr_style}\n")));
    }

    let fence_style = normalize_fence_style(&config.code_fence_style);
    for span in code_fence_spans {
        out.extend(fence_replacements(source, *span, fence_style));
    }

    if config.blockquote_single_space {
        for span in blockquote_spans {
            out.extend(blockquote_replacements(source, *span));
        }
    }

    out
}

fn normalize_hr_style(configured: &str) -> &str {
    match configured {
        "---" | "***" | "___" => configured,
        _ => "---",
    }
}

fn normalize_fence_style(configured: &str) -> char {
    match configured {
        "```" => '`',
        "~~~" => '~',
        _ => '`',
    }
}

/// Rewrites a fenced code block's opening fence characters, and its closing fence characters if
/// one is actually present (an unterminated fence at EOF is left as-is — there's nothing to
/// rewrite). The fence length is preserved from the source, except when converting to a
/// character that appears in a run of the same-or-greater length inside the block's own content
/// — in that case the new fence is lengthened just enough to stay unambiguous.
fn fence_replacements(source: &str, span: ByteRange, fence_char: char) -> Vec<(ByteRange, String)> {
    let text = &source[span.start..span.end];
    let bytes = text.as_bytes();

    let open_char = bytes[0] as char;
    let mut open_len = 0usize;
    while bytes.get(open_len).is_some_and(|b| *b as char == open_char) {
        open_len += 1;
    }

    if open_char == fence_char {
        // Already the configured style; nothing to do for this block.
        return Vec::new();
    }

    let content_start = text.find('\n').map(|i| i + 1).unwrap_or(text.len());

    // Determine whether the last line is a genuine closing fence: entirely (after any leading
    // indentation) the same character as the opening fence, with length >= the opening fence.
    let last_line_start = text[..text.len()].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let last_line = &text[last_line_start..];
    let last_line_trimmed = last_line.trim_start_matches([' ', '\t']);
    let has_closing_fence = last_line_start > content_start.saturating_sub(1)
        && !last_line_trimmed.is_empty()
        && last_line_trimmed.chars().all(|c| c == open_char)
        && last_line_trimmed.len() >= open_len;

    let content_end = if has_closing_fence {
        last_line_start
    } else {
        text.len()
    };
    let content = &text[content_start..content_end];

    // Pick a fence length that can't collide with any same-character run already present in the
    // content (a line consisting solely of `fence_char` repeated >= our chosen length would
    // otherwise prematurely close the block).
    let longest_run_in_content = content
        .lines()
        .map(|line| {
            let trimmed = line.trim_start_matches([' ', '\t']);
            if !trimmed.is_empty() && trimmed.chars().all(|c| c == fence_char) {
                trimmed.len()
            } else {
                0
            }
        })
        .max()
        .unwrap_or(0);
    let new_len = open_len.max(longest_run_in_content + 1).max(3);
    let fence_text: String = std::iter::repeat_n(fence_char, new_len).collect();

    let mut out = Vec::with_capacity(2);
    out.push((
        ByteRange::new(span.start, span.start + open_len),
        fence_text.clone(),
    ));
    if has_closing_fence {
        let close_start =
            span.start + last_line_start + (last_line.len() - last_line_trimmed.len());
        out.push((
            ByteRange::new(
                close_start,
                close_start + open_len.min(last_line_trimmed.len()),
            ),
            fence_text,
        ));
    }
    out
}

/// Normalizes every line within a top-level blockquote span so each `>` marker (at every nesting
/// level present on that line) is followed by exactly one space. Lazy-continuation lines that
/// don't start with `>` at all are left untouched, as is any leading indentation before the first
/// `>` on a line.
fn blockquote_replacements(source: &str, span: ByteRange) -> Vec<(ByteRange, String)> {
    let text = &source[span.start..span.end];
    let mut out = Vec::new();
    let mut offset = span.start;

    for line in text.split_inclusive('\n') {
        let line_body = line.strip_suffix('\n').unwrap_or(line);
        if let Some(replacement) = normalize_blockquote_line(line_body) {
            out.push((
                ByteRange::new(offset, offset + replacement.0),
                replacement.1,
            ));
        }
        offset += line.len();
    }

    out
}

/// Returns `(prefix_byte_len, replacement_text)` for a line's leading indentation + `>` marker
/// run, or `None` if the line doesn't start with `>` (after up to 3 leading spaces) at all.
fn normalize_blockquote_line(line: &str) -> Option<(usize, String)> {
    let bytes = line.as_bytes();
    let mut i = 0;
    let mut leading_spaces = 0;
    while leading_spaces < 3 && bytes.get(i) == Some(&b' ') {
        i += 1;
        leading_spaces += 1;
    }

    if bytes.get(i) != Some(&b'>') {
        return None;
    }

    let indent = &line[..i];
    let mut depth = 0usize;
    while bytes.get(i) == Some(&b'>') {
        i += 1;
        depth += 1;
        while bytes.get(i).is_some_and(|b| *b == b' ' || *b == b'\t') {
            i += 1;
        }
    }

    let replacement = format!("{indent}{}", "> ".repeat(depth));
    Some((i, replacement))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::FormatterConfig;
    use crate::formatter::format_document;
    use crate::parser::structure::parse_structure;

    fn apply(source: &str, config: &MiscConfig) -> String {
        let structure = parse_structure(source);
        let mut reps = replacements(
            source,
            &structure.rule_spans,
            &structure.code_fence_spans,
            &structure.blockquote_spans,
            config,
        );
        reps.sort_by_key(|(r, _)| r.start);
        crate::formatter::zones::splice_ranges(source, &reps)
    }

    #[test]
    fn test_hr_normalized_to_configured_style() {
        let config = MiscConfig {
            enable: true,
            hr_style: "***".to_string(),
            code_fence_style: "```".to_string(),
            blockquote_single_space: true,
        };
        let out = apply("a\n\n---\n\nb\n", &config);
        assert_eq!(out, "a\n\n***\n\nb\n");
    }

    #[test]
    fn test_frontmatter_fence_not_touched_by_hr_normalization() {
        let md = "---\ntitle: X\n---\n\nbody\n\n---\n\nafter\n";
        let out = apply(md, &MiscConfig::default());
        assert!(out.starts_with("---\ntitle: X\n---\n"));
    }

    #[test]
    fn test_code_fence_style_backtick_to_tilde() {
        let config = MiscConfig {
            enable: true,
            hr_style: "---".to_string(),
            code_fence_style: "~~~".to_string(),
            blockquote_single_space: true,
        };
        let out = apply("```rust\nlet x = 1;\n```\n", &config);
        assert_eq!(out, "~~~rust\nlet x = 1;\n~~~\n");
    }

    #[test]
    fn test_code_fence_style_tilde_to_backtick() {
        let out = apply("~~~rust\nlet x = 1;\n~~~\n", &MiscConfig::default());
        assert_eq!(out, "```rust\nlet x = 1;\n```\n");
    }

    #[test]
    fn test_code_fence_content_never_touched() {
        let config = MiscConfig {
            enable: true,
            hr_style: "---".to_string(),
            code_fence_style: "~~~".to_string(),
            blockquote_single_space: true,
        };
        let out = apply("```rust\nlet x = \"a`b\";\n```\n", &config);
        assert_eq!(out, "~~~rust\nlet x = \"a`b\";\n~~~\n");
    }

    #[test]
    fn test_unterminated_fence_only_opening_rewritten() {
        let config = MiscConfig {
            enable: true,
            hr_style: "---".to_string(),
            code_fence_style: "~~~".to_string(),
            blockquote_single_space: true,
        };
        let out = apply("```rust\nlet x = 1;\n", &config);
        assert_eq!(out, "~~~rust\nlet x = 1;\n");
    }

    #[test]
    fn test_fence_indented_inside_list_item_preserved() {
        let out = apply(
            "- item\n\n  ```rust\n  code\n  ```\n",
            &MiscConfig::default(),
        );
        assert_eq!(out, "- item\n\n  ```rust\n  code\n  ```\n");
    }

    #[test]
    fn test_blockquote_single_space_enforced() {
        let out = apply("> line one\n>line two\n", &MiscConfig::default());
        assert_eq!(out, "> line one\n> line two\n");
    }

    #[test]
    fn test_nested_blockquote_every_level_spaced() {
        let out = apply("> outer\n>> inner\n", &MiscConfig::default());
        assert_eq!(out, "> outer\n> > inner\n");
    }

    #[test]
    fn test_blockquote_lazy_continuation_line_untouched() {
        let out = apply("> first line\nlazy continued\n", &MiscConfig::default());
        assert_eq!(out, "> first line\nlazy continued\n");
    }

    #[test]
    fn test_blockquote_disabled_via_config_flag() {
        let config = MiscConfig {
            enable: true,
            hr_style: "---".to_string(),
            code_fence_style: "```".to_string(),
            blockquote_single_space: false,
        };
        let out = apply("> line one\n>line two\n", &config);
        assert_eq!(out, "> line one\n>line two\n");
    }

    #[test]
    fn test_end_to_end_idempotent_via_format_document() {
        let mut cfg = FormatterConfig::default();
        cfg.misc.hr_style = "***".to_string();
        cfg.misc.code_fence_style = "~~~".to_string();
        let input = "a\n\n---\n\n>quote\n\n```rust\ncode\n```\n";
        let pass1 = format_document(input, &cfg);
        let pass2 = format_document(&pass1, &cfg);
        assert_eq!(pass1, pass2);
    }
}
