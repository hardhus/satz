use crate::config::EmphasisConfig;
use crate::model::ByteRange;
use crate::parser::structure::{EmphasisKind, EmphasisSpan};

/// Computes marker-only splice replacements for every detected emphasis/strong span, normalizing
/// delimiters to the configured style (`*`/`_` for italic, `**`/`__` for bold).
///
/// Only the delimiter bytes themselves are ever touched (one point-range for the opening
/// delimiter, one for the closing) — the content between them, including nested emphasis/strong
/// or `[[wikilink]]` syntax, is never re-examined or altered. Nested spans (e.g. `***text***`
/// parsing as `Emphasis` wrapping `Strong`) naturally decompose into non-overlapping marker
/// ranges since a construct's delimiters never share a byte position with another construct's.
pub fn replacements(
    source: &str,
    spans: &[EmphasisSpan],
    config: &EmphasisConfig,
) -> Vec<(ByteRange, String)> {
    let mut out = Vec::with_capacity(spans.len() * 2);

    for span in spans {
        let marker = match span.kind {
            EmphasisKind::Italic => normalize_marker(&config.italic_marker, 1, "*"),
            EmphasisKind::Bold => normalize_marker(&config.bold_marker, 2, "**"),
        };
        let marker_len = marker.len();

        // `_` cannot open or close emphasis inside a word (`a*b*c` is emphasis, `a_b_c` is not),
        // and an inner `_` would be ambiguous; such a span keeps the delimiters it was written with.
        if marker.starts_with('_') && !source[span.range.start..].starts_with('_') {
            let inner = &source[span.range.start + marker_len..span.range.end - marker_len];
            let before = source[..span.range.start].chars().next_back();
            let after = source[span.range.end..].chars().next();
            if before.is_some_and(char::is_alphanumeric)
                || after.is_some_and(char::is_alphanumeric)
                || inner.contains('_')
            {
                continue;
            }
        }

        let open_range = ByteRange::new(span.range.start, span.range.start + marker_len);
        let close_range = ByteRange::new(span.range.end - marker_len, span.range.end);

        out.push((open_range, marker.clone()));
        out.push((close_range, marker));
    }

    out
}

/// Validates a configured marker against the known-good options for its length; falls back to
/// `default` for anything else (typo, wrong length, unsupported character).
fn normalize_marker(configured: &str, expected_len: usize, default: &str) -> String {
    let valid = match expected_len {
        1 => matches!(configured, "*" | "_"),
        2 => matches!(configured, "**" | "__"),
        _ => false,
    };
    if valid {
        configured.to_string()
    } else {
        default.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::FormatterConfig;
    use crate::formatter::format_document;
    use crate::parser::structure::parse_structure;

    fn apply(source: &str, config: &EmphasisConfig) -> String {
        let structure = parse_structure(source);
        let mut reps = replacements(source, &structure.emphasis_spans, config);
        reps.sort_by_key(|(r, _)| r.start);
        crate::formatter::zones::splice_ranges(source, &reps)
    }

    #[test]
    fn test_normalizes_underscore_italic_to_default_star() {
        let out = apply("a _ital_ b\n", &EmphasisConfig::default());
        assert_eq!(out, "a *ital* b\n");
    }

    #[test]
    fn test_normalizes_underscore_bold_to_default_double_star() {
        let out = apply("a __bold__ b\n", &EmphasisConfig::default());
        assert_eq!(out, "a **bold** b\n");
    }

    #[test]
    fn test_normalizes_star_to_configured_underscore() {
        let config = EmphasisConfig {
            enable: true,
            italic_marker: "_".to_string(),
            bold_marker: "__".to_string(),
        };
        let out = apply("a *ital* and **bold** b\n", &config);
        assert_eq!(out, "a _ital_ and __bold__ b\n");
    }

    #[test]
    fn test_already_correct_style_is_a_no_op() {
        let out = apply("a *ital* and **bold** b\n", &EmphasisConfig::default());
        assert_eq!(out, "a *ital* and **bold** b\n");
    }

    #[test]
    fn test_wikilink_inside_emphasis_survives_verbatim() {
        let out = apply("*[[not]]* text\n", &EmphasisConfig::default());
        assert_eq!(out, "*[[not]]* text\n");

        let config = EmphasisConfig {
            enable: true,
            italic_marker: "_".to_string(),
            bold_marker: "__".to_string(),
        };
        let out = apply("*[[not]]* text\n", &config);
        assert_eq!(out, "_[[not]]_ text\n");
    }

    #[test]
    fn test_triple_nested_strong_and_emphasis_both_normalized() {
        let config = EmphasisConfig {
            enable: true,
            italic_marker: "_".to_string(),
            bold_marker: "__".to_string(),
        };
        let out = apply("***bold italic***\n", &config);
        assert_eq!(out, "___bold italic___\n");
    }

    #[test]
    fn test_invalid_configured_marker_falls_back_to_default() {
        let config = EmphasisConfig {
            enable: true,
            italic_marker: "xx".to_string(),
            bold_marker: "".to_string(),
        };
        let out = apply("a *ital* and **bold** b\n", &config);
        assert_eq!(out, "a *ital* and **bold** b\n");
    }

    #[test]
    fn test_end_to_end_idempotent_via_format_document() {
        let mut cfg = FormatterConfig::default();
        cfg.emphasis.italic_marker = "_".to_string();
        cfg.emphasis.bold_marker = "__".to_string();
        let input = "Mixed *style* and _also this_ and **loud** and __also loud__.\n";
        let pass1 = format_document(input, &cfg);
        let pass2 = format_document(&pass1, &cfg);
        assert_eq!(pass1, pass2);
        assert_eq!(
            pass1,
            "Mixed _style_ and _also this_ and __loud__ and __also loud__.\n"
        );
    }

    fn underscore() -> EmphasisConfig {
        EmphasisConfig {
            enable: true,
            italic_marker: "_".to_string(),
            bold_marker: "__".to_string(),
        }
    }

    #[test]
    fn intraword_emphasis_keeps_its_stars_when_underscores_are_configured() {
        // `_` cannot open or close emphasis inside a word, so converting would turn emphasis into
        // literal underscores.
        for src in [
            "a*b*c\n",
            "a**b**c\n",
            "*a*b\n",
            "a*b*\n",
            "ç*x*ç\n",
            "2*x*3\n",
            "foo*bar*\n",
            "*foo*bar baz\n",
        ] {
            assert_eq!(apply(src, &underscore()), src, "{src:?}");
        }
    }

    #[test]
    fn emphasis_containing_the_target_marker_keeps_its_stars() {
        for src in ["*a_b*\n", "**a__b**\n", "*snake_case_name*\n"] {
            assert_eq!(apply(src, &underscore()), src, "{src:?}");
        }
    }

    #[test]
    fn emphasis_at_word_boundaries_still_converts() {
        for (src, expected) in [
            ("x *y* z\n", "x _y_ z\n"),
            ("*a*\n", "_a_\n"),
            ("(*x*)\n", "(_x_)\n"),
            ("*x*, *y*.\n", "_x_, _y_.\n"),
            ("**a** b\n", "__a__ b\n"),
            ("— *dash* —\n", "— _dash_ —\n"),
            ("*a*\r\n", "_a_\r\n"),
        ] {
            assert_eq!(apply(src, &underscore()), expected, "{src:?}");
        }
    }

    #[test]
    fn converting_to_stars_is_always_allowed() {
        let star = EmphasisConfig::default();
        assert_eq!(apply("a_b_c\n", &star), "a_b_c\n"); // not emphasis at all
        assert_eq!(apply("x _y_ z\n", &star), "x *y* z\n");
        assert_eq!(apply("a__b__ c\n", &star), "a__b__ c\n"); // intraword `__` is not strong
    }

    #[test]
    fn underscore_conversion_is_idempotent_and_keeps_the_rendered_result() {
        let cfg = FormatterConfig {
            emphasis: underscore(),
            ..FormatterConfig::default()
        };
        let src = "a*b*c and *ok* and **b**d and **fine**\n";
        let once = format_document(src, &cfg);
        assert_eq!(once, "a*b*c and _ok_ and **b**d and __fine__\n");
        assert_eq!(format_document(&once, &cfg), once);
    }
}
