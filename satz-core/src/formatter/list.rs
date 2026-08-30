use crate::config::ListsConfig;
use crate::model::ByteRange;
use crate::parser::structure::{ListItemSpan, TaskMarkerSpan};

/// Computes splice replacements for list item markers and task-list checkboxes.
///
/// Each item's marker prefix (from its own range start — which is always exactly the marker's
/// first byte, indentation before it belongs to the parent — through the whitespace that follows
/// it) is replaced with the configured marker character (unordered) or a renumbered `N.` (ordered,
/// when `renumber_ordered` is set) collapsed to exactly one trailing space. Content, indentation,
/// and nesting are never touched: only the marker-and-following-whitespace prefix is rewritten.
pub fn replacements(
    source: &str,
    items: &[ListItemSpan],
    task_markers: &[TaskMarkerSpan],
    config: &ListsConfig,
) -> Vec<(ByteRange, String)> {
    let marker_char = normalize_marker_char(&config.marker);
    let mut out = Vec::with_capacity(items.len() + task_markers.len());

    for item in items {
        if let Some(replacement) =
            normalize_item_marker(source, item, marker_char, config.renumber_ordered)
        {
            out.push(replacement);
        }
    }

    for marker in task_markers {
        let canonical = if marker.checked { "[x]" } else { "[ ]" };
        out.push((marker.range, canonical.to_string()));
    }

    out
}

fn normalize_marker_char(configured: &str) -> char {
    match configured {
        "-" => '-',
        "*" => '*',
        "+" => '+',
        _ => '-',
    }
}

enum ParsedMarker {
    Unordered {
        marker_end: usize,
    },
    /// `original_digits` is whatever the user actually typed for this item's number — CommonMark
    /// (and pulldown-cmark's AST) only preserves the *list's* starting number, not each
    /// individual item's, so when `renumber_ordered` is off we fall back to this raw text.
    Ordered {
        delimiter_end: usize,
        original_digits: String,
    },
}

fn parse_marker(source: &str, start: usize) -> Option<ParsedMarker> {
    let bytes = source.as_bytes();
    let first = *bytes.get(start)?;

    if matches!(first, b'-' | b'*' | b'+') {
        return Some(ParsedMarker::Unordered {
            marker_end: start + 1,
        });
    }

    if first.is_ascii_digit() {
        let mut i = start;
        while bytes.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
        let digits_end = i;
        let delimiter = *bytes.get(i)?;
        if delimiter == b'.' || delimiter == b')' {
            return Some(ParsedMarker::Ordered {
                delimiter_end: i + 1,
                original_digits: source[start..digits_end].to_string(),
            });
        }
    }

    None
}

fn normalize_item_marker(
    source: &str,
    item: &ListItemSpan,
    marker_char: char,
    renumber_ordered: bool,
) -> Option<(ByteRange, String)> {
    let parsed = parse_marker(source, item.range.start)?;

    let (marker_end, new_marker_text) = match parsed {
        ParsedMarker::Unordered { marker_end } => (marker_end, marker_char.to_string()),
        ParsedMarker::Ordered {
            delimiter_end,
            original_digits,
        } => {
            // The delimiter is always normalized to "." (single style decision, matching the
            // rest of the formatter's philosophy) regardless of `renumber_ordered`, which only
            // controls whether the *number* is recomputed sequentially or left as the user wrote.
            let number = if renumber_ordered {
                item.ordinal.to_string()
            } else {
                original_digits
            };
            (delimiter_end, format!("{number}."))
        }
    };

    let bytes = source.as_bytes();
    let mut whitespace_end = marker_end;
    while bytes
        .get(whitespace_end)
        .is_some_and(|b| *b == b' ' || *b == b'\t')
    {
        whitespace_end += 1;
    }

    let has_content_after = bytes
        .get(whitespace_end)
        .is_some_and(|b| *b != b'\n' && *b != b'\r');

    let replacement = if has_content_after {
        format!("{new_marker_text} ")
    } else {
        new_marker_text
    };

    Some((
        ByteRange::new(item.range.start, whitespace_end),
        replacement,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::FormatterConfig;
    use crate::formatter::format_document;
    use crate::parser::structure::parse_structure;

    fn apply(source: &str, config: &ListsConfig) -> String {
        let structure = parse_structure(source);
        let mut reps = replacements(
            source,
            &structure.list_items,
            &structure.task_markers,
            config,
        );
        reps.sort_by_key(|(r, _)| r.start);
        crate::formatter::zones::splice_ranges(source, &reps)
    }

    #[test]
    fn test_normalizes_mixed_unordered_markers_to_configured_char() {
        let out = apply("- a\n* b\n+ c\n", &ListsConfig::default());
        assert_eq!(out, "- a\n- b\n- c\n");
    }

    #[test]
    fn test_normalizes_to_star_marker() {
        let config = ListsConfig {
            enable: true,
            marker: "*".to_string(),
            renumber_ordered: true,
        };
        let out = apply("- a\n- b\n", &config);
        assert_eq!(out, "* a\n* b\n");
    }

    #[test]
    fn test_collapses_extra_marker_whitespace_to_one_space() {
        let out = apply("-    a\n-  b\n", &ListsConfig::default());
        assert_eq!(out, "- a\n- b\n");
    }

    #[test]
    fn test_renumbers_ordered_list_regardless_of_source_numbers() {
        let out = apply("1. a\n1. b\n1. c\n", &ListsConfig::default());
        assert_eq!(out, "1. a\n2. b\n3. c\n");
    }

    #[test]
    fn test_ordered_list_respects_custom_start_number() {
        let out = apply("5. a\n5. b\n", &ListsConfig::default());
        assert_eq!(out, "5. a\n6. b\n");
    }

    #[test]
    fn test_ordered_delimiter_normalized_to_period_even_without_renumbering() {
        let config = ListsConfig {
            enable: true,
            marker: "-".to_string(),
            renumber_ordered: false,
        };
        let out = apply("1) a\n2) b\n", &config);
        assert_eq!(out, "1. a\n2. b\n");
    }

    #[test]
    fn test_no_renumber_keeps_original_digits() {
        let config = ListsConfig {
            enable: true,
            marker: "-".to_string(),
            renumber_ordered: false,
        };
        // User wrote inconsistent numbers; without renumbering we must not "fix" them.
        let out = apply("1. a\n1. b\n9. c\n", &config);
        assert_eq!(out, "1. a\n1. b\n9. c\n");
    }

    #[test]
    fn test_task_list_checkbox_canonicalized() {
        let out = apply("- [ ] todo\n- [x] done\n", &ListsConfig::default());
        assert_eq!(out, "- [ ] todo\n- [x] done\n");
    }

    #[test]
    fn test_nested_list_indentation_and_independent_numbering_preserved() {
        // Three levels deep, mixed ordered/unordered — indentation must survive untouched, and
        // the nested unordered list must not disturb the surrounding ordered list's own
        // sequential numbering (n1, n2, n3 stay one continuous list around it).
        // Note: the nested list's marker must align to at least item "n2"'s own content column
        // (5, i.e. 5 spaces) or CommonMark treats it as breaking the ordered list into separate
        // sibling lists instead of nesting it under "n2" — verified against pulldown-cmark's
        // event stream.
        let md = "- a\n  1. n1\n  1. n2\n     - deep1\n     - deep2\n  1. n3\n- b\n";
        let out = apply(md, &ListsConfig::default());
        assert_eq!(
            out,
            "- a\n  1. n1\n  2. n2\n     - deep1\n     - deep2\n  3. n3\n- b\n"
        );
    }

    #[test]
    fn test_end_to_end_idempotent_via_format_document() {
        let cfg = FormatterConfig::default();
        let input = "- a\n* b\n+ c\n\n1. x\n1. y\n1. z\n\n- [ ] todo\n- [X] done\n";
        let pass1 = format_document(input, &cfg);
        let pass2 = format_document(&pass1, &cfg);
        assert_eq!(pass1, pass2);
    }
}
