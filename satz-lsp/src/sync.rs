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

    for ch in line_slice.chars() {
        if current_utf16 >= target_utf16 {
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
}
