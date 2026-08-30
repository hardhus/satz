use std::collections::HashMap;
use std::path::{Path, PathBuf};

use ropey::Rope;
use satz_core::{Index, VaultConfig, walk_vault};
use tower_lsp_server::ls_types::TextDocumentContentChangeEvent;

use std::time::Instant;
use tokio::task::JoinHandle;

/// In-memory representation of an open text document with a Rope buffer.
#[allow(dead_code)]
pub struct OpenDocument {
    pub uri: String,
    pub path: PathBuf,
    pub rope: Rope,
    pub version: i32,
    pub first_change_at: Option<Instant>,
    pub pending_task: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for OpenDocument {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenDocument")
            .field("uri", &self.uri)
            .field("path", &self.path)
            .field("version", &self.version)
            .field("first_change_at", &self.first_change_at)
            .finish()
    }
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
            first_change_at: None,
            pending_task: None,
        }
    }
}

/// Simple (non-LRU) cache mapping a document's content hash to its already-computed formatted
/// text, used by `satz.formatWorkspace` to skip reformatting files whose content hasn't changed
/// since the last workspace-format call. Once at capacity, new distinct hashes are silently not
/// cached — existing entries keep serving hits rather than anything being evicted.
#[derive(Debug, Clone)]
pub struct FormatCache {
    entries: HashMap<u64, String>,
    capacity: usize,
}

impl FormatCache {
    pub fn new(capacity: usize) -> Self {
        Self {
            entries: HashMap::new(),
            capacity,
        }
    }

    pub fn get(&self, hash: u64) -> Option<&str> {
        self.entries.get(&hash).map(String::as_str)
    }

    pub fn insert(&mut self, hash: u64, formatted: String) {
        if self.entries.len() >= self.capacity && !self.entries.contains_key(&hash) {
            return;
        }
        self.entries.insert(hash, formatted);
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl Default for FormatCache {
    fn default() -> Self {
        Self::new(satz_core::config::LspConfig::default().format_cache_capacity)
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

    /// Flag indicating that open document identity keys changed and peers need diagnostic refresh
    pub peers_dirty: bool,

    /// `satz.formatWorkspace` result cache — see `FormatCache`.
    pub format_cache: FormatCache,
}

pub fn identity_keys(d: &satz_core::Document) -> std::collections::HashSet<String> {
    std::iter::once(satz_core::fold_key(&d.title))
        .chain(d.frontmatter.aliases.iter().map(|a| satz_core::fold_key(a)))
        .chain(
            d.path
                .file_stem()
                .and_then(|s| s.to_str())
                .map(satz_core::fold_key),
        )
        .collect()
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
        let format_cache = FormatCache::new(config.lsp.format_cache_capacity);

        Ok(Self {
            vault_root: Some(vault_root),
            index,
            open_docs: HashMap::new(),
            config,
            client_supports_pull_diagnostics: false,
            peers_dirty: false,
            format_cache,
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
        let rel_path_str = rel_path.to_string_lossy().replace('\\', "/");
        let doc_id = satz_core::DocId::new(&rel_path_str);

        let old_keys = self
            .index
            .get_doc(&doc_id)
            .map(identity_keys)
            .unwrap_or_default();
        let new_doc = satz_core::parse_document(content, &rel_path);
        let new_keys = identity_keys(&new_doc);

        if !old_keys.is_empty() && old_keys != new_keys {
            self.peers_dirty = true;
        }

        self.open_docs.insert(
            uri.to_string(),
            OpenDocument::new(uri, path.to_path_buf(), content, version),
        );

        self.index.replace_doc(new_doc);
    }

    /// Re-parses the in-memory rope content of an open document and updates index.
    pub fn reparse_open_document(&mut self, uri: &str) {
        let Some(open_doc) = self.open_docs.get_mut(uri) else {
            return;
        };
        open_doc.first_change_at = None;
        let path = open_doc.path.clone();
        let content = open_doc.rope.to_string();

        let rel_path = Self::get_rel_path(&path, self.vault_root.as_deref());
        let rel_path_str = rel_path.to_string_lossy().replace('\\', "/");
        let doc_id = satz_core::DocId::new(&rel_path_str);

        let old_keys = self
            .index
            .get_doc(&doc_id)
            .map(identity_keys)
            .unwrap_or_default();
        let new_doc = satz_core::parse_document(&content, &rel_path);
        let new_keys = identity_keys(&new_doc);

        if old_keys != new_keys {
            self.peers_dirty = true;
        }

        self.index.replace_doc(new_doc);
    }

    /// Re-parses a single document upon changes (full text) and updates the index.
    #[allow(dead_code)]
    pub fn reparse_document(&mut self, uri: &str, content: &str, path: &Path, version: i32) {
        self.open_document(uri, content, path, version);
    }

    /// Applies incremental changes to an open document and updates the index.
    #[allow(dead_code)]
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

        self.reparse_open_document(uri);
    }

    /// Closes and untracks an open document, aborting any background debounce tasks.
    pub fn close_document(&mut self, uri: &str) {
        if let Some(mut doc) = self.open_docs.remove(uri)
            && let Some(task) = doc.pending_task.take()
        {
            task.abort();
        }
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

    #[test]
    fn test_identity_keys_change_detected() {
        let doc1 = satz_core::parse_document(
            "---\ntitle: Eski Başlık\naliases: [alias1]\n---\n# Content",
            Path::new("doc.md"),
        );
        let doc2 = satz_core::parse_document(
            "---\ntitle: Yeni Başlık\naliases: [alias1]\n---\n# Content",
            Path::new("doc.md"),
        );

        let keys1 = identity_keys(&doc1);
        let keys2 = identity_keys(&doc2);

        assert_ne!(keys1, keys2);
        assert!(keys1.contains("eski baslik") || keys1.contains("eski başlık"));
        assert!(keys2.contains("yeni baslik") || keys2.contains("yeni başlık"));
    }

    #[test]
    fn test_debounce_and_max_wait_delay_calculation() {
        let debounce = std::time::Duration::from_millis(200);
        let max_wait = std::time::Duration::from_millis(500);

        // At t = 0
        let elapsed_0 = std::time::Duration::from_millis(0);
        let delay_0 = debounce.min(max_wait.saturating_sub(elapsed_0));
        assert_eq!(delay_0, std::time::Duration::from_millis(200));

        // At t = 100
        let elapsed_100 = std::time::Duration::from_millis(100);
        let delay_100 = debounce.min(max_wait.saturating_sub(elapsed_100));
        assert_eq!(delay_100, std::time::Duration::from_millis(200));

        // At t = 400 (max_wait capping)
        let elapsed_400 = std::time::Duration::from_millis(400);
        let delay_400 = debounce.min(max_wait.saturating_sub(elapsed_400));
        assert_eq!(delay_400, std::time::Duration::from_millis(100));

        // At t = 550 (max_wait exceeded)
        let elapsed_550 = std::time::Duration::from_millis(550);
        let delay_550 = debounce.min(max_wait.saturating_sub(elapsed_550));
        assert_eq!(delay_550, std::time::Duration::from_millis(0));
    }

    #[tokio::test]
    async fn test_async_debounced_task_execution() {
        use std::sync::Arc;
        use tokio::sync::RwLock;

        let state = Arc::new(RwLock::new(SatzState::default()));
        let uri = "file:///test.md";
        let path = Path::new("test.md");

        {
            let mut s = state.write().await;
            s.open_document(uri, "# Initial", path, 1);
        }

        // Send 3 rapid changes
        for i in 2..=4 {
            let mut s = state.write().await;
            if let Some(doc) = s.open_docs.get_mut(uri) {
                if let Some(prev) = doc.pending_task.take() {
                    prev.abort();
                }
                doc.rope = ropey::Rope::from_str(&format!("# Version {}", i));
                doc.version = i;

                let state_clone = state.clone();
                let uri_clone = uri.to_string();
                let handle = tokio::task::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                    let mut st = state_clone.write().await;
                    st.reparse_open_document(&uri_clone);
                });
                doc.pending_task = Some(handle);
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }

        // Wait for final debounced task to complete
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let s = state.read().await;
        let doc_id = satz_core::DocId::new("test.md");
        let parsed = s.index.get_doc(&doc_id).expect("Doc should exist in index");
        assert_eq!(parsed.title, "Version 4");
    }
}
