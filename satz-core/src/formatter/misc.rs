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

    // The span starts at the opening fence characters themselves (any indentation or `>` quote
    // marker before them is outside it); anything else means it isn't a shape we understand.
    let open_char = match bytes.first() {
        Some(b'`') => '`',
        Some(b'~') => '~',
        _ => return Vec::new(),
    };
    let mut open_len = 0usize;
    while bytes.get(open_len).is_some_and(|b| *b as char == open_char) {
        open_len += 1;
    }

    if open_char == fence_char {
        // Already the configured style; nothing to do for this block.
        return Vec::new();
    }

    let first_line_end = text.find('\n').unwrap_or(text.len());
    // A backtick fence's info string may not contain a backtick, so a tilde fence like
    // `~~~ js `x`` can't be converted: it would stop being a code block.
    if fence_char == '`' && text[open_len..first_line_end].contains('`') {
        return Vec::new();
    }

    let content_start = text.find('\n').map(|i| i + 1).unwrap_or(text.len());

    // On continuation lines inside a blockquote or list item, the fence characters are preceded
    // by indentation and/or `>` markers.
    let split_prefix =
        |line: &str| -> usize { line.len() - line.trim_start_matches([' ', '\t', '>']).len() };

    // Determine whether the last line is a genuine closing fence: (after that prefix, and ignoring
    // trailing whitespace) entirely the same character as the opening fence, with length >= the
    // opening fence.
    let last_line_start = text.rfind('\n').map(|i| i + 1).unwrap_or(0);
    let last_line = &text[last_line_start..];
    let close_prefix_len = split_prefix(last_line);
    let close_run = last_line[close_prefix_len..].trim_end();
    let has_closing_fence = last_line_start > content_start.saturating_sub(1)
        && !close_run.is_empty()
        && close_run.chars().all(|c| c == open_char)
        && close_run.len() >= open_len;

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
            let run = line[split_prefix(line)..].trim_end();
            if !run.is_empty() && run.chars().all(|c| c == fence_char) {
                run.len()
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
        // Replace the WHOLE closing run: it may be longer than the opening fence, and leaving the
        // extra characters behind would make the closer invalid.
        let close_start = span.start + last_line_start + close_prefix_len;
        out.push((
            ByteRange::new(close_start, close_start + close_run.len()),
            fence_text,
        ));
    }
    out
}

/// Normalizes the `>` markers of every line within a top-level blockquote span: consecutive
/// markers are separated by exactly one space (`>>` becomes `> >`), and the last marker is followed
/// by a space when the line has content. Whitespace AFTER the last marker is never collapsed:
/// beyond the marker's own optional space it is indentation and can be significant (indented code,
/// nested lists), so `>     code` must stay as it is. Lazy-continuation lines that don't start with
/// `>` at all are left untouched, as is any leading indentation before the first `>` on a line.
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
pub(super) fn normalize_blockquote_line(line: &str) -> Option<(usize, String)> {
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
    // Byte offset just after the last marker (the whitespace run that follows it is preserved).
    let mut after_last_marker;
    loop {
        // `bytes[i]` is a `>` here.
        i += 1;
        depth += 1;
        after_last_marker = i;
        while bytes.get(i).is_some_and(|b| *b == b' ' || *b == b'\t') {
            i += 1;
        }
        if bytes.get(i) != Some(&b'>') {
            break;
        }
    }

    let kept_whitespace = i > after_last_marker;
    let has_content = i < bytes.len();
    let mut replacement = format!("{indent}{}>", "> ".repeat(depth - 1));
    if !kept_whitespace && has_content {
        replacement.push(' ');
    }
    // Only the prefix up to the last marker is rewritten; the whitespace run after it (if any)
    // stays exactly as written.
    if replacement == line[..after_last_marker] {
        return None;
    }
    Some((after_last_marker, replacement))
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

    fn html(src: &str) -> String {
        let mut options = pulldown_cmark::Options::empty();
        options.insert(pulldown_cmark::Options::ENABLE_TABLES);
        let mut out = String::new();
        pulldown_cmark::html::push_html(&mut out, pulldown_cmark::Parser::new_ext(src, options));
        out
    }

    /// The formatted text must render exactly like the original.
    fn assert_same_rendering(input: &str, output: &str) {
        assert_eq!(
            html(input),
            html(output),
            "rendering changed:\n--- input ---\n{input}\n--- output ---\n{output}"
        );
    }

    fn to_tilde() -> MiscConfig {
        MiscConfig {
            code_fence_style: "~~~".to_string(),
            ..MiscConfig::default()
        }
    }

    #[test]
    fn a_closing_fence_longer_than_the_opening_is_replaced_entirely() {
        // Only the first 3 bytes of the closer used to be replaced, leaving stray fence
        // characters ("```~") that no longer close the block and swallow the rest of the file.
        let cases = [
            ("~~~\ncode\n~~~~\n\nafter\n", "```\ncode\n```\n\nafter\n"),
            (
                "~~~\ncode\n~~~~~~~~\n\nafter\n",
                "```\ncode\n```\n\nafter\n",
            ),
            (
                "~~~~\ncode\n~~~~~\n\nafter\n",
                "````\ncode\n````\n\nafter\n",
            ),
        ];
        for (input, expected) in cases {
            let out = apply(input, &MiscConfig::default());
            assert_eq!(out, expected, "input {input:?}");
            assert_same_rendering(input, &out);
        }
        let out = apply("```\ncode\n`````\n\nafter\n", &to_tilde());
        assert_eq!(out, "~~~\ncode\n~~~\n\nafter\n");
    }

    #[test]
    fn a_closing_fence_with_trailing_spaces_or_indent_is_still_a_closing_fence() {
        let out = apply("~~~\ncode\n~~~   \n\nafter\n", &MiscConfig::default());
        assert_eq!(out, "```\ncode\n```   \n\nafter\n");
        let out = apply("~~~\ncode\n  ~~~\n\nafter\n", &MiscConfig::default());
        assert_eq!(out, "```\ncode\n  ```\n\nafter\n");
        let out = apply("~~~\ncode\n   ~~~~  \n\nafter\n", &MiscConfig::default());
        assert_eq!(out, "```\ncode\n   ```  \n\nafter\n");
    }

    #[test]
    fn a_tilde_fence_whose_info_string_has_a_backtick_is_not_converted_to_backticks() {
        // A backtick fence may not have a backtick in its info string, so converting would turn
        // the block into something else entirely.
        let input = "~~~ js `x`\ncode\n~~~\n\nafter\n";
        let out = apply(input, &MiscConfig::default());
        assert_eq!(out, input);
        assert_same_rendering(input, &out);
        // Without a backtick in the info string the conversion is fine.
        assert_eq!(
            apply("~~~ js\ncode\n~~~\n", &MiscConfig::default()),
            "``` js\ncode\n```\n"
        );
        assert_eq!(
            apply("~~~\ncode\n~~~\n", &MiscConfig::default()),
            "```\ncode\n```\n"
        );
    }

    #[test]
    fn the_new_fence_is_lengthened_when_the_content_contains_the_target_character() {
        let out = apply("~~~\n```\ninner\n```\n~~~\n", &MiscConfig::default());
        assert_eq!(out, "````\n```\ninner\n```\n````\n");
        assert_same_rendering("~~~\n```\ninner\n```\n~~~\n", &out);
    }

    #[test]
    fn a_shorter_run_is_content_not_a_closing_fence() {
        // "````" is closed only by 4+ fence characters; the "```" line is content and the block
        // is unterminated, so only the opening fence is rewritten.
        let input = "````\ncode\n```\n";
        let out = apply(input, &to_tilde());
        assert_eq!(out, "~~~~\ncode\n```\n");
        assert_same_rendering(input, &out);
    }

    #[test]
    fn fences_inside_quotes_and_lists_never_change_how_the_document_renders() {
        let cases = [
            "> ~~~\n> code in quote\n> ~~~\n\nafter\n",
            "> ```\n> code in quote\n> ```\n\nafter\n",
            "- item\n\n  ~~~\n  code in list\n  ~~~\n\nafter\n",
            "- item\n\n  ```\n  code in list\n  ```\n\nafter\n",
            "1. item\n\n   ~~~ rust\n   code\n   ~~~~\n\nafter\n",
            "> - item\n>\n>   ~~~\n>   code\n>   ~~~\n",
        ];
        for input in cases {
            for config in [MiscConfig::default(), to_tilde()] {
                let out = apply(input, &config);
                assert_same_rendering(input, &out);
            }
        }
    }

    #[test]
    fn multiple_fences_and_odd_shapes_survive() {
        let input = "```rust\na\n```\n\ntext\n\n~~~\nb\n~~~~\n\n~~~\n~~~\n";
        for config in [MiscConfig::default(), to_tilde()] {
            let out = apply(input, &config);
            assert_same_rendering(input, &out);
        }
        // A document that is only a fence, terminated and not.
        assert_eq!(apply("~~~\n~~~\n", &MiscConfig::default()), "```\n```\n");
        assert_eq!(apply("~~~\n", &MiscConfig::default()), "```\n");
        assert_eq!(apply("~~~", &MiscConfig::default()), "```");
    }

    #[test]
    fn blockquote_marker_spacing_rules() {
        // Between consecutive markers: exactly one space. After the LAST marker: at least one
        // space when there is content, but extra spaces are kept -- they are indentation (code,
        // nested lists), not decoration.
        for (input, expected) in [
            ("> line one\n>line two\n", "> line one\n> line two\n"),
            (">text\n", "> text\n"),
            (">>text\n", "> > text\n"),
            ("> >text\n", "> > text\n"),
            (">>>x\n", "> > > x\n"),
            (">  >  text\n", "> >  text\n"),
            ("> a\n>> b\n", "> a\n> > b\n"),
            // Extra spaces after the last marker are content and stay.
            (">  two spaces\n", ">  two spaces\n"),
            (">     code\n", ">     code\n"),
            ("> > >   deep\n", "> > >   deep\n"),
            // Empty quote lines get no trailing space added.
            (">\n", ">\n"),
            ("> >\n", "> >\n"),
            (">>\n", "> >\n"),
            // A marker followed only by whitespace is left alone.
            ("> \n", "> \n"),
            // A tab after the marker is kept.
            (">\ttab\n", ">\ttab\n"),
            // Up to 3 spaces of indentation before the first marker are kept.
            ("   > indented marker\n", "   > indented marker\n"),
            ("  >text\n", "  > text\n"),
            // Non-ASCII content.
            (">Türkçe içerik\n", "> Türkçe içerik\n"),
            // Not a quote line: untouched.
            ("plain > not a marker\n", "plain > not a marker\n"),
        ] {
            assert_eq!(
                apply(input, &MiscConfig::default()),
                expected,
                "input {input:?}"
            );
        }
    }

    #[test]
    fn blockquote_normalisation_never_changes_how_content_renders() {
        let cases = [
            // indented code inside a quote
            "> intro\n>\n>     code line\n>     second\n",
            // nested list depends on the indentation after the marker
            "> - outer\n>     - nested\n> - second\n",
            "> - outer\n>   - nested\n",
            // fenced code with indentation inside a quote
            "> ```\n>   indented\n> ```\n",
            // a list item containing a fence inside a quote
            "> - item\n>\n>   ~~~\n>   code\n>   ~~~\n",
            // nested quotes with code
            ">> outer\n>>     code\n",
            "> > text\n> >   more\n",
            // lazy continuation
            "> first\nlazy\n> second\n",
            // headings, rules and lists in quotes
            ">#  Heading\n>\n>1.  one\n>2.  two\n",
        ];
        for input in cases {
            let out = apply(input, &MiscConfig::default());
            assert_same_rendering(input, &out);
        }
    }

    #[test]
    fn blockquote_normalisation_is_idempotent() {
        for input in [
            ">a\n>>b\n",
            ">  a\n>     code\n",
            "> > >x\n",
            "> a\n>\n> b\n",
        ] {
            let once = apply(input, &MiscConfig::default());
            let twice = apply(&once, &MiscConfig::default());
            assert_eq!(once, twice, "input {input:?}");
        }
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
