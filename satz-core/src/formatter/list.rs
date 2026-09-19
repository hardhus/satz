use crate::config::ListsConfig;
use crate::model::ByteRange;
use crate::parser::structure::{ListItemSpan, ListSpan, TaskMarkerSpan};

/// Computes splice replacements for list item markers and task-list checkboxes.
///
/// Each item's marker prefix (from its own range start — which is always exactly the marker's
/// first byte, indentation before it belongs to the parent — through the whitespace that follows
/// it) is replaced with the configured marker character (unordered) or a renumbered `N.` (ordered,
/// when `renumber_ordered` is set) collapsed to exactly one trailing space. Content, indentation,
/// and nesting are never touched: only the marker-and-following-whitespace prefix is rewritten.
pub fn replacements(
    source: &str,
    lists: &[ListSpan],
    items: &[ListItemSpan],
    task_markers: &[TaskMarkerSpan],
    keep_zones: &[ByteRange],
    config: &ListsConfig,
) -> Vec<(ByteRange, String)> {
    let markers = effective_markers(source, lists, items, config);
    let mut out = Vec::with_capacity(items.len() + task_markers.len());

    for item in items {
        let (marker_char, delimiter) = markers[item.list_id];
        out.extend(item_replacements(
            source,
            item,
            marker_char,
            delimiter,
            config.renumber_ordered,
            keep_zones,
        ));
    }

    for marker in task_markers {
        let canonical = if marker.checked { "[x]" } else { "[ ]" };
        out.push((marker.range, canonical.to_string()));
    }

    out
}

/// The marker (`-`/`*`/`+`) and ordered delimiter (`.`/`)`) each list is rewritten with.
///
/// Normally every list gets the configured marker. But a different marker (or delimiter) is what
/// makes CommonMark start a NEW list, so a list directly after another list of the same kind must
/// not end up with the same marker as its predecessor: it keeps its own marker when that differs,
/// otherwise the first one that does.
fn effective_markers(
    source: &str,
    lists: &[ListSpan],
    items: &[ListItemSpan],
    config: &ListsConfig,
) -> Vec<(char, char)> {
    let configured = normalize_marker_char(&config.marker);
    let mut first_item_start: Vec<Option<usize>> = vec![None; lists.len()];
    for item in items {
        first_item_start[item.list_id].get_or_insert(item.range.start);
    }
    let separator_only = |text: &str| text.chars().all(|c| c.is_whitespace() || c == '>');

    let mut result = vec![(configured, '.'); lists.len()];
    // Per nesting depth: the previous list there (end offset, ordered, its effective marker).
    let mut previous: Vec<Option<(usize, bool, char)>> = Vec::new();
    for (id, list) in lists.iter().enumerate() {
        previous.truncate(list.depth + 1);
        previous.resize(list.depth + 1, None);
        let original = first_item_start[id]
            .and_then(|start| parse_marker(source, start))
            .map(|m| match m {
                ParsedMarker::Unordered { marker_end } => source.as_bytes()[marker_end - 1] as char,
                ParsedMarker::Ordered { delimiter_end, .. } => {
                    source.as_bytes()[delimiter_end - 1] as char
                }
            });
        let neighbour = previous[list.depth]
            .filter(|(end, ordered, _)| {
                *ordered == list.ordered
                    && source
                        .get(*end..list.range.start)
                        .is_some_and(separator_only)
            })
            .map(|(_, _, effective)| effective);

        let effective = if list.ordered {
            let default = '.';
            match neighbour {
                Some(n) if n == default => original.filter(|o| *o != n).unwrap_or(')'),
                _ => default,
            }
        } else {
            match neighbour {
                Some(n) if n == configured => original
                    .filter(|o| *o != n)
                    .or_else(|| ['-', '*', '+'].into_iter().find(|c| *c != n))
                    .unwrap_or(configured),
                _ => configured,
            }
        };
        result[id] = if list.ordered {
            (configured, effective)
        } else {
            (effective, '.')
        };
        previous[list.depth] = Some((list.range.end, list.ordered, effective));
    }
    result
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

/// The rewrites for one item: its marker prefix and, when the prefix changes width, the
/// indentation of the item's continuation lines so its content keeps the same column relative to
/// its children (`-   a\n    cont` -> `- a\n  cont`).
///
/// Whatever cannot be re-indented safely (a lazy or tab-indented continuation, an item inside a
/// blockquote or containing a table) keeps its spacing: only the marker character is rewritten,
/// and only when that does not change the width. Not formatting beats breaking the structure.
fn item_replacements(
    source: &str,
    item: &ListItemSpan,
    marker_char: char,
    delimiter: char,
    renumber_ordered: bool,
    keep_zones: &[ByteRange],
) -> Vec<(ByteRange, String)> {
    let Some(parsed) = parse_marker(source, item.range.start) else {
        return Vec::new();
    };

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
            (delimiter_end, format!("{number}{delimiter}"))
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
    let has_tab = source[marker_end..whitespace_end].contains('\t');

    let has_content_after = bytes
        .get(whitespace_end)
        .is_some_and(|b| *b != b'\n' && *b != b'\r');
    let new_prefix = if has_content_after {
        format!("{new_marker_text} ")
    } else {
        new_marker_text.clone()
    };
    let prefix_replacement = (
        ByteRange::new(item.range.start, whitespace_end),
        new_prefix.clone(),
    );

    let delta = new_prefix.len() as isize - (whitespace_end - item.range.start) as isize;
    // Continuation lines: every non-blank line of the item after its first.
    let item_end = item.range.end.min(source.len());
    let first_line_end = source[item.range.start..item_end]
        .find('\n')
        .map(|i| item.range.start + i + 1);
    let mut continuation: Vec<(usize, usize)> = Vec::new(); // (line start, leading spaces)
    if let Some(mut line_start) = first_line_end {
        while line_start < item_end {
            let line_end = source[line_start..item_end]
                .find('\n')
                .map_or(item_end, |i| line_start + i);
            let line = source[line_start..line_end].trim_end_matches('\r');
            if !line.trim().is_empty() {
                let leading = line.len() - line.trim_start_matches(' ').len();
                continuation.push((line_start, leading));
            }
            line_start = line_end + 1;
        }
    }

    if continuation.is_empty() || (delta == 0 && !has_tab) {
        return vec![prefix_replacement];
    }

    let in_keep_zone = keep_zones
        .iter()
        .any(|zone| zone.start < item_end && item.range.start < zone.end);
    let cannot_shrink = delta < 0
        && continuation
            .iter()
            .any(|(_, lead)| *lead < (-delta) as usize);
    if has_tab || in_keep_zone || cannot_shrink {
        // Only the marker character, and only if the width stays the same.
        let old_marker_len = marker_end - item.range.start;
        return if new_marker_text.len() == old_marker_len
            && new_marker_text != source[item.range.start..marker_end]
        {
            vec![(
                ByteRange::new(item.range.start, marker_end),
                new_marker_text,
            )]
        } else {
            Vec::new()
        };
    }

    let mut out = vec![prefix_replacement];
    for (line_start, _) in continuation {
        if delta > 0 {
            out.push((
                ByteRange::new(line_start, line_start),
                " ".repeat(delta as usize),
            ));
        } else {
            out.push((
                ByteRange::new(line_start, line_start + (-delta) as usize),
                String::new(),
            ));
        }
    }
    out
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
            &structure.list_spans,
            &structure.list_items,
            &structure.task_markers,
            &[],
            config,
        );
        reps.sort_by_key(|(r, _)| r.start);
        crate::formatter::zones::splice_ranges(source, &reps)
    }

    #[test]
    fn test_normalizes_mixed_unordered_markers_to_configured_char() {
        // Three different markers are three separate lists in CommonMark. Normalizing must not
        // merge them, so the middle one keeps a marker that differs from its neighbours.
        let out = apply("- a\n* b\n+ c\n", &ListsConfig::default());
        assert_eq!(out, "- a\n* b\n- c\n");
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

    fn fmt(src: &str) -> String {
        format_document(src, &FormatterConfig::default())
    }

    fn fmt_marker(src: &str, marker: &str) -> String {
        let mut cfg = FormatterConfig::default();
        cfg.lists.marker = marker.to_string();
        format_document(src, &cfg)
    }

    // ---- adjacent lists stay separate lists (a different marker starts a new list) ----

    #[test]
    fn a_marker_change_still_starts_a_new_list_after_normalizing() {
        for (src, expected) in [
            ("- a\n\n* c\n", "- a\n\n* c\n"),
            ("- a\n* b\n", "- a\n* b\n"),
            ("* a\n\n+ b\n", "- a\n\n+ b\n"),
            ("+ a\n\n- b\n\n* c\n", "- a\n\n* b\n\n- c\n"),
            ("- a\n- a2\n\n* b\n* b2\n", "- a\n- a2\n\n* b\n* b2\n"),
            ("1. a\n\n1) b\n", "1. a\n\n1) b\n"),
            ("> - a\n>\n> * b\n", "> - a\n>\n> * b\n"),
            ("- p\n  - x\n\n  * y\n", "- p\n  - x\n\n  * y\n"),
        ] {
            assert_eq!(fmt(src), expected, "{src:?}");
            assert_eq!(fmt(expected), expected, "idempotency for {src:?}");
        }
    }

    #[test]
    fn lists_that_are_not_adjacent_are_normalized_independently() {
        assert_eq!(fmt("* a\n\n# H\n\n* b\n"), "- a\n\n# H\n\n- b\n");
        assert_eq!(fmt("* a\n\ntext\n\n+ b\n"), "- a\n\ntext\n\n- b\n");
        assert_eq!(fmt("* a\n* b\n"), "- a\n- b\n");
        assert_eq!(fmt("1. a\n\ntext\n\n1) b\n"), "1. a\n\ntext\n\n1. b\n");
    }

    #[test]
    fn the_configured_marker_is_used_and_the_neighbour_still_differs() {
        assert_eq!(fmt_marker("- a\n\n* b\n", "*"), "* a\n\n- b\n");
        assert_eq!(fmt_marker("+ a\n\n+ b\n", "+"), "+ a\n\n+ b\n"); // one list, stays one
        assert_eq!(fmt_marker("- a\n\n* b\n", "+"), "+ a\n\n* b\n");
    }

    #[test]
    fn a_single_list_keeps_all_its_items_on_one_marker() {
        assert_eq!(fmt("* a\n* b\n* c\n"), "- a\n- b\n- c\n");
        assert_eq!(fmt("- a\n  * b\n  * c\n- d\n"), "- a\n  - b\n  - c\n- d\n");
    }

    // ---- the content column of an item never moves relative to its children ----

    #[test]
    fn continuation_lines_follow_a_narrower_marker() {
        for (src, expected) in [
            ("-   a\n    cont\n", "- a\n  cont\n"),
            ("1.  a\n    cont\n2.  b\n", "1. a\n   cont\n2. b\n"),
            ("-   a\n    - b\n    - c\n", "- a\n  - b\n  - c\n"),
            (
                "-   a\n\n    ```\n    code\n    ```\n",
                "- a\n\n  ```\n  code\n  ```\n",
            ),
            (
                "-   a\n\n    para two\n\n-   b\n",
                "- a\n\n  para two\n\n- b\n",
            ),
            ("-  a\n   cont\n", "- a\n  cont\n"),
        ] {
            assert_eq!(fmt(src), expected, "{src:?}");
            assert_eq!(fmt(expected), expected, "idempotency for {src:?}");
        }
    }

    #[test]
    fn continuation_lines_follow_a_wider_marker() {
        // Renumbering a list that starts at 9 turns `9.` into `10.`.
        assert_eq!(
            fmt("9. a\n   x\n9. b\n   y\n"),
            "9. a\n   x\n10. b\n    y\n"
        );
        // Nested content moves with it.
        assert_eq!(
            fmt("9. a\n9. b\n   - n1\n   - n2\n"),
            "9. a\n10. b\n    - n1\n    - n2\n"
        );
        let once = fmt("9. a\n   x\n9. b\n   y\n");
        assert_eq!(fmt(&once), once);
    }

    #[test]
    fn an_item_whose_children_cannot_be_reindented_keeps_its_spacing() {
        for src in [
            // A lazy continuation line has no indentation to give back.
            "-   a\nlazy\n",
            // A tab after the marker has no fixed width.
            "-\ta\n  cont\n",
            // Inside a blockquote the indentation follows the `>` prefix.
            "> -   a\n>     cont\n",
        ] {
            assert_eq!(fmt(src), src, "{src:?}");
        }
    }

    #[test]
    fn an_item_with_a_table_inside_is_left_alone() {
        let src = "-   a\n\n    | x | y |\n    |---|---|\n    | 1 | 2 |\n";
        let out = fmt(src);
        assert!(out.starts_with("-   a\n"), "{out:?}");
        assert!(out.contains("| x"), "{out:?}");
    }

    #[test]
    fn single_line_items_still_collapse_their_spacing() {
        assert_eq!(fmt("-   a\n-   b\n"), "- a\n- b\n");
        assert_eq!(fmt("1.   a\n2.   b\n"), "1. a\n2. b\n");
        assert_eq!(fmt("-   a\n\n-   b\n"), "- a\n\n- b\n");
    }
}
