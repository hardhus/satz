use std::collections::HashMap;
use std::path::{Path, PathBuf};

use ropey::Rope;
use satz_core::{Index, VaultConfig, walk_vault};
use tower_lsp_server::ls_types::TextDocumentContentChangeEvent;

/// In-memory representation of an open text document with a Rope buffer.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct OpenDocument {
    pub uri: String,
    pub path: PathBuf,
    pub rope: Rope,
    pub version: i32,
}

#[allow(dead_code)]
impl OpenDocument {
    pub fn new(
        uri: impl Into<String>,
        path: PathBuf,
        content: impl Into<String>,
        version: i32,
    ) -> Self {
        let rope = Rope::from_str(&content.into());
        Self {
            uri: uri.into(),
            path,
            rope,
            version,
        }
    }
}

/// Global server state.
#[derive(Debug, Default)]
pub struct SatzState {
    /// Vault root path (from LSP initialize params)
    pub vault_root: Option<PathBuf>,

    /// In-memory vault index
    pub index: Index,

    /// Currently open documents tracked by the client
    pub open_docs: HashMap<String, OpenDocument>,

    /// Vault configuration (.satz.toml or default)
    pub config: VaultConfig,

    /// Whether the client supports pull diagnostics
    pub client_supports_pull_diagnostics: bool,
}

impl SatzState {
    /// Discovers and indexes all `.md` files in the vault.
    pub fn initialize_index(vault_root: PathBuf) -> anyhow::Result<Self> {
        let config_file = vault_root.join(".satz.toml");
        let config = std::fs::read_to_string(&config_file)
            .ok()
            .and_then(|s| VaultConfig::from_toml(&s).ok())
            .unwrap_or_default();

        let docs = walk_vault(&vault_root)?;
        let index = Index::build(docs);

        Ok(Self {
            vault_root: Some(vault_root),
            index,
            open_docs: HashMap::new(),
            config,
            client_supports_pull_diagnostics: false,
        })
    }

    pub fn get_rel_path(path: &Path, root: Option<&Path>) -> PathBuf {
        let Some(root) = root else {
            return path.to_path_buf();
        };
        if let Ok(rel) = path.strip_prefix(root) {
            return rel.to_path_buf();
        }

        let mut path_comps = path.components();
        for rc in root.components() {
            let mut clone_comps = path_comps.clone();
            match clone_comps.next() {
                Some(pc)
                    if satz_core::slug::fold_key(&pc.as_os_str().to_string_lossy())
                        == satz_core::slug::fold_key(&rc.as_os_str().to_string_lossy()) =>
                {
                    path_comps = clone_comps;
                }
                _ => return path.to_path_buf(),
            }
        }
        path_comps.as_path().to_path_buf()
    }

    /// Handles opening a new document.
    pub fn open_document(&mut self, uri: &str, content: &str, path: &Path, version: i32) {
        let rel_path = Self::get_rel_path(path, self.vault_root.as_deref());

        let new_doc = satz_core::parse_document(content, &rel_path);

        self.open_docs.insert(
            uri.to_string(),
            OpenDocument::new(uri, path.to_path_buf(), content, version),
        );

        self.index.replace_doc(new_doc);
    }

    /// Re-parses a single document upon changes (full text) and updates the index.
    #[allow(dead_code)]
    pub fn reparse_document(&mut self, uri: &str, content: &str, path: &Path, version: i32) {
        self.open_document(uri, content, path, version);
    }

    /// Applies incremental changes to an open document and updates the index.
    pub fn apply_changes(
        &mut self,
        uri: &str,
        changes: Vec<TextDocumentContentChangeEvent>,
        version: i32,
    ) {
        let Some(open_doc) = self.open_docs.get_mut(uri) else {
            return;
        };

        crate::sync::apply_changes_to_rope(&mut open_doc.rope, changes);
        open_doc.version = version;

        let path = open_doc.path.clone();
        let content = open_doc.rope.to_string();

        let rel_path = Self::get_rel_path(&path, self.vault_root.as_deref());

        let new_doc = satz_core::parse_document(&content, &rel_path);
        self.index.replace_doc(new_doc);
    }

    /// Closes and untracks an open document.
    pub fn close_document(&mut self, uri: &str) {
        self.open_docs.remove(uri);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_rel_path_with_turkish_vault_root() {
        let root = Path::new("/notlar/İş");
        let path = Path::new("/notlar/İş/projeler/proje1.md");
        let rel = SatzState::get_rel_path(path, Some(root));
        assert_eq!(rel, PathBuf::from("projeler/proje1.md"));

        // Case-insensitive test on Windows path format
        let root_win = Path::new("C:\\Notlar\\İş");
        let path_win = Path::new("c:\\notlar\\iş\\projeler\\proje1.md");
        let rel_win = SatzState::get_rel_path(path_win, Some(root_win));
        assert_eq!(rel_win, PathBuf::from("projeler\\proje1.md"));
    }
}
