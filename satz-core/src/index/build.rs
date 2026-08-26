use std::path::PathBuf;

use crate::index::lookup::Index;
use crate::model::{Document, LinkKind};

impl Index {
    /// Builds an in-memory index from a collection of parsed `Document`s.
    ///
    /// Performs a two-pass construction:
    /// - Pass 1: Populates documents, path lookups, title/alias lookups, and tags.
    /// - Pass 2: Resolves link targets and populates the backlink graph and broken link count.
    pub fn build(docs: Vec<Document>) -> Self {
        let mut index = Index::default();

        // Pass 1: Register all documents, paths, aliases, and tags
        for doc in docs {
            let normalized_path = PathBuf::from(doc.path.to_string_lossy().replace('\\', "/"));
            index.by_path.insert(normalized_path, doc.id.clone());
            index.by_path.insert(doc.path.clone(), doc.id.clone());

            let title_key = doc.title.to_lowercase();
            if index.by_title_alias.contains_key(&title_key) {
                tracing::warn!(
                    "title conflict: '{}' (overwriting previous entry)",
                    title_key
                );
            }
            index.by_title_alias.insert(title_key, doc.id.clone());

            for alias in &doc.frontmatter.aliases {
                let alias_key = alias.to_lowercase();
                if index.by_title_alias.contains_key(&alias_key) {
                    tracing::warn!(
                        "alias conflict: '{}' (overwriting previous entry)",
                        alias_key
                    );
                }
                index.by_title_alias.insert(alias_key, doc.id.clone());
            }

            for tag in &doc.tags {
                let tag_key = tag.name.trim_start_matches('#').to_lowercase();
                index
                    .tags
                    .entry(tag_key)
                    .or_default()
                    .insert(doc.id.clone());
            }

            index.docs.insert(doc.id.clone(), doc);
        }

        // Pass 2: Resolve links and compute incoming backlinks and broken links
        let all_ids: Vec<_> = index.docs.keys().cloned().collect();
        let mut broken = 0usize;

        for src_id in &all_ids {
            let doc = &index.docs[src_id];
            for link in &doc.links {
                match link.kind {
                    LinkKind::WikiLink | LinkKind::Embed => {
                        if link.target_doc.is_empty() {
                            continue;
                        }
                        if let Some(target_id) = index.resolve_link(&link.target_doc).cloned() {
                            index
                                .backlinks
                                .entry(target_id)
                                .or_default()
                                .insert(src_id.clone());
                        } else {
                            broken += 1;
                        }
                    }
                    LinkKind::Markdown => {
                        if link.target_doc.is_empty()
                            || link.target_doc.starts_with("http://")
                            || link.target_doc.starts_with("https://")
                        {
                            continue;
                        }
                        if let Some(target_id) = index.resolve_link(&link.target_doc).cloned() {
                            index
                                .backlinks
                                .entry(target_id)
                                .or_default()
                                .insert(src_id.clone());
                        } else {
                            broken += 1;
                        }
                    }
                    LinkKind::Footnote => {}
                }
            }
        }

        index.broken_link_count = broken;
        index
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::parser::parse_document;

    #[test]
    fn test_build_empty_index() {
        let index = Index::build(vec![]);
        assert_eq!(index.doc_count(), 0);
        assert_eq!(index.total_links(), 0);
        assert_eq!(index.broken_link_count(), 0);
    }

    #[test]
    fn test_build_basic_backlinks() {
        let doc_a_src = "# Note A\n\nLinks to [[note-b]].";
        let doc_b_src = "# Note B\n\nContent.";

        let doc_a = parse_document(doc_a_src, Path::new("note-a.md"));
        let doc_b = parse_document(doc_b_src, Path::new("note-b.md"));

        let index = Index::build(vec![doc_a, doc_b]);
        assert_eq!(index.doc_count(), 2);

        let b_id = index.resolve_link("note-b").unwrap();
        let backlinks: Vec<&str> = index.backlinks_of(b_id).map(|id| id.as_str()).collect();
        assert_eq!(backlinks, vec!["note-a.md"]);
    }

    #[test]
    fn test_build_alias_resolution() {
        let doc_src = "---\ntitle: Long Title\naliases: [short-alias]\n---\n# Long Title";
        let doc = parse_document(doc_src, Path::new("notes/long.md"));

        let index = Index::build(vec![doc]);
        assert!(index.resolve_link("notes/long.md").is_some());
        assert!(index.resolve_link("notes/long").is_some());
        assert!(index.resolve_link("Long Title").is_some());
        assert!(index.resolve_link("short-alias").is_some());
        assert!(index.resolve_link("nonexistent").is_none());
    }

    #[test]
    fn test_build_tag_index() {
        let doc_a = parse_document(
            "---\ntags: [Rust, Web]\n---\n# Doc A\n#coding",
            Path::new("a.md"),
        );
        let doc_b = parse_document("---\ntags: [rust]\n---\n# Doc B", Path::new("b.md"));

        let index = Index::build(vec![doc_a, doc_b]);
        let rust_docs: Vec<&str> = index.docs_with_tag("rust").map(|d| d.id.as_str()).collect();
        assert_eq!(rust_docs.len(), 2);

        let web_docs: Vec<&str> = index.docs_with_tag("Web").map(|d| d.id.as_str()).collect();
        assert_eq!(web_docs.len(), 1);

        let coding_docs: Vec<&str> = index
            .docs_with_tag("#coding")
            .map(|d| d.id.as_str())
            .collect();
        assert_eq!(coding_docs.len(), 1);
    }

    #[test]
    fn test_orphan_and_broken_links() {
        let doc_a = parse_document(
            "# Doc A\n\nLinks to [[missing-doc]] and [Ext](https://example.com).",
            Path::new("a.md"),
        );
        let doc_b = parse_document("# Doc B\n\nNo links here.", Path::new("b.md"));

        let index = Index::build(vec![doc_a, doc_b]);
        assert_eq!(index.broken_link_count(), 1);

        let orphans: Vec<&str> = index.orphan_docs().map(|d| d.id.as_str()).collect();
        // Both A and B are orphans since nothing links to them
        assert_eq!(orphans.len(), 2);
    }

    #[test]
    fn test_replace_doc_updates_state() {
        let doc_v1 = parse_document(
            "---\ntitle: Old Title\ntags: [rust]\n---\n# Old Title",
            Path::new("test.md"),
        );
        let mut index = Index::build(vec![doc_v1]);

        assert!(index.resolve_link("Old Title").is_some());
        assert_eq!(index.docs_with_tag("rust").count(), 1);

        let doc_v2 = parse_document(
            "---\ntitle: New Title\ntags: [python]\n---\n# New Title",
            Path::new("test.md"),
        );
        index.replace_doc(doc_v2);

        assert!(index.resolve_link("Old Title").is_none());
        assert!(index.resolve_link("New Title").is_some());
        assert_eq!(index.docs_with_tag("rust").count(), 0);
        assert_eq!(index.docs_with_tag("python").count(), 1);
    }

    #[test]
    fn test_replace_doc_updates_backlinks() {
        let doc_a_v1 = parse_document("# A\n\nLinks to [[b]] and [[c]].", Path::new("a.md"));
        let doc_b = parse_document("# B", Path::new("b.md"));
        let doc_c = parse_document("# C", Path::new("c.md"));
        let doc_d = parse_document("# D", Path::new("d.md"));

        let mut index = Index::build(vec![doc_a_v1, doc_b, doc_c, doc_d]);

        let b_id = index.resolve_link("b").unwrap().clone();
        let c_id = index.resolve_link("c").unwrap().clone();
        let d_id = index.resolve_link("d").unwrap().clone();

        assert_eq!(index.backlinks_of(&b_id).count(), 1);
        assert_eq!(index.backlinks_of(&c_id).count(), 1);
        assert_eq!(index.backlinks_of(&d_id).count(), 0);

        // Update A to link to [[d]] instead of [[b]] and [[c]]
        let doc_a_v2 = parse_document("# A\n\nLinks only to [[d]].", Path::new("a.md"));
        index.replace_doc(doc_a_v2);

        assert_eq!(index.backlinks_of(&b_id).count(), 0);
        assert_eq!(index.backlinks_of(&c_id).count(), 0);
        assert_eq!(index.backlinks_of(&d_id).count(), 1);

        // Remove A completely
        let a_id = index.resolve_link("a").unwrap().clone();
        index.remove_doc(&a_id);
        assert_eq!(index.backlinks_of(&d_id).count(), 0);
    }
}
