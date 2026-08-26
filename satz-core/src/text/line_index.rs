use crate::model::range::ByteRange;

/// LSP-compliant position (0-indexed line, 0-indexed UTF-16 column code units).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct Position {
    pub line: u32,
    pub character: u32,
}

impl Position {
    #[inline]
    pub const fn new(line: u32, character: u32) -> Self {
        Self { line, character }
    }
}

/// A line-start byte index table for UTF-16 safe position conversions.
///
/// LSP `Position.character` counts UTF-16 code units.
/// `pulldown-cmark` yields byte offsets.
/// Multibyte UTF-8 characters (e.g. Turkish chars, emoji) take 1-4 bytes but 1-2 UTF-16 code units.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LineIndex {
    /// Byte offsets where each line begins. Always starts with 0.
    line_starts: Vec<u32>,
    /// The original source text.
    source: String,
}

impl LineIndex {
    /// Builds a new `LineIndex` from the given source string.
    pub fn new(source: &str) -> Self {
        let mut line_starts = vec![0];
        for (i, b) in source.bytes().enumerate() {
            if b == b'\n' {
                line_starts.push((i + 1) as u32);
            }
        }
        Self {
            line_starts,
            source: source.to_string(),
        }
    }

    /// Total number of lines in the document.
    #[inline]
    pub fn line_count(&self) -> usize {
        self.line_starts.len()
    }

    /// Returns the underlying source text.
    #[inline]
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Converts a byte offset to a (line, UTF-16 column) `Position`.
    pub fn byte_to_position(&self, byte_offset: usize) -> Position {
        if self.source.is_empty() {
            return Position::new(0, 0);
        }

        let clamped_offset = byte_offset.min(self.source.len());
        // Find line index via binary search
        let line_idx = self
            .line_starts
            .partition_point(|&start| (start as usize) <= clamped_offset)
            .saturating_sub(1);

        let line_start = self.line_starts[line_idx] as usize;

        // Ensure we don't slice across a char boundary
        let safe_offset = find_char_boundary(&self.source, clamped_offset);
        let line_slice = &self.source[line_start..safe_offset.max(line_start)];

        let mut utf16_col = 0u32;
        for c in line_slice.chars() {
            // Do not count the newline characters themselves if they fall at the end
            if c == '\n' || c == '\r' {
                continue;
            }
            utf16_col += c.len_utf16() as u32;
        }

        Position::new(line_idx as u32, utf16_col)
    }

    /// Converts a (line, UTF-16 column) `Position` to a byte offset.
    pub fn position_to_byte(&self, pos: Position) -> usize {
        if self.line_starts.is_empty() || self.source.is_empty() {
            return 0;
        }

        let line_idx = (pos.line as usize).min(self.line_starts.len() - 1);
        let line_start = self.line_starts[line_idx] as usize;
        let line_end = self
            .line_starts
            .get(line_idx + 1)
            .map(|&s| s as usize)
            .unwrap_or(self.source.len());

        let line_str = &self.source[line_start..line_end];
        let mut cur_utf16 = 0u32;
        let mut byte_offset_in_line = 0usize;

        for c in line_str.chars() {
            if cur_utf16 >= pos.character {
                break;
            }
            if c == '\n' || c == '\r' {
                break;
            }
            cur_utf16 += c.len_utf16() as u32;
            byte_offset_in_line += c.len_utf8();
        }

        line_start + byte_offset_in_line
    }

    /// Converts a `ByteRange` to a pair of `(Position, Position)`.
    pub fn byte_range_to_positions(&self, range: ByteRange) -> (Position, Position) {
        (
            self.byte_to_position(range.start),
            self.byte_to_position(range.end),
        )
    }
}

/// Returns the nearest char boundary at or before `offset`.
#[inline]
fn find_char_boundary(s: &str, offset: usize) -> usize {
    if offset >= s.len() {
        return s.len();
    }
    let mut i = offset;
    while !s.is_char_boundary(i) && i > 0 {
        i -= 1;
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_string() {
        let index = LineIndex::new("");
        assert_eq!(index.byte_to_position(0), Position::new(0, 0));
        assert_eq!(index.position_to_byte(Position::new(0, 0)), 0);
    }

    #[test]
    fn test_ascii_multiline() {
        let text = "hello\nworld\nfoo";
        let index = LineIndex::new(text);
        assert_eq!(index.line_count(), 3);

        // 'h'
        assert_eq!(index.byte_to_position(0), Position::new(0, 0));
        // 'w' -> line 1, col 0 (offset 6)
        assert_eq!(index.byte_to_position(6), Position::new(1, 0));
        // 'r' -> line 1, col 2 (offset 8)
        assert_eq!(index.byte_to_position(8), Position::new(1, 2));

        assert_eq!(index.position_to_byte(Position::new(1, 2)), 8);
    }

    #[test]
    fn test_turkish_characters() {
        // 'ğ' is 2 bytes UTF-8, 1 code unit in UTF-16
        // 'ş' is 2 bytes UTF-8, 1 code unit in UTF-16
        // 'ı' is 2 bytes UTF-8, 1 code unit in UTF-16
        let text = "ağb\nşık";
        let index = LineIndex::new(text);

        // Line 0: "a" (1B, col 0), "ğ" (2B, col 1), "b" (1B, col 2)
        // Offset of 'b' is 1 + 2 = 3 bytes
        assert_eq!(index.byte_to_position(3), Position::new(0, 2));
        assert_eq!(index.position_to_byte(Position::new(0, 2)), 3);

        // Line 1 start is at byte 5 ("ağb\n" = 1+2+1+1 = 5)
        // "ş" is 2B at offset 5. 'ı' is at offset 7, col 1
        assert_eq!(index.byte_to_position(7), Position::new(1, 1));
        assert_eq!(index.position_to_byte(Position::new(1, 1)), 7);
    }

    #[test]
    fn test_emoji_surrogate_pairs() {
        // '😀' is 4 bytes in UTF-8, 2 code units in UTF-16
        let text = "a😀b";
        let index = LineIndex::new(text);

        // 'a' -> byte 0, col 0
        assert_eq!(index.byte_to_position(0), Position::new(0, 0));
        // '😀' -> byte 1, col 1
        assert_eq!(index.byte_to_position(1), Position::new(0, 1));
        // 'b' -> byte 5 (1 + 4), col 3 (1 + 2)
        assert_eq!(index.byte_to_position(5), Position::new(0, 3));

        assert_eq!(index.position_to_byte(Position::new(0, 3)), 5);
        assert_eq!(index.position_to_byte(Position::new(0, 1)), 1);
    }

    #[test]
    fn test_turkish_plus_emoji_wikilink() {
        // "ığ😀[[x]]"
        // 'ı' (2B, col 0)
        // 'ğ' (2B, col 1)
        // '😀' (4B, col 2..4 -> takes 2 utf16 units)
        // '[' (1B at byte 8, col 4)
        let text = "ığ😀[[x]]";
        let index = LineIndex::new(text);

        let bracket_start = text.find("[[").unwrap();
        assert_eq!(bracket_start, 8);
        assert_eq!(index.byte_to_position(bracket_start), Position::new(0, 4));

        let bracket_end = text.find("]]").unwrap() + 2;
        assert_eq!(bracket_end, 13);
        assert_eq!(index.byte_to_position(bracket_end), Position::new(0, 9));
    }

    #[test]
    fn test_crlf_newlines() {
        let text = "hello\r\nworld\r\n";
        let index = LineIndex::new(text);

        // 'w' is at byte 7
        assert_eq!(index.byte_to_position(7), Position::new(1, 0));
        assert_eq!(index.position_to_byte(Position::new(1, 0)), 7);
    }
}
