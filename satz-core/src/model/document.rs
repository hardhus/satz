use std::path::{Path, PathBuf};

use crate::model::footnote::FootnoteTable;
use crate::model::frontmatter::Frontmatter;
use crate::model::heading::Heading;
use crate::model::link::Link;
use crate::model::tag::Tag;
use crate::text::LineIndex;

#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct DocId(pub String);

impl DocId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for DocId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Document {
    pub id: DocId,
    pub path: PathBuf,
    pub title: String,
    pub frontmatter: Frontmatter,
    pub headings: Vec<Heading>,
    pub links: Vec<Link>,
    pub tags: Vec<Tag>,
    pub footnotes: FootnoteTable,
    pub line_index: LineIndex,
    pub content_hash: u64,
}

impl Document {
    /// Resolves the document title according to priority:
    /// 1. `frontmatter.title` (if non-empty)
    /// 2. First level 1 heading (`# Heading 1`)
    /// 3. File stem (e.g. "note" for "notes/note.md")
    /// 4. Fallback: "Untitled"
    pub fn resolve_title(frontmatter: &Frontmatter, headings: &[Heading], path: &Path) -> String {
        if let Some(t) = &frontmatter.title {
            let trimmed = t.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }

        if let Some(h1) = headings.iter().find(|h| h.level == 1) {
            let trimmed = h1.text.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }

        if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
            let trimmed = stem.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }

        "Untitled".to_string()
    }
}
