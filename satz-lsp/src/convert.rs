use satz_core::{ByteRange, LineIndex, Position as SatzPosition};
use std::path::{Path, PathBuf};
use tower_lsp_server::ls_types as lsp;

/// Converts a `satz_core::Position` to an `lsp_types::Position`.
#[inline]
pub fn satz_pos_to_lsp(pos: SatzPosition) -> lsp::Position {
    lsp::Position {
        line: pos.line,
        character: pos.character,
    }
}

/// Converts an `lsp_types::Position` to a `satz_core::Position`.
#[allow(dead_code)]
#[inline]
pub fn lsp_pos_to_satz(pos: lsp::Position) -> SatzPosition {
    SatzPosition::new(pos.line, pos.character)
}

/// Converts a `ByteRange` into an `lsp_types::Range` using the document's UTF-16 safe `LineIndex`.
pub fn byte_range_to_lsp(range: ByteRange, line_index: &LineIndex) -> lsp::Range {
    let (start, end) = line_index.byte_range_to_positions(range);
    lsp::Range {
        start: satz_pos_to_lsp(start),
        end: satz_pos_to_lsp(end),
    }
}

/// Converts a file URI string into a local filesystem `PathBuf`.
pub fn uri_to_path(uri_str: &str) -> Option<PathBuf> {
    let uri: lsp::Uri = uri_str.parse().ok()?;
    let path = uri.to_file_path().map(|p| p.into_owned())?;
    Some(normalize_windows_drive_root(path))
}

/// A Windows drive-root URI (`file:///m:/`) can round-trip through
/// `Uri::to_file_path()` as the bare drive-relative path `"m:"` (no root
/// separator) instead of the drive root `"m:\"`. Those are different paths
/// on Windows — `"m:"` means "wherever that drive's own current directory
/// happens to be", not its root — so every relative-path computation done
/// against a `vault_root` built from it comes out short one separator (e.g.
/// stripping `"m:"` off `"m:\tlp\1.md"` leaves `"\tlp\1.md"` instead of
/// `"tlp\1.md"`), which then never matches the separator-free relative
/// paths `walk_vault` computes internally — silently splitting every
/// document into two different, colliding `DocId`s. Restore the missing
/// separator whenever `to_file_path()` produced a bare drive prefix.
fn normalize_windows_drive_root(path: PathBuf) -> PathBuf {
    use std::path::Component;
    if path.has_root() || !matches!(path.components().next(), Some(Component::Prefix(_))) {
        return path;
    }
    let mut fixed = path.into_os_string();
    fixed.push(std::path::MAIN_SEPARATOR.to_string());
    PathBuf::from(fixed)
}

/// Converts a filesystem `Path` into an `lsp::Uri`.
#[allow(dead_code)]
pub fn path_to_uri(path: &Path) -> Option<lsp::Uri> {
    lsp::Uri::from_file_path(path)
}

/// Converts line-based diff edits (`satz_core::formatter::diff::line_diff`) into minimal LSP
/// `TextEdit`s against a document's current `LineIndex`, instead of one edit replacing the whole
/// document. `old_start_line`/`old_end_line` are used directly as line numbers with character
/// `0`, except when a `LineEdit` extends past the document's last addressable line (only
/// possible when the document has no trailing newline), in which case the range's end is clamped
/// to the document's actual end position.
pub fn line_edits_to_text_edits(
    line_index: &LineIndex,
    edits: &[satz_core::formatter::diff::LineEdit],
) -> Vec<lsp::TextEdit> {
    let total_lines = line_index.line_count() as u32;
    let doc_end = satz_pos_to_lsp(line_index.byte_to_position(line_index.source().len()));

    edits
        .iter()
        .map(|edit| {
            let start = lsp::Position::new(edit.old_start_line as u32, 0);
            let end = if (edit.old_end_line as u32) < total_lines {
                lsp::Position::new(edit.old_end_line as u32, 0)
            } else {
                doc_end
            };
            lsp::TextEdit {
                range: lsp::Range::new(start, end),
                new_text: edit.new_lines.concat(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pos_conversions() {
        let satz_pos = SatzPosition::new(10, 5);
        let lsp_pos = satz_pos_to_lsp(satz_pos);
        assert_eq!(lsp_pos.line, 10);
        assert_eq!(lsp_pos.character, 5);
        assert_eq!(lsp_pos_to_satz(lsp_pos), satz_pos);
    }

    #[test]
    fn test_line_edits_to_text_edits_middle_of_document() {
        let source = "a\nb\nc\nd\n";
        let line_index = LineIndex::new(source);
        let edits = vec![satz_core::formatter::diff::LineEdit {
            old_start_line: 1,
            old_end_line: 2,
            new_lines: vec!["B\n".to_string()],
        }];

        let text_edits = line_edits_to_text_edits(&line_index, &edits);
        assert_eq!(text_edits.len(), 1);
        assert_eq!(text_edits[0].range.start, lsp::Position::new(1, 0));
        assert_eq!(text_edits[0].range.end, lsp::Position::new(2, 0));
        assert_eq!(text_edits[0].new_text, "B\n");
    }

    #[test]
    fn test_line_edits_to_text_edits_end_of_document_with_trailing_newline() {
        // "a\nb\n" has a trailing newline, so LSP counts a phantom empty final line (line 2) —
        // an edit reaching that line's start is a normal, directly addressable position.
        let source = "a\nb\n";
        let line_index = LineIndex::new(source);
        assert_eq!(line_index.line_count(), 3); // "a", "b", "" (phantom)

        let edits = vec![satz_core::formatter::diff::LineEdit {
            old_start_line: 1,
            old_end_line: 2,
            new_lines: vec!["B\n".to_string()],
        }];
        let text_edits = line_edits_to_text_edits(&line_index, &edits);
        assert_eq!(text_edits[0].range.end, lsp::Position::new(2, 0));
    }

    #[test]
    fn test_line_edits_to_text_edits_end_of_document_without_trailing_newline() {
        // "a\nb" has no trailing newline — there is no line 2 to address, so an edit whose
        // old_end_line reaches 2 must clamp to the actual end-of-document position (line 1,
        // character 1), not an out-of-range Position(2, 0).
        let source = "a\nb";
        let line_index = LineIndex::new(source);
        assert_eq!(line_index.line_count(), 2); // "a", "b" (no phantom line)

        let edits = vec![satz_core::formatter::diff::LineEdit {
            old_start_line: 1,
            old_end_line: 2,
            new_lines: vec!["B".to_string()],
        }];
        let text_edits = line_edits_to_text_edits(&line_index, &edits);
        assert_eq!(text_edits[0].range.start, lsp::Position::new(1, 0));
        assert_eq!(text_edits[0].range.end, lsp::Position::new(1, 1));
    }

    #[cfg(windows)]
    #[test]
    fn test_normalize_windows_drive_root_restores_missing_separator() {
        // "z:" (drive-relative, no root) must become "z:\" (drive root).
        assert!(normalize_windows_drive_root(PathBuf::from("z:")).has_root());
        assert_eq!(
            normalize_windows_drive_root(PathBuf::from("z:")),
            PathBuf::from("z:\\")
        );
    }

    #[cfg(windows)]
    #[test]
    fn test_normalize_windows_drive_root_leaves_well_formed_paths_alone() {
        // Already-rooted paths (the common case, e.g. a subfolder vault root) must be untouched.
        assert_eq!(
            normalize_windows_drive_root(PathBuf::from("C:\\vault")),
            PathBuf::from("C:\\vault")
        );
        assert_eq!(
            normalize_windows_drive_root(PathBuf::from("z:\\")),
            PathBuf::from("z:\\")
        );
    }

    // Not run by default: exercises the full `Uri` parsing round-trip for a
    // specific drive letter ("m:"), which only matters if that letter is
    // ever actually used as a vault root — the real regression coverage is
    // `test_normalize_windows_drive_root_restores_missing_separator` above,
    // which tests the fix directly and unconditionally. Kept here (manual
    // `cargo test -- --ignored`) as an end-to-end sanity check tied to the
    // exact URI that originally triggered this bug.
    #[cfg(windows)]
    #[test]
    #[ignore = "exercises one specific drive letter end-to-end; the real coverage is above"]
    fn test_uri_to_path_drive_root_has_separator() {
        let path = uri_to_path("file:///m:/").expect("should parse a drive-root URI");
        assert!(
            path.has_root(),
            "drive-root URI must produce a rooted path, got {path:?}"
        );
    }

    #[test]
    fn test_byte_range_to_lsp_utf16() {
        // "ağ😀[[x]]" -> 'ğ' is 2B UTF-8 (1 UTF-16), '😀' is 4B UTF-8 (2 UTF-16)
        let text = "ağ😀[[x]]";
        let line_index = LineIndex::new(text);
        let range = ByteRange::new(0, text.len());
        let lsp_range = byte_range_to_lsp(range, &line_index);

        assert_eq!(lsp_range.start, lsp::Position::new(0, 0));
        // total UTF-16 length: 1 ('a') + 1 ('ğ') + 2 ('😀') + 5 ('[[x]]') = 9
        assert_eq!(lsp_range.end, lsp::Position::new(0, 9));
    }
}
