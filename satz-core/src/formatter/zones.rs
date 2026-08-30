use crate::model::ByteRange;

/// Replaces each given byte range in `source` with its corresponding replacement text, leaving
/// everything outside those ranges untouched. `replacements` must be sorted by `range.start`;
/// a range that starts before the current cursor (e.g. from unexpected overlap) is skipped
/// defensively rather than panicking, leaving that region's original text in place.
pub fn splice_ranges(source: &str, replacements: &[(ByteRange, String)]) -> String {
    let mut out = String::with_capacity(source.len());
    let mut cursor = 0usize;

    for (span, replacement) in replacements {
        if span.start < cursor {
            continue;
        }
        out.push_str(&source[cursor..span.start]);
        out.push_str(replacement);
        cursor = span.end;
    }
    out.push_str(&source[cursor..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_splice_single_range() {
        let source = "before [TABLE] after";
        let span = ByteRange::new(7, 14);
        let out = splice_ranges(source, &[(span, "<rendered>".to_string())]);
        assert_eq!(out, "before <rendered> after");
    }

    #[test]
    fn test_splice_multiple_ranges_in_order() {
        let source = "AAABBBCCC";
        let replacements = vec![
            (ByteRange::new(0, 3), "1".to_string()),
            (ByteRange::new(3, 6), "2".to_string()),
        ];
        let out = splice_ranges(source, &replacements);
        assert_eq!(out, "12CCC");
    }

    #[test]
    fn test_splice_no_ranges_is_passthrough() {
        let source = "unchanged text";
        assert_eq!(splice_ranges(source, &[]), source);
    }

    #[test]
    fn test_splice_skips_overlapping_range_defensively() {
        let source = "0123456789";
        let replacements = vec![
            (ByteRange::new(0, 5), "AAAAA".to_string()),
            // overlaps the previous range's end (starts at 3 < cursor 5) — must be skipped
            (ByteRange::new(3, 8), "BBBBB".to_string()),
        ];
        let out = splice_ranges(source, &replacements);
        assert_eq!(out, "AAAAA56789");
    }
}
