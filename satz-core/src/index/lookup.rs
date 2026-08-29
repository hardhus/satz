use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::model::{DocId, Document, Link, LinkKind};
use crate::slug::fold_key;

/// In-memory vault index.
#[derive(Debug, Default)]
pub struct Index {
    pub(crate) docs: HashMap<DocId, Document>,
    pub(crate) by_path: HashMap<PathBuf, DocId>,
    pub(crate) by_stem: HashMap<String, DocId>,
    pub(crate) by_title_alias: HashMap<String, DocId>,
    pub(crate) backlinks: HashMap<DocId, HashSet<DocId>>,
    pub(crate) tags: HashMap<String, HashSet<DocId>>,
    pub(crate) broken_link_count: usize,
}

impl Index {
    /// Returns an iterator over all indexed documents.
    pub fn documents(&self) -> impl Iterator<Item = &Document> {
        self.docs.values()
    }

    /// Total number of indexed documents.
    pub fn doc_count(&self) -> usize {
        self.docs.len()
    }

    /// Total number of links across all documents.
    pub fn total_links(&self) -> usize {
        self.docs.values().map(|d| d.links.len()).sum()
    }

    /// Total number of broken internal links (calculated at index build time).
    pub fn broken_link_count(&self) -> usize {
        self.broken_link_count
    }

    /// Resolves a raw link target (e.g. `"file"`, `"folder/file"`, or alias/title) to a `DocId`.
    ///
    /// Priority:
    /// 1. Exact path match (`by_path`)
    /// 2. Path with `.md` extension appended (`by_path`)
    /// 3. Stem match (`by_stem`)
    /// 4. Lowercase title or alias match (`by_title_alias`)
    pub fn resolve_link(&self, raw_target: &str) -> Option<&DocId> {
        let normalized = raw_target.replace('\\', "/");
        let as_path = PathBuf::from(&normalized);
        if let Some(id) = self.by_path.get(&as_path) {
            return Some(id);
        }

        let with_ext = PathBuf::from(format!("{}.md", normalized));
        if let Some(id) = self.by_path.get(&with_ext) {
            return Some(id);
        }

        if let Some(stem) = as_path.file_stem().and_then(|s| s.to_str()) {
            let stem_lower = fold_key(stem);
            if let Some(id) = self.by_stem.get(&stem_lower) {
                return Some(id);
            }
        }

        self.by_title_alias.get(&fold_key(raw_target))
    }

    /// Retrieves a document by its `DocId`.
    pub fn get_doc(&self, id: &DocId) -> Option<&Document> {
        self.docs.get(id)
    }

    /// Retrieves a document by its vault-relative `Path`.
    pub fn get_doc_by_path(&self, path: &Path) -> Option<&Document> {
        self.by_path.get(path).and_then(|id| self.docs.get(id))
    }

    /// Returns an iterator over all document IDs that link to the given `id`.
    pub fn backlinks_of(&self, id: &DocId) -> impl Iterator<Item = &DocId> {
        self.backlinks.get(id).into_iter().flat_map(|s| s.iter())
    }

    /// Returns an iterator over documents with no incoming backlinks (orphan notes).
    pub fn orphan_docs(&self) -> impl Iterator<Item = &Document> {
        self.docs.values().filter(|d| {
            !self.backlinks.contains_key(&d.id)
                || self.backlinks.get(&d.id).is_some_and(|s| s.is_empty())
        })
    }

    /// Returns an iterator over documents tagged with the specified tag name (case-insensitive).
    pub fn docs_with_tag<'a>(&'a self, tag: &str) -> impl Iterator<Item = &'a Document> + 'a {
        let clean = fold_key(tag.trim_start_matches('#'));
        self.tags
            .get(&clean)
            .into_iter()
            .flat_map(|ids| ids.iter())
            .filter_map(|id| self.docs.get(id))
    }

    /// Returns a sorted list of all unique tag names in the vault.
    pub fn all_tags(&self) -> Vec<&str> {
        let mut tags: Vec<&str> = self.tags.keys().map(|s| s.as_str()).collect();
        tags.sort_unstable();
        tags
    }

    /// Returns an iterator of documents containing broken internal links, along with the broken link items.
    pub fn docs_with_broken_links(&self) -> impl Iterator<Item = (&Document, Vec<&Link>)> {
        self.docs.values().filter_map(|doc| {
            let broken: Vec<&Link> = doc
                .links
                .iter()
                .filter(|l| {
                    matches!(
                        l.kind,
                        LinkKind::WikiLink | LinkKind::Embed | LinkKind::Markdown
                    ) && !l.target_doc.is_empty()
                        && !l.target_doc.starts_with("http://")
                        && !l.target_doc.starts_with("https://")
                        && self.resolve_link(&l.target_doc).is_none()
                })
                .collect();
            if broken.is_empty() {
                None
            } else {
                Some((doc, broken))
            }
        })
    }

    /// Replaces or inserts a document in the index, updating paths, title/aliases, tags, and backlinks.
    pub fn replace_doc(&mut self, new_doc: Document) {
        let id = new_doc.id.clone();

        // If old doc exists, clean up old references
        if let Some(old_doc) = self.docs.get(&id) {
            // Remove old tags
            for tag in &old_doc.tags {
                let tag_key = fold_key(tag.name.trim_start_matches('#'));
                if let Some(set) = self.tags.get_mut(&tag_key) {
                    set.remove(&id);
                    if set.is_empty() {
                        self.tags.remove(&tag_key);
                    }
                }
            }

            // Remove old title and aliases from by_title_alias if pointing to this doc
            let old_title_key = fold_key(&old_doc.title);
            if self.by_title_alias.get(&old_title_key) == Some(&id) {
                self.by_title_alias.remove(&old_title_key);
            }
            for alias in &old_doc.frontmatter.aliases {
                let alias_key = fold_key(alias);
                if self.by_title_alias.get(&alias_key) == Some(&id) {
                    self.by_title_alias.remove(&alias_key);
                }
            }

            // Remove old stem
            let old_stem_key = old_doc
                .path
                .file_stem()
                .and_then(|s| s.to_str())
                .map(fold_key)
                .unwrap_or_default();
            if !old_stem_key.is_empty() && self.by_stem.get(&old_stem_key) == Some(&id) {
                self.by_stem.remove(&old_stem_key);
            }

            // Remove old path
            let old_normalized = PathBuf::from(old_doc.path.to_string_lossy().replace('\\', "/"));
            if self.by_path.get(&old_normalized) == Some(&id) {
                self.by_path.remove(&old_normalized);
            }
            if self.by_path.get(&old_doc.path) == Some(&id) {
                self.by_path.remove(&old_doc.path);
            }

            // Remove outgoing backlinks from old_doc
            for link in &old_doc.links {
                if matches!(
                    link.kind,
                    LinkKind::WikiLink | LinkKind::Embed | LinkKind::Markdown
                ) && !link.target_doc.is_empty()
                    && !link.target_doc.starts_with("http://")
                    && !link.target_doc.starts_with("https://")
                    && let Some(target_id) = self.resolve_link(&link.target_doc).cloned()
                    && let Some(set) = self.backlinks.get_mut(&target_id)
                {
                    set.remove(&id);
                    if set.is_empty() {
                        self.backlinks.remove(&target_id);
                    }
                }
            }
        }

        // Insert new path
        let normalized_path = PathBuf::from(new_doc.path.to_string_lossy().replace('\\', "/"));
        self.by_path.insert(normalized_path, id.clone());
        self.by_path.insert(new_doc.path.clone(), id.clone());

        // Insert new stem
        let new_stem_key = new_doc
            .path
            .file_stem()
            .and_then(|s| s.to_str())
            .map(fold_key)
            .unwrap_or_default();
        if !new_stem_key.is_empty() {
            match self.by_stem.entry(new_stem_key) {
                std::collections::hash_map::Entry::Occupied(e) => {
                    tracing::warn!("stem conflict: '{}' (keeping first entry)", e.key());
                }
                std::collections::hash_map::Entry::Vacant(e) => {
                    e.insert(id.clone());
                }
            }
        }

        // Insert new title and aliases
        let title_key = fold_key(&new_doc.title);
        self.by_title_alias.insert(title_key, id.clone());
        for alias in &new_doc.frontmatter.aliases {
            let alias_key = fold_key(alias);
            self.by_title_alias.insert(alias_key, id.clone());
        }

        // Insert new tags
        for tag in &new_doc.tags {
            let tag_key = fold_key(tag.name.trim_start_matches('#'));
            self.tags.entry(tag_key).or_default().insert(id.clone());
        }

        // Insert new outgoing backlinks
        for link in &new_doc.links {
            if matches!(
                link.kind,
                LinkKind::WikiLink | LinkKind::Embed | LinkKind::Markdown
            ) && !link.target_doc.is_empty()
                && !link.target_doc.starts_with("http://")
                && !link.target_doc.starts_with("https://")
                && let Some(target_id) = self.resolve_link(&link.target_doc).cloned()
            {
                self.backlinks
                    .entry(target_id)
                    .or_default()
                    .insert(id.clone());
            }
        }

        self.docs.insert(id, new_doc);
    }

    /// Removes a document from the index.
    pub fn remove_doc(&mut self, id: &DocId) {
        if let Some(old_doc) = self.docs.remove(id) {
            for tag in &old_doc.tags {
                let tag_key = fold_key(tag.name.trim_start_matches('#'));
                if let Some(set) = self.tags.get_mut(&tag_key) {
                    set.remove(id);
                    if set.is_empty() {
                        self.tags.remove(&tag_key);
                    }
                }
            }

            let old_title_key = fold_key(&old_doc.title);
            if self.by_title_alias.get(&old_title_key) == Some(id) {
                self.by_title_alias.remove(&old_title_key);
            }
            for alias in &old_doc.frontmatter.aliases {
                let alias_key = fold_key(alias);
                if self.by_title_alias.get(&alias_key) == Some(id) {
                    self.by_title_alias.remove(&alias_key);
                }
            }

            let old_stem_key = old_doc
                .path
                .file_stem()
                .and_then(|s| s.to_str())
                .map(fold_key)
                .unwrap_or_default();
            if !old_stem_key.is_empty() && self.by_stem.get(&old_stem_key) == Some(id) {
                self.by_stem.remove(&old_stem_key);
            }

            let old_normalized = PathBuf::from(old_doc.path.to_string_lossy().replace('\\', "/"));
            if self.by_path.get(&old_normalized) == Some(id) {
                self.by_path.remove(&old_normalized);
            }
            if self.by_path.get(&old_doc.path) == Some(id) {
                self.by_path.remove(&old_doc.path);
            }

            // Remove outgoing backlinks
            for link in &old_doc.links {
                if matches!(
                    link.kind,
                    LinkKind::WikiLink | LinkKind::Embed | LinkKind::Markdown
                ) && !link.target_doc.is_empty()
                    && !link.target_doc.starts_with("http://")
                    && !link.target_doc.starts_with("https://")
                    && let Some(target_id) = self.resolve_link(&link.target_doc).cloned()
                    && let Some(set) = self.backlinks.get_mut(&target_id)
                {
                    set.remove(id);
                    if set.is_empty() {
                        self.backlinks.remove(&target_id);
                    }
                }
            }

            self.backlinks.remove(id);
        }
    }

    /// Generates summary statistics of the indexed vault.
    pub fn stats(&self) -> IndexStats {
        let total_headings = self.docs.values().map(|d| d.headings.len()).sum();
        let total_words = self
            .docs
            .values()
            .map(|d| d.line_index.source().split_whitespace().count())
            .sum();

        IndexStats {
            doc_count: self.doc_count(),
            total_links: self.total_links(),
            broken_links: self.broken_link_count,
            unique_tags: self.tags.len(),
            orphan_docs: self.orphan_docs().count(),
            total_headings,
            total_words,
        }
    }
}

/// Summary statistics of an indexed vault.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct IndexStats {
    pub doc_count: usize,
    pub total_links: usize,
    pub broken_links: usize,
    pub unique_tags: usize,
    pub orphan_docs: usize,
    pub total_headings: usize,
    pub total_words: usize,
}
