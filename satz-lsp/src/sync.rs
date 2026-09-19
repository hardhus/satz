use ropey::Rope;
use tower_lsp_server::ls_types::{Position, TextDocumentContentChangeEvent};

/// Converts an LSP UTF-16 Position `(line, character)` to a `Rope` char index.
pub fn lsp_pos_to_rope_char(rope: &Rope, pos: Position) -> usize {
    if rope.len_chars() == 0 {
        return 0;
    }

    let total_lines = rope.len_lines();
    if (pos.line as usize) >= total_lines {
        return rope.len_chars();
    }

    let line_idx = pos.line as usize;
    let line_start_char = rope.line_to_char(line_idx);
    let line_slice = rope.line(line_idx);

    let target_utf16 = pos.character as usize;
    let mut current_utf16 = 0;
    let mut char_offset = 0;

    // The line terminator is not part of the line: a column past the end means its end.
    for ch in line_slice.chars() {
        if current_utf16 >= target_utf16 || ch == '\n' || ch == '\r' {
            break;
        }
        current_utf16 += ch.len_utf16();
        char_offset += 1;
    }

    (line_start_char + char_offset).min(rope.len_chars())
}

/// Applies a sequence of LSP `TextDocumentContentChangeEvent`s incrementally to a `Rope`.
pub fn apply_changes_to_rope(
    rope: &mut Rope,
    changes: impl IntoIterator<Item = TextDocumentContentChangeEvent>,
) {
    for change in changes {
        if let Some(range) = change.range {
            let start_char = lsp_pos_to_rope_char(rope, range.start);
            let end_char = lsp_pos_to_rope_char(rope, range.end);
            let start = start_char.min(rope.len_chars());
            let end = end_char.min(rope.len_chars()).max(start);

            rope.remove(start..end);
            if !change.text.is_empty() {
                rope.insert(start, &change.text);
            }
        } else {
            *rope = Rope::from_str(&change.text);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tower_lsp_server::ls_types::Range;

    #[test]
    fn test_full_content_sync() {
        let mut rope = Rope::from_str("Initial content");
        let change = TextDocumentContentChangeEvent {
            range: None,
            range_length: None,
            text: "Replaced content".to_string(),
        };

        apply_changes_to_rope(&mut rope, vec![change]);
        assert_eq!(rope.to_string(), "Replaced content");
    }

    #[test]
    fn test_incremental_insertion_and_deletion() {
        let mut rope = Rope::from_str("Hello World");

        // Insert " beautiful" after "Hello"
        let insert_change = TextDocumentContentChangeEvent {
            range: Some(Range {
                start: Position::new(0, 5),
                end: Position::new(0, 5),
            }),
            range_length: None,
            text: " beautiful".to_string(),
        };
        apply_changes_to_rope(&mut rope, vec![insert_change]);
        assert_eq!(rope.to_string(), "Hello beautiful World");

        // Delete " beautiful"
        let delete_change = TextDocumentContentChangeEvent {
            range: Some(Range {
                start: Position::new(0, 5),
                end: Position::new(0, 15),
            }),
            range_length: None,
            text: String::new(),
        };
        apply_changes_to_rope(&mut rope, vec![delete_change]);
        assert_eq!(rope.to_string(), "Hello World");
    }

    #[test]
    fn test_incremental_multiline_and_emoji_utf16() {
        // "Line 1: 🦀\nLine 2: Türkçe"
        // 🦀 is 2 UTF-16 code units (surrogate pair)
        let mut rope = Rope::from_str("Line 1: 🦀\nLine 2: Türkçe");

        // Replace "🦀" (from UTF-16 col 8 to col 10) with "Rust"
        let emoji_replace = TextDocumentContentChangeEvent {
            range: Some(Range {
                start: Position::new(0, 8),
                end: Position::new(0, 10),
            }),
            range_length: None,
            text: "Rust".to_string(),
        };
        apply_changes_to_rope(&mut rope, vec![emoji_replace]);
        assert_eq!(rope.to_string(), "Line 1: Rust\nLine 2: Türkçe");

        // Append newline and line 3
        let append_change = TextDocumentContentChangeEvent {
            range: Some(Range {
                start: Position::new(1, 14),
                end: Position::new(1, 14),
            }),
            range_length: None,
            text: "\nLine 3".to_string(),
        };
        apply_changes_to_rope(&mut rope, vec![append_change]);
        assert_eq!(rope.to_string(), "Line 1: Rust\nLine 2: Türkçe\nLine 3");
    }

    fn edit(text: &str, start: (u32, u32), end: (u32, u32), new: &str) -> String {
        let mut rope = Rope::from_str(text);
        let change = TextDocumentContentChangeEvent {
            range: Some(Range {
                start: Position::new(start.0, start.1),
                end: Position::new(end.0, end.1),
            }),
            range_length: None,
            text: new.to_string(),
        };
        apply_changes_to_rope(&mut rope, vec![change]);
        rope.to_string()
    }

    fn insert(text: &str, at: (u32, u32), new: &str) -> String {
        edit(text, at, at, new)
    }

    // ---- a column past the end of a line means the end of THAT line ----

    #[test]
    fn a_column_past_the_line_end_clamps_to_the_end_of_that_line() {
        assert_eq!(insert("ab\ncd", (0, 10), "X"), "abX\ncd");
        assert_eq!(insert("ab\r\ncd", (0, 10), "X"), "abX\r\ncd");
        assert_eq!(insert("ab\rcd", (0, 10), "X"), "abX\rcd");
        assert_eq!(insert("ab\ncd", (1, 10), "X"), "ab\ncdX");
        assert_eq!(insert("\n\nx", (0, 5), "X"), "X\n\nx");
        assert_eq!(insert("a\n\nb", (1, 3), "X"), "a\nX\nb");
        // Exactly at the end is not "past" it.
        assert_eq!(insert("ab\ncd", (0, 2), "X"), "abX\ncd");
    }

    #[test]
    fn past_the_last_line_means_the_end_of_the_document() {
        assert_eq!(insert("ab\ncd", (9, 0), "X"), "ab\ncdX");
        assert_eq!(insert("ab\ncd\n", (9, 0), "X"), "ab\ncd\nX");
        assert_eq!(insert("", (3, 3), "X"), "X");
    }

    #[test]
    fn a_range_with_both_ends_out_of_bounds_edits_the_right_text() {
        // From column 5 of line 0 (clamped to its end) through line 1 (clamped): removes "\ncd".
        assert_eq!(edit("ab\ncd\nef", (0, 5), (1, 99), ""), "ab\nef");
        assert_eq!(edit("ab\ncd\nef", (0, 1), (0, 99), "-"), "a-\ncd\nef");
    }

    #[test]
    fn columns_inside_surrogate_pairs_and_wide_lines_still_work() {
        // A column in the middle of a surrogate pair rounds forward past the whole character.
        assert_eq!(insert("a🦀b\nc", (0, 2), "X"), "a🦀Xb\nc");
        assert_eq!(insert("a🦀b\nc", (0, 4), "X"), "a🦀bX\nc");
        assert_eq!(insert("a🦀b\nc", (0, 40), "X"), "a🦀bX\nc");
    }

    // ---- only \n, \r\n and \r end a line (the LSP definition) ----

    #[test]
    fn unicode_line_separators_are_not_lines() {
        for sep in ['\u{000B}', '\u{000C}', '\u{0085}', '\u{2028}', '\u{2029}'] {
            let text = format!("a{sep}b\nc");
            assert_eq!(
                insert(&text, (1, 0), "X"),
                format!("a{sep}b\nXc"),
                "U+{:04X}",
                sep as u32
            );
            // A column after the separator is on the same line.
            assert_eq!(
                insert(&text, (0, 3), "X"),
                format!("a{sep}bX\nc"),
                "U+{:04X}",
                sep as u32
            );
        }
    }

    #[test]
    fn a_lone_carriage_return_is_a_line_break() {
        assert_eq!(insert("a\rb", (1, 0), "X"), "a\rXb");
        assert_eq!(insert("a\r\nb\rc", (2, 0), "X"), "a\r\nb\rXc");
    }

    fn change(
        range: Option<((u32, u32), (u32, u32))>,
        text: &str,
    ) -> TextDocumentContentChangeEvent {
        TextDocumentContentChangeEvent {
            range: range.map(|(s, e)| Range {
                start: Position::new(s.0, s.1),
                end: Position::new(e.0, e.1),
            }),
            range_length: None,
            text: text.to_string(),
        }
    }

    #[test]
    fn a_stale_version_is_not_applied() {
        let mut doc = crate::state::OpenDocument::new("u", "a.md".into(), "abc", 5);
        assert!(!doc.apply_change_events(4, vec![change(Some(((0, 0), (0, 0))), "X")]));
        assert_eq!(doc.rope.to_string(), "abc");
        assert_eq!(doc.version, 5);
    }

    #[test]
    fn the_same_or_a_newer_version_is_applied_in_order() {
        let mut doc = crate::state::OpenDocument::new("u", "a.md".into(), "abc", 5);
        assert!(doc.apply_change_events(
            6,
            vec![
                change(Some(((0, 0), (0, 0))), "X"),
                change(Some(((0, 4), (0, 4))), "Y"),
            ],
        ));
        assert_eq!(doc.rope.to_string(), "XabcY");
        assert_eq!(doc.version, 6);
        // Same version: still applied (some clients re-send the version).
        assert!(doc.apply_change_events(6, vec![change(None, "full")]));
        assert_eq!(doc.rope.to_string(), "full");
        assert_eq!(doc.version, 6);
    }
}
