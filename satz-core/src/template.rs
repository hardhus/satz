use chrono::Local;

/// Generates a standard frontmatter block with title, date, aliases, and tags.
pub fn generate_frontmatter_block(title: &str, date: Option<&str>) -> String {
    let date_str = match date {
        Some(d) => d.to_string(),
        None => Local::now().format("%Y-%m-%d").to_string(),
    };

    format!(
        "---\ntitle: {}\ndate: {}\naliases: []\ntags: []\n---\n\n",
        title, date_str
    )
}

/// Generates complete initial document content with frontmatter and H1 heading.
pub fn generate_document_template(title: &str, date: Option<&str>) -> String {
    let block = generate_frontmatter_block(title, date);
    format!("{}# {}\n", block, title)
}

#[cfg(test)]
mod tests {
    use super::*;

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
