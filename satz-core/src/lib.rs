pub mod config;
pub mod formatter;
pub mod index;
pub mod model;
pub mod parser;
pub mod slug;
pub mod template;
pub mod text;
pub mod walk;

pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

// Convenience re-exports
pub use config::VaultConfig;
pub use index::{IdScheme, Index, IndexStats, LinkResolution, PathScheme};
pub use model::{
    ByteRange, DocId, Document, FootnoteDef, FootnoteTable, Frontmatter, Heading, Link, LinkKind,
    Tag,
};
pub use parser::parse_document;
pub use slug::{fold_key, heading_matches, slugify};
pub use template::{generate_document_template, generate_frontmatter_block};
pub use text::{LineIndex, Position};
pub use walk::walk_vault;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_version() {
        assert!(!version().is_empty());
    }
}
