pub mod diff;
pub(crate) mod emphasis;
pub(crate) mod line_pass;
pub(crate) mod links;
pub(crate) mod list;
mod math;
pub(crate) mod misc;
pub(crate) mod table;
pub(crate) mod wrap;
pub(crate) mod zones;

use crate::config::FormatterConfig;
use crate::model::ByteRange;

/// Formats a markdown document deterministically according to the provided `FormatterConfig`.
///
/// Four stages, in order:
/// 0. `links::normalize` trims the whitespace inside wikilinks (outside code and frontmatter), first,
///    so tables and wrapping measure the final link text and one pass is always enough.
/// 1. A single `parse_structure` call gathers every construct's byte ranges (tables,
///    emphasis/strong, list markers, rule lines, blockquote markers, code fences), each
///    sub-module turns its ranges into replacement text, and all replacements are spliced into
///    the source in one shot via `zones::splice_ranges`.
/// 2. `wrap::wrap` (opt-in, off by default — see `WrapConfig`) reflows top-level paragraphs to
///    `line_width`. This is a genuinely separate pass, re-parsing the already-spliced text from
///    scratch, rather than another entry in stage 1's replacement list: a paragraph's
///    whole-span replacement would overlap any emphasis/wikilink replacement already computed
///    for text inside it, and `splice_ranges` silently drops overlapping ranges rather than
///    merging them. Running after stage 1 also means wrapping sees the already-normalized
///    `**bold**`/list-marker/etc. text, not the pre-formatting source.
/// 3. `line_pass::layout` (trim/blank-line/heading-spacing/final-newline) runs on the result;
///    frontmatter, code blocks (fenced or indented) and HTML blocks are copied through untouched.
///
/// Stage 1's ranges never overlap each other by construction: every construct there replaces
/// only marker/fence bytes or (for tables) a region whose inner content is deliberately
/// reproduced verbatim rather than re-examined — see `table::parse_table_block`. One consequence
/// of that verbatim-cell policy: emphasis/list/etc. markers *inside* a table cell are not
/// separately normalized by this pass (they're copied as-is by the table renderer); this is an
/// accepted, narrow scope limitation, not a correctness bug.
///
/// Line endings are preserved: a document whose lines mostly end in `\r\n` is formatted as if it
/// used `\n` and converted back afterwards, so a CRLF file stays CRLF (and formats exactly like its
/// LF twin). A document mixing both styles is normalised to whichever is more common (LF on a tie).
/// A lone `\r` that isn't part of `\r\n` is ordinary content and is left alone.
pub fn format_document(source: &str, config: &FormatterConfig) -> String {
    // A leading byte order mark belongs to the file, not to the text: format what follows and put
    // the mark back, so the frontmatter fence is still recognised and the file keeps its BOM.
    if let Some(rest) = source.strip_prefix('\u{feff}') {
        return format!("\u{feff}{}", format_document(rest, config));
    }
    if !source.contains('\r') {
        return format_lf(source, config);
    }
    let crlf = uses_crlf(source);
    let normalized = source.replace("\r\n", "\n");
    let formatted = format_lf(&normalized, config);
    if crlf {
        formatted.replace('\n', "\r\n")
    } else {
        formatted
    }
}

/// True if more of the document's line endings are `\r\n` than bare `\n`.
fn uses_crlf(source: &str) -> bool {
    let crlf = source.matches("\r\n").count();
    let lf = source.matches('\n').count() - crlf;
    crlf > lf
}

/// Formats LF text with math protected: formulas are masked before the stages run and put back
/// afterwards (see `math`). If the masks cannot be put back intact the document is returned as it
/// was -- not formatting is safer than damaging a formula.
fn format_lf(source: &str, config: &FormatterConfig) -> String {
    match math::mask(source) {
        None => format_stages(source, config),
        Some((masked, mask)) => {
            let formatted = format_stages(&masked, config);
            mask.restore(&formatted)
                .unwrap_or_else(|| source.to_string())
        }
    }
}

/// The formatting pipeline proper; expects `\n` line endings only.
fn format_stages(source: &str, config: &FormatterConfig) -> String {
    // Stage 0: wikilink whitespace. Done first so tables and wrapping measure the final link text.
    let source = links::normalize(source, config);
    let source = source.as_str();
    let structure = crate::parser::structure::parse_structure(source);
    let mut replacements: Vec<(ByteRange, String)> = Vec::new();

    if config.tables.enable {
        let mut spans = structure.table_spans.clone();
        spans.sort_by_key(|s| s.start);
        for span in spans {
            // A table whose rows have more cells than its header (typically an unescaped `|`
            // inside a wikilink or code span) can only be re-rendered by deleting the extra
            // cells from the file, so it is left exactly as written; so is one in a container
            // whose line markers cannot be read (see `table::format_at`).
            replacements.push((
                span,
                table::format_at(
                    source,
                    span,
                    &config.tables,
                    config.misc.enable && config.misc.blockquote_single_space,
                ),
            ));
        }
    }

    if config.lists.enable {
        let keep_zones: Vec<ByteRange> = structure
            .table_spans
            .iter()
            .chain(structure.blockquote_spans.iter())
            .copied()
            .collect();
        replacements.extend(list::replacements(
            source,
            &structure.list_spans,
            &structure.list_items,
            &structure.task_markers,
            &keep_zones,
            &config.lists,
        ));
    }

    if config.emphasis.enable {
        replacements.extend(emphasis::replacements(
            source,
            &structure.emphasis_spans,
            &config.emphasis,
        ));
    }

    if config.misc.enable {
        replacements.extend(misc::replacements(
            source,
            &structure.rule_spans,
            &structure.code_fence_spans,
            &structure.blockquote_spans,
            &config.misc,
        ));
    }

    replacements.sort_by_key(|(r, _)| r.start);
    let spliced = zones::splice_ranges(source, &replacements);
    let wrapped = wrap::wrap(&spliced, config);

    line_pass::layout(&wrapped, config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    /// Idempotency safety net: formatting every fixture in the test corpus must reach a fixed
    /// point on the first pass (format(format(x)) == format(x)). This is the regression guard
    /// that later formatter phases must not break.
    #[test]
    fn test_idempotent_across_all_fixtures() {
        let config = FormatterConfig::default();
        let fixtures_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let fixtures = collect_markdown_files(&fixtures_root);
        assert!(
            !fixtures.is_empty(),
            "no fixture files found under {}",
            fixtures_root.display()
        );

        for path in fixtures {
            let source = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("failed to read {}: {}", path.display(), e));
            let pass1 = format_document(&source, &config);
            let pass2 = format_document(&pass1, &config);
            assert_eq!(
                pass1,
                pass2,
                "formatter is not idempotent for fixture: {}",
                path.display()
            );
        }
    }

    fn crlf(s: &str) -> String {
        s.replace('\n', "\r\n")
    }

    const DIRTY_LF: &str =
        "# T\n\nText with   trailing   \n\n\n\n- a\n- b\n\n| a | b |\n|---|---|\n| 1 | 2 |\n";

    #[test]
    fn crlf_document_formats_like_its_lf_twin_and_stays_crlf() {
        let config = FormatterConfig::default();
        let expected = crlf(&format_document(DIRTY_LF, &config));
        let got = format_document(&crlf(DIRTY_LF), &config);
        assert_eq!(got, expected);
        assert!(got.contains("\r\n"));
        assert!(
            !got.replace("\r\n", "").contains('\n'),
            "bare LF in CRLF output: {got:?}"
        );
    }

    #[test]
    fn lf_document_stays_lf() {
        let out = format_document(DIRTY_LF, &FormatterConfig::default());
        assert!(!out.contains('\r'), "{out:?}");
    }

    #[test]
    fn mixed_line_endings_follow_the_dominant_style() {
        let config = FormatterConfig::default();
        // 2 CRLF vs 1 LF -> CRLF
        assert_eq!(format_document("a\r\nb\r\nc\n", &config), "a\r\nb\r\nc\r\n");
        // 1 CRLF vs 2 LF -> LF
        assert_eq!(format_document("a\nb\nc\r\n", &config), "a\nb\nc\n");
        // tie -> LF
        assert_eq!(format_document("a\r\nb\n", &config), "a\nb\n");
    }

    #[test]
    fn crlf_edge_cases() {
        let config = FormatterConfig::default();
        // Empty and line-ending-only documents.
        assert_eq!(
            format_document("\r\n", &config),
            format_document("\n", &config)
        );
        assert_eq!(
            format_document("\r\n\r\n\r\n", &config),
            format_document("\n\n\n", &config)
        );
        // No line ending at all: nothing to be "dominant"; the final newline is added as LF.
        assert_eq!(format_document("abc", &config), "abc\n");
        // A single CRLF line keeps CRLF.
        assert_eq!(format_document("abc\r\n", &config), "abc\r\n");
        // final_newline = false: no trailing ending is added or kept, others stay CRLF.
        let no_final = FormatterConfig {
            final_newline: false,
            ..FormatterConfig::default()
        };
        assert_eq!(format_document("a\r\nb   \r\n", &no_final), "a\r\nb");
    }

    #[test]
    fn crlf_code_block_frontmatter_and_table_are_preserved() {
        let config = FormatterConfig::default();
        let code = "```\r\ncode with trailing spaces   \r\n\r\n\r\n\r\nmore\r\n```\r\n";
        assert_eq!(format_document(code, &config), code);

        let fm = "---\r\ntitle: x\r\n---\r\n\r\n# H\r\n";
        assert_eq!(format_document(fm, &config), fm);

        let table = "| a | b |\r\n|---|---|\r\n| 1 | 2 |\r\n";
        assert_eq!(
            format_document(table, &config),
            "| a   | b   |\r\n|-----|-----|\r\n| 1   | 2   |\r\n"
        );
    }

    #[test]
    fn a_lone_carriage_return_is_content_not_a_line_ending() {
        let out = format_document("a\rb\n", &FormatterConfig::default());
        assert_eq!(out, "a\rb\n");
    }

    #[test]
    fn links_are_normalised_before_tables_and_wrapping_so_one_pass_is_enough() {
        let config = FormatterConfig::default();
        // The link's normalised width decides the column width, in a single pass.
        let table = "| x | y |\n|---|---|\n| [[ a ]] | b |\n";
        let once = format_document(table, &config);
        assert_eq!(once, "| x     | y   |\n|-------|-----|\n| [[a]] | b   |\n");
        assert_eq!(format_document(&once, &config), once);

        // Wrapping measures links after normalisation, so a second pass changes nothing.
        let mut wrap = FormatterConfig::default();
        wrap.wrap.enable = true;
        wrap.line_width = 20;
        let text = "Inline code `[[ a ]]` and [[ b ]] then more words follow here.\n";
        let once = format_document(text, &wrap);
        assert_eq!(format_document(&once, &wrap), once);
        assert!(
            once.contains("`[[ a ]]`") && once.contains("[[b]]"),
            "{once:?}"
        );
    }

    #[test]
    fn crlf_formatting_is_idempotent() {
        let config = FormatterConfig::default();
        let once = format_document(&crlf(DIRTY_LF), &config);
        assert_eq!(format_document(&once, &config), once);
    }

    /// Same idempotency guarantee with paragraph wrapping ON, over every fixture plus paragraphs
    /// built to provoke the wrap pass (block markers at wrap points, hard breaks, long links,
    /// footnotes) at several widths.
    #[test]
    fn test_idempotent_with_wrap_enabled_over_fixtures_and_tricky_paragraphs() {
        let fixtures_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let mut sources: Vec<(String, String)> = collect_markdown_files(&fixtures_root)
            .into_iter()
            .map(|p| {
                let s = std::fs::read_to_string(&p).unwrap();
                (p.display().to_string(), s)
            })
            .collect();
        for tricky in [
            "aaaa bbbb cccc dddd - eeee 1. ffff # gggg > hhhh --- iiii\n",
            "first\\\nsecond line that has to wrap around a narrow width\n",
            "aaaa bbbb\\ cccc dddd eeee ffff gggg\n",
            "x [[tlp/sozluk#Olgu Bağlamı|olgu bağlamlarının]] y z w v u t s r q p o n m\n",
            "Body[^1] text goes on for a while here.\n\n[^1]: note text that is fairly long indeed.\n",
        ] {
            sources.push(("tricky".to_string(), tricky.to_string()));
        }

        for width in [20usize, 40, 80] {
            let mut config = FormatterConfig::default();
            config.wrap.enable = true;
            config.line_width = width;
            for (name, source) in &sources {
                let pass1 = format_document(source, &config);
                let pass2 = format_document(&pass1, &config);
                assert_eq!(
                    pass1, pass2,
                    "not idempotent with wrap at width {width}: {name} ({source:?})"
                );
            }
        }
    }

    fn collect_markdown_files(root: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if path
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("md"))
                {
                    out.push(path);
                }
            }
        }
        out
    }

    #[test]
    fn test_table_disabled_leaves_pipes_untouched() {
        let input = "|  A |B|\n|---|---|\n|1|2|\n";
        let mut config = FormatterConfig::default();
        config.tables.enable = false;
        let out = format_document(input, &config);
        assert_eq!(out, "|  A |B|\n|---|---|\n|1|2|\n");
    }

    #[test]
    fn test_table_formatting_end_to_end_idempotent() {
        let input = "Intro.\n\n|Name|Score|\n|:--|--:|\n|Alice|10|\n|Bob|9|\n\nOutro.\n";
        let config = FormatterConfig::default();
        let pass1 = format_document(input, &config);
        let pass2 = format_document(&pass1, &config);
        assert_eq!(pass1, pass2);
        assert!(pass1.contains("Intro."));
        assert!(pass1.contains("Outro."));
    }

    #[test]
    fn test_all_passes_disabled_leaves_content_untouched_besides_line_pass() {
        let input = "- a\n* b\n\n_ital_ **bold**\n\n---\n\n|a|b|\n|-|-|\n|1|2|\n";
        let mut config = FormatterConfig::default();
        config.tables.enable = false;
        config.lists.enable = false;
        config.emphasis.enable = false;
        config.misc.enable = false;
        let out = format_document(input, &config);
        assert_eq!(out, input);
    }

    #[test]
    fn test_kitchen_sink_all_passes_together_idempotent() {
        let config = FormatterConfig::default();
        let input = concat!(
            "# Mixed styles doc\n\n",
            "Some *italic*, some _also italic_, some **bold**, some __also bold__.\n\n",
            "- a\n",
            "* b\n",
            "+ c\n\n",
            "1. one\n",
            "1. two\n",
            "1. three\n\n",
            "- [ ] todo\n",
            "- [x] done\n\n",
            "***\n\n",
            "> quote line\n",
            ">no space\n\n",
            "~~~rust\nlet x = 1;\n~~~\n\n",
            "| Col A | Col B |\n",
            "|---|---|\n",
            "| 1 | 2 |\n",
        );
        let pass1 = format_document(input, &config);
        let pass2 = format_document(&pass1, &config);
        assert_eq!(pass1, pass2, "kitchen-sink document must be idempotent");
    }

    // ---- tables inside block quotes and list items ----

    fn formatted(source: &str) -> String {
        format_document(source, &FormatterConfig::default())
    }

    #[test]
    fn a_table_in_a_block_quote_is_aligned_and_keeps_its_markers() {
        assert_eq!(
            formatted("> | a | b |\n> |---|---|\n> | longer cell | 2 |\n"),
            "> | a           | b   |\n> |-------------|-----|\n> | longer cell | 2   |\n"
        );
    }

    #[test]
    fn a_table_in_a_nested_block_quote_keeps_every_marker() {
        assert_eq!(
            formatted("> > | a | b |\n> > |---|:-:|\n> > | xxxx | 2 |\n"),
            "> > | a    | b   |\n> > |------|:---:|\n> > | xxxx | 2   |\n"
        );
    }

    #[test]
    fn a_quote_without_spaces_after_the_marker_gets_one_on_every_line() {
        assert_eq!(
            formatted(">| a | b |\n>|---|---|\n>| xxxx | 2 |\n"),
            "> | a    | b   |\n> |------|-----|\n> | xxxx | 2   |\n"
        );
    }

    #[test]
    fn a_table_in_a_list_item_is_aligned_at_its_indentation() {
        assert_eq!(
            formatted("- item\n\n  | a | b |\n  |---|---|\n  | 1 | 2222 |\n"),
            "- item\n\n  | a   | b    |\n  |-----|------|\n  | 1   | 2222 |\n"
        );
        assert_eq!(
            formatted("1. item\n\n   | a | b |\n   |---|---|\n   | 1 | 2222 |\n"),
            "1. item\n\n   | a   | b    |\n   |-----|------|\n   | 1   | 2222 |\n"
        );
    }

    #[test]
    fn a_table_in_a_quote_or_list_is_formatted_once_and_stays_put() {
        for text in [
            "> | a | b |\n> |---|---|\n> | longer cell | 2 |\n",
            "> > | a | b |\n> > |---|---|\n> > | x | 2 |\n",
            "- item\n\n  | a | b |\n  |---|---|\n  | 1 | 2222 |\n",
        ] {
            let once = formatted(text);
            assert_eq!(formatted(&once), once, "{text:?}");
            let crlf = text.replace('\n', "\r\n");
            assert_eq!(
                formatted(&crlf),
                once.replace('\n', "\r\n"),
                "CRLF {text:?}"
            );
        }
    }

    #[test]
    fn pipes_that_are_not_cell_borders_stay_intact_in_a_quoted_table() {
        // An escaped pipe and a wikilink alias belong to their cell.
        let out = formatted("> | a | b |\n> |---|---|\n> | x \\| y | [[n\\|m]] |\n");
        assert!(out.contains("x \\| y"), "{out}");
        assert!(out.contains("[[n\\|m]]"), "{out}");
        assert_eq!(out.lines().count(), 3);
        assert!(out.lines().all(|l| l.starts_with("> ")), "{out}");
    }

    #[test]
    fn tables_the_prefix_rule_cannot_read_are_left_exactly_as_written() {
        for text in [
            // a quote inside a list item, and a list inside a quote
            "- item\n\n  > | a | b |\n  > |---|---|\n  > | 1 | 2 |\n",
            "> - item\n>\n>   | a | b |\n>   |---|---|\n>   | 1 | 2 |\n",
            // tab-indented table under a list item
            "- item\n\n\t| a | b |\n\t|---|---|\n\t| 1 | 2 |\n",
        ] {
            let out = formatted(text);
            assert_eq!(formatted(&out), out, "idempotent: {text:?}");
            // Nothing is lost: every cell text is still there.
            for cell in ["a", "b", "1", "2"] {
                assert!(out.contains(cell), "{cell} lost from {out:?}");
            }
        }
    }

    // ---- a table whose rows end in an empty extra cell is still aligned ----

    #[test]
    fn empty_extra_cells_at_the_end_of_a_row_do_not_stop_the_table_being_aligned() {
        assert_eq!(
            formatted("| a | b |\n|---|---|\n| 1 | 2 | |\n| longer | 4 |  |\n"),
            "| a      | b   |\n|--------|-----|\n| 1      | 2   |\n| longer | 4   |\n"
        );
        // A header with more cells than the delimiter row is not a table at all (GFM), so it stays.
        let not_a_table = "| a | b | |\n|---|---|\n| 1 | 2 |\n";
        assert_eq!(formatted(not_a_table), not_a_table);
        // The same in a block quote.
        assert_eq!(
            formatted("> | a | b |\n> |---|---|\n> | 1 | 2 | |\n"),
            "> | a   | b   |\n> |-----|-----|\n> | 1   | 2   |\n"
        );
    }

    #[test]
    fn a_row_with_a_real_extra_cell_still_leaves_the_table_exactly_as_written() {
        for text in [
            "| a | b |\n|---|---|\n| 1 | 2 | 3 |\n",
            "| a | b |\n|---|---|\n| 1 | 2 | | 3 |\n",
            "| a | b |\n|---|---|\n| 1 | 2 | x | |\n",
        ] {
            assert_eq!(formatted(text), text, "{text:?}");
        }
    }

    #[test]
    fn a_table_with_empty_extra_cells_is_stable_after_one_pass() {
        let once = formatted("| a | b |\n|---|---|\n| 1 | 2 | |\n");
        assert_eq!(formatted(&once), once);
        assert!(!once.contains("| |"), "{once:?}");
    }
}
