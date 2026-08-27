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

    /// Checks if a link heading target (raw text or slug) matches this heading.
    pub fn matches(&self, link_heading: &str) -> bool {
        self.slug == link_heading || self.slug == crate::slug::slugify(link_heading)
    }
}
