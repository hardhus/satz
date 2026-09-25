use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use rayon::prelude::*;

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

/// Outcome of resolving a Markdown link path relative to its own note.
enum RelativeResolution<'a> {
    Found(&'a DocId),
    /// The path climbs above the vault root.
    EscapesVault,
    /// Not a relative path, or nothing there: use the vault-wide lookup.
    NotApplicable,
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
    /// Folded tag -> notes carrying it. Ordered, so a tag and its sub-tags are one key range.
    pub(crate) tags: std::collections::BTreeMap<String, HashSet<DocId>>,
    /// Counts the changes made to this index: it names the state something was computed from.
    pub(crate) revision: u64,
    /// The relative daily-note aliases (`[[bugün]]`) and the date "today" means; `None`: not known.
    pub(crate) daily: Option<(crate::config::DailyNoteConfig, chrono::NaiveDate)>,
}

impl Index {
    /// Tells the index which relative daily-note aliases (`[[bugün]]`) exist and which date
    /// "today" is. Links written with an alias then count as links to that day's note (backlinks,
    /// orphans), the way go-to-definition and diagnostics already read them. The date is the
    /// caller's, not the clock's; when the day changes the caller sets it again. Setting what is
    /// already set does nothing.
    pub fn set_daily(
        &mut self,
        daily: Option<(crate::config::DailyNoteConfig, chrono::NaiveDate)>,
    ) {
        if self.daily == daily {
            return;
        }
        self.daily = daily;
        self.rebuild_derived(false);
    }

    /// The daily-note aliases and the date "today" means, as last set by `set_daily`.
    pub fn daily(&self) -> Option<&(crate::config::DailyNoteConfig, chrono::NaiveDate)> {
        self.daily.as_ref()
    }

    /// A number that changes whenever the index changes (a note added, edited or removed) and only
    /// then. Anything computed from the index -- diagnostics above all, which depend on every
    /// note -- can be tagged with it, and "has anything changed?" is a comparison.
    pub fn revision(&self) -> u64 {
        self.revision
    }

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

    /// The note a Markdown link's destination names, seen from the note at `from`: first relative to
    /// its folder, then by the vault-wide rules; a path that climbs out of the vault is broken (it is
    /// not matched by file name as a wikilink would be). The one rule link resolution, backlinks and
    /// the graph all share.
    pub(crate) fn resolve_markdown_target(&self, from: &Path, target: &str) -> Option<&DocId> {
        match self.resolve_relative_to(from, target) {
            RelativeResolution::Found(id) => Some(id),
            RelativeResolution::EscapesVault => None,
            RelativeResolution::NotApplicable => self.resolve_link(target),
        }
    }

    /// Resolves a Markdown link path against the folder of the note `from` that contains it:
    /// `.` and `..` components are applied, and the result is looked up as a vault path (with or
    /// without `.md`, ignoring case).
    fn resolve_relative_to(&self, from: &Path, target: &str) -> RelativeResolution<'_> {
        let target = target.trim().replace('\\', "/");
        if target.starts_with('/') {
            return RelativeResolution::NotApplicable;
        }
        let mut parts: Vec<String> = from
            .parent()
            .map(|dir| {
                dir.components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        for component in target.split('/') {
            match component {
                "" | "." => {}
                ".." => {
                    if parts.pop().is_none() {
                        return RelativeResolution::EscapesVault;
                    }
                }
                other => parts.push(other.to_string()),
            }
        }
        let joined = parts.join("/");
        if let Some(id) = self.by_path.get(Path::new(&joined)) {
            return RelativeResolution::Found(id);
        }
        if let Some(id) = self.by_path.get(Path::new(&format!("{joined}.md"))) {
            return RelativeResolution::Found(id);
        }
        match self.by_path_folded.get(&fold_path_key(&joined)) {
            Some(id) => RelativeResolution::Found(id),
            None => RelativeResolution::NotApplicable,
        }
    }

    /// Resolves relative daily note aliases like `[[bugün]]`, `[[dün]]`, `[[yarın]]`
    /// to the target `DocId` based on `DailyNoteConfig`.
    pub fn resolve_relative_daily(
        &self,
        raw_target: &str,
        config: &crate::config::DailyNoteConfig,
    ) -> Option<&DocId> {
        self.resolve_relative_daily_on(raw_target, config, chrono::Local::now().date_naive())
    }

    /// `resolve_relative_daily` for a given "today".
    pub fn resolve_relative_daily_on(
        &self,
        raw_target: &str,
        config: &crate::config::DailyNoteConfig,
        today: chrono::NaiveDate,
    ) -> Option<&DocId> {
        let clean = fold_key(raw_target);
        let names = |aliases: &[String]| aliases.iter().any(|a| fold_key(a) == clean);

        let target_date = if names(&config.aliases.today) {
            Some(today)
        } else if names(&config.aliases.yesterday) {
            today.pred_opt()
        } else if names(&config.aliases.tomorrow) {
            today.succ_opt()
        } else {
            None
        };

        let date = target_date?;
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
        self.resolve_link(&full_path)
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

        let found = if link.target_doc.is_empty() {
            None
        } else {
            match (link.kind, current_doc) {
                (LinkKind::Markdown, Some(from)) => {
                    self.resolve_markdown_target(&from.path, &link.target_doc)
                }
                _ => self.resolve_link(&link.target_doc),
            }
        };
        let resolved_id = match (found, config) {
            (Some(id), _) => Some(id),
            (None, Some(cfg)) if !link.target_doc.is_empty() => {
                self.resolve_relative_daily(&link.target_doc, &cfg.daily_note)
            }
            _ => None,
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
            if let Some(b) = target_doc
                .resolve_block(block_id)
                .map(|i| &target_doc.blocks[i])
            {
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
                .find(|h| h.matches_with_slug(heading_ref, &link_slug))
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

    /// Returns the documents tagged with the specified tag name (case-insensitive; a tag also
    /// matches its sub-tags: `rust` -> `rust/async`, not `rustic`), in the order of their ids.
    pub fn docs_with_tag<'a>(&'a self, tag: &str) -> impl Iterator<Item = &'a Document> + 'a {
        let clean = fold_key(tag.trim_start_matches('#'));
        let prefix = format!("{}/", clean);
        let mut matched_ids = std::collections::BTreeSet::new();

        if let Some(ids) = self.tags.get(&clean) {
            matched_ids.extend(ids);
        }
        for (_, ids) in self
            .tags
            .range::<str, _>((
                std::ops::Bound::Included(prefix.as_str()),
                std::ops::Bound::Unbounded,
            ))
            .take_while(|(k, _)| k.starts_with(&prefix))
        {
            matched_ids.extend(ids);
        }

        matched_ids.into_iter().filter_map(|id| self.docs.get(id))
    }

    /// Returns a sorted list of all unique tag names in the vault.
    pub fn all_tags(&self) -> Vec<&str> {
        self.tags.keys().map(|s| s.as_str()).collect()
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
    pub(crate) fn link_target(&self, src: &Document, link: &Link) -> Option<DocId> {
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
                    Some(src.id.clone())
                } else {
                    // A note with that name wins; otherwise a daily alias (`[[bugün]]`) means
                    // that day's note -- the same order full link resolution uses.
                    self.resolve_link(&link.target_doc)
                        .or_else(|| {
                            let (config, today) = self.daily.as_ref()?;
                            self.resolve_relative_daily_on(&link.target_doc, config, *today)
                        })
                        .cloned()
                }
            }
            LinkKind::Markdown => {
                if link.target_doc.is_empty() {
                    None
                } else {
                    self.resolve_markdown_target(&src.path, &link.target_doc)
                        .cloned()
                }
            }
            LinkKind::Footnote => None,
        }
    }

    /// What one note adds to the lookup tables, worked out from the note alone.
    fn note_keys(doc: &Document) -> NoteKeys {
        let path = PathBuf::from(doc.path.to_string_lossy().replace('\\', "/"));
        let folded = fold_path_key(&path.to_string_lossy());
        let stem = doc
            .path
            .file_stem()
            .and_then(|s| s.to_str())
            .map(fold_key)
            .unwrap_or_default();
        let names = std::iter::once(fold_key(&doc.title))
            .chain(doc.frontmatter.aliases.iter().map(|a| fold_key(a)))
            .collect();
        NoteKeys {
            path,
            folded,
            stem,
            names,
            tags: Self::tag_keys(doc),
        }
    }

    /// Exact path -> note; the last note of a clash wins.
    fn fill_by_path(table: &mut HashMap<PathBuf, DocId>, ids: &[DocId], paths: Vec<PathBuf>) {
        for (id, path) in ids.iter().zip(paths) {
            table.insert(path, id.clone());
        }
    }

    /// Folded path -> note; the first note of a clash wins.
    fn fill_by_path_folded(table: &mut HashMap<String, DocId>, ids: &[DocId], folded: Vec<String>) {
        for (id, key) in ids.iter().zip(folded) {
            table.entry(key).or_insert_with(|| id.clone());
        }
    }

    /// File name -> note; the first note of a clash wins. Returns the clashes, by note index.
    fn fill_by_stem(
        table: &mut HashMap<String, DocId>,
        ids: &[DocId],
        stems: Vec<String>,
        log_conflicts: bool,
    ) -> Vec<(usize, String)> {
        let mut conflicts = Vec::new();
        for (at, (id, stem)) in ids.iter().zip(stems).enumerate() {
            if stem.is_empty() {
                continue;
            }
            match table.entry(stem) {
                std::collections::hash_map::Entry::Occupied(e) => {
                    if log_conflicts {
                        conflicts.push((
                            at,
                            format!(
                                "stem conflict: '{}' (keeping {:?}, ignoring {:?})",
                                e.key(),
                                e.get(),
                                id
                            ),
                        ));
                    }
                }
                std::collections::hash_map::Entry::Vacant(e) => {
                    e.insert(id.clone());
                }
            }
        }
        conflicts
    }

    /// Title or alias -> note; the last note of a clash wins. Returns the clashes, by note index.
    fn fill_by_title_alias(
        table: &mut HashMap<String, DocId>,
        ids: &[DocId],
        names: Vec<Vec<String>>,
        log_conflicts: bool,
    ) -> Vec<(usize, String)> {
        let mut conflicts = Vec::new();
        for (at, (id, keys)) in ids.iter().zip(names).enumerate() {
            for key in keys {
                if log_conflicts && table.get(&key).is_some_and(|other| other != id) {
                    conflicts.push((
                        at,
                        format!("title/alias conflict: '{key}' (overwriting previous entry)"),
                    ));
                }
                table.insert(key, id.clone());
            }
        }
        conflicts
    }

    fn fill_tags(
        table: &mut std::collections::BTreeMap<String, HashSet<DocId>>,
        ids: &[DocId],
        tag_keys: Vec<Vec<String>>,
    ) {
        for (id, keys) in ids.iter().zip(tag_keys) {
            for key in keys {
                table.entry(key).or_default().insert(id.clone());
            }
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
        let targets = self.doc_targets(id);
        self.record_edges(id, targets);
    }

    /// The notes `id`'s links resolve to against the current lookup tables (each once).
    fn doc_targets(&self, id: &DocId) -> HashSet<DocId> {
        let Some(doc) = self.docs.get(id) else {
            return HashSet::new();
        };
        doc.links
            .iter()
            .filter_map(|link| self.link_target(doc, link))
            .collect()
    }

    /// Records `targets` as the notes `id` links to: in `outgoing`, and `id` in each one's `backlinks`.
    fn record_edges(&mut self, id: &DocId, targets: HashSet<DocId>) {
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
        self.rebuild_derived_with(log_conflicts, rebuild_is_parallel(self.docs.len()));
    }

    /// `rebuild_derived`, with the choice of sharing the work between cores made by the caller (a
    /// test asks for both and compares them). The tables come out the same either way.
    /// Returns the conflicts it found between notes (a file name, a title or an alias that two
    /// notes share) as messages, in the order of the notes; they are also logged. Empty unless
    /// `log_conflicts`.
    pub(crate) fn rebuild_derived_with(
        &mut self,
        log_conflicts: bool,
        parallel: bool,
    ) -> Vec<String> {
        self.by_path.clear();
        self.by_path_folded.clear();
        self.by_stem.clear();
        self.by_title_alias.clear();
        self.backlinks.clear();
        self.tags.clear();
        self.outgoing.clear();

        let mut ids: Vec<DocId> = self.docs.keys().cloned().collect();
        ids.sort();

        // Pass 1: lookup tables + tags, so pass 2 sees a complete index. What a note contributes
        // to them is worked out from the note alone (for all notes at once, on every core, for a
        // vault big enough to make that worth it) ...
        let this: &Index = self;
        let keys: Vec<NoteKeys> = if parallel {
            ids.par_iter()
                .map(|id| Index::note_keys(&this.docs[id]))
                .collect()
        } else {
            ids.iter()
                .map(|id| Index::note_keys(&this.docs[id]))
                .collect()
        };
        let mut keys = NoteKeysByTable::from(keys);

        // ... and each table is then filled note after note in the order of `ids`, which is what
        // decides who wins a clash. The tables do not depend on one another, so they are filled at
        // the same time.
        let Index {
            by_path,
            by_path_folded,
            by_stem,
            by_title_alias,
            tags,
            ..
        } = self;
        let (mut stem_conflicts, mut title_conflicts) = (Vec::new(), Vec::new());
        if parallel {
            let (ids, keys) = (&ids, &mut keys);
            let (paths, folded) = (
                std::mem::take(&mut keys.paths),
                std::mem::take(&mut keys.folded),
            );
            let (stems, names) = (
                std::mem::take(&mut keys.stems),
                std::mem::take(&mut keys.names),
            );
            let tag_keys = std::mem::take(&mut keys.tags);
            let (stem_out, title_out) = (&mut stem_conflicts, &mut title_conflicts);
            rayon::scope(|scope| {
                scope.spawn(move |_| Index::fill_by_path(by_path, ids, paths));
                scope.spawn(move |_| Index::fill_by_path_folded(by_path_folded, ids, folded));
                scope.spawn(move |_| {
                    *stem_out = Index::fill_by_stem(by_stem, ids, stems, log_conflicts)
                });
                scope.spawn(move |_| {
                    *title_out =
                        Index::fill_by_title_alias(by_title_alias, ids, names, log_conflicts)
                });
                scope.spawn(move |_| Index::fill_tags(tags, ids, tag_keys));
            });
        } else {
            Index::fill_by_path(by_path, &ids, keys.paths);
            Index::fill_by_path_folded(by_path_folded, &ids, keys.folded);
            stem_conflicts = Index::fill_by_stem(by_stem, &ids, keys.stems, log_conflicts);
            title_conflicts =
                Index::fill_by_title_alias(by_title_alias, &ids, keys.names, log_conflicts);
            Index::fill_tags(tags, &ids, keys.tags);
        }
        // The clashes are reported as the sequential rebuild reported them: note by note, a file
        // name clash before the title and alias clashes of the same note.
        let conflicts = merge_conflicts(stem_conflicts, title_conflicts);
        for message in &conflicts {
            tracing::warn!("{message}");
        }

        // Pass 2: resolve links. Which note each note's links reach only reads the tables that pass
        // 1 filled, so it is worked out for all notes at once (on every core, for a vault big
        // enough to make that worth it); what is recorded for it is then written note after note,
        // in the order of `ids`.
        let targets: Vec<HashSet<DocId>> = if parallel {
            #[cfg(test)]
            PARALLEL_REBUILDS.with(|n| n.set(n.get() + 1));
            let this: &Index = self;
            ids.par_iter().map(|id| this.doc_targets(id)).collect()
        } else {
            ids.iter().map(|id| self.doc_targets(id)).collect()
        };
        for (id, targets) in ids.iter().zip(targets) {
            self.record_edges(id, targets);
        }
        self.revision += 1;
        conflicts
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
        self.revision += 1;
    }

    /// Removes several documents with ONE rebuild of the derived tables (removing them one by one
    /// rebuilds after each, which is quadratic for a whole folder). The result is the same.
    pub fn remove_docs(&mut self, ids: &[DocId]) {
        let mut removed = false;
        for id in ids {
            removed |= self.docs.remove(id).is_some();
        }
        if removed {
            self.rebuild_derived(false);
        }
    }

    /// Inserts or replaces several documents with ONE rebuild of the derived tables; the same result
    /// as calling `replace_doc` for each.
    pub fn replace_docs(&mut self, docs: Vec<Document>) {
        if docs.is_empty() {
            return;
        }
        for doc in docs {
            self.docs.insert(doc.id.clone(), doc);
        }
        self.rebuild_derived(false);
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
        let (mut total_headings, mut total_words, mut total_links) = (0, 0, 0);
        for doc in self.docs.values() {
            total_headings += doc.headings.len();
            total_links += doc.links.len();
            total_words += doc.line_index.source().split_whitespace().count();
        }

        IndexStats {
            doc_count: self.doc_count(),
            total_links,
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
/// What one note adds to each lookup table.
struct NoteKeys {
    path: PathBuf,
    folded: String,
    stem: String,
    /// Its title and its aliases, folded.
    names: Vec<String>,
    tags: Vec<String>,
}

/// The same keys, gathered by table (each table is filled from its own list).
#[derive(Default)]
struct NoteKeysByTable {
    paths: Vec<PathBuf>,
    folded: Vec<String>,
    stems: Vec<String>,
    names: Vec<Vec<String>>,
    tags: Vec<Vec<String>>,
}

impl From<Vec<NoteKeys>> for NoteKeysByTable {
    fn from(keys: Vec<NoteKeys>) -> Self {
        let mut by_table = NoteKeysByTable::default();
        for key in keys {
            by_table.paths.push(key.path);
            by_table.folded.push(key.folded);
            by_table.stems.push(key.stem);
            by_table.names.push(key.names);
            by_table.tags.push(key.tags);
        }
        by_table
    }
}

/// Puts the two lists of clashes (each in the order of the notes) into one, a note's file name
/// clash before its title and alias clashes.
fn merge_conflicts(stems: Vec<(usize, String)>, titles: Vec<(usize, String)>) -> Vec<String> {
    let (mut stems, mut titles) = (stems.into_iter().peekable(), titles.into_iter().peekable());
    let mut merged = Vec::new();
    loop {
        let take_stem = match (stems.peek(), titles.peek()) {
            (Some((s, _)), Some((t, _))) => s <= t,
            (Some(_), None) => true,
            (None, Some(_)) => false,
            (None, None) => break,
        };
        let next = if take_stem {
            stems.next()
        } else {
            titles.next()
        };
        merged.extend(next.map(|(_, message)| message));
    }
    merged
}

/// From this many notes on, rebuilding the derived tables shares the work between cores. Below
/// it the threads cost more than they save: on a 4-core (8 threads) laptop, sequential against
/// shared, best of many rounds: 50 notes 0.79x (slower), 100 notes 0.96x, 200 notes 1.59x, 400 notes
/// 2.5x.
const PARALLEL_REBUILD_FROM: usize = 200;

#[cfg(test)]
thread_local! {
    /// How many rebuilds on this thread shared the work between cores (for the tests).
    static PARALLEL_REBUILDS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Whether a vault of `notes` notes is rebuilt on all cores.
fn rebuild_is_parallel(notes: usize) -> bool {
    notes >= PARALLEL_REBUILD_FROM
}

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

    // ---- what a link points at, and what is recorded for it: the same as before (4.1) ----

    /// `link_target` as it was: the target as an owned id, every branch cloning it.
    ///
    fn reference_link_target(index: &Index, src: &Document, link: &Link) -> Option<DocId> {
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
                    Some(src.id.clone())
                } else {
                    // A note with that name wins; otherwise a daily alias (`[[bugün]]`) means
                    // that day's note -- the same order full link resolution uses.
                    index
                        .resolve_link(&link.target_doc)
                        .or_else(|| {
                            let (config, today) = index.daily.as_ref()?;
                            index.resolve_relative_daily_on(&link.target_doc, config, *today)
                        })
                        .cloned()
                }
            }
            LinkKind::Markdown => {
                if link.target_doc.is_empty() {
                    None
                } else {
                    index
                        .resolve_markdown_target(&src.path, &link.target_doc)
                        .cloned()
                }
            }
            LinkKind::Footnote => None,
        }
    }

    struct T41Rng(u64);

    impl T41Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }

        fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }

        fn pick<'a>(&mut self, of: &[&'a str]) -> &'a str {
            of[self.below(of.len())]
        }
    }

    const T41_NAMES: &[&str] = &[
        "Alpha",
        "alpha",
        "Beta",
        "Gamma Delta",
        "Nota",
        "İş Notu",
        "ışık",
    ];

    /// What can stand where a link names a note.
    const T41_TARGETS: &[&str] = &[
        "Alpha",
        "alpha",
        "Beta",
        "Gamma Delta",
        "Nota",
        "İş Notu",
        "ışık",
        "n1",
        "n2",
        "f1/n2",
        "f0/dup",
        "dup",
        "nope",
        "bugün",
        "dün",
        "yarın",
        "today",
        "yesterday",
        "N1",
    ];

    /// What can stand where a Markdown link names a file.
    const T41_PATHS: &[&str] = &[
        "n1.md",
        "../f1/n2.md",
        "f0/dup.md",
        "dup.md",
        "missing.md",
        "sub/x.md",
        "N1.MD",
        "n%201.md",
        "n3.md",
        "./n2.md",
    ];

    const T41_LINKS: &[&str] = &[
        "[[{t}]]",
        "![[{t}]]",
        "[[{t}|shown]]",
        "[[{t}#Heading]]",
        "[[{t}#^blk]]",
        "![[{t}#H]]",
        "[[#Heading]]",
        "[[#^blk]]",
        "[[]]",
        "[[#]]",
        "[[|x]]",
        "[[ ]]",
        "[x]({p})",
        "[x]({p}#h)",
        "[x](#h)",
        "[x]()",
        "[x](https://a.b/c)",
        "[x](mailto:a@b.c)",
        "[^1] and more",
        "https://bare.link",
        "[[{t}]] and [[{t}]]",
    ];

    fn t41_body(rng: &mut T41Rng) -> String {
        let mut body = String::from("# Head\n\n## Heading\n\ntext ^blk\n\n");
        for _ in 0..rng.below(9) {
            let piece = T41_LINKS[rng.below(T41_LINKS.len())]
                .replace("{t}", rng.pick(T41_TARGETS))
                .replace("{p}", rng.pick(T41_PATHS));
            body.push_str(&piece);
            body.push(' ');
        }
        body
    }

    /// A vault of random notes; the note headers (which give the notes their names) are kept, so
    /// that a note can be written again with other links and the same name.
    struct T41Vault {
        index: Index,
        notes: Vec<(String, String)>,
    }

    fn t41_vault(rng: &mut T41Rng) -> T41Vault {
        let mut notes: Vec<(String, String)> = Vec::new();
        for i in 0..3 + rng.below(12) {
            let path = match rng.below(4) {
                0 => format!("n{i}.md"),
                1 => format!("f{}/n{i}.md", rng.below(3)),
                2 => format!(
                    "f{}/{}.md",
                    rng.below(3),
                    T41_NAMES[rng.below(T41_NAMES.len())]
                ),
                _ => format!("n{i}.md"),
            };
            if notes.iter().any(|(p, _)| *p == path) {
                continue;
            }
            let mut header = String::new();
            if rng.below(2) == 0 {
                header.push_str("---\n");
                if rng.below(2) == 0 {
                    header.push_str(&format!("title: {}\n", rng.pick(T41_NAMES)));
                }
                if rng.below(3) == 0 {
                    header.push_str(&format!(
                        "aliases: [{}, {}]\n",
                        rng.pick(T41_NAMES),
                        rng.pick(T41_NAMES)
                    ));
                }
                header.push_str("---\n");
            }
            notes.push((path, header));
        }
        for path in ["f0/dup.md", "f1/dup.md"] {
            if rng.below(2) == 0 && !notes.iter().any(|(p, _)| p == path) {
                notes.push((path.to_string(), String::new()));
            }
        }
        let today = chrono::Local::now().date_naive();
        let with_daily = rng.below(5) != 0;
        if with_daily {
            for day in [today.pred_opt().unwrap(), today, today.succ_opt().unwrap()] {
                if rng.below(3) != 0 {
                    notes.push((
                        format!("daily/{}.md", day.format("%Y-%m-%d")),
                        String::new(),
                    ));
                }
            }
        }
        let docs = notes
            .iter()
            .map(|(path, header)| doc(path, &format!("{header}{}", t41_body(rng))))
            .collect();
        let mut index = Index::build(docs);
        if with_daily {
            index.set_daily(Some((crate::config::DailyNoteConfig::default(), today)));
        }
        T41Vault { index, notes }
    }

    /// What the index records for links, worked out again from the notes with the reference.
    fn t41_expected_tables(
        index: &Index,
    ) -> (
        HashMap<DocId, HashSet<DocId>>,
        HashMap<DocId, HashSet<DocId>>,
    ) {
        let mut outgoing: HashMap<DocId, HashSet<DocId>> = HashMap::new();
        let mut backlinks: HashMap<DocId, HashSet<DocId>> = HashMap::new();
        for doc in index.docs.values() {
            let targets: HashSet<DocId> = doc
                .links
                .iter()
                .filter_map(|link| reference_link_target(index, doc, link))
                .collect();
            for target in &targets {
                backlinks
                    .entry(target.clone())
                    .or_default()
                    .insert(doc.id.clone());
            }
            if !targets.is_empty() {
                outgoing.insert(doc.id.clone(), targets);
            }
        }
        (outgoing, backlinks)
    }

    #[test]
    fn a_links_target_is_the_same_as_it_always_was() {
        let mut rng = T41Rng(0x9E37_79B9_7F4A_7C15);
        let (mut links, mut resolved, mut own) = (0, 0, 0);
        for round in 0..600 {
            let vault = t41_vault(&mut rng);
            for doc in vault.index.docs.values() {
                for link in &doc.links {
                    let got = vault.index.link_target(doc, link);
                    let want = reference_link_target(&vault.index, doc, link);
                    assert_eq!(
                        got,
                        want,
                        "round {round}: {:?} in {}",
                        link,
                        doc.id.as_str()
                    );
                    links += 1;
                    resolved += usize::from(want.is_some());
                    own += usize::from(want.as_ref() == Some(&doc.id));
                }
            }
        }
        assert!(links > 5000, "{links} links");
        assert!(resolved > 1500, "{resolved} resolved");
        assert!(own > 100, "{own} links to the note they are in");
    }

    #[test]
    fn what_is_recorded_for_links_is_the_same_after_building_and_after_every_change() {
        let mut rng = T41Rng(0x0123_4567_89AB_CDEF);
        for round in 0..300 {
            let mut vault = t41_vault(&mut rng);
            let (out, back) = t41_expected_tables(&vault.index);
            assert_eq!(vault.index.outgoing, out, "round {round}: built, outgoing");
            assert_eq!(
                vault.index.backlinks, back,
                "round {round}: built, backlinks"
            );

            for step in 0..4 {
                let (path, header) = vault.notes[rng.below(vault.notes.len())].clone();
                match rng.below(4) {
                    // Written again under the same name, with other links: only its own edges move.
                    0 | 1 => {
                        let text = format!("{header}{}", t41_body(&mut rng));
                        vault.index.replace_doc(doc(&path, &text));
                    }
                    // A new note, which may make links elsewhere resolve.
                    2 => {
                        let new_path = format!("new{step}-{round}.md");
                        let text = format!("# {}\n\n{}", rng.pick(T41_NAMES), t41_body(&mut rng));
                        vault.index.replace_doc(doc(&new_path, &text));
                        vault.notes.push((new_path, String::new()));
                    }
                    _ => {
                        vault.index.remove_doc(&DocId::new(&path));
                        vault.notes.retain(|(p, _)| *p != path);
                        if vault.notes.is_empty() {
                            break;
                        }
                    }
                }
                let (out, back) = t41_expected_tables(&vault.index);
                assert_eq!(
                    vault.index.outgoing, out,
                    "round {round}, step {step}: outgoing"
                );
                assert_eq!(
                    vault.index.backlinks, back,
                    "round {round}, step {step}: backlinks"
                );
            }
        }
    }

    #[test]
    fn links_the_parser_never_makes_get_the_same_answer_as_before() {
        // Every kind of link with every kind of target, heading and block, made by hand: the
        // parser does not produce empty wikilinks or targets like `https://x` in a wikilink, but
        // the rule for them is part of what `link_target` promises.
        let mut index = Index::build(vec![
            doc(
                "n1.md",
                "---\naliases: [\"https://x\", \"mailto:a@b.c\"]\n---\n# N1\n",
            ),
            doc("src.md", "# Src\n"),
            doc("sub/n2.md", "# N2\n"),
        ]);
        index.set_daily(Some((
            crate::config::DailyNoteConfig::default(),
            chrono::Local::now().date_naive(),
        )));
        let src = index.docs[&DocId::new("src.md")].clone();
        let mut checked = 0;
        for kind in [
            LinkKind::WikiLink,
            LinkKind::Embed,
            LinkKind::Markdown,
            LinkKind::Footnote,
        ] {
            for target in [
                "",
                "n1",
                "N1",
                "https://x",
                "mailto:a@b.c",
                "sub/n2.md",
                "n1.md",
                "bugün",
                "nope",
            ] {
                for heading in [None, Some(""), Some(" "), Some("H")] {
                    for block in [None, Some(""), Some(" "), Some("b")] {
                        let link = Link::new(
                            kind,
                            target.to_string(),
                            heading.map(str::to_string),
                            block.map(str::to_string),
                            None,
                            crate::ByteRange::new(0, 1),
                        );
                        assert_eq!(
                            index.link_target(&src, &link),
                            reference_link_target(&index, &src, &link),
                            "{link:?}"
                        );
                        checked += 1;
                    }
                }
            }
        }
        assert_eq!(checked, 4 * 9 * 4 * 4);
    }

    // ---- rebuilding the derived tables: the same tables as the sequential rebuild made (4.2) ----

    /// `rebuild_derived` as it was: one note after the other, tables filled and then links resolved.
    ///
    fn reference_rebuild(index: &mut Index, log_conflicts: bool) -> Vec<String> {
        let mut messages: Vec<String> = Vec::new();
        index.by_path.clear();
        index.by_path_folded.clear();
        index.by_stem.clear();
        index.by_title_alias.clear();
        index.backlinks.clear();
        index.tags.clear();
        index.outgoing.clear();

        let mut ids: Vec<DocId> = index.docs.keys().cloned().collect();
        ids.sort();

        // Pass 1: lookup tables + tags, so pass 2 sees a complete index.
        for id in &ids {
            let doc = &index.docs[id];
            let normalized_path = PathBuf::from(doc.path.to_string_lossy().replace('\\', "/"));
            index
                .by_path_folded
                .entry(fold_path_key(&normalized_path.to_string_lossy()))
                .or_insert_with(|| id.clone());
            index.by_path.insert(normalized_path, id.clone());

            let stem_key = doc
                .path
                .file_stem()
                .and_then(|s| s.to_str())
                .map(fold_key)
                .unwrap_or_default();
            if !stem_key.is_empty() {
                match index.by_stem.entry(stem_key) {
                    std::collections::hash_map::Entry::Occupied(e) => {
                        if log_conflicts {
                            messages.push(format!(
                                "stem conflict: '{}' (keeping {:?}, ignoring {:?})",
                                e.key(),
                                e.get(),
                                id
                            ));
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
                if log_conflicts && index.by_title_alias.get(&key).is_some_and(|o| o != id) {
                    messages.push(format!(
                        "title/alias conflict: '{}' (overwriting previous entry)",
                        key
                    ));
                }
                index.by_title_alias.insert(key, id.clone());
            }

            for tag_key in Index::tag_keys(doc) {
                index.tags.entry(tag_key).or_default().insert(id.clone());
            }
        }

        // Pass 2: resolve links.
        for id in &ids {
            index.add_doc_edges(id);
        }
        index.revision += 1;
        messages
    }

    struct T42Rng(u64);

    impl T42Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }

        fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }

        fn pick<'a>(&mut self, of: &[&'a str]) -> &'a str {
            of[self.below(of.len())]
        }
    }

    const T42_STEMS: &[&str] = &[
        "alpha", "Alpha", "beta", "gamma", "Nota", "nota", "İş", "ışık", "delta", "2.0121", "x",
        "index",
    ];
    const T42_FOLDERS: &[&str] = &["", "a/", "b/", "a/b/", "A/", "tlp/", "Books/"];
    const T42_TITLES: &[&str] = &[
        "Alpha",
        "alpha",
        "Beta",
        "Gamma Delta",
        "Nota",
        "İş Notu",
        "ışık",
        "Same Title",
        "same title",
    ];
    const T42_TAGS: &[&str] = &["rust", "Rust", "proje/x", "proje/y", "İş", "iş", "a-b"];

    /// A vault of `n` notes made to clash: the same file name in different folders, names that differ
    /// only in case, titles and aliases that are the same or are other notes' names, and links to all
    /// of these.
    fn t42_docs(rng: &mut T42Rng, n: usize) -> Vec<Document> {
        let mut docs: Vec<Document> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        for i in 0..n {
            let path = format!(
                "{}{}{}.md",
                rng.pick(T42_FOLDERS),
                rng.pick(T42_STEMS),
                if rng.below(3) == 0 {
                    i.to_string()
                } else {
                    String::new()
                }
            );
            // The same path twice is one note; a path that differs only in case is another.
            if !seen.insert(path.clone()) {
                continue;
            }
            let mut text = String::new();
            if rng.below(2) == 0 {
                text.push_str("---\n");
                if rng.below(2) == 0 {
                    text.push_str(&format!("title: {}\n", rng.pick(T42_TITLES)));
                }
                if rng.below(3) == 0 {
                    text.push_str(&format!(
                        "aliases: [{}, {}]\n",
                        rng.pick(T42_TITLES),
                        rng.pick(T42_STEMS)
                    ));
                }
                if rng.below(2) == 0 {
                    text.push_str(&format!(
                        "tags: [{}, {}]\n",
                        rng.pick(T42_TAGS),
                        rng.pick(T42_TAGS)
                    ));
                }
                text.push_str("---\n");
            }
            text.push_str(&format!("# {}\n\n", rng.pick(T42_TITLES)));
            for _ in 0..rng.below(7) {
                match rng.below(6) {
                    0 => text.push_str(&format!("[[{}]] ", rng.pick(T42_TITLES))),
                    1 => text.push_str(&format!("[[{}]] ", rng.pick(T42_STEMS))),
                    2 => text.push_str(&format!(
                        "[[{}{}]] ",
                        rng.pick(T42_FOLDERS),
                        rng.pick(T42_STEMS)
                    )),
                    3 => text.push_str(&format!(
                        "[x]({}{}.md) ",
                        rng.pick(&["", "../", "./", "a/", "../a/"]),
                        rng.pick(T42_STEMS)
                    )),
                    4 => text.push_str(&format!("[[{}#Head]] ", rng.pick(T42_TITLES))),
                    _ => text.push_str(&format!("#{} ", rng.pick(T42_TAGS))),
                }
            }
            docs.push(doc(&path, &text));
        }
        docs
    }

    /// The tables the sequential rebuild makes for `docs`.
    fn t42_reference_index(docs: &[Document], log_conflicts: bool) -> Index {
        let mut index = Index::default();
        for d in docs {
            index.docs.insert(d.id.clone(), d.clone());
        }
        reference_rebuild(&mut index, log_conflicts);
        index
    }

    #[test]
    fn the_derived_tables_are_the_same_as_the_sequential_rebuild_made_them() {
        let mut rng = T42Rng(0x9E37_79B9_7F4A_7C15);
        let (mut vaults, mut biggest, mut clashes) = (0, 0, 0usize);
        for round in 0..260 {
            // Small vaults, and some past the size where the work is shared between cores.
            let n = match round % 20 {
                0 => 300 + rng.below(500),
                1 => 200 + rng.below(30),
                _ => 2 + rng.below(40),
            };
            let docs = t42_docs(&mut rng, n);
            let built = Index::build(docs.clone());
            let reference = t42_reference_index(&docs, true);
            assert_eq!(
                built.snapshot(),
                reference.snapshot(),
                "round {round}: {} notes",
                docs.len()
            );
            assert_eq!(
                built.revision(),
                reference.revision(),
                "round {round}: revision"
            );
            vaults += 1;
            biggest = biggest.max(docs.len());
            clashes += docs.len().saturating_sub(built.by_stem.len());
        }
        assert_eq!(vaults, 260);
        assert!(biggest >= 300, "the biggest vault had {biggest} notes");
        assert!(
            clashes > 500,
            "{clashes} file names shared by several notes"
        );
    }

    #[test]
    fn rebuilding_again_gives_the_same_tables_every_time() {
        let mut rng = T42Rng(0x0123_4567_89AB_CDEF);
        for round in 0..30 {
            let n = 250 + rng.below(200);
            let docs = t42_docs(&mut rng, n);
            let mut index = Index::build(docs);
            let first = index.snapshot();
            let revision = index.revision();
            for again in 1..=4 {
                index.rebuild_derived(false);
                assert_eq!(index.snapshot(), first, "round {round}, rebuild {again}");
                assert_eq!(index.revision(), revision + again);
            }
        }
    }

    #[test]
    fn the_tables_stay_the_same_after_changes_that_rebuild_them() {
        // A new note, a note removed and a note whose name changed each rebuild every table.
        let mut rng = T42Rng(0xDEAD_BEEF_CAFE_F00D);
        for round in 0..40 {
            let n = 220 + rng.below(80);
            let mut docs = t42_docs(&mut rng, n);
            let mut index = Index::build(docs.clone());
            for step in 0..3 {
                match rng.below(3) {
                    0 => {
                        let new = doc(
                            &format!("added-{round}-{step}.md"),
                            &format!(
                                "# {}\n\n[[{}]]\n",
                                rng.pick(T42_TITLES),
                                rng.pick(T42_TITLES)
                            ),
                        );
                        docs.retain(|d| d.id != new.id);
                        docs.push(new.clone());
                        index.replace_doc(new);
                    }
                    1 if !docs.is_empty() => {
                        let gone = docs.remove(rng.below(docs.len()));
                        index.remove_doc(&gone.id);
                    }
                    _ if !docs.is_empty() => {
                        let at = rng.below(docs.len());
                        let text = format!("---\ntitle: {}\n---\n# X\n", rng.pick(T42_TITLES));
                        let renamed = doc(docs[at].path.to_str().unwrap(), &text);
                        docs[at] = renamed.clone();
                        index.replace_doc(renamed);
                    }
                    _ => {}
                }
                assert_eq!(
                    index.snapshot(),
                    t42_reference_index(&docs, false).snapshot(),
                    "round {round}, step {step}"
                );
            }
        }
    }

    #[test]
    fn sharing_the_work_between_cores_or_not_gives_the_same_tables() {
        let mut rng = T42Rng(0xFEED_FACE_0BAD_F00D);
        for round in 0..80 {
            let n = if round % 4 == 0 {
                250 + rng.below(400)
            } else {
                2 + rng.below(60)
            };
            let docs = t42_docs(&mut rng, n);
            let tables = |parallel: bool| {
                let mut index = Index::default();
                for d in &docs {
                    index.docs.insert(d.id.clone(), d.clone());
                }
                index.rebuild_derived_with(false, parallel);
                (index.snapshot(), index.revision())
            };
            let alone = tables(false);
            assert_eq!(tables(true), alone, "round {round}: {} notes", docs.len());
            assert_eq!(
                alone.0,
                t42_reference_index(&docs, false).snapshot(),
                "round {round}"
            );
        }
    }

    #[test]
    fn a_vault_is_rebuilt_on_all_cores_from_the_size_where_that_pays() {
        assert!(!rebuild_is_parallel(0));
        assert!(!rebuild_is_parallel(PARALLEL_REBUILD_FROM - 1));
        assert!(rebuild_is_parallel(PARALLEL_REBUILD_FROM));
        assert!(rebuild_is_parallel(10_000));
    }

    #[test]
    fn big_vaults_are_rebuilt_on_all_cores_and_small_ones_are_not() {
        let mut rng = T42Rng(0x1357_9BDF_2468_ACE0);
        let count = || PARALLEL_REBUILDS.with(|n| n.get());
        let before = count();
        let small = Index::build(t42_docs(&mut rng, 20));
        assert!(small.docs.len() < PARALLEL_REBUILD_FROM);
        assert_eq!(count(), before, "a small vault is rebuilt on one core");

        let big = Index::build(t42_docs(&mut rng, 700));
        assert!(
            big.docs.len() >= PARALLEL_REBUILD_FROM,
            "{} notes",
            big.docs.len()
        );
        assert_eq!(count(), before + 1, "a big vault is rebuilt on all cores");

        // Every rebuild counts by the size of the vault at that moment.
        let mut big = big;
        big.replace_doc(doc("brand-new-note.md", "# Brand new\n"));
        assert_eq!(count(), before + 2);
    }

    #[test]
    fn the_clashes_are_reported_as_before_and_in_the_same_order() {
        let mut rng = T42Rng(0xABCD_EF01_2345_6789);
        let mut reported = 0;
        for round in 0..60 {
            let n = if round % 5 == 0 {
                250 + rng.below(300)
            } else {
                2 + rng.below(50)
            };
            let docs = t42_docs(&mut rng, n);
            let messages = |parallel: bool, log: bool| {
                let mut index = Index::default();
                for d in &docs {
                    index.docs.insert(d.id.clone(), d.clone());
                }
                index.rebuild_derived_with(log, parallel)
            };
            let mut reference = Index::default();
            for d in &docs {
                reference.docs.insert(d.id.clone(), d.clone());
            }
            let wanted = reference_rebuild(&mut reference, true);
            assert_eq!(messages(false, true), wanted, "round {round}, on one core");
            assert_eq!(messages(true, true), wanted, "round {round}, on all cores");
            assert!(messages(true, false).is_empty() && messages(false, false).is_empty());
            reported += wanted.len();
        }
        assert!(reported > 300, "{reported} clashes reported");
    }
}
