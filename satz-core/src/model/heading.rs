use crate::model::range::ByteRange;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Heading {
    pub level: u8,
    pub text: String,
    pub slug: String,
    pub range: ByteRange,
}

impl Heading {
    pub fn new(level: u8, text: String, slug: String, range: ByteRange) -> Self {
        Self {
            level,
            text,
            slug,
            range,
        }
    }

    /// Splits a heading's text from a trailing ` ^block-id` (`Old ^blk` -> `("Old", Some("blk"))`).
    /// The id is `[A-Za-z0-9-]+`, must be separated from the text by whitespace and the text
    /// must not be empty; anything else is returned unchanged.
    pub fn split_block_id(text: &str) -> (&str, Option<&str>) {
        let is_blank = |c: char| c == ' ' || c == '\t';
        let trimmed = text.trim_end_matches(is_blank);
        let Some(caret) = trimmed.rfind('^') else {
            return (text, None);
        };
        let id = &trimmed[caret + 1..];
        let before = &trimmed[..caret];
        if id.is_empty()
            || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
            || !before.ends_with(is_blank)
        {
            return (text, None);
        }
        let name = before.trim_end_matches(is_blank);
        if name.is_empty() {
            return (text, None);
        }
        (name, Some(id))
    }

    /// Checks if a precomputed link slug matches this heading.
    pub fn matches_slug(&self, link_slug: &str) -> bool {
        self.slug == link_slug
    }

    /// Checks if a link heading target (raw text or slug) matches this heading.
    pub fn matches(&self, link_heading: &str) -> bool {
        self.slug == link_heading
            || self.text.eq_ignore_ascii_case(link_heading)
            || self.slug == crate::slug::slugify(link_heading)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse_document;
    use std::path::Path;

    #[test]
    fn a_trailing_block_id_is_split_off() {
        for (text, expected) in [
            ("Old ^blk", ("Old", Some("blk"))),
            ("Old  ^a-1", ("Old", Some("a-1"))),
            ("Old\t^X9", ("Old", Some("X9"))),
            ("Two words ^id", ("Two words", Some("id"))),
            ("Old ^blk  ", ("Old", Some("blk"))),
            ("Türkçe Başlık ^id", ("Türkçe Başlık", Some("id"))),
        ] {
            assert_eq!(Heading::split_block_id(text), expected, "{text:?}");
        }
    }

    #[test]
    fn anything_that_is_not_a_block_id_suffix_is_left_alone() {
        for text in [
            "",
            "Old",
            "^blk",
            " ^blk",
            "a^b",
            "Old ^",
            "Old ^a_b",
            "Old ^a b",
            "Old ^é",
            "Old^blk",
            "2 ^3 power",
        ] {
            assert_eq!(Heading::split_block_id(text), (text, None), "{text:?}");
        }
    }

    fn only_heading(md: &str) -> Heading {
        let doc = parse_document(md, Path::new("a.md"));
        assert_eq!(doc.headings.len(), 1, "{md:?}");
        doc.headings.into_iter().next().unwrap()
    }

    #[test]
    fn a_parsed_heading_has_its_block_id_removed_from_text_and_slug() {
        for md in [
            "## Old ^blk\n",
            "## Old ^blk",
            "## Old ^blk ##\n",
            "Old ^blk\n===\n",
            "## Old ^blk\r\n",
        ] {
            let h = only_heading(md);
            assert_eq!(h.text, "Old", "{md:?}");
            assert_eq!(h.slug, "old", "{md:?}");
            assert!(h.matches("Old") && h.matches("old"), "{md:?}");
        }
    }

    #[test]
    fn headings_that_only_look_like_they_have_a_block_id_keep_their_text() {
        assert_eq!(only_heading("## ^blk\n").text, "^blk");
        assert_eq!(only_heading("## a^b\n").text, "a^b");
        assert_eq!(
            only_heading("## Exponent 2 ^3 power\n").text,
            "Exponent 2 ^3 power"
        );
    }
}
