use chrono::Local;

/// Generates a standard frontmatter block with title, date, aliases, and tags.
pub fn generate_frontmatter_block(title: &str, date: Option<&str>) -> String {
    let date_str = match date {
        Some(d) => d.to_string(),
        None => Local::now().format("%Y-%m-%d").to_string(),
    };

    format!(
        "---\ntitle: {}\ndate: {}\naliases: []\ntags: []\n---\n\n",
        yaml_scalar(title),
        date_str
    )
}

/// Generates complete initial document content with frontmatter and H1 heading.
pub fn generate_document_template(title: &str, date: Option<&str>) -> String {
    let block = generate_frontmatter_block(title, date);
    format!("{}# {}\n", block, heading_text(title))
}

/// A title as a YAML scalar: left plain when that is unambiguous, otherwise double-quoted with
/// escapes. A raw title such as `Q: what` or `x\naliases: [evil]` would otherwise make the
/// frontmatter invalid (dropping the note's title, aliases and tags) or inject extra keys.
fn yaml_scalar(title: &str) -> String {
    if is_plain_safe(title) {
        return title.to_string();
    }
    let mut out = String::with_capacity(title.len() + 2);
    out.push('"');
    for c in title.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Conservative test for "this text is read back as exactly this string when written unquoted".
/// Anything doubtful returns false, which only costs a pair of quotes.
fn is_plain_safe(title: &str) -> bool {
    let Some(first) = title.chars().next() else {
        return false;
    };
    if title != title.trim() || title.chars().any(|c| c.is_control()) || title.contains('\\') {
        return false;
    }
    // Leading indicators, and anything that could be read as a number, date or version.
    if first.is_ascii_digit()
        || matches!(
            first,
            '-' | '?'
                | ':'
                | ','
                | '['
                | ']'
                | '{'
                | '}'
                | '#'
                | '&'
                | '*'
                | '!'
                | '|'
                | '>'
                | '\''
                | '"'
                | '%'
                | '@'
                | '`'
                | '+'
                | '.'
        )
    {
        return false;
    }
    if title.contains(": ") || title.contains(" #") || title.ends_with(':') {
        return false;
    }
    // YAML 1.1 booleans and null.
    !matches!(
        title.to_ascii_lowercase().as_str(),
        "true" | "false" | "yes" | "no" | "on" | "off" | "y" | "n" | "null" | "~"
    )
}

/// The H1 text: a title containing line breaks would end the heading early, so its lines are
/// joined with single spaces.
fn heading_text(title: &str) -> String {
    if title.contains(['\n', '\r']) {
        title
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>()
            .join(" ")
    } else {
        title.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// Titles a plain (unquoted) YAML scalar can hold as-is.
    const PLAIN_TITLES: &[&str] = &[
        "My Note",
        "Türkçe Başlık",
        "Chapter one",
        "a-b",
        "with.dot",
        "snake_case_title",
        "sub/folder/name",
        "emoji ✓ 🎉",
        "a \"quote\" inside",
    ];

    /// Titles that would break, or be mis-typed by, a plain YAML scalar.
    const RISKY_TITLES: &[&str] = &[
        "Q: what",
        "note: sub",
        "ends:",
        "has #comment",
        "#hash",
        "- item",
        "? q",
        ": colon",
        "[a]",
        "{a}",
        "&anchor",
        "*alias",
        "!tag",
        "|literal",
        ">folded",
        "'single",
        "\"double",
        "%directive",
        "@at",
        "`tick",
        ",comma",
        "yes",
        "No",
        "TRUE",
        "off",
        "null",
        "~",
        "123",
        "2.01231",
        "2026-09-19",
        "1e3",
        "0x1F",
        " leading",
        "trailing ",
        "line1\nline2",
        "tab\there",
        "back\\slash",
        "carriage\rreturn",
        "x\naliases: [evil]",
        "x\n---\n# injected",
    ];

    /// A generated note plus what the real parser reads back from it.
    struct Doc {
        text: String,
        parsed: crate::model::Document,
    }

    fn doc_for(title: &str) -> Doc {
        let text = generate_document_template(title, Some("2026-01-01"));
        let parsed = crate::parser::parse_document(&text, Path::new("note.md"));
        Doc { text, parsed }
    }

    #[test]
    fn safe_titles_stay_plain() {
        for title in PLAIN_TITLES {
            let d = doc_for(title);
            assert!(
                d.text.contains(&format!("title: {title}\n")),
                "{title:?} should stay an unquoted scalar:\n{}",
                d.text
            );
            assert_eq!(
                d.parsed.frontmatter.title.as_deref(),
                Some(*title),
                "{title:?}"
            );
        }
    }

    #[test]
    fn risky_titles_are_quoted_and_read_back_unchanged() {
        for title in RISKY_TITLES {
            let d = doc_for(title);
            assert!(
                d.text.contains("title: \""),
                "{title:?} must be double-quoted:\n{}",
                d.text
            );
            assert_eq!(
                d.parsed.frontmatter.title.as_deref(),
                Some(*title),
                "{title:?} did not survive a write/read round trip:\n{}",
                d.text
            );
        }
    }

    #[test]
    fn a_title_can_never_inject_frontmatter_keys_or_extra_blocks() {
        for title in [
            "x\naliases: [evil]",
            "x\ntags: [evil]",
            "x\n---\n# injected",
            "a: b",
        ] {
            let d = doc_for(title);
            assert!(d.parsed.frontmatter.aliases.is_empty(), "{title:?}");
            assert!(d.parsed.frontmatter.tags.is_empty(), "{title:?}");
            assert_eq!(
                d.parsed.frontmatter.date.as_deref().map(|s| s.to_string()),
                Some("2026-01-01".to_string()),
                "{title:?}"
            );
            assert!(d.parsed.frontmatter_range.is_some(), "{title:?}");
        }
    }

    #[test]
    fn empty_and_blank_titles_still_produce_a_valid_note() {
        for title in ["", "   ", "\n"] {
            let d = doc_for(title);
            assert!(
                d.parsed.frontmatter_range.is_some(),
                "{title:?}:\n{}",
                d.text
            );
            assert_eq!(d.parsed.frontmatter.aliases.len(), 0);
        }
    }

    #[test]
    fn the_heading_line_never_contains_a_line_break() {
        let d = doc_for("line1\nline2\r\nline3");
        assert!(d.text.contains("\n# line1 line2 line3\n"), "{}", d.text);
        assert_eq!(d.parsed.headings.len(), 1);
    }

    #[test]
    fn frontmatter_block_alone_is_quoted_the_same_way() {
        let block = generate_frontmatter_block("Q: what", Some("2026-01-01"));
        assert!(block.starts_with("---\ntitle: \"Q: what\"\n"), "{block}");
    }

    #[test]
    fn test_template_generation() {
        let block = generate_frontmatter_block("My Note", Some("2026-08-30"));
        assert_eq!(
            block,
            "---\ntitle: My Note\ndate: 2026-08-30\naliases: []\ntags: []\n---\n\n"
        );

        let doc = generate_document_template("My Note", Some("2026-08-30"));
        assert_eq!(
            doc,
            "---\ntitle: My Note\ndate: 2026-08-30\naliases: []\ntags: []\n---\n\n# My Note\n"
        );
    }
}
