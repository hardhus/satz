use crate::config::FormatterConfig;

/// Line-based formatting pass: trailing whitespace, frontmatter/code-block preservation,
/// blank-line and heading spacing normalization, wikilink whitespace trimming, final newline.
pub fn run(source: &str, config: &FormatterConfig) -> String {
    if source.is_empty() {
        return if config.final_newline {
            "\n".to_string()
        } else {
            String::new()
        };
    }

    let mut lines: Vec<String> = Vec::new();
    let mut in_frontmatter = false;
    let mut in_code_block = false;
    let mut code_fence = "";

    // 1. Initial pass: split into lines and handle frontmatter / code blocks / trailing whitespace / link normalization
    let raw_lines: Vec<&str> = source.lines().collect();
    for (i, raw_line) in raw_lines.iter().enumerate() {
        let trimmed_end = raw_line.trim_end();

        // Check frontmatter boundary
        if i == 0 && (trimmed_end == "---" || trimmed_end == "+++") {
            in_frontmatter = true;
            lines.push(trimmed_end.to_string());
            continue;
        }

        if in_frontmatter {
            if trimmed_end == "---" || trimmed_end == "+++" {
                in_frontmatter = false;
            }
            lines.push(trimmed_end.to_string());
            continue;
        }

        // Check code block fence
        let trimmed_start = trimmed_end.trim_start();
        if trimmed_start.starts_with("```") || trimmed_start.starts_with("~~~") {
            if in_code_block {
                if trimmed_start.starts_with(code_fence) {
                    in_code_block = false;
                    code_fence = "";
                }
            } else {
                in_code_block = true;
                code_fence = if trimmed_start.starts_with("```") {
                    "```"
                } else {
                    "~~~"
                };
            }
            lines.push(trimmed_end.to_string());
            continue;
        }

        if in_code_block {
            // Inside code block: preserve as is
            lines.push(raw_line.to_string());
            continue;
        }

        // Normal text line
        let mut processed = trimmed_end.to_string();

        // Link normalization if enabled
        if config.normalize_links {
            processed = normalize_wikilinks_in_line(&processed);
        }

        lines.push(processed);
    }

    // 2. Normalize blank lines and headings
    let mut result_lines: Vec<String> = Vec::new();
    let mut consecutive_blanks = 0;
    let mut in_fm = false;
    let mut in_cb = false;

    for (i, line) in lines.iter().enumerate() {
        if i == 0 && (line == "---" || line == "+++") {
            in_fm = true;
            result_lines.push(line.clone());
            continue;
        }
        if in_fm {
            if line == "---" || line == "+++" {
                in_fm = false;
            }
            result_lines.push(line.clone());
            continue;
        }

        let trimmed_start = line.trim_start();
        if trimmed_start.starts_with("```") || trimmed_start.starts_with("~~~") {
            in_cb = !in_cb;
            result_lines.push(line.clone());
            consecutive_blanks = 0;
            continue;
        }

        if in_cb {
            result_lines.push(line.clone());
            continue;
        }

        let is_heading = is_atx_heading(line);
        let is_blank = line.trim().is_empty();

        if is_heading {
            // Ensure configured blank lines before heading (if not at very start of content)
            let needed_before = config.blank_lines_around_headings as usize;
            if !result_lines.is_empty() {
                // Remove existing trailing blanks
                while let Some(last) = result_lines.last() {
                    if last.trim().is_empty() {
                        result_lines.pop();
                    } else {
                        break;
                    }
                }
                // Don't add blank lines if previous was frontmatter closing or empty doc
                let after_fm = result_lines.last().map(|s| s.as_str()) == Some("---");
                let target_blanks = if after_fm { 1 } else { needed_before.max(1) };
                for _ in 0..target_blanks {
                    result_lines.push(String::new());
                }
            }

            result_lines.push(line.clone());
            consecutive_blanks = 0;
            continue;
        }

        if is_blank {
            consecutive_blanks += 1;
            if consecutive_blanks <= 1 {
                result_lines.push(String::new());
            }
        } else {
            consecutive_blanks = 0;
            result_lines.push(line.clone());
        }
    }

    // 3. Join with newlines and apply final newline rule
    let mut formatted = result_lines.join("\n");

    // Clean any trailing whitespace / extra newlines at EOF
    formatted = formatted.trim_end().to_string();

    if config.final_newline && !formatted.is_empty() {
        formatted.push('\n');
    }

    formatted
}

fn is_atx_heading(line: &str) -> bool {
    let trimmed = line.trim_start();
    let bytes = trimmed.as_bytes();
    let mut level = 0;
    while level < bytes.len() && bytes[level] == b'#' {
        level += 1;
    }
    (1..=6).contains(&level) && bytes.get(level) == Some(&b' ')
}

fn normalize_wikilinks_in_line(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut cursor = 0;

    while let Some(start) = line[cursor..].find("[[") {
        let abs_start = cursor + start;
        out.push_str(&line[cursor..abs_start]);

        if let Some(end) = line[abs_start + 2..].find("]]") {
            let abs_end = abs_start + 2 + end;
            let inner = &line[abs_start + 2..abs_end];

            if let Some((target, display)) = inner.split_once('|') {
                let norm_target = target.trim();
                let norm_display = display.trim();
                out.push_str(&format!("[[{}|{}]]", norm_target, norm_display));
            } else {
                let norm_target = inner.trim();
                out.push_str(&format!("[[{}]]", norm_target));
            }

            cursor = abs_end + 2;
        } else {
            out.push_str("[[");
            cursor = abs_start + 2;
        }
    }

    out.push_str(&line[cursor..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_trailing_whitespace_removal() {
        let input = "Line 1   \nLine 2\t\t\nLine 3";
        let config = FormatterConfig::default();
        let formatted = run(input, &config);
        assert_eq!(formatted, "Line 1\nLine 2\nLine 3\n");
    }

    #[test]
    fn test_consecutive_blank_lines_collapsed() {
        let input = "Line 1\n\n\n\n\nLine 2";
        let config = FormatterConfig::default();
        let formatted = run(input, &config);
        assert_eq!(formatted, "Line 1\n\nLine 2\n");
    }

    #[test]
    fn test_link_normalization() {
        let input = "See [[  note a  ]] and [[  note b  |  alias b  ]].";
        let config = FormatterConfig::default();
        let formatted = run(input, &config);
        assert_eq!(formatted, "See [[note a]] and [[note b|alias b]].\n");
    }

    #[test]
    fn test_code_block_preserved_verbatim() {
        let input = "```rust\nlet x = 1;   \n\n\nlet y = 2;\n```";
        let config = FormatterConfig::default();
        let formatted = run(input, &config);
        assert_eq!(formatted, "```rust\nlet x = 1;   \n\n\nlet y = 2;\n```\n");
    }

    #[test]
    fn test_heading_spacing() {
        let input = "# Heading 1\nContent\n\n\n\n## Heading 2\nContent 2";
        let config = FormatterConfig::default();
        let formatted = run(input, &config);
        assert_eq!(
            formatted,
            "# Heading 1\nContent\n\n## Heading 2\nContent 2\n"
        );
    }

    #[test]
    fn test_frontmatter_preserved() {
        let input = "---\ntitle: Note Title\ntags: [a, b]\n---\n\n\n# Heading\nContent";
        let config = FormatterConfig::default();
        let formatted = run(input, &config);
        assert_eq!(
            formatted,
            "---\ntitle: Note Title\ntags: [a, b]\n---\n\n# Heading\nContent\n"
        );
    }

    #[test]
    fn test_formatter_idempotence() {
        let input = "# Heading\n\nText with [[  link  ]] and code:\n\n```\nfoo\n```\n";
        let config = FormatterConfig::default();
        let pass1 = run(input, &config);
        let pass2 = run(&pass1, &config);
        assert_eq!(pass1, pass2);
    }
}
