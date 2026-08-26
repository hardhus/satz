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
}
