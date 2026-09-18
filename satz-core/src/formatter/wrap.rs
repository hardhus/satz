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
/// Known, deliberate limitation: a paragraph containing an actual hard line break (trailing two
/// spaces, or a backslash, before a newline) has that break silently collapsed like any other
/// internal soft-wrap -- `line_pass::run`'s per-line `trim_end()` already destroys
/// trailing-space hard breaks unconditionally today regardless of this pass, so there was no
/// existing guarantee to preserve here; backslash hard breaks aren't specially preserved either,
/// for the same not-worth-the-complexity reason (none of this vault's own content uses them).
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
        if wrapped != body {
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
fn render_wrapped(source: &str, tokens: &[(ByteRange, usize)], line_width: usize) -> String {
    let mut out = String::new();
    let mut current_width = 0usize;
    for (idx, (range, width)) in tokens.iter().enumerate() {
        if idx == 0 {
            out.push_str(&source[range.start..range.end]);
            current_width = *width;
            continue;
        }
        if current_width + 1 + width > line_width {
            out.push('\n');
            current_width = *width;
        } else {
            out.push(' ');
            current_width += 1 + width;
        }
        out.push_str(&source[range.start..range.end]);
    }
    out
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

    #[test]
    fn test_wrap_is_idempotent() {
        let input = "one two three [[tlp/sozluk#Olgu Bağlamı|olgu bağlamlarının]] four five six seven eight nine ten eleven twelve.\n";
        let config = enabled_config(40);
        let pass1 = wrapped(input, &config);
        let pass2 = wrapped(&pass1, &config);
        assert_eq!(pass1, pass2);
    }
}
