use crate::config::FormatterConfig;
use crate::model::ByteRange;
use crate::parser::inline_scan;
use crate::parser::structure::parse_structure;

/// Trims the whitespace inside every `[[wikilink]]` / `![[embed]]`: `[[  a  |  b  ]]` becomes
/// `[[a|b]]`. Only whitespace around the target and around the display text (either side of the
/// `|`) is touched; the words themselves never change.
///
/// Wikilinks inside code (inline code, fenced or indented blocks) and inside the frontmatter are
/// literal text and are left alone -- which is why this uses the same inline scan as the index
/// (`inline_scan::scan_inline`, told about code spans and the frontmatter) instead of searching
/// lines for `[[`.
///
/// This runs BEFORE tables and wrapping, so their measurements (column widths, line widths) are
/// taken on the final link text and a single formatting pass is enough.
pub fn normalize(source: &str, config: &FormatterConfig) -> String {
    if !config.normalize_links || !source.contains("[[") {
        return source.to_string();
    }

    let structure = parse_structure(source);
    let mut skip = structure.code_spans.clone();
    if let Some(frontmatter) = structure.frontmatter_range {
        skip.push(frontmatter);
    }
    skip.sort_unstable_by_key(|s| s.start);
    let inline = inline_scan::scan_inline(source, &skip);

    let mut replacements: Vec<(ByteRange, String)> = Vec::new();
    for link in &inline.wiki_links {
        let raw = &source[link.range.start..link.range.end];
        let normalized = normalize_one(raw);
        if normalized != raw {
            replacements.push((link.range, normalized));
        }
    }
    if replacements.is_empty() {
        return source.to_string();
    }
    replacements.sort_by_key(|(range, _)| range.start);
    super::zones::splice_ranges(source, &replacements)
}

/// `raw` is a complete `[[...]]` or `![[...]]` as delimited by `scan_inline`.
fn normalize_one(raw: &str) -> String {
    let (open, rest) = match raw.strip_prefix("![[") {
        Some(rest) => ("![[", rest),
        None => ("[[", raw.strip_prefix("[[").unwrap_or(raw)),
    };
    let Some(inner) = rest.strip_suffix("]]") else {
        return raw.to_string();
    };
    match inner.split_once('|') {
        Some((target, display)) => format!("{open}{}|{}]]", target.trim(), display.trim()),
        None => format!("{open}{}]]", inner.trim()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn norm(s: &str) -> String {
        normalize(s, &FormatterConfig::default())
    }

    #[test]
    fn trims_target_and_display_but_never_the_words() {
        assert_eq!(norm("[[  a  ]]"), "[[a]]");
        assert_eq!(norm("[[ a b | c d ]]"), "[[a b|c d]]");
        assert_eq!(norm("![[ img.png ]]"), "![[img.png]]");
        assert_eq!(norm("[[a#h|b]]"), "[[a#h|b]]");
        assert_eq!(norm("[[]]"), "[[]]");
        assert_eq!(norm("[[ ]]"), "[[]]");
        assert_eq!(norm("[[|]]"), "[[|]]");
    }

    #[test]
    fn is_a_noop_when_disabled_or_when_there_is_nothing_to_do() {
        let off = FormatterConfig {
            normalize_links: false,
            ..FormatterConfig::default()
        };
        assert_eq!(normalize("[[ a ]]", &off), "[[ a ]]");
        assert_eq!(norm("no links here"), "no links here");
        assert_eq!(norm("[[already]] [[clean|x]]"), "[[already]] [[clean|x]]");
        assert_eq!(norm(""), "");
    }

    #[test]
    fn leaves_code_and_frontmatter_alone_and_handles_neighbours() {
        assert_eq!(norm("`[[ a ]]` [[ b ]]"), "`[[ a ]]` [[b]]");
        assert_eq!(
            norm("```\n[[ a ]]\n```\n[[ b ]]"),
            "```\n[[ a ]]\n```\n[[b]]"
        );
        assert_eq!(
            norm("---\nt: [[ a ]]\n---\n[[ b ]]"),
            "---\nt: [[ a ]]\n---\n[[b]]"
        );
        assert_eq!(norm("[[ a ]][[ b ]] x [[ c ]]"), "[[a]][[b]] x [[c]]");
        assert_eq!(norm("Türkçe [[ Ölçü | öğe ]] ✓"), "Türkçe [[Ölçü|öğe]] ✓");
    }

    #[test]
    fn a_link_spanning_lines_or_left_open_is_not_touched() {
        assert_eq!(norm("[[ a\nb ]]"), "[[ a\nb ]]");
        assert_eq!(norm("[[ a and more"), "[[ a and more");
    }
}
