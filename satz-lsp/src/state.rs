use std::collections::HashMap;
use std::path::{Path, PathBuf};

use satz_core::{Index, VaultConfig, walk_vault};

/// In-memory representation of an open text document.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct OpenDocument {
    pub uri: String,
    pub path: PathBuf,
    pub content: String,
    pub version: i32,
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

    /// Re-parses a single document upon changes and updates the index.
    pub fn reparse_document(&mut self, uri: &str, content: &str, path: &Path, version: i32) {
        let rel_path = match &self.vault_root {
            Some(root) => path.strip_prefix(root).unwrap_or(path),
            None => path,
        };

        let new_doc = satz_core::parse_document(content, rel_path);

        self.open_docs.insert(
            uri.to_string(),
            OpenDocument {
                uri: uri.to_string(),
                path: path.to_path_buf(),
                content: content.to_string(),
                version,
            },
        );

        self.index.replace_doc(new_doc);
    }

    /// Closes and untracks an open document.
    pub fn close_document(&mut self, uri: &str) {
        self.open_docs.remove(uri);
    }
}
