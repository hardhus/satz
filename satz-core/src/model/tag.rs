use crate::model::range::ByteRange;

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct Tag {
    /// Tag name without the leading `#` (e.g. "felsefe" or "topic/sub")
    pub name: String,
    /// Byte range in the document encompassing the whole tag (including `#` for body tags)
    pub range: ByteRange,
}

impl Tag {
    pub fn new(name: String, range: ByteRange) -> Self {
        Self { name, range }
    }
}
