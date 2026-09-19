pub mod diff;
pub mod emphasis;
pub mod line_pass;
pub mod list;
pub mod misc;
pub mod table;
pub mod wrap;
pub mod zones;

use crate::config::FormatterConfig;
use crate::model::ByteRange;

/// Formats a markdown document deterministically according to the provided `FormatterConfig`.
///
/// Three stages, in order:
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
/// 3. `line_pass` (trim/blank-line/heading-spacing/final-newline) runs on the result.
///
/// Stage 1's ranges never overlap each other by construction: every construct there replaces
/// only marker/fence bytes or (for tables) a region whose inner content is deliberately
/// reproduced verbatim rather than re-examined — see `table::parse_table_block`. One consequence
/// of that verbatim-cell policy: emphasis/list/etc. markers *inside* a table cell are not
/// separately normalized by this pass (they're copied as-is by the table renderer); this is an
/// accepted, narrow scope limitation, not a correctness bug.
pub fn format_document(source: &str, config: &FormatterConfig) -> String {
    let structure = crate::parser::structure::parse_structure(source);
    let mut replacements: Vec<(ByteRange, String)> = Vec::new();

    if config.tables.enable {
        let mut spans = structure.table_spans.clone();
        spans.sort_by_key(|s| s.start);
        for span in spans {
            let rendered = table::parse_table_block(source, span)
                .map(|block| table::render(&block, &config.tables))
                .unwrap_or_else(|| source[span.start..span.end].to_string());
            replacements.push((span, rendered));
        }
    }

    if config.lists.enable {
        replacements.extend(list::replacements(
            source,
            &structure.list_items,
            &structure.task_markers,
            &config.lists,
        ));
    }

    if config.emphasis.enable {
        replacements.extend(emphasis::replacements(
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

    line_pass::run(&wrapped, config)
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
}
