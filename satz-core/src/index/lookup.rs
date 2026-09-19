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
    /// Case-folded, `.md`-less path -> document; the first document (by `DocId`) wins a clash.
    pub(crate) by_path_folded: HashMap<String, DocId>,
    pub(crate) by_stem: HashMap<String, DocId>,
    pub(crate) by_title_alias: HashMap<String, DocId>,
    pub(crate) backlinks: HashMap<DocId, HashSet<DocId>>,
    /// Source document -> the documents its links resolved to when they were last (re)computed.
    /// Lets backlinks be removed exactly as they were added, instead of re-resolving the links
    /// against an index that may have changed since.
    pub(crate) outgoing: HashMap<DocId, HashSet<DocId>>,
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
    /// The target is trimmed and `\` is read as `/`. Priority:
    /// 1. Exact path (`by_path`)
    /// 2. Path with `.md` appended (`by_path`)
    /// 3. Path ignoring case and a `.md` suffix (`by_path_folded`)
    /// 4. File name (last path component) as a stem (`by_stem`)
    /// 5. Title or alias, case- and Unicode-folded (`by_title_alias`)
    ///
    /// Every target follows this one order, so a file at the vault root beats a same-named file in
    /// a folder for `[[x]]` as it does for `[[x.md]]`.
    pub fn resolve_link(&self, raw_target: &str) -> Option<&DocId> {
        let result = self.resolve_link_impl(raw_target);
        if result.is_none() {
            // trace, not debug: called for every link on every reparse/diagnostics
            // pass, so a broken link would otherwise flood even a debug-level log.
            tracing::trace!(raw_target, "resolve_link: no match");
        }
        result
    }

    fn resolve_link_impl(&self, raw_target: &str) -> Option<&DocId> {
        // One order for every kind of target, so `[[x]]` and `[[sub/x]]` follow the same rules:
        // exact path, path + `.md`, path ignoring case, file name, then title/alias.
        let trimmed = raw_target.trim();
        let normalized: std::borrow::Cow<str> = if trimmed.contains('\\') {
            trimmed.replace('\\', "/").into()
        } else {
            trimmed.into()
        };

        if let Some(id) = self.by_path.get(Path::new(&*normalized)) {
            return Some(id);
        }
        if let Some(id) = self.by_path.get(Path::new(&format!("{}.md", normalized))) {
            return Some(id);
        }
        if let Some(id) = self.by_path_folded.get(&fold_path_key(&normalized)) {
            return Some(id);
        }

        // The last path component, untouched: `.file_stem()` would treat the last `.` of a
        // dotted-decimal name like "2.0121" as an extension and chop it to "2", silently matching
        // an unrelated document. Only a real, explicit ".md" suffix is stripped.
        let file_name = normalized.rsplit('/').next().unwrap_or(&normalized);
        if let Some(id) = self.by_stem.get(&fold_key(strip_md_extension(file_name))) {
            return Some(id);
        }

        self.by_title_alias.get(&fold_key(trimmed))
    }

    /// Resolves relative daily note aliases like `[[bugün]]`, `[[dün]]`, `[[yarın]]`
    /// to the target `DocId` based on `DailyNoteConfig`.
    pub fn resolve_relative_daily(
        &self,
        raw_target: &str,
        config: &crate::config::DailyNoteConfig,
    ) -> Option<&DocId> {
        let clean = fold_key(raw_target);
        let today_match = config.aliases.today.iter().any(|a| fold_key(a) == clean);
        let yesterday_match = config
            .aliases
            .yesterday
            .iter()
            .any(|a| fold_key(a) == clean);
        let tomorrow_match = config.aliases.tomorrow.iter().any(|a| fold_key(a) == clean);

        let target_date = if today_match {
            Some(chrono::Local::now().date_naive())
        } else if yesterday_match {
            Some((chrono::Local::now() - chrono::Duration::days(1)).date_naive())
        } else if tomorrow_match {
            Some((chrono::Local::now() + chrono::Duration::days(1)).date_naive())
        } else {
            None
        };

        if let Some(date) = target_date {
            let formatted_date = date.format(&config.format).to_string();
            // Try resolving formatted date directly
            if let Some(doc_id) = self.resolve_link(&formatted_date) {
                return Some(doc_id);
            }
            // Try folder/formatted_date
            let full_path = if config.folder.is_empty() {
                formatted_date
            } else {
                format!("{}/{}", config.folder.trim_matches('/'), formatted_date)
            };
            return self.resolve_link(&full_path);
        }

        None
    }

    /// Fully resolves a `Link` against the index, checking both document existence and heading/block anchors,
    /// with optional `VaultConfig` for relative daily notes.
    pub fn resolve_link_full_with_config<'a>(
        &'a self,
        link: &Link,
        current_doc: Option<&'a Document>,
        config: Option<&crate::config::VaultConfig>,
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

        let resolved_id = if link.target_doc.is_empty() {
            None
        } else if let Some(id) = self.resolve_link(&link.target_doc) {
            Some(id)
        } else if let Some(cfg) = config {
            self.resolve_relative_daily(&link.target_doc, &cfg.daily_note)
        } else {
            None
        };

        let target_doc = if link.target_doc.is_empty() {
            match current_doc {
                Some(d) => d,
                None => return LinkResolution::DocMissing,
            }
        } else if let Some(target_id) = resolved_id {
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

    /// Fully resolves a `Link` against the index, checking both document existence and heading/block anchors.
    pub fn resolve_link_full<'a>(
        &'a self,
        link: &Link,
        current_doc: Option<&'a Document>,
    ) -> LinkResolution<'a> {
        self.resolve_link_full_with_config(link, current_doc, None)
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

    /// Returns an iterator over all document IDs that link to the given `id` -- INCLUDING `id`
    /// itself when it links to itself. See `incoming_from_others` for the count users see.
    pub fn backlinks_of(&self, id: &DocId) -> impl Iterator<Item = &DocId> {
        self.backlinks.get(id).into_iter().flat_map(|s| s.iter())
    }

    /// The documents that link to `id`, not counting `id` itself (a note linking to itself is
    /// not "referenced" by anyone else; this is the rule orphan detection and the backlink count
    /// shown to users follow).
    pub fn incoming_from_others<'a>(&'a self, id: &'a DocId) -> impl Iterator<Item = &'a DocId> {
        self.backlinks_of(id).filter(move |other| *other != id)
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
                    ) && !crate::model::link::is_external_target(&l.target_doc)
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

    /// Which document (if any) a link counts as a backlink *to*, for backlink/orphan purposes.
    ///
    /// The single rule shared by `build`, `replace_doc` and `remove_doc`: external `http(s)`
    /// links, degenerate links (`[[]]`, `[[#]]`, `[[|x]]`), footnotes, and Markdown links with an
    /// empty target (`[x](#h)`) never count; a wikilink/embed with an empty target but a
    /// heading/block (`[[#Heading]]`) is a self-link; everything else goes through
    /// `resolve_link`.
    pub(crate) fn link_target(&self, src: &DocId, link: &Link) -> Option<DocId> {
        if crate::model::link::is_external_target(&link.target_doc) {
            return None;
        }
        match link.kind {
            LinkKind::WikiLink | LinkKind::Embed => {
                let is_degenerate = link.target_doc.is_empty()
                    && link
                        .target_heading
                        .as_deref()
                        .is_none_or(|h| h.trim().is_empty())
                    && link
                        .target_block
                        .as_deref()
                        .is_none_or(|b| b.trim().is_empty());
                if is_degenerate {
                    None
                } else if link.target_doc.is_empty() {
                    Some(src.clone())
                } else {
                    self.resolve_link(&link.target_doc).cloned()
                }
            }
            LinkKind::Markdown => {
                if link.target_doc.is_empty() {
                    None
                } else {
                    self.resolve_link(&link.target_doc).cloned()
                }
            }
            LinkKind::Footnote => None,
        }
    }

    fn tag_keys(doc: &Document) -> Vec<String> {
        doc.tags
            .iter()
            .map(|t| fold_key(t.name.trim_start_matches('#')))
            .collect()
    }

    /// Resolves `id`'s outgoing links against the current lookup tables and records them in
    /// `outgoing` + `backlinks`.
    fn add_doc_edges(&mut self, id: &DocId) {
        let Some(doc) = self.docs.get(id) else {
            return;
        };
        let targets: HashSet<DocId> = doc
            .links
            .iter()
            .filter_map(|link| self.link_target(id, link))
            .collect();
        for target in &targets {
            self.backlinks
                .entry(target.clone())
                .or_default()
                .insert(id.clone());
        }
        if !targets.is_empty() {
            self.outgoing.insert(id.clone(), targets);
        }
    }

    /// Removes `id`'s outgoing edges using what was recorded when they were added -- never by
    /// re-resolving the links against the (possibly different) current index.
    fn remove_doc_edges(&mut self, id: &DocId) {
        let Some(targets) = self.outgoing.remove(id) else {
            return;
        };
        for target in targets {
            if let Some(set) = self.backlinks.get_mut(&target) {
                set.remove(id);
                if set.is_empty() {
                    self.backlinks.remove(&target);
                }
            }
        }
    }

    /// Rebuilds every table derived from `docs` (path/stem/title-alias lookups, tags, forward
    /// and backward link edges) from scratch, visiting documents in `DocId` order so the result
    /// never depends on insertion order.
    ///
    /// Conflict rules (unchanged from the original `build`): the first document (by `DocId`) wins
    /// a stem; the last one wins a title/alias.
    pub(crate) fn rebuild_derived(&mut self, log_conflicts: bool) {
        self.by_path.clear();
        self.by_path_folded.clear();
        self.by_stem.clear();
        self.by_title_alias.clear();
        self.backlinks.clear();
        self.tags.clear();
        self.outgoing.clear();

        let mut ids: Vec<DocId> = self.docs.keys().cloned().collect();
        ids.sort();

        // Pass 1: lookup tables + tags, so pass 2 sees a complete index.
        for id in &ids {
            let doc = &self.docs[id];
            let normalized_path = PathBuf::from(doc.path.to_string_lossy().replace('\\', "/"));
            self.by_path_folded
                .entry(fold_path_key(&normalized_path.to_string_lossy()))
                .or_insert_with(|| id.clone());
            self.by_path.insert(normalized_path, id.clone());

            let stem_key = doc
                .path
                .file_stem()
                .and_then(|s| s.to_str())
                .map(fold_key)
                .unwrap_or_default();
            if !stem_key.is_empty() {
                match self.by_stem.entry(stem_key) {
                    std::collections::hash_map::Entry::Occupied(e) => {
                        if log_conflicts {
                            tracing::warn!(
                                "stem conflict: '{}' (keeping {:?}, ignoring {:?})",
                                e.key(),
                                e.get(),
                                id
                            );
                        }
                    }
                    std::collections::hash_map::Entry::Vacant(e) => {
                        e.insert(id.clone());
                    }
                }
            }

            let title_and_aliases = std::iter::once(fold_key(&doc.title))
                .chain(doc.frontmatter.aliases.iter().map(|a| fold_key(a)));
            for key in title_and_aliases {
                if log_conflicts && self.by_title_alias.get(&key).is_some_and(|o| o != id) {
                    tracing::warn!(
                        "title/alias conflict: '{}' (overwriting previous entry)",
                        key
                    );
                }
                self.by_title_alias.insert(key, id.clone());
            }

            for tag_key in Self::tag_keys(doc) {
                self.tags.entry(tag_key).or_default().insert(id.clone());
            }
        }

        // Pass 2: resolve links.
        for id in &ids {
            self.add_doc_edges(id);
        }
    }

    /// Replaces or inserts a document in the index, keeping every derived table consistent.
    ///
    /// If the document already exists and its identity keys (title, aliases, stem) are
    /// unchanged -- the common keystroke-edit case -- only its own outgoing edges and tags are
    /// refreshed. Otherwise (new document, or a title/alias/stem change) links elsewhere in the
    /// vault may now resolve differently, so all derived tables are rebuilt.
    pub fn replace_doc(&mut self, new_doc: Document) {
        let id = new_doc.id.clone();
        tracing::trace!(?id, path = ?new_doc.path, "Index::replace_doc");

        let same_identity = self
            .docs
            .get(&id)
            .is_some_and(|old| old.identity_keys() == new_doc.identity_keys());

        if !same_identity {
            self.docs.insert(id, new_doc);
            self.rebuild_derived(false);
            return;
        }

        self.remove_doc_edges(&id);
        for tag_key in Self::tag_keys(&self.docs[&id]) {
            if let Some(set) = self.tags.get_mut(&tag_key) {
                set.remove(&id);
                if set.is_empty() {
                    self.tags.remove(&tag_key);
                }
            }
        }
        for tag_key in Self::tag_keys(&new_doc) {
            self.tags.entry(tag_key).or_default().insert(id.clone());
        }
        self.docs.insert(id.clone(), new_doc);
        self.add_doc_edges(&id);
    }

    /// Removes a document from the index.
    pub fn remove_doc(&mut self, id: &DocId) {
        tracing::debug!(?id, "Index::remove_doc");
        if self.docs.remove(id).is_some() {
            self.rebuild_derived(false);
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

/// `name.md` -> `name`, whatever the case of the extension; anything else is returned as is.
fn strip_md_extension(name: &str) -> &str {
    match name.len().checked_sub(3) {
        Some(cut) if name.is_char_boundary(cut) && name[cut..].eq_ignore_ascii_case(".md") => {
            &name[..cut]
        }
        _ => name,
    }
}

/// The key of `by_path_folded`: `/`-separated, without `.md`, case- and Unicode-folded.
pub(crate) fn fold_path_key(path: &str) -> String {
    fold_key(strip_md_extension(&path.replace('\\', "/")))
}

#[cfg(test)]
impl Index {
    /// Deterministic dump of every derived map, so incremental updates can be compared against
    /// a from-scratch `Index::build` of the same documents.
    pub(crate) fn snapshot(&self) -> String {
        fn sorted_set(s: &HashSet<DocId>) -> Vec<String> {
            let mut v: Vec<String> = s.iter().map(|d| d.as_str().to_string()).collect();
            v.sort();
            v
        }
        let mut lines: Vec<String> = Vec::new();
        for id in self.docs.keys() {
            lines.push(format!("doc {}", id.as_str()));
        }
        for (k, v) in &self.by_path {
            lines.push(format!("path {:?} -> {}", k, v.as_str()));
        }
        for (k, v) in &self.by_path_folded {
            lines.push(format!("path_folded {:?} -> {}", k, v.as_str()));
        }
        for (k, v) in &self.by_stem {
            lines.push(format!("stem {:?} -> {}", k, v.as_str()));
        }
        for (k, v) in &self.by_title_alias {
            lines.push(format!("title_alias {:?} -> {}", k, v.as_str()));
        }
        for (k, v) in &self.backlinks {
            lines.push(format!("backlinks {} <- {:?}", k.as_str(), sorted_set(v)));
        }
        for (k, v) in &self.outgoing {
            lines.push(format!("outgoing {} -> {:?}", k.as_str(), sorted_set(v)));
        }
        for (k, v) in &self.tags {
            lines.push(format!("tag {:?} -> {:?}", k, sorted_set(v)));
        }
        lines.sort();
        lines.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse_document;

    fn doc(path: &str, content: &str) -> Document {
        parse_document(content, Path::new(path))
    }

    #[test]
    fn replace_doc_resolves_previously_broken_incoming_links() {
        // The reported bug: `a.md` links to `[[new]]` while `new.md` doesn't exist yet. Creating
        // `new.md` afterwards must make that link resolve AND count as a backlink -- otherwise
        // the freshly created note is wrongly flagged as an orphan.
        let mut index = Index::build(vec![doc("a.md", "# A\n\nSee [[new]].")]);
        assert_eq!(index.broken_link_count(), 1);

        index.replace_doc(doc("new.md", "# New"));

        let new_id = DocId::new("new.md");
        let backlinks: Vec<&str> = index.backlinks_of(&new_id).map(|d| d.as_str()).collect();
        assert_eq!(backlinks, vec!["a.md"]);
        assert_eq!(index.broken_link_count(), 0);
        let orphans: Vec<&str> = index.orphan_docs().map(|d| d.id.as_str()).collect();
        assert!(
            !orphans.contains(&"new.md"),
            "new.md wrongly orphan: {orphans:?}"
        );
    }

    #[test]
    fn removed_and_recreated_note_regains_incoming_backlinks() {
        let mut index = Index::build(vec![doc("a.md", "# A\n\nSee [[b]]."), doc("b.md", "# B")]);
        let b_id = DocId::new("b.md");
        assert_eq!(index.backlinks_of(&b_id).count(), 1);

        index.remove_doc(&b_id);
        assert_eq!(index.backlinks_of(&b_id).count(), 0);
        assert_eq!(index.broken_link_count(), 1);

        index.replace_doc(doc("b.md", "# B"));
        assert_eq!(index.backlinks_of(&b_id).count(), 1);
        assert_eq!(index.broken_link_count(), 0);
    }

    #[test]
    fn removing_alias_owner_restores_the_shadowed_alias() {
        let a = doc("a.md", "---\naliases: [shared]\n---\n# A");
        let b = doc("b.md", "---\naliases: [shared]\n---\n# B");
        let mut index = Index::build(vec![a, b]);
        // `by_title_alias` is last-wins, and now deterministic (sorted by DocId): b.md wins.
        assert_eq!(index.resolve_link("shared"), Some(&DocId::new("b.md")));

        index.remove_doc(&DocId::new("b.md"));
        assert_eq!(index.resolve_link("shared"), Some(&DocId::new("a.md")));
    }

    #[test]
    fn stem_conflict_is_deterministic_and_loser_is_promoted_on_removal() {
        let make = |order: &[&str]| {
            Index::build(
                order
                    .iter()
                    .map(|p| doc(p, "# Foo"))
                    .collect::<Vec<Document>>(),
            )
        };
        // Same winner regardless of insertion order (first by sorted DocId).
        let forward = make(&["a/foo.md", "b/foo.md"]);
        let reverse = make(&["b/foo.md", "a/foo.md"]);
        assert_eq!(forward.resolve_link("foo"), Some(&DocId::new("a/foo.md")));
        assert_eq!(reverse.resolve_link("foo"), Some(&DocId::new("a/foo.md")));

        let mut index = reverse;
        index.remove_doc(&DocId::new("a/foo.md"));
        assert_eq!(index.resolve_link("foo"), Some(&DocId::new("b/foo.md")));
    }

    #[test]
    fn build_and_replace_agree_on_empty_target_markdown_link() {
        // `[Self](#section)` has an empty `target_doc`. `build` never counted it as a backlink;
        // `replace_doc` used to count it as a self-link. Both paths must agree.
        let content = "# Self\n\n[Self](#section)";
        let built = Index::build(vec![doc("self.md", content)]);
        let mut replaced = Index::default();
        replaced.replace_doc(doc("self.md", content));

        let id = DocId::new("self.md");
        assert_eq!(built.backlinks_of(&id).count(), 0);
        assert_eq!(replaced.backlinks_of(&id).count(), 0);
        assert_eq!(built.snapshot(), replaced.snapshot());
    }

    /// Tiny deterministic PRNG so the parity test needs no extra dependency.
    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self, bound: usize) -> usize {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((self.0 >> 33) as usize) % bound
        }
    }

    #[test]
    fn incremental_updates_always_match_a_fresh_build() {
        const PATHS: [&str; 6] = [
            "n1.md",
            "n2.md",
            "sub/n3.md",
            "sub/n1.md", // stem conflict with n1.md
            "n4.md",
            "d/n2.md", // stem conflict with n2.md
        ];
        const VARIANTS: [&str; 5] = [
            "# T\n\n[[n1]] [[n2]]",
            "---\ntitle: Alias One\naliases: [shared, x1]\n---\n# Alias One\n\n[[shared]] [[n3#h]]",
            "# Heading\n\n[[#Heading]] [Self](#heading) #tag1",
            "---\naliases: [shared]\ntags: [tag2]\n---\n# Other\n\n[[Alias One]] [[n4]]",
            "# Plain",
        ];

        for seed in 1..=8u64 {
            let mut rng = Lcg(seed);
            let mut index = Index::default();
            let mut current: HashMap<&str, Document> = HashMap::new();

            for step in 0..120 {
                let path = PATHS[rng.next(PATHS.len())];
                if rng.next(10) < 7 {
                    let d = doc(path, VARIANTS[rng.next(VARIANTS.len())]);
                    current.insert(path, d.clone());
                    index.replace_doc(d);
                } else if current.remove(path).is_some() {
                    index.remove_doc(&DocId::new(path));
                }

                let mut docs: Vec<Document> = current.values().cloned().collect();
                docs.sort_by(|a, b| a.id.cmp(&b.id));
                let fresh = Index::build(docs);
                assert_eq!(
                    index.snapshot(),
                    fresh.snapshot(),
                    "seed {seed}, step {step}: incremental index diverged from a fresh build"
                );
            }
        }
    }

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

    #[test]
    fn dotted_decimal_target_does_not_falsely_match_shorter_sibling_stem() {
        // Regression test: "tlp/2.0121" doesn't exist, but "tlp/2.md" does. `file_stem()`
        // treats the last '.' as an extension separator, so a naive stem fallback would chop
        // "2.0121" down to "2" and wrongly resolve to "tlp/2.md" instead of reporting the link
        // as broken.
        let doc = parse_document("# 2\nİçerik", Path::new("tlp/2.md"));
        let index = Index::build(vec![doc]);

        assert_eq!(index.resolve_link("tlp/2.0121"), None);
        // The exact target still resolves once it actually exists.
        let doc2 = parse_document("# 2.0121\nİçerik", Path::new("tlp/2.0121.md"));
        let index2 = Index::build(vec![
            parse_document("# 2\nİçerik", Path::new("tlp/2.md")),
            doc2,
        ]);
        assert_eq!(
            index2.resolve_link("tlp/2.0121"),
            Some(&DocId::new("tlp/2.0121.md"))
        );
    }
}
