use pulldown_cmark::{Event, Options, Parser, Tag};

use crate::config::FormatterConfig;
use crate::model::ByteRange;
use crate::parser::inline_scan;
use crate::parser::structure::{self, StructureOutput};

/// Reflows top-level paragraphs to `config.line_width`, never splitting a wikilink/embed
/// (`[[...]]`/`![[...]]`), inline code span, or standard markdown link (`[text](url)`) across a
/// line break -- see `WrapConfig`'s doc comment for why and for the raw/display width modes.
/// No-op (returns `source` unchanged) if `config.wrap.enable` is false.
///
/// Scoped to top-level paragraphs only (not list items or blockquotes -- see
/// `StructureOutput::paragraph_spans`) and re-parses the ALREADY-spliced text from a fresh
/// `parse_structure`/`scan_inline` call: running this as part of the same single-pass
/// replacement list as `table`/`list`/`emphasis`/`misc` would make its whole-paragraph
/// replacements overlap theirs (a paragraph containing `**bold**` also contains the emphasis
/// span `zones::splice_ranges` needs to rewrite), which `splice_ranges` would then silently
/// drop. Running after splicing avoids the conflict and also means wrapping is computed against
/// the already-normalized emphasis/list/table text, not the pre-formatting source.
///
/// Hard breaks are preserved: backslash ones (`text\` + newline) and trailing-space ones (two or
/// more spaces + newline, written back exactly as they were).
///
/// A wrapped paragraph is only written back if re-parsing it still yields exactly one paragraph
/// with the same words (`is_single_paragraph` / `same_words`), so a construct the wrapping rules
/// don't anticipate leaves the paragraph as it was rather than corrupting it.
pub fn wrap(source: &str, config: &FormatterConfig) -> String {
    if !config.wrap.enable {
        return source.to_string();
    }

    let structure = structure::parse_structure(source);
    if structure.paragraph_spans.is_empty() {
        return source.to_string();
    }

    let mut code_spans_for_scan = structure.code_spans.clone();
    if let Some(fm) = structure.frontmatter_range {
        code_spans_for_scan.push(fm);
    }
    code_spans_for_scan.sort_unstable_by_key(|s| s.start);
    let inline = inline_scan::scan_inline(source, &code_spans_for_scan);

    let atomic_spans =
        collect_atomic_spans(source, &structure, &inline, &config.wrap.link_width_mode);

    let mut replacements: Vec<(ByteRange, String)> = Vec::new();
    for para in &structure.paragraph_spans {
        let text = &source[para.start..para.end];
        let body_len = text.trim_end_matches('\n').len();
        let body_range = ByteRange::new(para.start, para.start + body_len);
        let body = &source[body_range.start..body_range.end];

        let tokens = tokenize_with_width(source, body_range, &atomic_spans);
        if tokens.len() < 2 {
            continue; // nothing to possibly wrap
        }
        let wrapped = render_wrapped(source, &tokens, config.line_width);
        // Safety net, independent of the rules in `render_wrapped`: a paragraph is only replaced
        // if re-parsing the result still yields exactly one paragraph with the same words.
        // Anything that slips past the rules keeps its original text instead of being corrupted.
        if wrapped != body && same_words(body, &wrapped) && is_single_paragraph(&wrapped) {
            replacements.push((body_range, wrapped));
        }
    }

    if replacements.is_empty() {
        return source.to_string();
    }
    replacements.sort_by_key(|(r, _)| r.start);
    crate::formatter::zones::splice_ranges(source, &replacements)
}

/// Collects the byte ranges that must never be split mid-span, each paired with its "visual
/// width" per `link_width_mode` ("raw": full source text; "display": alias/display text, or the
/// bare target when there's no alias -- see `WrapConfig`).
fn collect_atomic_spans(
    source: &str,
    structure: &StructureOutput,
    inline: &inline_scan::InlineScanOutput,
    link_width_mode: &str,
) -> Vec<(ByteRange, usize)> {
    let display_mode = link_width_mode == "display";
    let mut spans: Vec<(ByteRange, usize)> = Vec::new();

    for link in &inline.wiki_links {
        let raw = &source[link.range.start..link.range.end];
        let width = if display_mode {
            link.display
                .as_ref()
                .map(|d| d.chars().count())
                .unwrap_or_else(|| {
                    let mut w = link.target_doc.chars().count();
                    if let Some(h) = &link.target_heading {
                        w += 1 + h.chars().count();
                    }
                    w
                })
        } else {
            raw.chars().count()
        };
        spans.push((link.range, width));
    }

    for span in &structure.code_spans {
        spans.push((*span, source[span.start..span.end].chars().count()));
    }

    // An image is one piece: its alt text and title contain spaces the wrapper must not break at.
    for span in &structure.image_spans {
        let raw = &source[span.start..span.end];
        let width = if display_mode {
            // The alt text is what a rendered viewer shows: `![alt](...)` -> `alt`.
            raw.strip_prefix("![")
                .and_then(|rest| rest.split_once(']'))
                .map_or_else(|| raw.chars().count(), |(alt, _)| alt.chars().count())
        } else {
            raw.chars().count()
        };
        spans.push((*span, width));
    }

    for link in &structure.std_links {
        let raw = &source[link.range.start..link.range.end];
        let width = if display_mode {
            link.display
                .as_ref()
                .map(|d| d.chars().count())
                .unwrap_or_else(|| link.target_doc.chars().count())
        } else {
            raw.chars().count()
        };
        spans.push((link.range, width));
    }

    spans.sort_by_key(|(r, _)| r.start);
    spans
}

/// Walks `range` (a paragraph's body, with any trailing newline already stripped by the caller)
/// splitting on whitespace, except that a token overlapping an atomic span always swallows the
/// span's ENTIRE range as one unit (even across the span's own internal whitespace, e.g. a
/// `[[path#Two Words|alias]]` wikilink) and glues any immediately-adjacent non-whitespace
/// characters (e.g. trailing punctuation right after the closing `]]`) into the same token.
/// Returns each token's byte range plus its precomputed visual width.
fn tokenize_with_width(
    source: &str,
    range: ByteRange,
    atomic_spans: &[(ByteRange, usize)],
) -> Vec<(ByteRange, usize)> {
    let mut tokens: Vec<(ByteRange, usize)> = Vec::new();
    let mut pos = range.start;
    let mut token_start: Option<usize> = None;
    let mut token_width = 0usize;
    let mut ai = atomic_spans.partition_point(|(r, _)| r.end <= pos);

    while pos < range.end {
        while ai < atomic_spans.len() && atomic_spans[ai].0.end <= pos {
            ai += 1;
        }
        if ai < atomic_spans.len()
            && atomic_spans[ai].0.start == pos
            && atomic_spans[ai].0.end <= range.end
        {
            let (span, width) = atomic_spans[ai];
            if token_start.is_none() {
                token_start = Some(pos);
            }
            token_width += width;
            pos = span.end;
            continue;
        }

        let c = source[pos..].chars().next().expect("pos within range.end");
        if c.is_whitespace() {
            if let Some(start) = token_start.take() {
                tokens.push((ByteRange::new(start, pos), token_width));
                token_width = 0;
            }
        } else {
            if token_start.is_none() {
                token_start = Some(pos);
            }
            token_width += 1;
        }
        pos += c.len_utf8();
    }
    if let Some(start) = token_start {
        tokens.push((ByteRange::new(start, range.end), token_width));
    }
    tokens
}

/// Greedy word-wrap: places tokens on the current line while `current_width + 1 (space) +
/// next_width <= line_width`; otherwise starts a new line. A token wider than `line_width` on
/// its own is never split -- it simply becomes an over-length line by itself (the line-width
/// limit is best-effort, not a hard guarantee, by explicit design: rescuing unbounded lines
/// matters far more than shaving a few columns off one unavoidably-long link).
///
/// Three things override the width rule, because a newline is not always just whitespace in
/// Markdown:
/// - A backslash hard break in the original (`text\` + newline) is kept as a forced newline.
/// - A newline is never placed right after a token ending in a literal backslash, which would
///   silently turn it into a hard break.
/// - A token that would start a different block if it began a line (`#`, `-`, `1.`, `>`, ...)
///   stays on the previous line instead, even if that overruns the width.
fn render_wrapped(source: &str, tokens: &[(ByteRange, usize)], line_width: usize) -> String {
    let mut out = String::new();
    let mut current_width = 0usize;
    for (idx, (range, width)) in tokens.iter().enumerate() {
        let text = &source[range.start..range.end];
        if idx == 0 {
            out.push_str(text);
            current_width = *width;
            continue;
        }

        let (prev_range, _) = tokens[idx - 1];
        let prev = &source[prev_range.start..prev_range.end];
        let prev_ends_in_backslash = ends_with_odd_backslashes(prev);
        let hard_break = prev_ends_in_backslash
            && (source[prev_range.end..].starts_with('\n')
                || source[prev_range.end..].starts_with("\r\n"));
        // Two or more spaces right before a newline are a hard break too; they are written back
        // exactly as they were.
        let gap = &source[prev_range.end..range.start];
        let break_spaces = gap
            .split_once('\n')
            .map(|(before, _)| before.trim_end_matches('\r'))
            .filter(|before| before.len() >= 2 && before.bytes().all(|b| b == b' '));

        let overflows = current_width + 1 + width > line_width;
        let may_break = !prev_ends_in_backslash && !would_start_block(text);
        if let Some(spaces) = break_spaces {
            out.push_str(spaces);
            out.push('\n');
            current_width = *width;
        } else if hard_break || (overflows && may_break) {
            out.push('\n');
            current_width = *width;
        } else {
            out.push(' ');
            current_width += 1 + width;
        }
        out.push_str(text);
    }
    out
}

/// True if `token` ends in an odd number of backslashes, i.e. the last one is an unescaped `\`
/// (an even count is just escaped backslashes, `\\`).
fn ends_with_odd_backslashes(token: &str) -> bool {
    token.chars().rev().take_while(|&c| c == '\\').count() % 2 == 1
}

/// True if starting a line with `token` could make the line (and the paragraph around it) a
/// different kind of block. Conservative on purpose: wrongly keeping a token on the previous
/// line only overruns the width; wrongly moving it to a new line corrupts the document.
fn would_start_block(token: &str) -> bool {
    let only = |set: &[char]| !token.is_empty() && token.chars().all(|c| set.contains(&c));

    // ATX heading: 1-6 `#` on their own.
    if token.len() <= 6 && only(&['#']) {
        return true;
    }
    // Bullet marker, thematic break, setext underline, table delimiter cell: runs made only of
    // `- + * _ =` (and `:`/`|` for table delimiters).
    if only(&['-', '+', '*', '_', '=']) || (only(&['-', ':', '|']) && token.contains('-')) {
        return true;
    }
    // Ordered list marker. Only one starting at 1 can interrupt a paragraph, so `2024.` at a
    // sentence end is left free to wrap.
    if let Some(digits) = token.strip_suffix(['.', ')'])
        && !digits.is_empty()
        && digits.len() <= 9
        && digits.chars().all(|c| c.is_ascii_digit())
        && digits.parse::<u32>() == Ok(1)
    {
        return true;
    }
    // Blockquote, code fence, math block, table row.
    if token.starts_with('>')
        || token.starts_with("```")
        || token.starts_with("~~~")
        || token.starts_with("$$")
        || token.starts_with('|')
    {
        return true;
    }
    // HTML block start (`<div>`, `</p>`, `<!-- -->`, `<?php`).
    if let Some(rest) = token.strip_prefix('<')
        && rest
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || matches!(c, '/' | '!' | '?'))
    {
        return true;
    }
    // Footnote / link-reference definition marker: `[^x]:` / `[x]:`.
    token.starts_with('[') && token.ends_with("]:")
}

/// True if both texts have exactly the same whitespace-separated words in the same order.
fn same_words(a: &str, b: &str) -> bool {
    a.split_whitespace().eq(b.split_whitespace())
}

/// True if `text`, parsed on its own, is exactly one plain paragraph -- no heading, list, quote,
/// rule, code, table, or HTML block anywhere in it.
fn is_single_paragraph(text: &str) -> bool {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_FOOTNOTES);
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_TASKLISTS);
    options.insert(Options::ENABLE_HEADING_ATTRIBUTES);

    let mut paragraphs = 0usize;
    let mut depth = 0usize;
    for event in Parser::new_ext(text, options) {
        match event {
            Event::Start(Tag::Paragraph) if depth == 0 => {
                paragraphs += 1;
                depth += 1;
            }
            // Any other block opening at the top level (heading, list, quote, table, code...).
            Event::Start(_) if depth == 0 => return false,
            Event::Start(_) => depth += 1,
            Event::End(_) => depth = depth.saturating_sub(1),
            Event::Rule | Event::Html(_) => return false,
            _ => {}
        }
    }
    paragraphs == 1
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::FormatterConfig;

    fn wrapped(input: &str, config: &FormatterConfig) -> String {
        wrap(input, config)
    }

    fn enabled_config(line_width: usize) -> FormatterConfig {
        let mut config = FormatterConfig::default();
        config.wrap.enable = true;
        config.line_width = line_width;
        config
    }

    #[test]
    fn test_disabled_by_default_is_passthrough() {
        let input = "This is a fairly long plain sentence that would exceed a tiny line width.\n";
        let config = FormatterConfig::default();
        assert_eq!(wrapped(input, &config), input);
    }

    #[test]
    fn test_plain_prose_wraps_at_word_boundaries() {
        let input = "one two three four five six seven eight nine ten\n";
        let config = enabled_config(20);
        let out = wrapped(input, &config);
        for line in out.lines() {
            assert!(line.chars().count() <= 20, "line too long: {line:?}");
        }
        // No words lost or reordered.
        assert_eq!(
            out.split_whitespace().collect::<Vec<_>>(),
            input.split_whitespace().collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_wikilink_never_split_even_when_it_alone_exceeds_width() {
        let input = "Şey, [[tlp/sozluk#Olgu Bağlamı|olgu bağlamlarının]] içinde yer alabilir.\n";
        let config = enabled_config(20);
        let out = wrapped(input, &config);
        assert!(
            out.contains("[[tlp/sozluk#Olgu Bağlamı|olgu bağlamlarının]]"),
            "wikilink must appear byte-for-byte intact: {out:?}"
        );
        // It must not have been split across a line break internally.
        for line in out.lines() {
            if line.contains("[[") {
                assert!(
                    line.contains("]]"),
                    "link opened and closed on the same line: {line:?}"
                );
            }
        }
    }

    #[test]
    fn test_punctuation_glued_to_link_stays_attached() {
        let input = "Bakınız [[tlp/1]]. Devamı burada.\n";
        let config = enabled_config(10);
        let out = wrapped(input, &config);
        assert!(
            out.contains("[[tlp/1]]."),
            "trailing period must stay glued to the link: {out:?}"
        );
    }

    #[test]
    fn test_raw_vs_display_mode_produce_different_wrap_points() {
        let input = "x [[tlp/sozluk#Olgu Bağlamı|kısa]] y z w v u\n";
        let mut raw_config = enabled_config(30);
        raw_config.wrap.link_width_mode = "raw".to_string();
        let mut display_config = enabled_config(30);
        display_config.wrap.link_width_mode = "display".to_string();

        let raw_out = wrapped(input, &raw_config);
        let display_out = wrapped(input, &display_config);
        assert_ne!(
            raw_out, display_out,
            "raw (full [[...]] text) and display (just the alias) modes must weigh this \
             link differently and so wrap it differently"
        );
    }

    #[test]
    fn test_list_item_and_blockquote_paragraphs_untouched() {
        let input = "- a very long list item line that would otherwise exceed the configured width easily\n\n> a very long quoted line that would otherwise exceed the configured width easily\n";
        let config = enabled_config(20);
        assert_eq!(wrapped(input, &config), input);
    }

    #[test]
    fn test_inline_code_never_split() {
        let input = "Run `cargo test --workspace --all-targets` to check everything.\n";
        let config = enabled_config(15);
        let out = wrapped(input, &config);
        assert!(out.contains("`cargo test --workspace --all-targets`"));
    }

    /// Tokens that, if wrapping left one at the START of a continuation line, would turn the
    /// paragraph into a heading, list, quote, rule, code fence, HTML/table/definition block, ...
    const BLOCK_STARTERS: &[&str] = &[
        "#", "###", "-", "+", "*", "1.", "1)", ">", ">>", "---", "===", "***", "___", "```", "~~~",
        "<div>", "</p>", "|", "|---|", ":--", "[^x]:", "[ref]:", "$$",
    ];

    #[test]
    fn test_wrap_never_starts_a_line_with_a_block_marker() {
        for marker in BLOCK_STARTERS {
            // "aaaa bbbb cccc dddd" is exactly 19 columns, so a naive greedy wrap at width 19
            // would put the marker at the start of the second line.
            let input = format!("aaaa bbbb cccc dddd {marker} eeee ffff\n");
            let out = wrapped(&input, &enabled_config(19));
            assert!(
                out.trim_end().contains('\n'),
                "expected the paragraph to be wrapped somewhere for {marker:?}: {out:?}"
            );
            for line in out.lines().skip(1) {
                assert!(
                    !line.starts_with(marker),
                    "{marker:?} landed at the start of a line: {out:?}"
                );
            }
            assert_eq!(
                out.split_whitespace().collect::<Vec<_>>(),
                input.split_whitespace().collect::<Vec<_>>(),
                "content changed for {marker:?}"
            );
        }
    }

    #[test]
    fn test_backslash_hard_break_is_preserved() {
        let input = "first line\\\nsecond line is long enough that it has to wrap around\n";
        let out = wrapped(input, &enabled_config(20));
        assert!(
            out.starts_with("first line\\\n"),
            "hard break lost: {out:?}"
        );
        assert!(
            !out.contains("\\ "),
            "backslash left dangling in text: {out:?}"
        );
    }

    #[test]
    fn test_wrap_never_turns_a_literal_backslash_into_a_hard_break() {
        // `bbbb\` followed by a SPACE is a literal backslash; wrapping must not put a newline
        // right after it, because backslash + newline IS a hard break.
        let input = "aaaa bbbb\\ cccc dddd eeee\n";
        let out = wrapped(input, &enabled_config(9));
        assert!(
            !out.contains("\\\n"),
            "literal backslash became a hard break: {out:?}"
        );
    }

    #[test]
    fn test_is_single_paragraph_classifies_block_shapes() {
        for ok in [
            "plain words",
            "two\nlines of one paragraph",
            "with **emphasis\nacross** lines and [[a link|alias]]",
            "inline `code` and <b>html</b> and a footnote[^1]",
            "hard\\\nbreak stays a paragraph",
        ] {
            assert!(is_single_paragraph(ok), "should be one paragraph: {ok:?}");
        }
        for bad in [
            "a\n# heading",
            "a\n- item",
            "a\n1. item",
            "a\n> quote",
            "a\n---",
            "a\n===",
            "a\n```\ncode\n```",
            "a\n\nb",
            "a\n<div>x</div>",
            "| a | b |\n|---|---|\n| 1 | 2 |",
            "",
        ] {
            assert!(
                !is_single_paragraph(bad),
                "should NOT be one paragraph: {bad:?}"
            );
        }
    }

    #[test]
    fn test_same_words_ignores_whitespace_but_not_content() {
        assert!(same_words("a b  c", "a\nb c"));
        assert!(!same_words("a b c", "a bc"));
        assert!(!same_words("a b c", "a b"));
    }

    #[test]
    fn test_every_token_that_actually_breaks_a_paragraph_is_guarded() {
        // Cross-checks the hand-written `would_start_block` rules against the real parser: any
        // token that, at the start of a continuation line, makes pulldown-cmark stop treating
        // the text as a single paragraph MUST be guarded.
        let extras = [
            "word", "2024.", "2.", "10)", "1.5", "01.", "#tag", "#", "#######", ">=", "<b>", "<3",
            "**bold**", "*em*", "[x](y)", "![i](u)", ":", "|a|", "---x", "-x", "+x", "====", "~",
            "~~x~~", "1986.", "*", "+", "-", "---", "***", "___", "\\#", "a:b",
        ];
        for token in BLOCK_STARTERS.iter().chain(extras.iter()) {
            let breaks = !is_single_paragraph(&format!("aaaa\n{token} bbbb"));
            if breaks {
                assert!(
                    would_start_block(token),
                    "{token:?} turns a paragraph into another block but is not guarded"
                );
            }
        }
    }

    #[test]
    fn test_footnote_definition_paragraph_is_not_wrapped() {
        let def = "[^1]: A long footnote definition text that would certainly exceed the width.";
        let input = format!("Body text.[^1]\n\n{def}\n");
        let out = wrapped(&input, &enabled_config(20));
        assert!(
            out.contains(def),
            "footnote definition was reflowed: {out:?}"
        );
    }

    #[test]
    fn test_wrap_is_idempotent() {
        let input = "one two three [[tlp/sozluk#Olgu Bağlamı|olgu bağlamlarının]] four five six seven eight nine ten eleven twelve.\n";
        let config = enabled_config(40);
        let pass1 = wrapped(input, &config);
        let pass2 = wrapped(&pass1, &config);
        assert_eq!(pass1, pass2);
    }

    // ---- images are atomic: their alt text and title are never split across lines ----

    fn html_of(md: &str) -> String {
        let mut out = String::new();
        pulldown_cmark::html::push_html(&mut out, pulldown_cmark::Parser::new(md));
        // A soft line break renders as a newline, a space renders as a space: same text.
        out.replace('\n', " ")
    }

    #[test]
    fn an_image_with_spaces_in_alt_and_title_is_never_split() {
        let image = "![two words alt](img.png \"a long title\")";
        let input = format!("first words here {image} then some more words after it\n");
        for width in [10, 20, 30, 45] {
            let out = wrapped(&input, &enabled_config(width));
            assert!(out.contains(image), "width {width}: {out:?}");
            assert_eq!(html_of(&out), html_of(&input), "width {width}");
        }
    }

    #[test]
    fn a_nested_image_link_stays_in_one_piece() {
        let nested = "[![alt text here](i.png \"the title\")](note.md)";
        let input = format!("before {nested} after and then plenty of extra words to wrap\n");
        let out = wrapped(&input, &enabled_config(24));
        assert!(out.contains(nested), "{out:?}");
        assert_eq!(html_of(&out), html_of(&input));
    }

    #[test]
    fn images_next_to_punctuation_start_or_end_of_a_paragraph_and_reference_style() {
        for input in [
            "![a b c](x.png), then more words that need wrapping around here\n",
            "words that need wrapping around here and then an image ![a b c](x.png).\n",
            "![a b c](x.png) ![d e f](y.png) and a few more words to wrap over lines\n",
            "some words ![alt two][ref] more words to wrap them properly here\n\n[ref]: img.png\n",
        ] {
            let out = wrapped(input, &enabled_config(18));
            for image in ["![a b c](x.png)", "![d e f](y.png)", "![alt two][ref]"] {
                if input.contains(image) {
                    assert!(out.contains(image), "{image} in {out:?}");
                }
            }
            assert_eq!(html_of(&out), html_of(input), "{input:?}");
        }
    }

    #[test]
    fn a_very_long_image_overflows_the_line_instead_of_being_broken() {
        let image = "![a really long alternative text with many words](some/long/path/to/an/image.png \"and a title\")";
        let input = format!("short {image} tail\n");
        let out = wrapped(&input, &enabled_config(20));
        assert!(out.contains(image), "{out:?}");
        assert_eq!(out.lines().filter(|l| l.contains("![")).count(), 1);
    }

    #[test]
    fn both_width_modes_keep_images_whole_and_wrapping_is_idempotent() {
        let input = "one two three ![alt with words](pic.png \"t t\") four five six seven eight nine ten eleven\n";
        for mode in ["raw", "display"] {
            let mut config = enabled_config(28);
            config.wrap.link_width_mode = mode.to_string();
            let once = wrapped(input, &config);
            assert!(
                once.contains("![alt with words](pic.png \"t t\")"),
                "{mode}: {once:?}"
            );
            assert_eq!(wrapped(&once, &config), once, "{mode}");
            assert_eq!(html_of(&once), html_of(input), "{mode}");
        }
    }
}
