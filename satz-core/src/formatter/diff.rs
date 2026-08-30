use similar::{DiffTag, TextDiff};

/// One contiguous line-range replacement: lines `[old_start_line, old_end_line)` (0-indexed,
/// end-exclusive) of the original text should be replaced with `new_lines`. Every entry of
/// `new_lines` carries its own trailing line terminator exactly as it appeared in the new text,
/// except possibly the very last line of the whole document if it has none — so concatenating
/// `new_lines` reproduces the replacement text byte-for-byte with no re-joining logic needed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineEdit {
    pub old_start_line: usize,
    pub old_end_line: usize,
    pub new_lines: Vec<String>,
}

/// Computes the minimal set of line-range replacements that turn `old` into `new`, via a
/// line-based diff (`similar::TextDiff::from_lines`). Unchanged regions never appear in the
/// result — each returned `LineEdit` corresponds to one contiguous run of added/removed/changed
/// lines, so a document with several scattered changes yields several small edits rather than
/// one edit spanning the whole file.
///
/// Line indices are 0-indexed and line-boundary-aligned the same way `LineIndex` counts LSP
/// lines (both split immediately after every `\n`), so callers needing LSP `Position`s can use
/// `old_start_line`/`old_end_line` directly as line numbers with character `0` — except when
/// `old_end_line` reaches past the last real line (only possible when `old` has no trailing
/// newline), in which case the caller should clamp to the document's actual end position.
pub fn line_diff(old: &str, new: &str) -> Vec<LineEdit> {
    let diff = TextDiff::from_lines(old, new);
    let new_slices = diff.new_slices();

    diff.ops()
        .iter()
        .filter(|op| op.tag() != DiffTag::Equal)
        .map(|op| {
            let old_range = op.old_range();
            let new_range = op.new_range();
            LineEdit {
                old_start_line: old_range.start,
                old_end_line: old_range.end,
                new_lines: new_slices[new_range]
                    .iter()
                    .map(|s| s.to_string())
                    .collect(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_identical_text_produces_no_edits() {
        let text = "a\nb\nc\n";
        assert!(line_diff(text, text).is_empty());
    }

    #[test]
    fn test_single_line_change_produces_one_edit() {
        let old = "a\nb\nc\n";
        let new = "a\nB\nc\n";
        let edits = line_diff(old, new);
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].old_start_line, 1);
        assert_eq!(edits[0].old_end_line, 2);
        assert_eq!(edits[0].new_lines, vec!["B\n".to_string()]);
    }

    #[test]
    fn test_scattered_changes_produce_separate_edits_not_one_blob() {
        let old = "1\n2\n3\n4\n5\n6\n7\n";
        let new = "1\nTWO\n3\n4\nFIVE\n6\n7\n";
        let edits = line_diff(old, new);
        assert_eq!(
            edits.len(),
            2,
            "two disjoint changed regions must yield two separate edits, not one spanning both"
        );

        assert_eq!(edits[0].old_start_line, 1);
        assert_eq!(edits[0].old_end_line, 2);
        assert_eq!(edits[0].new_lines, vec!["TWO\n".to_string()]);

        assert_eq!(edits[1].old_start_line, 4);
        assert_eq!(edits[1].old_end_line, 5);
        assert_eq!(edits[1].new_lines, vec!["FIVE\n".to_string()]);
    }

    #[test]
    fn test_reconstruction_via_concatenation_matches_new_text() {
        let old = "alpha\nbeta\ngamma\ndelta\n";
        let new = "alpha\nBETA\ngamma\nDELTA\nepsilon\n";
        let edits = line_diff(old, new);

        let old_lines: Vec<&str> = old.split_inclusive('\n').collect();
        let mut rebuilt = String::new();
        let mut cursor = 0usize;
        for edit in &edits {
            for line in &old_lines[cursor..edit.old_start_line] {
                rebuilt.push_str(line);
            }
            for line in &edit.new_lines {
                rebuilt.push_str(line);
            }
            cursor = edit.old_end_line;
        }
        for line in &old_lines[cursor..] {
            rebuilt.push_str(line);
        }

        assert_eq!(rebuilt, new);
    }

    #[test]
    fn test_appended_line_at_end_without_trailing_newline() {
        let old = "a\nb\n";
        let new = "a\nb\nc";
        let edits = line_diff(old, new);
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].old_start_line, 2);
        assert_eq!(edits[0].old_end_line, 2);
        assert_eq!(edits[0].new_lines, vec!["c".to_string()]);
    }

    #[test]
    fn test_completely_different_content_is_one_replace_edit() {
        let old = "old content\nsecond line\n";
        let new = "brand new content\n";
        let edits = line_diff(old, new);
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].old_start_line, 0);
        assert_eq!(edits[0].old_end_line, 2);
        assert_eq!(edits[0].new_lines, vec!["brand new content\n".to_string()]);
    }
}
