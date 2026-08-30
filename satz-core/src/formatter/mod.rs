pub mod line_pass;
pub mod table;
pub mod zones;

use crate::config::FormatterConfig;
use crate::model::ByteRange;

/// Formats a markdown document deterministically according to the provided `FormatterConfig`.
///
/// Runs structure-aware passes before the line-based pass: currently just GFM table
/// re-alignment (`table`), spliced into the source via `zones::splice_ranges` so table cell
/// content is never touched by anything other than the table renderer itself. Future formatter
/// phases (lists, emphasis, ...) are added the same way — detect byte ranges, render each region
/// from scratch, splice, then let `line_pass` handle the remaining line-based rules.
pub fn format_document(source: &str, config: &FormatterConfig) -> String {
    let with_tables = if config.tables.enable {
        splice_tables(source, config)
    } else {
        source.to_string()
    };
    line_pass::run(&with_tables, config)
}

/// Detects every GFM table in `source` and replaces each with its canonically re-rendered form.
fn splice_tables(source: &str, config: &FormatterConfig) -> String {
    let structure = crate::parser::structure::parse_structure(source);
    if structure.table_spans.is_empty() {
        return source.to_string();
    }

    let mut spans = structure.table_spans.clone();
    spans.sort_by_key(|s| s.start);

    let replacements: Vec<(ByteRange, String)> = spans
        .into_iter()
        .map(|span| {
            let rendered = table::parse_table_block(source, span)
                .map(|block| table::render(&block, &config.tables))
                .unwrap_or_else(|| source[span.start..span.end].to_string());
            (span, rendered)
        })
        .collect();

    zones::splice_ranges(source, &replacements)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    /// Idempotency safety net: formatting every fixture in the test corpus must reach a fixed
    /// point on the first pass (format(format(x)) == format(x)). This is the regression guard
    /// that later formatter phases (lists, emphasis, ...) must not break.
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
}
