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

pub(super) struct Line {
    pub(super) start: usize,
    /// End of the text, before the line ending.
    pub(super) end: usize,
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
    let code_spans = SpanSet::new(&structure.code_spans);
    let inline = find_inline(source, &lines, &skip, &structure, &|at| {
        code_spans.contains(at)
    });
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

pub(super) fn split_lines(source: &str) -> Vec<Line> {
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
pub(super) fn protected_lines(lines: &[Line], structure: &StructureOutput) -> Vec<bool> {
    let mut spans: Vec<ByteRange> = structure
        .code_block_spans
        .iter()
        .chain(&structure.html_block_spans)
        .copied()
        .collect();
    spans.extend(structure.frontmatter_range);
    lines_touching(lines, &spans)
}

/// Which lines touch at least one of `spans` (`span.start <= line.end && span.end > line.start`),
/// in any order of the spans.
///
/// The lines are in order and do not overlap, so the lines one span touches are a run of
/// neighbours: found by two binary searches and counted in a difference array. That is
/// `O(lines + spans * log lines)`; asking every line about every span is what made a note with
/// many code blocks take time in proportion to the square of its size.
pub(super) fn lines_touching(lines: &[Line], spans: &[ByteRange]) -> Vec<bool> {
    let mut opened = vec![0i32; lines.len() + 1];
    for span in spans {
        let first = lines.partition_point(|line| line.end < span.start);
        let after_last = lines.partition_point(|line| line.start < span.end);
        if first < after_last {
            opened[first] += 1;
            opened[after_last] -= 1;
        }
    }
    let mut open = 0;
    opened[..lines.len()]
        .iter()
        .map(|change| {
            open += change;
            open > 0
        })
        .collect()
}

/// Whether a byte offset lies inside one of a set of spans (`start <= at < end`), answered by a
/// binary search instead of a look at every span.
struct SpanSet {
    /// Start of every span, ascending.
    starts: Vec<usize>,
    /// For each start: the furthest end among the spans that start there or earlier. Spans that
    /// overlap or nest are no problem.
    reach: Vec<usize>,
}

impl SpanSet {
    fn new(spans: &[ByteRange]) -> Self {
        let mut sorted: Vec<ByteRange> = spans.to_vec();
        sorted.sort_by_key(|span| span.start);
        let mut furthest = 0;
        let mut reach = Vec::with_capacity(sorted.len());
        for span in &sorted {
            furthest = furthest.max(span.end);
            reach.push(furthest);
        }
        Self {
            starts: sorted.iter().map(|span| span.start).collect(),
            reach,
        }
    }

    fn contains(&self, at: usize) -> bool {
        let started = self.starts.partition_point(|&start| start <= at);
        started > 0 && self.reach[started - 1] > at
    }
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
    in_code: &dyn Fn(usize) -> bool,
) -> Vec<ByteRange> {
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

    // ---- the fast lookups give what looking at everything gave ----

    /// Small xorshift generator: fixed seeds, no dependency.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }

        fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }
    }

    /// The straightforward definition: every line asked about every span.
    fn naive_lines_touching(lines: &[Line], spans: &[ByteRange]) -> Vec<bool> {
        lines
            .iter()
            .map(|line| {
                spans
                    .iter()
                    .any(|span| span.start <= line.end && span.end > line.start)
            })
            .collect()
    }

    fn naive_contains(spans: &[ByteRange], at: usize) -> bool {
        spans.iter().any(|s| s.start <= at && at < s.end)
    }

    fn random_spans(rng: &mut Rng, upto: usize) -> Vec<ByteRange> {
        (0..rng.below(14))
            .map(|_| {
                let start = rng.below(upto + 4);
                // Empty spans, spans that nest or overlap, spans past the end of the text.
                let end = if rng.below(5) == 0 {
                    start
                } else {
                    start + rng.below(upto / 2 + 3)
                };
                ByteRange::new(start, end)
            })
            .collect()
    }

    #[test]
    fn the_lines_a_span_touches_are_the_same_as_when_every_line_asks_every_span() {
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        for case in 0..4000 {
            // Text of short lines, empty ones included, with or without a last line ending.
            let mut text = String::new();
            for _ in 0..rng.below(30) {
                text.push_str(&"x".repeat(rng.below(7)));
                text.push('\n');
            }
            if rng.below(3) == 0 {
                text.push_str("tail");
            }
            let lines = split_lines(&text);
            let spans = random_spans(&mut rng, text.len());
            assert_eq!(
                lines_touching(&lines, &spans),
                naive_lines_touching(&lines, &spans),
                "case {case}: text {text:?}, spans {spans:?}"
            );
        }
        // Nothing at all.
        assert_eq!(
            lines_touching(&[], &[ByteRange::new(0, 5)]),
            Vec::<bool>::new()
        );
        assert_eq!(
            lines_touching(&split_lines("a\nb\n"), &[]),
            vec![false, false]
        );
    }

    #[test]
    fn a_span_set_answers_as_when_every_span_is_looked_at() {
        let mut rng = Rng(0x0123_4567_89AB_CDEF);
        for case in 0..3000 {
            let upto = rng.below(60);
            let spans = random_spans(&mut rng, upto);
            let set = SpanSet::new(&spans);
            for at in 0..upto + 12 {
                assert_eq!(
                    set.contains(at),
                    naive_contains(&spans, at),
                    "case {case}: at {at}, spans {spans:?}"
                );
            }
        }
        assert!(!SpanSet::new(&[]).contains(0));
    }

    #[test]
    fn masking_looks_up_code_the_same_way_on_documents_of_every_shape() {
        // Pieces of a note: code blocks of both kinds, HTML, frontmatter, tables, quotes, lists,
        // money and formulas -- in random order, so blocks open and never close, sit in quotes,
        // and meet the `$` signs in every way.
        const PIECES: &[&str] = &[
            "plain text",
            "a $x$ b",
            "cost $5 and $10",
            "`code $y$`",
            "```",
            "code line",
            "~~~",
            "$$",
            "$$x$$",
            "  $$",
            "<div>",
            "</div>",
            "---",
            "title: t",
            "| a | $b|c$ |",
            "|---|---|",
            "> quote $q$",
            "- item `c` $i$",
            "    indented code",
            "",
            "",
            "\\$ escaped $",
            "$$ a $$ and $b$",
            "text `a` and `b` $c$ `d`",
        ];
        let mut rng = Rng(0xDEAD_BEEF_0BAD_F00D);
        for case in 0..600 {
            let mut lines: Vec<&str> = Vec::new();
            if rng.below(4) == 0 {
                lines.extend(["---", "title: t", "---"]);
            }
            for _ in 0..3 + rng.below(30) {
                lines.push(PIECES[rng.below(PIECES.len())]);
            }
            let mut source = lines.join("\n");
            if rng.below(2) == 0 {
                source.push('\n');
            }

            let structure = parse_structure(&source);
            let lines = split_lines(&source);
            let mut spans: Vec<ByteRange> = structure
                .code_block_spans
                .iter()
                .chain(&structure.html_block_spans)
                .copied()
                .collect();
            spans.extend(structure.frontmatter_range);
            let protected = protected_lines(&lines, &structure);
            assert_eq!(
                protected,
                naive_lines_touching(&lines, &spans),
                "case {case}: {source:?}"
            );

            let set = SpanSet::new(&structure.code_spans);
            let fast = find_inline(&source, &lines, &protected, &structure, &|at| {
                set.contains(at)
            });
            let slow = find_inline(&source, &lines, &protected, &structure, &|at| {
                naive_contains(&structure.code_spans, at)
            });
            assert_eq!(fast, slow, "case {case}: {source:?}");

            // And the whole thing still puts the formulas back where they were. (Only for a text
            // that ends in a line ending: restoring a block that ends the text gives it one.)
            if !source.ends_with('\n') {
                continue;
            }
            if let Some((masked, mask)) = mask(&source) {
                assert_eq!(
                    mask.restore(&masked).as_deref(),
                    Some(source.as_str()),
                    "case {case}"
                );
            }
        }
    }

    #[test]
    fn doubling_the_document_does_not_quadruple_the_masking() {
        // Not a time limit: the ratio of two sizes of the same kind of note, each the best of
        // several runs, and it passes if any of three attempts comes out under the limit. Noise
        // makes single times jump about, it does not make a ratio of best times stay high three
        // times running; a cost in proportion to the square of the size (a ratio near 4) cannot
        // pass at all.
        fn best(doc: &str) -> Duration {
            (0..7)
                .map(|_| timed(|| mask(doc)))
                .min()
                .expect("seven runs")
        }
        for (what, small, large) in [
            (
                "code blocks and one `$5`",
                code_heavy(3000),
                code_heavy(6000),
            ),
            (
                "inline math next to inline code",
                math_and_code_inline(6000),
                math_and_code_inline(12000),
            ),
        ] {
            let ratios: Vec<f64> = (0..3)
                .map(|_| ms(best(&large)) / ms(best(&small)))
                .collect();
            assert!(
                ratios.iter().any(|ratio| *ratio < 3.2),
                "{what}: twice the text took {ratios:?} times as long (linear: about 2, quadratic: about 4)"
            );
        }
    }

    // ---- how the cost of masking grows with the size of the document (manual) ----
    //
    // Run with:  cargo test --release -p satz-core --lib -- --ignored --nocapture math_scaling_probe
    //
    // A single timing on a busy machine says little, so nothing here is an absolute number the
    // result depends on: every round times all sizes back to back (in a rotating order), the best
    // of many rounds is kept (noise only adds time), and each size is also compared with
    // `parse_structure` of the same text, timed at the same moment. That part of `mask` is linear
    // and unavoidable, so `mask / parse_structure` is constant for linear code and grows with the
    // size for quadratic code, whatever else the machine is doing. The exponent is
    // `log2(time(2n) / time(n))`: about 1 for linear code, about 2 for quadratic code.

    use std::time::{Duration, Instant};

    fn timed<T>(f: impl FnOnce() -> T) -> Duration {
        let started = Instant::now();
        std::hint::black_box(f());
        started.elapsed()
    }

    /// Best time of each measured thing over `rounds` rounds; `things[k]` is measured for every
    /// size, sizes in a different order each round.
    fn best_times(
        sizes: &[usize],
        rounds: usize,
        kinds: usize,
        measure: &dyn Fn(usize, usize) -> Duration,
    ) -> Vec<Vec<Duration>> {
        let mut best = vec![vec![Duration::MAX; kinds]; sizes.len()];
        for round in 0..rounds {
            for step in 0..sizes.len() {
                let i = (step + round) % sizes.len();
                for (k, slot) in best[i].iter_mut().enumerate() {
                    *slot = (*slot).min(measure(i, k));
                }
            }
        }
        best
    }

    fn ms(d: Duration) -> f64 {
        d.as_secs_f64() * 1000.0
    }

    /// n fenced code blocks (with one inline code span each), the way a code-heavy note looks, and
    /// a single `$5` on top: `mask` runs, finds no math, and still has to look at every line.
    fn code_heavy(n: usize) -> String {
        format!(
            "costs $5 and more\n\n{}",
            "text `code` here\n\n```\ncode\n```\n\n".repeat(n)
        )
    }

    /// A note with a lot of inline math and inline code side by side.
    fn math_and_code_inline(n: usize) -> String {
        "`c` and $x$ y\n".repeat(n)
    }

    fn report(title: &str, sizes: &[usize], docs: &[String], best: &[Vec<Duration>]) {
        println!("\n{title}");
        println!(
            "{:>8} {:>10} {:>11} {:>11} {:>13} {:>9}",
            "n", "bytes", "mask ms", "parse ms", "mask/parse", "exponent"
        );
        for i in 0..sizes.len() {
            let exponent = if i == 0 {
                String::from("-")
            } else {
                format!(
                    "{:.2}",
                    (ms(best[i][0]) / ms(best[i - 1][0])).log2()
                        / (sizes[i] as f64 / sizes[i - 1] as f64).log2()
                )
            };
            println!(
                "{:>8} {:>10} {:>11.2} {:>11.2} {:>13.2} {:>9}",
                sizes[i],
                docs[i].len(),
                ms(best[i][0]),
                ms(best[i][1]),
                ms(best[i][0]) / ms(best[i][1]),
                exponent
            );
        }
    }

    #[test]
    #[ignore = "manual measurement, see the comment above"]
    fn math_scaling_probe() {
        // 1. Code blocks and a single `$`: the case of the audit report.
        let sizes = [500usize, 1000, 2000, 4000, 8000];
        let docs: Vec<String> = sizes.iter().map(|&n| code_heavy(n)).collect();
        let best = best_times(&sizes, 15, 2, &|i, k| {
            if k == 0 {
                timed(|| mask(&docs[i]))
            } else {
                timed(|| parse_structure(&docs[i]))
            }
        });
        report("code blocks + one `$5` (mask alone)", &sizes, &docs, &best);

        // 2. What drives it: the number of lines is fixed, the number of code blocks changes.
        println!("\nfixed 24000 lines, a changing number of code blocks (mask alone)");
        println!("{:>10} {:>11} {:>11}", "blocks", "mask ms", "parse ms");
        let block_counts = [0usize, 500, 1000, 2000, 4000];
        let docs: Vec<String> = block_counts
            .iter()
            .map(|&s| {
                let mut text = String::from("costs $5 and more\n\n");
                text.push_str(&"```\ncode\n```\n".repeat(s));
                let so_far = text.lines().count();
                text.push_str(&"plain line\n".repeat(24_000 - so_far));
                text
            })
            .collect();
        let best = best_times(&block_counts, 15, 2, &|i, k| {
            if k == 0 {
                timed(|| mask(&docs[i]))
            } else {
                timed(|| parse_structure(&docs[i]))
            }
        });
        for i in 0..block_counts.len() {
            println!(
                "{:>10} {:>11.2} {:>11.2}",
                block_counts[i],
                ms(best[i][0]),
                ms(best[i][1])
            );
        }

        // 3. Much inline math next to much inline code (a second suspect: every `$` looks through
        //    every code span).
        let sizes = [2000usize, 4000, 8000, 16000, 32000];
        let docs: Vec<String> = sizes.iter().map(|&n| math_and_code_inline(n)).collect();
        let best = best_times(&sizes, 15, 2, &|i, k| {
            if k == 0 {
                timed(|| mask(&docs[i]))
            } else {
                timed(|| parse_structure(&docs[i]))
            }
        });
        report(
            "inline math + inline code on every line (mask alone)",
            &sizes,
            &docs,
            &best,
        );

        // 4. The whole formatter on about 1 MB, with and without the single `$5`.
        let config = crate::config::FormatterConfig::default();
        let with_dollar = code_heavy(18_000);
        let without_dollar = with_dollar.replacen('$', "", 1);
        let mut best = [Duration::MAX; 2];
        for _ in 0..5 {
            best[0] = best[0].min(timed(|| {
                crate::formatter::format_document(&without_dollar, &config)
            }));
            best[1] = best[1].min(timed(|| {
                crate::formatter::format_document(&with_dollar, &config)
            }));
        }
        println!(
            "\nformat_document on {} bytes: without `$` {:.1} ms, with one `$5` {:.1} ms ({:.1}x)",
            with_dollar.len(),
            ms(best[0]),
            ms(best[1]),
            ms(best[1]) / ms(best[0])
        );
    }
}
