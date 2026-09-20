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
///
/// Only `file:` URIs name local files: `untitled:` (an unsaved buffer), `https:` and the like give
/// `None` instead of a bogus relative path. On Windows a file URI with a host is a network share
/// (`file://server/share/x.md` -> `\\server\share\x.md`); the host must not be dropped.
pub fn uri_to_path(uri_str: &str) -> Option<PathBuf> {
    let uri: lsp::Uri = uri_str.parse().ok()?;
    if !uri.scheme().as_str().eq_ignore_ascii_case("file") {
        return None;
    }
    let host = uri
        .authority()
        .map(|a| a.host().to_string())
        .filter(|h| !h.is_empty() && !h.eq_ignore_ascii_case("localhost"));
    let path = uri.to_file_path().map(|p| p.into_owned())?;
    if let Some(host) = host {
        // A remote host is only reachable as a UNC path, and only on Windows.
        if !cfg!(windows) {
            return None;
        }
        let share_path = path.to_string_lossy().replace('/', "\\");
        return Some(PathBuf::from(format!("\\\\{host}{share_path}")));
    }
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

/// Converts a filesystem `Path` into an `lsp::Uri`. A Windows network path
/// (`\\server\share\x.md`) becomes `file://server/share/x.md`.
pub fn path_to_uri(path: &Path) -> Option<lsp::Uri> {
    let text = path.to_string_lossy();
    if let Some(unc) = unc_parts(&text) {
        let encoded: Vec<String> = unc
            .1
            .iter()
            .map(|segment| percent_encode(segment))
            .collect();
        return format!("file://{}/{}", percent_encode(&unc.0), encoded.join("/"))
            .parse()
            .ok();
    }
    lsp::Uri::from_file_path(path)
}

/// `(server, path segments)` of a Windows network path (`\\server\share\dir\x.md`, also the
/// `\\?\UNC\server\share\...` spelling), `None` for any other path.
fn unc_parts(path: &str) -> Option<(String, Vec<String>)> {
    let rest = path
        .strip_prefix("\\\\?\\UNC\\")
        .or_else(|| path.strip_prefix("\\\\").filter(|r| !r.starts_with('?')))?;
    let mut parts = rest.split(['\\', '/']).filter(|s| !s.is_empty());
    let server = parts.next()?.to_string();
    Some((server, parts.map(str::to_string).collect()))
}

/// Percent-encodes everything but URI path characters.
fn percent_encode(segment: &str) -> String {
    let mut out = String::with_capacity(segment.len());
    for byte in segment.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'@' | b':' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
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

/// Test helper: applies LSP `TextEdit`s (all expressed against `text`, as the protocol requires)
/// and returns the resulting text, so tests can assert on what the user would actually end up
/// with instead of only on the edits' `new_text`.
#[cfg(test)]
pub fn apply_text_edits(text: &str, edits: &[lsp::TextEdit]) -> String {
    let line_index = LineIndex::new(text);
    let mut spans: Vec<(usize, usize, &str)> = edits
        .iter()
        .map(|e| {
            (
                line_index.position_to_byte(lsp_pos_to_satz(e.range.start)),
                line_index.position_to_byte(lsp_pos_to_satz(e.range.end)),
                e.new_text.as_str(),
            )
        })
        .collect();
    spans.sort_by_key(|(start, end, _)| (*start, *end));
    let mut out = String::new();
    let mut cursor = 0;
    for (start, end, new_text) in spans {
        assert!(start >= cursor, "overlapping edits: {edits:?}");
        out.push_str(&text[cursor..start]);
        out.push_str(new_text);
        cursor = end;
    }
    out.push_str(&text[cursor..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_text_edits_applies_several_edits_against_the_original_text() {
        let text = "a\nb\nc\n";
        let edits = vec![
            lsp::TextEdit::new(
                lsp::Range::new(lsp::Position::new(0, 0), lsp::Position::new(1, 0)),
                "A\n".to_string(),
            ),
            lsp::TextEdit::new(
                lsp::Range::new(lsp::Position::new(2, 0), lsp::Position::new(2, 1)),
                "CC".to_string(),
            ),
        ];
        assert_eq!(apply_text_edits(text, &edits), "A\nb\nCC\n");
        assert_eq!(apply_text_edits(text, &[]), text);
    }

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

    // ---- file URIs as clients spell them ----

    #[test]
    fn a_plain_file_uri_becomes_a_path_and_back() {
        let p = uri_to_path("file:///home/user/notes/a.md").unwrap();
        assert!(
            p.ends_with("notes/a.md") || p.ends_with("notes\\a.md"),
            "{p:?}"
        );
        let with_space = uri_to_path("file:///home/user/my%20notes/a%20b.md").unwrap();
        assert!(
            with_space.to_string_lossy().contains("my notes"),
            "{with_space:?}"
        );
        assert!(
            with_space.to_string_lossy().contains("a b.md"),
            "{with_space:?}"
        );
        let turkish = uri_to_path("file:///home/user/%C4%B1%C5%9F%C4%B1k/g%C3%BCn.md").unwrap();
        assert!(turkish.to_string_lossy().contains("ışık"), "{turkish:?}");
        assert!(turkish.to_string_lossy().contains("gün.md"), "{turkish:?}");
        let emoji = uri_to_path("file:///home/user/%F0%9F%A6%80/x.md").unwrap();
        assert!(emoji.to_string_lossy().contains('🦀'), "{emoji:?}");
    }

    #[test]
    fn schemes_that_are_not_local_files_have_no_path() {
        for uri in [
            "untitled:Untitled-1",
            "https://example.com/a.md",
            "vscode-notebook-cell:/x/a.md#W0sZmlsZQ%3D%3D",
            "",
            "not a uri",
            "file:",
        ] {
            assert_eq!(uri_to_path(uri), None, "{uri:?}");
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_drive_and_network_share_uris_give_usable_paths() {
        // The drive letter in either case, escaped colon (VS Code), and the drive root.
        for uri in [
            "file:///C:/notes/a.md",
            "file:///c:/notes/a.md",
            "file:///c%3A/notes/a.md",
        ] {
            let p = uri_to_path(uri).unwrap_or_else(|| panic!("{uri} has no path"));
            let text = p.to_string_lossy().to_lowercase();
            assert!(text.starts_with("c:"), "{uri} -> {p:?}");
            assert!(
                text.replace('/', "\\").ends_with("notes\\a.md"),
                "{uri} -> {p:?}"
            );
        }
        let root = uri_to_path("file:///m:/").unwrap();
        assert!(root.has_root(), "{root:?}");
        // A network share: `\\server\share\dir\a.md`.
        let unc = uri_to_path("file://server/share/dir/a.md")
            .expect("a UNC URI must give a path, not be ignored");
        assert_eq!(
            unc.to_string_lossy().replace('/', "\\"),
            "\\\\server\\share\\dir\\a.md"
        );
        let spaced = uri_to_path("file://server/share/my%20dir/a.md").unwrap();
        assert!(spaced.to_string_lossy().contains("my dir"), "{spaced:?}");
    }

    #[cfg(windows)]
    #[test]
    fn a_network_path_becomes_a_uri_with_the_server_as_host() {
        let unc = PathBuf::from("\\\\server\\share\\dir\\a.md");
        let uri = path_to_uri(&unc).expect("a UNC path has a URI");
        assert!(
            uri.as_str().starts_with("file://server/share/"),
            "{}",
            uri.as_str()
        );
        assert_eq!(uri_to_path(uri.as_str()).unwrap(), unc);
    }
}
