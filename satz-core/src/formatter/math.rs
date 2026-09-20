//! Keeps math (`$...$` inline, `$$...$$` display) out of the formatter's way.
//!
//! pulldown-cmark does not know math, so without protection the formatter reads a formula as
//! prose: `*` becomes emphasis, a `- ` line becomes a list item, trailing spaces and blank lines
//! are "cleaned", and wrapping joins or splits the lines of a `$$` block. Instead of teaching every
//! formatting stage about math, the formula text is masked before formatting and put back after:
//!
//! * a display block becomes a fenced code block (`satz-math-<n>` info string) around its exact
//!   lines, which every stage already leaves alone and which stops the surrounding paragraph from
//!   being joined with it;
//! * an inline formula becomes a run of one private-use character as long as the formula, so it
//!   is one unbreakable word of the same width.
//!
//! Anything doubtful (private-use characters already in the text, too many formulas, a stage that
//! changed a mask) means the formatter does not touch the document rather than risk the formula.

use crate::model::ByteRange;
use crate::parser::structure::{StructureOutput, parse_structure};

const PRIVATE_USE_START: u32 = 0xE000;
const PRIVATE_USE_END: u32 = 0xF8FF;
/// More formulas than distinct mask characters: not masked at all.
const MAX_INLINE: usize = (PRIVATE_USE_END - PRIVATE_USE_START) as usize;
const BLOCK_MARK: &str = "satz-math-";

/// What was masked, needed to put it back.
pub struct MathMask {
    /// Original text of each inline formula, indexed by its mask character.
    inline: Vec<String>,
    /// Number of display blocks masked.
    blocks: usize,
}

struct Line {
    start: usize,
    /// End of the text, before the line ending.
    end: usize,
}

/// Masks the math in `source`. `None` when there is nothing to mask or masking is not safe; the
/// caller then formats `source` as it is.
pub fn mask(source: &str) -> Option<(String, MathMask)> {
    if !source.contains('$')
        || source
            .chars()
            .any(|c| (PRIVATE_USE_START..=PRIVATE_USE_END).contains(&(c as u32)))
    {
        return None;
    }
    let structure = parse_structure(source);
    let lines = split_lines(source);
    let protected = protected_lines(&lines, &structure);

    let blocks = find_blocks(source, &lines, &protected);
    let mut in_block = vec![false; lines.len()];
    for &(first, last) in &blocks {
        in_block[first..=last].fill(true);
    }
    let skip: Vec<bool> = (0..lines.len())
        .map(|i| protected[i] || in_block[i])
        .collect();
    let inline = find_inline(source, &lines, &skip, &structure);
    if inline.len() > MAX_INLINE || (blocks.is_empty() && inline.is_empty()) {
        return None;
    }

    // Replacements in source order.
    let mut replacements: Vec<(ByteRange, String)> = Vec::new();
    for (k, &(first, last)) in blocks.iter().enumerate() {
        let range = ByteRange::new(lines[first].start, lines[last].end);
        let original = &source[range.start..range.end];
        let indent: String = original
            .chars()
            .take_while(|c| *c == ' ' || *c == '\t')
            .collect();
        let fence = "~".repeat(longest_run(original, '~').max(2) + 1);
        replacements.push((
            range,
            format!("{indent}{fence}{BLOCK_MARK}{k}\n{original}\n{indent}{fence}"),
        ));
    }
    let mut inline_texts = Vec::with_capacity(inline.len());
    for (k, range) in inline.iter().enumerate() {
        let original = &source[range.start..range.end];
        let mask_char = char::from_u32(PRIVATE_USE_START + k as u32)?;
        replacements.push((
            *range,
            std::iter::repeat_n(mask_char, original.chars().count()).collect(),
        ));
        inline_texts.push(original.to_string());
    }
    replacements.sort_by_key(|(range, _)| range.start);
    let masked = crate::formatter::zones::splice_ranges(source, &replacements);
    Some((
        masked,
        MathMask {
            inline: inline_texts,
            blocks: blocks.len(),
        },
    ))
}

impl MathMask {
    /// Puts the masked math back into the formatted text. `None` when a mask is missing, repeated
    /// or altered, i.e. when the formatted text cannot be trusted.
    pub fn restore(&self, formatted: &str) -> Option<String> {
        let inline_restored = self.restore_inline(formatted)?;
        self.restore_blocks(&inline_restored)
    }

    fn restore_inline(&self, formatted: &str) -> Option<String> {
        let mut out = String::with_capacity(formatted.len());
        let mut seen = vec![false; self.inline.len()];
        let mut chars = formatted.chars().peekable();
        while let Some(c) = chars.next() {
            let code = c as u32;
            if !(PRIVATE_USE_START..=PRIVATE_USE_END).contains(&code) {
                out.push(c);
                continue;
            }
            let k = (code - PRIVATE_USE_START) as usize;
            let mut run = 1;
            while chars.peek() == Some(&c) {
                chars.next();
                run += 1;
            }
            let original = self.inline.get(k)?;
            if seen[k] || run != original.chars().count() {
                return None;
            }
            seen[k] = true;
            out.push_str(original);
        }
        seen.iter().all(|s| *s).then_some(out)
    }

    fn restore_blocks(&self, text: &str) -> Option<String> {
        let lines: Vec<&str> = text.split_inclusive('\n').collect();
        let mut out = String::with_capacity(text.len());
        let mut next_block = 0usize;
        let mut i = 0;
        while i < lines.len() {
            let Some((fence_char, fence_len, index)) = parse_block_open(lines[i]) else {
                out.push_str(lines[i]);
                i += 1;
                continue;
            };
            if index != next_block {
                return None;
            }
            next_block += 1;
            // Content up to the closing fence stays; both fence lines go.
            let close = (i + 1..lines.len())
                .find(|&j| is_closing_fence(lines[j], fence_char, fence_len))?;
            for line in &lines[i + 1..close] {
                out.push_str(line);
            }
            i = close + 1;
        }
        (next_block == self.blocks).then_some(out)
    }
}

/// `(fence character, fence length, block number)` of a masked block's opening fence line.
fn parse_block_open(line: &str) -> Option<(char, usize, usize)> {
    let t = line.trim();
    let fence_char = t.chars().next().filter(|c| *c == '~' || *c == '`')?;
    let len = t.chars().take_while(|c| *c == fence_char).count();
    if len < 3 {
        return None;
    }
    let number = t[len..].strip_prefix(BLOCK_MARK)?;
    (!number.is_empty() && number.bytes().all(|b| b.is_ascii_digit()))
        .then(|| number.parse().ok())
        .flatten()
        .map(|n| (fence_char, len, n))
}

fn is_closing_fence(line: &str, fence_char: char, min_len: usize) -> bool {
    let t = line.trim();
    t.len() >= min_len && t.chars().all(|c| c == fence_char)
}

fn longest_run(text: &str, ch: char) -> usize {
    let mut longest = 0;
    let mut current = 0;
    for c in text.chars() {
        if c == ch {
            current += 1;
            longest = longest.max(current);
        } else {
            current = 0;
        }
    }
    longest
}

fn split_lines(source: &str) -> Vec<Line> {
    let mut lines = Vec::new();
    let mut start = 0;
    for raw in source.split_inclusive('\n') {
        let end = start + raw.trim_end_matches('\n').len();
        lines.push(Line { start, end });
        start += raw.len();
    }
    lines
}

/// Lines that are code, raw HTML or frontmatter: math is not looked for there.
fn protected_lines(lines: &[Line], structure: &StructureOutput) -> Vec<bool> {
    let mut spans: Vec<ByteRange> = structure
        .code_block_spans
        .iter()
        .chain(&structure.html_block_spans)
        .copied()
        .collect();
    spans.extend(structure.frontmatter_range);
    lines
        .iter()
        .map(|line| {
            spans
                .iter()
                .any(|span| span.start <= line.end && span.end > line.start)
        })
        .collect()
}

/// Display blocks as `(first line, last line)`: a line starting with `$$` and no closing `$$` of
/// its own, through the next line that contains `$$`. An unclosed `$$`, a block that reaches into
/// code, or one whose lines are indented less than its opening line is not masked.
fn find_blocks(source: &str, lines: &[Line], protected: &[bool]) -> Vec<(usize, usize)> {
    let indent_of = |line: &Line| {
        source[line.start..line.end]
            .bytes()
            .take_while(|b| *b == b' ' || *b == b'\t')
            .count()
    };
    let mut blocks = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let text = &source[lines[i].start..lines[i].end];
        let trimmed = text.trim_start();
        if protected[i] || !trimmed.starts_with("$$") || trimmed[2..].contains("$$") {
            i += 1;
            continue;
        }
        let indent = indent_of(&lines[i]);
        let mut close = None;
        for j in i + 1..lines.len() {
            if protected[j] {
                break;
            }
            let line_text = &source[lines[j].start..lines[j].end];
            if !line_text.trim().is_empty() && indent_of(&lines[j]) < indent {
                break;
            }
            if line_text.contains("$$") {
                close = Some(j);
                break;
            }
        }
        match close {
            Some(j) => {
                blocks.push((i, j));
                i = j + 1;
            }
            None => i += 1,
        }
    }
    blocks
}

/// Inline formulas (`$...$`, and `$$...$$` on one line) outside the skipped lines and code spans.
/// A `$` opens one when text follows it directly; it closes at a `$` that is not preceded by
/// whitespace and not followed by a digit (so "$5 and $10" is money, not math). Formulas do not
/// span lines, and one containing a `|` inside a table is left alone (a `|` splits table cells).
fn find_inline(
    source: &str,
    lines: &[Line],
    skip: &[bool],
    structure: &StructureOutput,
) -> Vec<ByteRange> {
    let in_code = |at: usize| {
        structure
            .code_spans
            .iter()
            .any(|s| s.start <= at && at < s.end)
    };
    let mut found = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if skip[i] {
            continue;
        }
        let text = &source[line.start..line.end];
        if !text.contains('$') {
            continue;
        }
        let bytes = text.as_bytes();
        let mut at = 0;
        while at < bytes.len() {
            if bytes[at] == b'\\' {
                at += 2; // an escaped character, `\$` included
                continue;
            }
            if bytes[at] != b'$' || in_code(line.start + at) {
                at += 1;
                continue;
            }
            let display = bytes.get(at + 1) == Some(&b'$');
            let open_len = if display { 2 } else { 1 };
            let Some(close) = find_close(text, at + open_len, display, |p| in_code(line.start + p))
            else {
                at += open_len;
                continue;
            };
            let end = close + open_len;
            let range = ByteRange::new(line.start + at, line.start + end);
            let has_pipe_in_table = source[range.start..range.end].contains('|')
                && structure
                    .table_spans
                    .iter()
                    .any(|t| t.start <= range.start && range.end <= t.end);
            if !has_pipe_in_table {
                found.push(range);
            }
            at = end;
        }
    }
    found
}

/// Byte offset (within `text`) of the `$` / `$$` that closes a formula whose content starts at
/// `from`, if any.
fn find_close(
    text: &str,
    from: usize,
    display: bool,
    in_code: impl Fn(usize) -> bool,
) -> Option<usize> {
    let bytes = text.as_bytes();
    if from >= bytes.len() {
        return None;
    }
    let first = text[from..].chars().next()?;
    if first.is_whitespace() || first == '$' {
        return None;
    }
    let mut p = from;
    while p < bytes.len() {
        match bytes[p] {
            b'\\' => p += 2,
            b'$' if !in_code(p) => {
                let closes = if display {
                    bytes.get(p + 1) == Some(&b'$')
                } else if bytes.get(p + 1) == Some(&b'$') || bytes[p - 1] == b'$' {
                    // Part of a `$$` pair: that is display math, never the end of an inline one.
                    p += 1;
                    continue;
                } else {
                    true
                };
                let prev_is_space = text[..p]
                    .chars()
                    .next_back()
                    .is_none_or(char::is_whitespace);
                let after = p + if display { 2 } else { 1 };
                let followed_by_digit = bytes.get(after).is_some_and(u8::is_ascii_digit);
                if closes && p > from && !prev_is_space && (display || !followed_by_digit) {
                    return Some(p);
                }
                if display && closes {
                    p += 2;
                } else {
                    p += 1;
                }
            }
            _ => p += 1,
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_to_mask_without_math_or_with_reserved_characters() {
        assert!(mask("plain text, no dollars\n").is_none());
        assert!(mask("costs $5 and $10\n").is_none(), "money is not math");
        assert!(mask("has \u{E123} and $x$\n").is_none());
        assert!(mask("$$\nunclosed\n").is_none());
    }

    #[test]
    fn masking_then_restoring_gives_the_source_back() {
        for src in [
            "a $x*y$ b\n",
            "$$\n- a\n$$\n",
            "  $$\n  x\n  $$\n",
            "t $a$ and $$b$$ and\n\n$$\nc\n$$\nend\n",
            "$$\nx ~~~ tildes inside\n$$\n",
        ] {
            let (masked, mask) = mask(src).unwrap_or_else(|| panic!("nothing masked: {src:?}"));
            assert_ne!(masked, src);
            assert_eq!(mask.restore(&masked).as_deref(), Some(src), "{src:?}");
        }
    }

    #[test]
    fn a_tilde_run_inside_a_block_gets_a_longer_fence() {
        let (masked, _) = mask("$$\nx ~~~~ inside\n$$\n").unwrap();
        assert!(masked.starts_with("~~~~~satz-math-0\n"), "{masked:?}");
    }

    #[test]
    fn a_mask_that_was_changed_repeated_or_lost_is_refused() {
        let (masked, mask) = mask("a $x$ b\n\n$$\ny\n$$\n").unwrap();
        // Lost inline mask.
        assert!(mask.restore(&masked.replace('\u{E000}', "")).is_none());
        // Inline mask with a different length (a stage split or grew it).
        assert!(
            mask.restore(&masked.replace('\u{E000}', "\u{E000}\u{E000}"))
                .is_none()
        );
        // Repeated inline mask.
        assert!(
            mask.restore(&format!("{masked}\u{E000}\u{E000}\u{E000}"))
                .is_none()
        );
        // Missing closing fence.
        let no_close = masked.replace("\n~~~\n", "\n");
        assert!(mask.restore(&no_close).is_none());
        // Missing opening fence line.
        let no_open: String = masked
            .lines()
            .filter(|l| !l.contains(BLOCK_MARK))
            .map(|l| format!("{l}\n"))
            .collect();
        assert!(mask.restore(&no_open).is_none());
    }

    #[test]
    fn fences_that_another_stage_rewrote_to_backticks_are_still_recognised() {
        let (masked, mask) = mask("$$\n- a\n$$\n").unwrap();
        let rewritten = masked.replace('~', "`");
        assert_eq!(mask.restore(&rewritten).as_deref(), Some("$$\n- a\n$$\n"));
    }
}
