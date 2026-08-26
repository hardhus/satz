use crate::model::range::ByteRange;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BlockAnchor {
    pub id: String,
    pub range: ByteRange,
}

impl BlockAnchor {
    pub fn new(id: impl Into<String>, range: ByteRange) -> Self {
        Self {
            id: id.into(),
            range,
        }
    }
}
