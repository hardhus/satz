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
pub fn replacements(spans: &[EmphasisSpan], config: &EmphasisConfig) -> Vec<(ByteRange, String)> {
    let mut out = Vec::with_capacity(spans.len() * 2);

    for span in spans {
        let marker = match span.kind {
            EmphasisKind::Italic => normalize_marker(&config.italic_marker, 1, "*"),
            EmphasisKind::Bold => normalize_marker(&config.bold_marker, 2, "**"),
        };
        let marker_len = marker.len();

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
        let mut reps = replacements(&structure.emphasis_spans, config);
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
}
