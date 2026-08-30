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
        if c == 'İ' {
            slug.push('i');
            prev_is_dash = false;
        } else if c.is_alphanumeric() {
            for lc in c.to_lowercase() {
                if lc != '\u{0307}' {
                    slug.push(lc);
                }
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

/// Index anahtarları için birleşik katlama. slugify ile aynı İ/U+0307 davranışı.
pub fn fold_key_ext(s: &str, turkish_i_folding: bool) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_space = false;
    for c in s.trim().chars() {
        if c == 'İ' || (turkish_i_folding && (c == 'I' || c == 'ı')) {
            out.push('i');
            prev_space = false;
            continue;
        }
        if c == 'i' {
            out.push('i');
            prev_space = false;
            continue;
        }
        if c.is_whitespace() {
            if !prev_space {
                out.push(' ');
                prev_space = true;
            }
            continue;
        }
        prev_space = false;
        for lc in c.to_lowercase() {
            if lc != '\u{0307}' {
                if turkish_i_folding && lc == 'ı' {
                    out.push('i');
                } else {
                    out.push(lc);
                }
            }
        }
    }
    out
}

/// Index anahtarları için varsayılan birleşik katlama.
pub fn fold_key(s: &str) -> String {
    fold_key_ext(s, false)
}

/// Checks if a link heading target (raw text or slug) matches a heading.
pub fn heading_matches(link_heading: &str, h: &crate::model::Heading) -> bool {
    h.matches(link_heading)
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

    #[test]
    fn test_fold_key_basic() {
        assert_eq!(fold_key("İstemciler"), "istemciler");
        assert_eq!(fold_key("istemciler"), "istemciler");
        assert_eq!(fold_key("İSTEMCİLER"), "istemciler");
        assert_eq!(
            fold_key("  Language   Server  Protocol  "),
            "language server protocol"
        );
    }

    #[test]
    fn test_turkish_i_folding_option() {
        // With turkish_i_folding = false (Obsidian default)
        assert_eq!(fold_key_ext("Işık", false), "işık");
        assert_eq!(fold_key_ext("ışık", false), "ışık");
        assert_eq!(fold_key_ext("İstemciler", false), "istemciler");
        assert_eq!(fold_key_ext("Istemciler", false), "istemciler");
        assert_eq!(fold_key_ext("ıstemciler", false), "ıstemciler");

        // With turkish_i_folding = true (Fold both I/ı and İ/i into i)
        assert_eq!(fold_key_ext("Işık", true), "işik");
        assert_eq!(fold_key_ext("ışık", true), "işik");
        assert_eq!(fold_key_ext("İstemciler", true), "istemciler");
        assert_eq!(fold_key_ext("Istemciler", true), "istemciler");
        assert_eq!(fold_key_ext("istemciler", true), "istemciler");
        assert_eq!(fold_key_ext("ıstemciler", true), "istemciler");
    }
}
