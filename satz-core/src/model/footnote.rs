use crate::model::range::ByteRange;

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct FootnoteDef {
    pub label: String,
    pub range: ByteRange,
}

impl FootnoteDef {
    pub fn new(label: String, range: ByteRange) -> Self {
        Self { label, range }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FootnoteTable {
    pub definitions: Vec<FootnoteDef>,
}
