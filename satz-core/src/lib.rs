pub mod config;
pub mod model;
pub mod parser;
pub mod slug;
pub mod text;

pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

// Convenience re-exports
pub use config::VaultConfig;
pub use model::{
    ByteRange, DocId, Document, FootnoteDef, FootnoteTable, Frontmatter, Heading, Link, LinkKind,
    Tag,
};
pub use parser::parse_document;
pub use slug::slugify;
pub use text::{LineIndex, Position};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_version() {
        assert!(!version().is_empty());
    }
}
