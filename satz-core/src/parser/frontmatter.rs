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

    // 2. Aliases: `aliases` and `alias` are both read (Obsidian accepts either spelling).
    let mut aliases: Vec<String> = Vec::new();
    for key in ["aliases", "alias"] {
        if let Some(val) = map.remove(key) {
            for alias in string_items(val, false) {
                if !alias.is_empty() && !aliases.contains(&alias) {
                    aliases.push(alias);
                }
            }
        }
    }

    // 3. Tags: `tags` and `tag`. A plain string holds several tags separated by commas or spaces;
    // list items are taken whole. Like a body tag, a tag needs at least one letter.
    let mut tags: Vec<String> = Vec::new();
    for key in ["tags", "tag"] {
        if let Some(val) = map.remove(key) {
            for item in string_items(val, true) {
                let clean = item.trim().trim_start_matches('#').to_string();
                if clean.chars().any(char::is_alphabetic) && !tags.contains(&clean) {
                    tags.push(clean);
                }
            }
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

/// The strings a frontmatter list value holds: each item of a list whole, or -- for a plain string
/// -- the whole string (`split` false) or its comma/space separated parts (`split` true).
fn string_items(value: Value, split: bool) -> Vec<String> {
    match value {
        Value::Array(items) => items.into_iter().filter_map(value_to_string).collect(),
        Value::String(s) if split => s
            .split(|c: char| c == ',' || c.is_whitespace())
            .filter(|part| !part.is_empty())
            .map(str::to_string)
            .collect(),
        Value::String(s) => vec![s],
        Value::Number(n) => vec![n.to_string()],
        _ => Vec::new(),
    }
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

    fn tags_of(yaml: &str) -> Vec<String> {
        parse_frontmatter(yaml).unwrap().tags
    }

    fn aliases_of(yaml: &str) -> Vec<String> {
        parse_frontmatter(yaml).unwrap().aliases
    }

    #[test]
    fn a_plain_string_of_tags_is_split_on_commas_and_spaces() {
        assert_eq!(tags_of("tags: a b c"), vec!["a", "b", "c"]);
        assert_eq!(tags_of("tags: a, b"), vec!["a", "b"]);
        assert_eq!(tags_of("tags: a, b c"), vec!["a", "b", "c"]);
        assert_eq!(tags_of("tags: \"#a #b\""), vec!["a", "b"]);
        assert_eq!(tags_of("tags: şeker ünlü"), vec!["şeker", "ünlü"]);
        assert_eq!(tags_of("tags: single"), vec!["single"]);
        assert!(tags_of("tags: \"\"").is_empty());
        assert!(tags_of("tags: \"   \"").is_empty());
    }

    #[test]
    fn list_items_are_never_split() {
        assert_eq!(tags_of("tags: [a, \"b c\"]"), vec!["a", "b c"]);
        assert_eq!(
            aliases_of("aliases: [\"two words\", x]"),
            vec!["two words", "x"]
        );
        // A single string alias is one alias, spaces and all.
        assert_eq!(aliases_of("aliases: two words"), vec!["two words"]);
    }

    #[test]
    fn both_spellings_of_the_key_are_used() {
        assert_eq!(tags_of("tags: a\ntag: b"), vec!["a", "b"]);
        assert_eq!(tags_of("tag: b\ntags: [a]"), vec!["a", "b"]);
        assert_eq!(tags_of("tags: [a, b]\ntag: b"), vec!["a", "b"]);
        // (A bare `y` would be a YAML boolean, so other letters are used.)
        assert_eq!(aliases_of("aliases: x\nalias: z"), vec!["x", "z"]);
        assert_eq!(aliases_of("aliases: [x, w]\nalias: w"), vec!["x", "w"]);
        // Neither key is left behind as an unknown field.
        let fm = parse_frontmatter("tags: a\ntag: b\naliases: x\nalias: z").unwrap();
        assert!(fm.extra.is_empty(), "{:?}", fm.extra);
    }

    #[test]
    fn a_tag_needs_a_letter_like_a_body_tag() {
        assert_eq!(tags_of("tags: [2024, x, 12/3, 2024-05]"), vec!["x"]);
        assert!(tags_of("tags: 2024").is_empty());
        assert_eq!(tags_of("tags: y2024"), vec!["y2024"]);
        // Numbers are still fine as aliases.
        assert_eq!(aliases_of("aliases: [2024]"), vec!["2024"]);
        assert_eq!(aliases_of("aliases: 7"), vec!["7"]);
    }
}
