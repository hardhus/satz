use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::model::{DocId, Document, Link, LinkKind};
use crate::slug::fold_key;

/// Result of resolving a link against the index.
#[derive(Debug, Clone, PartialEq)]
pub enum LinkResolution<'a> {
    /// The target document exists and the requested anchor (heading or block) was found (or no anchor was requested).
    Resolved {
        doc: &'a Document,
        anchor: Option<crate::model::range::ByteRange>,
    },
    /// The target document exists, but the specified heading or block anchor was not found.
    AnchorMissing { doc: &'a Document },
    /// The target document could not be resolved.
    DocMissing,
}

/// In-memory vault index.
#[derive(Debug, Default)]
pub struct Index {
    pub(crate) docs: HashMap<DocId, Document>,
    pub(crate) by_path: HashMap<PathBuf, DocId>,
    pub(crate) by_stem: HashMap<String, DocId>,
    pub(crate) by_title_alias: HashMap<String, DocId>,
    pub(crate) backlinks: HashMap<DocId, HashSet<DocId>>,
    pub(crate) tags: HashMap<String, HashSet<DocId>>,
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

    /// Total number of broken internal links (calculated on demand).
    pub fn broken_link_count(&self) -> usize {
        self.docs_with_broken_links()
            .map(|(_, links)| links.len())
            .sum()
    }

    /// Resolves a raw link target (e.g. `"file"`, `"folder/file"`, or alias/title) to a `DocId`.
    ///
    /// Priority:
    /// 1. Exact path match (`by_path`)
    /// 2. Path with `.md` extension appended (`by_path`)
    /// 3. Stem match (`by_stem`)
    /// 4. Lowercase title or alias match (`by_title_alias`)
    pub fn resolve_link(&self, raw_target: &str) -> Option<&DocId> {
        // Fast path: if target has no path separators or extension, check stem & title/alias directly
        if !raw_target.contains('/') && !raw_target.contains('\\') && !raw_target.contains('.') {
            let folded = fold_key(raw_target);
            if let Some(id) = self.by_stem.get(&folded) {
                return Some(id);
            }
            if let Some(id) = self.by_title_alias.get(&folded) {
                return Some(id);
            }
            let as_path = Path::new(raw_target);
            if let Some(id) = self.by_path.get(as_path) {
                return Some(id);
            }
            return None;
        }

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

    /// Fully resolves a `Link` against the index, checking both document existence and heading/block anchors.
    pub fn resolve_link_full<'a>(
        &'a self,
        link: &Link,
        current_doc: Option<&'a Document>,
    ) -> LinkResolution<'a> {
        let heading_empty = link
            .target_heading
            .as_deref()
            .is_none_or(|h| h.trim().is_empty());
        let block_empty = link
            .target_block
            .as_deref()
            .is_none_or(|b| b.trim().is_empty());
        if link.target_doc.is_empty() && heading_empty && block_empty {
            return match current_doc {
                Some(d) => LinkResolution::Resolved {
                    doc: d,
                    anchor: None,
                },
                None => LinkResolution::DocMissing,
            };
        }

        let target_doc = if link.target_doc.is_empty() {
            match current_doc {
                Some(d) => d,
                None => return LinkResolution::DocMissing,
            }
        } else if let Some(target_id) = self.resolve_link(&link.target_doc) {
            match self.get_doc(target_id) {
                Some(d) => d,
                None => return LinkResolution::DocMissing,
            }
        } else {
            return LinkResolution::DocMissing;
        };

        if let Some(block_id) = &link.target_block {
            if let Some(b) = target_doc.blocks.iter().find(|b| &b.id == block_id) {
                LinkResolution::Resolved {
                    doc: target_doc,
                    anchor: Some(b.range),
                }
            } else {
                LinkResolution::AnchorMissing { doc: target_doc }
            }
        } else if let Some(heading_ref) = &link.target_heading {
            let link_slug = crate::slug::slugify(heading_ref);
            if let Some(h) = target_doc
                .headings
                .iter()
                .find(|h| h.matches_slug(&link_slug) || h.matches(heading_ref))
            {
                LinkResolution::Resolved {
                    doc: target_doc,
                    anchor: Some(h.range),
                }
            } else {
                LinkResolution::AnchorMissing { doc: target_doc }
            }
        } else {
            LinkResolution::Resolved {
                doc: target_doc,
                anchor: None,
            }
        }
    }

    /// Retrieves a document by its `DocId`.
    pub fn get_doc(&self, id: &DocId) -> Option<&Document> {
        self.docs.get(id)
    }

    /// Retrieves a document by its vault-relative `Path`.
    pub fn get_doc_by_path(&self, path: &Path) -> Option<&Document> {
        let normalized = PathBuf::from(path.to_string_lossy().replace('\\', "/"));
        self.by_path
            .get(&normalized)
            .and_then(|id| self.docs.get(id))
    }

    /// Returns an iterator over all document IDs that link to the given `id`.
    pub fn backlinks_of(&self, id: &DocId) -> impl Iterator<Item = &DocId> {
        self.backlinks.get(id).into_iter().flat_map(|s| s.iter())
    }

    /// Returns an iterator over documents with no incoming backlinks (orphan notes).
    pub fn orphan_docs(&self) -> impl Iterator<Item = &Document> {
        self.docs.values().filter(|d| {
            self.backlinks
                .get(&d.id)
                .is_none_or(|s| s.iter().all(|id| id == &d.id))
        })
    }

    /// Returns an iterator over documents tagged with the specified tag name (case-insensitive and hierarchical prefix matching).
    pub fn docs_with_tag<'a>(&'a self, tag: &str) -> impl Iterator<Item = &'a Document> + 'a {
        let clean = fold_key(tag.trim_start_matches('#'));
        let prefix = format!("{}/", clean);
        let mut matched_ids = std::collections::HashSet::new();

        for (k, ids) in &self.tags {
            if k == &clean || k.starts_with(&prefix) {
                for id in ids {
                    matched_ids.insert(id);
                }
            }
        }

        matched_ids.into_iter().filter_map(|id| self.docs.get(id))
    }

    /// Returns a sorted list of all unique tag names in the vault.
    pub fn all_tags(&self) -> Vec<&str> {
        let mut tags: Vec<&str> = self.tags.keys().map(|s| s.as_str()).collect();
        tags.sort_unstable();
        tags
    }

    /// Returns an iterator of documents containing broken internal links, along with the broken link items and resolution status.
    pub fn docs_with_broken_links(
        &self,
    ) -> impl Iterator<Item = (&Document, Vec<(&Link, LinkResolution<'_>)>)> {
        self.docs.values().filter_map(|doc| {
            let broken: Vec<(&Link, LinkResolution)> = doc
                .links
                .iter()
                .filter_map(|l| {
                    if matches!(
                        l.kind,
                        LinkKind::WikiLink | LinkKind::Embed | LinkKind::Markdown
                    ) && !l.target_doc.starts_with("http://")
                        && !l.target_doc.starts_with("https://")
                    {
                        let res = self.resolve_link_full(l, Some(doc));
                        if matches!(
                            res,
                            LinkResolution::DocMissing | LinkResolution::AnchorMissing { .. }
                        ) {
                            Some((l, res))
                        } else {
                            None
                        }
                    } else {
                        None
                    }
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
            // Remove outgoing backlinks from old_doc FIRST
            for link in &old_doc.links {
                if matches!(
                    link.kind,
                    LinkKind::WikiLink | LinkKind::Embed | LinkKind::Markdown
                ) && !link.target_doc.starts_with("http://")
                    && !link.target_doc.starts_with("https://")
                {
                    let is_degenerate = link.target_doc.is_empty()
                        && link
                            .target_heading
                            .as_deref()
                            .is_none_or(|h| h.trim().is_empty())
                        && link
                            .target_block
                            .as_deref()
                            .is_none_or(|b| b.trim().is_empty());

                    let target_id = if is_degenerate {
                        None
                    } else if link.target_doc.is_empty() {
                        Some(id.clone())
                    } else {
                        self.resolve_link(&link.target_doc).cloned()
                    };
                    if let Some(target_id) = target_id
                        && let Some(set) = self.backlinks.get_mut(&target_id)
                    {
                        set.remove(&id);
                        if set.is_empty() {
                            self.backlinks.remove(&target_id);
                        }
                    }
                }
            }

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
        }

        // Insert new path
        let normalized_path = PathBuf::from(new_doc.path.to_string_lossy().replace('\\', "/"));
        self.by_path.insert(normalized_path, id.clone());

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
            ) && !link.target_doc.starts_with("http://")
                && !link.target_doc.starts_with("https://")
            {
                let is_degenerate = link.target_doc.is_empty()
                    && link
                        .target_heading
                        .as_deref()
                        .is_none_or(|h| h.trim().is_empty())
                    && link
                        .target_block
                        .as_deref()
                        .is_none_or(|b| b.trim().is_empty());

                let target_id = if is_degenerate {
                    None
                } else if link.target_doc.is_empty() {
                    Some(id.clone())
                } else {
                    self.resolve_link(&link.target_doc).cloned()
                };
                if let Some(target_id) = target_id {
                    self.backlinks
                        .entry(target_id)
                        .or_default()
                        .insert(id.clone());
                }
            }
        }

        self.docs.insert(id, new_doc);
    }

    /// Removes a document from the index.
    pub fn remove_doc(&mut self, id: &DocId) {
        if let Some(old_doc) = self.docs.remove(id) {
            // Remove outgoing backlinks
            for link in &old_doc.links {
                if matches!(
                    link.kind,
                    LinkKind::WikiLink | LinkKind::Embed | LinkKind::Markdown
                ) && !link.target_doc.starts_with("http://")
                    && !link.target_doc.starts_with("https://")
                {
                    let is_degenerate = link.target_doc.is_empty()
                        && link
                            .target_heading
                            .as_deref()
                            .is_none_or(|h| h.trim().is_empty())
                        && link
                            .target_block
                            .as_deref()
                            .is_none_or(|b| b.trim().is_empty());

                    let target_id = if is_degenerate {
                        None
                    } else if link.target_doc.is_empty() {
                        Some(id.clone())
                    } else {
                        self.resolve_link(&link.target_doc).cloned()
                    };
                    if let Some(target_id) = target_id
                        && let Some(set) = self.backlinks.get_mut(&target_id)
                    {
                        set.remove(id);
                        if set.is_empty() {
                            self.backlinks.remove(&target_id);
                        }
                    }
                }
            }

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
            broken_links: self.broken_link_count(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse_document;

    #[test]
    fn empty_and_degenerate_links_are_silent() {
        let content = "# Test\n\nEmpty: [[]] and [[#]] and [[|sadece-display]]";
        let doc = parse_document(content, Path::new("test.md"));
        let index = Index::build(vec![doc.clone()]);

        // 0 broken links (degenerate links are resolved silently as intra-doc without diagnostics)
        assert_eq!(index.broken_link_count(), 0);

        // 0 incoming backlinks (degenerate links produce 0 backlinks)
        let doc_id = DocId::new("test.md");
        assert_eq!(index.backlinks_of(&doc_id).count(), 0);

        // Individual full resolution
        for link in &doc.links {
            let res = index.resolve_link_full(link, Some(&doc));
            assert!(
                matches!(res, LinkResolution::Resolved { anchor: None, .. }),
                "Expected Resolved anchor: None for link: {:?}",
                link
            );
        }
    }

    #[test]
    fn test_fast_path_resolve_equivalence() {
        let doc1 = parse_document(
            "---\ntitle: Rust Rehberi\naliases: [Guide, Rehber]\n---\n# Rust Rehberi\nİçerik",
            Path::new("books/rust.md"),
        );
        let doc2 = parse_document("# Genel Not\nİçerik", Path::new("genel.md"));
        let index = Index::build(vec![doc1, doc2]);

        // Targets without slashes/extensions (fast path candidates)
        let fast_candidates = vec![
            "rust",
            "Rust",
            "RUST",
            "guide",
            "Guide",
            "rehber",
            "Rust Rehberi",
            "genel",
            "Genel",
            "Genel Not",
            "nonexistent",
        ];

        for target in fast_candidates {
            let res = index.resolve_link(target);
            if target.eq_ignore_ascii_case("nonexistent") {
                assert_eq!(res, None);
            } else if target.to_lowercase().contains("rust")
                || target.eq_ignore_ascii_case("guide")
                || target.eq_ignore_ascii_case("rehber")
            {
                assert_eq!(res, Some(&DocId::new("books/rust.md")));
            } else {
                assert_eq!(res, Some(&DocId::new("genel.md")));
            }
        }

        // Targets with paths/extensions
        assert_eq!(
            index.resolve_link("books/rust"),
            Some(&DocId::new("books/rust.md"))
        );
        assert_eq!(
            index.resolve_link("books/rust.md"),
            Some(&DocId::new("books/rust.md"))
        );
        assert_eq!(
            index.resolve_link("books\\rust.md"),
            Some(&DocId::new("books/rust.md"))
        );
    }
}
