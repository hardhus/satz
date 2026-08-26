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
    uri.to_file_path().map(|p| p.into_owned())
}

/// Converts a filesystem `Path` into an `lsp::Uri`.
#[allow(dead_code)]
pub fn path_to_uri(path: &Path) -> Option<lsp::Uri> {
    lsp::Uri::from_file_path(path)
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
