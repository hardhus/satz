/// Generates an Obsidian-compatible heading slug.
/// e.g. "Merhaba Dünya! (2024)" -> "merhaba-dünya-2024"
///
/// Rules:
/// 1. Unicode lowercase
/// 2. Non-alphanumeric Unicode characters (except ASCII alphanumeric & Unicode letters/numbers) replaced by '-'
/// 3. Consecutive '-' characters collapsed into a single '-'
/// 4. Leading and trailing '-' characters trimmed
pub fn slugify(text: &str) -> String {
    let mut slug = String::with_capacity(text.len());
    let mut prev_is_dash = false;

    for c in text.chars() {
        if c.is_alphanumeric() {
            for lc in c.to_lowercase() {
                slug.push(lc);
            }
            prev_is_dash = false;
        } else if !prev_is_dash && !slug.is_empty() {
            slug.push('-');
            prev_is_dash = true;
        }
    }

    // Trim trailing dashes
    while slug.ends_with('-') {
        slug.pop();
    }

    slug
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_slugify_basic() {
        assert_eq!(slugify("Hello World"), "hello-world");
        assert_eq!(slugify("Merhaba Dünya"), "merhaba-dünya");
        assert_eq!(
            slugify("Section 1.1: Introduction!"),
            "section-1-1-introduction"
        );
        assert_eq!(slugify("   multiple   spaces  "), "multiple-spaces");
        assert_eq!(slugify("---dashes---"), "dashes");
        assert_eq!(slugify(""), "");
    }
}
