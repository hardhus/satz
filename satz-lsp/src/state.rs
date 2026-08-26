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
    pub content: String,
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
        let content_str = content.into();
        let rope = Rope::from_str(&content_str);
        Self {
            uri: uri.into(),
            path,
            rope,
            content: content_str,
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
        })
    }

    /// Handles opening a new document.
    pub fn open_document(&mut self, uri: &str, content: &str, path: &Path, version: i32) {
        let rel_path = match &self.vault_root {
            Some(root) => path.strip_prefix(root).unwrap_or(path),
            None => path,
        };

        let new_doc = satz_core::parse_document(content, rel_path);

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
        open_doc.content = content.clone();

        let rel_path = match &self.vault_root {
            Some(root) => path.strip_prefix(root).unwrap_or(&path),
            None => &path,
        };

        let new_doc = satz_core::parse_document(&content, rel_path);
        self.index.replace_doc(new_doc);
    }

    /// Closes and untracks an open document.
    pub fn close_document(&mut self, uri: &str) {
        self.open_docs.remove(uri);
    }
}
