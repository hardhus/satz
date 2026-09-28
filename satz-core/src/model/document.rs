use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::model::block::BlockAnchor;
use crate::model::footnote::FootnoteTable;
use crate::model::frontmatter::Frontmatter;
use crate::model::heading::Heading;
use crate::model::link::Link;
use crate::model::range::ByteRange;
use crate::model::tag::Tag;
use crate::text::LineIndex;

/// A note's id: its vault-relative path, `/`-separated. Shared, not copied, on `clone()` -- every
/// lookup table and every edge of the index holds its own `DocId` for the same note, so a vault of
/// any size clones one far more often than it makes a new one (measured: a 10,000-note vault's full
/// rebuild clones about 390,000 of them). `Arc<str>` makes that clone an atomic increment instead of
/// an allocation and a copy; every other property (`Ord`, `Hash`, `Eq`, `Debug`) stays exactly what
/// it was with a plain `String`, since `Arc<T>` forwards all of them to `T` itself, never to the
/// pointer -- two `DocId`s built separately from the same text still compare, hash and order equal,
/// which `rebuild_derived`'s determinism (documents always visited in `DocId` order) depends on.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DocId(Arc<str>);

impl DocId {
    pub fn new(id: impl Into<Arc<str>>) -> Self {
        Self(id.into())
    }

    /// The id of the note at `path`: `path`, spelled with `/` whatever separator it was given
    /// with. A relative, vault-relative path gives the id every other note's id is comparable
    /// with; an absolute path gives a `DocId` that is well-formed but not a lookup key (nothing
    /// vault-relative starts with a drive letter or a leading separator).
    pub fn from_path(path: &Path) -> Self {
        Self(Arc::from(
            path.to_string_lossy().replace('\\', "/").as_str(),
        ))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

// Written by hand, not derived: nothing in production serializes a `DocId` today (`graph.rs`
// converts to a plain `String` first; `IndexStats` never carries one) so the exact mechanism does
// not matter, but a manual impl keeps the JSON shape (a bare string, `"a.md"`) that `#[derive]` on
// `DocId(String)` used to produce, without asking for serde's `rc` feature.
impl serde::Serialize for DocId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> serde::Deserialize<'de> for DocId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer).map(DocId::new)
    }
}

impl std::fmt::Display for DocId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// One parsed note.
///
/// The fields are read directly, but they belong together: `line_index` holds the exact text that
/// `content_hash`, `title`, `headings`, `links`, `tags` and `blocks` were derived from, and every
/// range in them is a byte range into that text. Only `parse_document` / `parse_document_owned`
/// produce a consistent `Document`; changing a field by hand (or mixing fields of two documents)
/// breaks that, so build a new one by parsing instead.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Document {
    pub id: DocId,
    pub path: PathBuf,
    pub title: String,
    pub frontmatter: Frontmatter,
    pub frontmatter_range: Option<ByteRange>,
    /// Why the frontmatter block could not be read (invalid YAML, or not a mapping); `None` when it
    /// is fine or absent. Title, aliases and tags of a broken block are ignored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frontmatter_error: Option<String>,
    pub headings: Vec<Heading>,
    pub links: Vec<Link>,
    pub tags: Vec<Tag>,
    pub footnotes: FootnoteTable,
    /// `[^label]`-shaped references found in the raw text whose `label` has no matching
    /// definition in `footnotes.definitions`. Kept separate from `links` (rather than mixed in
    /// as `LinkKind::Footnote` entries) because every `LinkKind::Footnote` in `links` is, by
    /// construction, already resolved -- pulldown-cmark never emits a footnote-reference event
    /// for an undefined label in the first place, so mixing an unresolved one in would break
    /// that implicit invariant for existing consumers (hover, go-to-definition, completion).
    pub broken_footnote_refs: Vec<Link>,
    pub blocks: Vec<BlockAnchor>,
    pub line_index: LineIndex,
    pub content_hash: u64,
}

impl Document {
    /// The folded keys other documents can use to link to this one: title, frontmatter aliases,
    /// and file stem. When these change, links elsewhere in the vault may resolve differently.
    pub fn identity_keys(&self) -> std::collections::HashSet<String> {
        std::iter::once(crate::slug::fold_key(&self.title))
            .chain(
                self.frontmatter
                    .aliases
                    .iter()
                    .map(|a| crate::slug::fold_key(a)),
            )
            .chain(
                self.path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .map(crate::slug::fold_key),
            )
            .collect()
    }

    /// Index of the heading a `#Heading` reference resolves to: the FIRST heading that matches it.
    /// With duplicate headings every reference therefore belongs to the first one.
    pub fn resolve_heading(&self, reference: &str) -> Option<usize> {
        let slug = crate::slug::slugify(reference);
        self.headings
            .iter()
            .position(|h| h.matches_with_slug(reference, &slug))
    }

    /// The link under a byte offset. Links can nest (`[see [[x]]](y.md)`): the innermost one wins,
    /// and of equally wide ones the first.
    pub fn link_at(&self, byte: usize) -> Option<&Link> {
        self.links
            .iter()
            .filter(|l| l.range.contains(byte))
            .min_by_key(|l| l.range.end - l.range.start)
    }

    /// Index of the block a `#^id` reference resolves to. Block ids are ASCII (`[A-Za-z0-9-]`) and
    /// matched ignoring case, like Obsidian; with duplicates the first one wins.
    pub fn resolve_block(&self, id: &str) -> Option<usize> {
        self.blocks
            .iter()
            .position(|b| b.id.eq_ignore_ascii_case(id))
    }

    /// Resolves the document title according to priority:
    /// 1. `frontmatter.title` (if non-empty)
    /// 2. First level 1 heading (`# Heading 1`)
    /// 3. File stem (e.g. "note" for "notes/note.md")
    /// 4. Fallback: "Untitled"
    pub fn resolve_title(frontmatter: &Frontmatter, headings: &[Heading], path: &Path) -> String {
        if let Some(t) = &frontmatter.title {
            let trimmed = t.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }

        if let Some(h1) = headings.iter().find(|h| h.level == 1) {
            let trimmed = h1.text.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }

        if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
            let trimmed = stem.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }

        "Untitled".to_string()
    }
}

#[cfg(test)]
mod tests {
    use crate::parser::parse_document;
    use std::path::Path;

    fn doc(text: &str) -> crate::model::Document {
        parse_document(text, Path::new("a.md"))
    }

    #[test]
    fn a_reference_resolves_to_the_first_matching_heading() {
        let d = doc("# T\n\n## Notes\n\ntext\n\n## Notes\n\n## Other\n\n## Notes\n");
        assert_eq!(d.resolve_heading("Notes"), Some(1));
        assert_eq!(d.resolve_heading("notes"), Some(1));
        assert_eq!(d.resolve_heading("NOTES"), Some(1));
        assert_eq!(d.resolve_heading("Other"), Some(3));
        assert_eq!(d.resolve_heading("T"), Some(0));
    }

    #[test]
    fn an_unknown_reference_resolves_to_nothing() {
        let d = doc("# T\n\n## Notes\n");
        assert_eq!(d.resolve_heading("Missing"), None);
        assert_eq!(d.resolve_heading(""), None);
        assert_eq!(doc("no headings").resolve_heading("x"), None);
    }

    #[test]
    fn slug_and_turkish_case_variants_match() {
        let d = doc("## Günün Özeti\n");
        assert_eq!(d.resolve_heading("günün özeti"), Some(0));
        assert_eq!(d.resolve_heading("günün-özeti"), Some(0));
    }

    // ---- `DocId::from_path`: `/` whatever the path was spelled with, `parse_document` agrees ----

    #[test]
    fn from_path_spells_the_id_with_forward_slashes() {
        assert_eq!(super::DocId::from_path(Path::new("a.md")).as_str(), "a.md");
        assert_eq!(
            super::DocId::from_path(Path::new("sub/a.md")).as_str(),
            "sub/a.md"
        );
        assert_eq!(
            super::DocId::from_path(
                &["sub", "deep", "a.md"]
                    .iter()
                    .collect::<std::path::PathBuf>()
            )
            .as_str(),
            "sub/deep/a.md",
            "joined with the platform's own separator, the id still reads with `/`"
        );
        assert_eq!(super::DocId::from_path(Path::new("")).as_str(), "");
        // Decomposed text is carried as given -- folding for lookups is a separate step.
        assert_eq!(
            super::DocId::from_path(Path::new("caf\u{65}\u{301}.md")).as_str(),
            "cafe\u{301}.md"
        );
    }

    #[test]
    fn a_parsed_documents_id_is_from_path_of_its_own_path() {
        for path in ["a.md", "sub/a.md", "sub/deep/a.md", "İş/çalışma.md"] {
            let d = parse_document("# T\n", Path::new(path));
            assert_eq!(d.id, super::DocId::from_path(Path::new(path)), "{path}");
            assert_eq!(d.id, super::DocId::from_path(&d.path), "{path}");
        }
    }

    // ---- `DocId` shares its text (`Arc<str>`); every property stays value-based, not pointer-based ----

    #[test]
    fn two_separately_built_doc_ids_with_the_same_text_compare_and_hash_as_one_value() {
        use std::hash::{Hash, Hasher};
        let a = super::DocId::new(String::from("sub/a.md"));
        let b = super::DocId::from_path(Path::new("sub/a.md"));
        assert_eq!(a, b, "same text, built two different ways");
        assert_eq!(a.cmp(&b), std::cmp::Ordering::Equal);

        fn hash_of(id: &super::DocId) -> u64 {
            let mut h = std::collections::hash_map::DefaultHasher::new();
            id.hash(&mut h);
            h.finish()
        }
        assert_eq!(
            hash_of(&a),
            hash_of(&b),
            "two Arcs holding equal text must hash equal, or every HashMap/HashSet keyed by \
             DocId silently stops finding what it put in"
        );

        // Different text is still told apart (the migration must not make everything look equal).
        assert_ne!(a, super::DocId::new(String::from("sub/b.md")));
    }

    #[test]
    fn doc_id_still_debug_formats_as_a_bare_quoted_string() {
        // What `#[derive(Debug)]` on `DocId(String)` produced; assertion failure messages and logs
        // must not change shape just because the field is now an `Arc<str>`.
        assert_eq!(
            format!("{:?}", super::DocId::new("a.md")),
            "DocId(\"a.md\")"
        );
    }

    #[test]
    fn doc_id_serializes_as_a_bare_json_string_and_round_trips() {
        let id = super::DocId::new(String::from("sub/a.md"));
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(
            json, "\"sub/a.md\"",
            "the shape #[derive(Serialize)] on a String field gave"
        );
        let back: super::DocId = serde_json::from_str(&json).unwrap();
        assert_eq!(back, id);
    }
}
