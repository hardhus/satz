use crate::model::frontmatter::Frontmatter;
use serde_json::Value;

#[derive(Debug, thiserror::Error)]
pub enum FrontmatterError {
    #[error("YAML parse error: {0}")]
    YamlParse(String),
    #[error("Frontmatter is not a YAML mapping")]
    NotAMapping,
}

/// Parses raw YAML frontmatter into a strongly-typed `Frontmatter` struct.
///
/// Handles both single values and arrays for `aliases`/`alias` and `tags`/`tag`.
/// Unknown fields are preserved in `extra` without error.
pub fn parse_frontmatter(yaml_str: &str) -> Result<Frontmatter, FrontmatterError> {
    let trimmed = yaml_str.trim();
    if trimmed.is_empty() {
        return Ok(Frontmatter::default());
    }

    let parsed_val: Value =
        serde_saphyr::from_str(trimmed).map_err(|e| FrontmatterError::YamlParse(e.to_string()))?;

    let mut map = match parsed_val {
        Value::Object(m) => m,
        Value::Null => return Ok(Frontmatter::default()),
        _ => return Err(FrontmatterError::NotAMapping),
    };

    // 1. Title
    let title = map.remove("title").and_then(|v| match v {
        Value::String(s) => Some(s),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    });

    // 2. Aliases (support "aliases" and "alias")
    let mut aliases = Vec::new();
    let alias_val = map.remove("aliases").or_else(|| map.remove("alias"));
    if let Some(val) = alias_val {
        match val {
            Value::Array(arr) => {
                for item in arr {
                    if let Some(s) = value_to_string(item).filter(|s| !s.is_empty()) {
                        aliases.push(s);
                    }
                }
            }

            Value::String(s) => {
                if !s.is_empty() {
                    aliases.push(s);
                }
            }
            Value::Number(n) => {
                aliases.push(n.to_string());
            }
            _ => {}
        }
    }

    // 3. Tags (support "tags" and "tag")
    let mut tags = Vec::new();
    let tag_val = map.remove("tags").or_else(|| map.remove("tag"));
    if let Some(val) = tag_val {
        match val {
            Value::Array(arr) => {
                for item in arr {
                    if let Some(s) = value_to_string(item) {
                        let clean = s.trim().trim_start_matches('#').to_string();
                        if !clean.is_empty() {
                            tags.push(clean);
                        }
                    }
                }
            }
            Value::String(s) => {
                // If tags are comma or space separated or single
                let trimmed = s.trim();
                if trimmed.contains(',') {
                    for part in trimmed.split(',') {
                        let clean = part.trim().trim_start_matches('#').to_string();
                        if !clean.is_empty() {
                            tags.push(clean);
                        }
                    }
                } else {
                    let clean = trimmed.trim_start_matches('#').to_string();
                    if !clean.is_empty() {
                        tags.push(clean);
                    }
                }
            }
            Value::Number(n) => {
                tags.push(n.to_string());
            }
            _ => {}
        }
    }

    // 4. Date
    let date = map.remove("date").and_then(|v| match v {
        Value::String(s) => Some(s),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    });

    // 5. Remaining fields become `extra`
    Ok(Frontmatter {
        title,
        aliases,
        tags,
        date,
        extra: map,
    })
}

fn value_to_string(v: Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_frontmatter() {
        let fm = parse_frontmatter("").unwrap();
        assert_eq!(fm, Frontmatter::default());

        let fm = parse_frontmatter("   \n  ").unwrap();
        assert_eq!(fm, Frontmatter::default());
    }

    #[test]
    fn test_full_frontmatter() {
        let yaml = r#"
title: "My Note"
aliases:
  - note1
  - note2
tags:
  - felsefe
  - wittgenstein
date: "2024-01-01"
author: "Ludwig"
custom_number: 42
"#;
        let fm = parse_frontmatter(yaml).unwrap();
        assert_eq!(fm.title.as_deref(), Some("My Note"));
        assert_eq!(fm.aliases, vec!["note1", "note2"]);
        assert_eq!(fm.tags, vec!["felsefe", "wittgenstein"]);
        assert_eq!(fm.date.as_deref(), Some("2024-01-01"));
        assert_eq!(fm.extra.get("author").unwrap(), "Ludwig");
        assert_eq!(fm.extra.get("custom_number").unwrap(), 42);
    }

    #[test]
    fn test_singular_alias_and_tag() {
        let yaml = r#"
title: Single Test
alias: single-alias
tag: single-tag
"#;
        let fm = parse_frontmatter(yaml).unwrap();
        assert_eq!(fm.aliases, vec!["single-alias"]);
        assert_eq!(fm.tags, vec!["single-tag"]);
    }

    #[test]
    fn test_leading_hash_in_tags() {
        let yaml = r##"
tags:
  - "#tag1"
  - "#tag2/nested"
"##;
        let fm = parse_frontmatter(yaml).unwrap();
        assert_eq!(fm.tags, vec!["tag1", "tag2/nested"]);
    }
}
