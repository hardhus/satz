pub mod line_pass;

use crate::config::FormatterConfig;

/// Formats a markdown document deterministically according to the provided `FormatterConfig`.
///
/// Currently a thin wrapper around the line-based pass (`line_pass::run`); future formatter
/// phases (tables, lists, emphasis, ...) are added as additional passes here without changing
/// this entry point's signature.
pub fn format_document(source: &str, config: &FormatterConfig) -> String {
    line_pass::run(source, config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    /// Idempotency safety net: formatting every fixture in the test corpus must reach a fixed
    /// point on the first pass (format(format(x)) == format(x)). This is the regression guard
    /// that later formatter phases (tables, lists, emphasis, ...) must not break.
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
                } else if path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("md")) {
                    out.push(path);
                }
            }
        }
        out
    }
}
