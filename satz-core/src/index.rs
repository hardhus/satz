pub mod build;
pub mod lookup;

use crate::model::DocId;
use std::path::Path;

pub use lookup::{Index, IndexStats, LinkResolution};

/// Document identity and resolution scheme.
pub trait IdScheme: Send + Sync {
    /// Generates a `DocId` from a vault-relative path.
    fn id_from_path(&self, vault_rel: &Path) -> DocId;
    /// Resolves a raw target string into a document ID using the index.
    fn resolve_raw<'a>(&self, raw_target: &str, index: &'a Index) -> Option<&'a DocId>;
}

/// Default path-based resolution scheme (compatible with Marksman and Obsidian).
#[derive(Debug, Clone, Copy, Default)]
pub struct PathScheme;

impl IdScheme for PathScheme {
    fn id_from_path(&self, vault_rel: &Path) -> DocId {
        DocId(vault_rel.to_string_lossy().replace('\\', "/"))
    }

    fn resolve_raw<'a>(&self, raw_target: &str, index: &'a Index) -> Option<&'a DocId> {
        index.resolve_link(raw_target)
    }
}
