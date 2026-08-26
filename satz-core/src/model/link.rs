use crate::model::range::ByteRange;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum LinkKind {
    /// `[[target]]` or `[[target#heading]]` or `[[target|display]]`
    WikiLink,
    /// `[text](url)` or `[text](url#heading)`
    Markdown,
    /// `[^label]` reference
    Footnote,
    /// `![[target]]` or `![[target#heading]]`
    Embed,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct Link {
    pub kind: LinkKind,
    /// Target document path/name, or empty string `""` for same-document links
    pub target_doc: String,
    /// Target heading if specified (`#heading`)
    pub target_heading: Option<String>,
    /// Target block anchor if specified (`#^block-id`)
    pub target_block: Option<String>,
    /// Custom display text / alias (e.g. `[[target|display]]` or `[display](target)`)
    pub display: Option<String>,
    /// Byte range of the entire link syntax
    pub range: ByteRange,
}

impl Link {
    pub fn new(
        kind: LinkKind,
        target_doc: String,
        target_heading: Option<String>,
        target_block: Option<String>,
        display: Option<String>,
        range: ByteRange,
    ) -> Self {
        Self {
            kind,
            target_doc,
            target_heading,
            target_block,
            display,
            range,
        }
    }
}
