use std::path::Path;

use satz_core::{DocId, Document, fold_key, slugify};
use tower_lsp_server::ls_types::{Location, ReferenceParams, Uri};

use crate::convert::{byte_range_to_lsp, path_to_uri};
use crate::state::SatzState;

#[derive(Debug, Clone, PartialEq, Eq)]
enum CursorTarget {
    Tag(String),
    /// `index` is the heading it resolves to (the first match; duplicates own no links); `None`
    /// for a reference to a heading that does not exist.
    Heading {
        doc: DocId,
        slug: String,
        index: Option<usize>,
    },
    Block {
        doc: DocId,
        id: String,
    },
    Doc(DocId),
}

fn cursor_target(doc: &Document, off: usize, state: &SatzState) -> Option<CursorTarget> {
    let index = &state.index;
    // Priority order: link > block > heading > tag > document
    if let Some(l) = doc.link_at(off) {
        let target = state.link_target_doc(doc, l)?.clone();
        return Some(match (&l.target_block, &l.target_heading) {
            (Some(b), _) => CursorTarget::Block {
                doc: target,
                id: b.clone(),
            },
            (_, Some(h)) => CursorTarget::Heading {
                index: index.get_doc(&target).and_then(|d| d.resolve_heading(h)),
                doc: target,
                slug: slugify(h),
            },
            _ => CursorTarget::Doc(target),
        });
    }

    if let Some(b) = doc.blocks.iter().find(|b| b.range.contains(off)) {
        return Some(CursorTarget::Block {
            doc: doc.id.clone(),
            id: b.id.clone(),
        });
    }

    if let Some(i) = doc.headings.iter().position(|h| h.range.contains(off)) {
        return Some(CursorTarget::Heading {
            doc: doc.id.clone(),
            slug: doc.headings[i].slug.clone(),
            index: Some(i),
        });
    }

    if let Some(t) = doc.tags.iter().find(|t| t.range.contains(off)) {
        return Some(CursorTarget::Tag(
            t.name.trim_start_matches('#').to_string(),
        ));
    }

    Some(CursorTarget::Doc(doc.id.clone()))
}

fn doc_uri(doc: &Document, vault_root: Option<&Path>) -> Option<Uri> {
    let doc_path = match vault_root {
        Some(root) if !doc.path.is_absolute() => root.join(&doc.path),
        _ => doc.path.clone(),
    };
    path_to_uri(&doc_path)
}

pub fn find_references(params: ReferenceParams, state: &SatzState) -> Option<Vec<Location>> {
    let uri = params.text_document_position.text_document.uri.as_str();
    let pos = params.text_document_position.position;
    tracing::debug!(uri, ?pos, "find_references");

    let (_, doc) = state.doc_for_uri(uri)?;

    let byte_offset = crate::convert::lsp_pos_to_byte(&doc.line_index, pos);

    let target = cursor_target(doc, byte_offset, state)?;
    let mut locations = Vec::new();
    // Where the target is declared (the heading, the block, the note's start), if it has a place.
    let mut declaration: Option<Location> = None;

    match target {
        CursorTarget::Tag(ref tag_name) => {
            let clean_query = fold_key(tag_name.trim_start_matches('#'));
            let prefix = format!("{}/", clean_query);

            for tagged_doc in state.index.docs_with_tag(tag_name) {
                let Some(u) = doc_uri(tagged_doc, state.vault_root()) else {
                    continue;
                };
                for t in &tagged_doc.tags {
                    let clean_t = fold_key(t.name.trim_start_matches('#'));
                    if clean_t == clean_query || clean_t.starts_with(&prefix) {
                        locations.push(Location::new(
                            u.clone(),
                            byte_range_to_lsp(t.range, &tagged_doc.line_index),
                        ));
                    }
                }
            }
        }
        CursorTarget::Block { ref doc, ref id } => {
            if let Some(target_doc) = state.index.get_doc(doc)
                && let Some(b) = target_doc.resolve_block(id).map(|i| &target_doc.blocks[i])
                && let Some(u) = doc_uri(target_doc, state.vault_root())
            {
                let location = Location::new(u, byte_range_to_lsp(b.range, &target_doc.line_index));
                declaration = Some(location.clone());
                locations.push(location);
            }

            let mut candidate_ids: Vec<DocId> = state.index.backlinks_of(doc).cloned().collect();
            if !candidate_ids.contains(doc) {
                candidate_ids.push(doc.clone());
            }

            for src_id in &candidate_ids {
                if let Some(src_doc) = state.index.get_doc(src_id) {
                    let Some(src_uri) = doc_uri(src_doc, state.vault_root()) else {
                        continue;
                    };

                    for link in &src_doc.links {
                        let resolves_to_target = state.link_target_doc(src_doc, link) == Some(doc);

                        if resolves_to_target
                            && link
                                .target_block
                                .as_deref()
                                .is_some_and(|b| b.eq_ignore_ascii_case(id))
                        {
                            locations.push(Location::new(
                                src_uri.clone(),
                                byte_range_to_lsp(link.range, &src_doc.line_index),
                            ));
                        }
                    }
                }
            }
        }
        CursorTarget::Heading {
            ref doc,
            ref slug,
            index: heading_index,
        } => {
            let target_doc_opt = state.index.get_doc(doc);
            if let Some(target_doc) = target_doc_opt
                && let Some(h) = match heading_index {
                    Some(i) => target_doc.headings.get(i),
                    None => target_doc.headings.iter().find(|h| &h.slug == slug),
                }
                && let Some(u) = doc_uri(target_doc, state.vault_root())
            {
                let location = Location::new(u, byte_range_to_lsp(h.range, &target_doc.line_index));
                declaration = Some(location.clone());
                locations.push(location);
            }

            let mut candidate_ids: Vec<DocId> = state.index.backlinks_of(doc).cloned().collect();
            if !candidate_ids.contains(doc) {
                candidate_ids.push(doc.clone());
            }

            for src_id in &candidate_ids {
                if let Some(src_doc) = state.index.get_doc(src_id) {
                    let Some(src_uri) = doc_uri(src_doc, state.vault_root()) else {
                        continue;
                    };

                    for link in &src_doc.links {
                        let resolves_to_target = state.link_target_doc(src_doc, link) == Some(doc);

                        // A reference belongs to the first heading it matches; a later
                        // duplicate owns none. (An unresolved heading falls back to the slug.)
                        let owns_link = link.target_heading.as_deref().is_some_and(|th| {
                            match (heading_index, target_doc_opt) {
                                (Some(i), Some(target_doc)) => {
                                    target_doc.resolve_heading(th) == Some(i)
                                }
                                _ => slugify(th) == *slug,
                            }
                        });
                        if resolves_to_target && owns_link {
                            locations.push(Location::new(
                                src_uri.clone(),
                                byte_range_to_lsp(link.range, &src_doc.line_index),
                            ));
                        }
                    }
                }
            }
        }
        CursorTarget::Doc(ref target_doc_id) => {
            if let Some(target_doc) = state.index.get_doc(target_doc_id)
                && let Some(u) = doc_uri(target_doc, state.vault_root())
            {
                let range = if let Some(h) = target_doc.headings.first() {
                    byte_range_to_lsp(h.range, &target_doc.line_index)
                } else {
                    byte_range_to_lsp(satz_core::ByteRange::new(0, 0), &target_doc.line_index)
                };
                let location = Location::new(u, range);
                declaration = Some(location.clone());
                locations.push(location);
            }

            let mut candidate_ids: Vec<DocId> =
                state.index.backlinks_of(target_doc_id).cloned().collect();
            if !candidate_ids.contains(target_doc_id) {
                candidate_ids.push(target_doc_id.clone());
            }

            for src_id in &candidate_ids {
                if let Some(src_doc) = state.index.get_doc(src_id) {
                    let Some(src_uri) = doc_uri(src_doc, state.vault_root()) else {
                        continue;
                    };

                    for link in &src_doc.links {
                        let resolves_to_target =
                            state.link_target_doc(src_doc, link) == Some(target_doc_id);

                        if resolves_to_target {
                            locations.push(Location::new(
                                src_uri.clone(),
                                byte_range_to_lsp(link.range, &src_doc.line_index),
                            ));
                        }
                    }
                }
            }
        }
    }

    // Without the declaration, drop exactly that location -- not whatever is under the cursor (a
    // link there is a real reference). Locations compare by `Uri` value, not by spelling.
    if !params.context.include_declaration
        && let Some(declaration) = &declaration
    {
        locations.retain(|loc| loc != declaration);
    }

    locations.sort_by(|a, b| {
        (a.uri.as_str(), a.range.start.line, a.range.start.character).cmp(&(
            b.uri.as_str(),
            b.range.start.line,
            b.range.start.character,
        ))
    });
    locations.dedup();

    Some(locations)
}

#[cfg(test)]
// Test states are built field by field so each test shows exactly what it sets up.
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use std::path::Path;

    use satz_core::{Index, parse_document};
    use tower_lsp_server::ls_types::{
        Position, ReferenceContext, TextDocumentIdentifier, TextDocumentPositionParams,
    };

    use super::*;

    #[test]
    fn test_find_tag_references() {
        let abs_a = if cfg!(windows) {
            Path::new("C:\\doc-a.md")
        } else {
            Path::new("/doc-a.md")
        };

        let rel_a = Path::new("doc-a.md");
        let rel_b = Path::new("doc-b.md");

        let doc_a = parse_document("# Doc A\n\nSome text with #rust tag.", rel_a);
        let doc_b = parse_document(
            "---\ntags: [rust]\n---\n# Doc B\n\nAlso has #rust tag.",
            rel_b,
        );

        let mut state = SatzState::default();
        state.index = Index::build(vec![doc_a, doc_b]);
        state.set_vault_root(Some(if cfg!(windows) {
            Path::new("C:\\").to_path_buf()
        } else {
            Path::new("/").to_path_buf()
        }));

        let uri_a_str = if cfg!(windows) {
            "file:///C:/doc-a.md"
        } else {
            "file:///doc-a.md"
        };

        state.open_docs.insert(
            uri_a_str.to_string(),
            crate::state::OpenDocument::new(
                uri_a_str,
                abs_a.to_path_buf(),
                "# Doc A\n\nSome text with #rust tag.",
                1,
            ),
        );

        let params = ReferenceParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: uri_a_str.parse().unwrap(),
                },
                position: Position::new(2, 16), // on "#rust"
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: ReferenceContext {
                include_declaration: true,
            },
        };

        let refs = find_references(params, &state).expect("References expected");
        // doc_a has 1 #rust tag, doc_b has 2 tags (frontmatter + body) -> total 3
        assert_eq!(refs.len(), 3);
    }

    #[test]
    fn test_find_block_references() {
        let abs_daily = if cfg!(windows) {
            Path::new("C:\\daily.md")
        } else {
            Path::new("/daily.md")
        };

        let rel_lsp = Path::new("LSP.md");
        let rel_daily = Path::new("daily.md");

        let doc_lsp = parse_document(
            "# LSP\n\nArchitecture definition here ^mimari-tanim",
            rel_lsp,
        );
        let doc_daily = parse_document("# Daily\n\nSee [[LSP#^mimari-tanim]] for info.", rel_daily);

        let mut state = SatzState::default();
        state.index = Index::build(vec![doc_lsp, doc_daily]);
        state.set_vault_root(Some(if cfg!(windows) {
            Path::new("C:\\").to_path_buf()
        } else {
            Path::new("/").to_path_buf()
        }));

        let uri_daily_str = if cfg!(windows) {
            "file:///C:/daily.md"
        } else {
            "file:///daily.md"
        };

        state.open_docs.insert(
            uri_daily_str.to_string(),
            crate::state::OpenDocument::new(
                uri_daily_str,
                abs_daily.to_path_buf(),
                "# Daily\n\nSee [[LSP#^mimari-tanim]] for info.",
                1,
            ),
        );

        let params = ReferenceParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: uri_daily_str.parse().unwrap(),
                },
                position: Position::new(2, 10), // on [[LSP#^mimari-tanim]]
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: ReferenceContext {
                include_declaration: true,
            },
        };

        let refs = find_references(params, &state).expect("References expected");
        // 1 block definition in LSP.md + 1 link in daily.md = 2
        assert_eq!(refs.len(), 2);
    }

    // ---- harness: references over a small vault ----

    fn root() -> std::path::PathBuf {
        if cfg!(windows) {
            Path::new("C:\\vault").to_path_buf()
        } else {
            Path::new("/vault").to_path_buf()
        }
    }

    fn uri_of(rel: &str) -> String {
        crate::convert::path_to_uri(&root().join(rel))
            .unwrap()
            .as_str()
            .to_string()
    }

    /// `(file, line)` of every reference found from `at` in `open`, sorted as returned.
    fn refs(
        files: &[(&str, &str)],
        open: &str,
        at: (u32, u32),
        include_declaration: bool,
    ) -> Vec<(String, u32)> {
        let mut state = SatzState::default();
        state.index = Index::build(
            files
                .iter()
                .map(|(rel, text)| parse_document(text, Path::new(rel)))
                .collect(),
        );
        state.set_vault_root(Some(root()));
        for (rel, text) in files {
            let uri = uri_of(rel);
            state.open_docs.insert(
                uri.clone(),
                crate::state::OpenDocument::new(&uri, root().join(rel), *text, 1),
            );
        }
        let params = ReferenceParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: uri_of(open).parse().unwrap(),
                },
                position: Position::new(at.0, at.1),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: ReferenceContext {
                include_declaration,
            },
        };
        find_references(params, &state)
            .unwrap_or_default()
            .into_iter()
            .map(|l| {
                let rel = crate::convert::uri_to_path(l.uri.as_str())
                    .unwrap()
                    .strip_prefix(root())
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                (rel, l.range.start.line)
            })
            .collect()
    }

    fn at(rel: &str, line: u32) -> (String, u32) {
        (rel.to_string(), line)
    }

    #[test]
    fn duplicate_headings_only_the_first_owns_the_links() {
        let files = [
            ("a.md", "## Notes\nfirst\n\n## Notes\nsecond\n"),
            ("b.md", "See [[a#Notes]]\n"),
        ];
        assert_eq!(
            refs(&files, "a.md", (0, 4), true),
            vec![at("a.md", 0), at("b.md", 0)]
        );
        // The second heading is only itself: no link resolves to it.
        assert_eq!(refs(&files, "a.md", (3, 4), true), vec![at("a.md", 3)]);
        assert_eq!(
            refs(&files, "a.md", (3, 4), false),
            Vec::<(String, u32)>::new()
        );
    }

    #[test]
    fn include_declaration_false_drops_the_declaration_not_the_cursor_spot() {
        let files = [
            ("a.md", "## Notes\ntext\n"),
            ("b.md", "See [[a#Notes]]\n\nAnd [[a#Notes]] again\n"),
        ];
        // From a link: the linking spots stay (including the one under the cursor), the heading goes.
        assert_eq!(
            refs(&files, "b.md", (0, 8), false),
            vec![at("b.md", 0), at("b.md", 2)]
        );
        assert_eq!(
            refs(&files, "b.md", (0, 8), true),
            vec![at("a.md", 0), at("b.md", 0), at("b.md", 2)]
        );
        // From the heading itself: links only.
        assert_eq!(
            refs(&files, "a.md", (0, 4), false),
            vec![at("b.md", 0), at("b.md", 2)]
        );
    }

    #[test]
    fn include_declaration_false_for_blocks_and_documents() {
        let blocks = [("a.md", "# A\n\ntext ^blk\n"), ("b.md", "[[a#^blk]]\n")];
        assert_eq!(refs(&blocks, "b.md", (0, 3), false), vec![at("b.md", 0)]);
        assert_eq!(refs(&blocks, "a.md", (2, 7), false), vec![at("b.md", 0)]);
        assert_eq!(
            refs(&blocks, "a.md", (2, 7), true),
            vec![at("a.md", 2), at("b.md", 0)]
        );

        let docs = [("a.md", "# A\n"), ("b.md", "[[a]] and [[a]]\n")];
        assert_eq!(
            refs(&docs, "b.md", (0, 2), false),
            vec![at("b.md", 0), at("b.md", 0)]
        );
        assert_eq!(
            refs(&docs, "b.md", (0, 2), true),
            vec![at("a.md", 0), at("b.md", 0), at("b.md", 0)]
        );
    }

    #[test]
    fn references_from_a_nested_link_follow_the_innermost_link() {
        let files = [
            ("a.md", "[see [[inner]]](outer.md)\n"),
            ("inner.md", "# inner\n"),
            ("outer.md", "# outer\n"),
            ("uses_inner.md", "[[inner]]\n"),
            ("uses_outer.md", "[[outer]]\n"),
        ];
        let names = |found: Vec<(String, u32)>| -> Vec<String> {
            let mut v: Vec<String> = found.into_iter().map(|(f, _)| f).collect();
            v.sort();
            v
        };
        let inner = names(refs(&files, "a.md", (0, 8), false));
        assert_eq!(inner, vec!["a.md", "uses_inner.md"]);
        let outer = names(refs(&files, "a.md", (0, 2), false));
        assert_eq!(outer, vec!["a.md", "uses_outer.md"]);
    }

    #[test]
    fn a_block_reference_matches_its_definition_ignoring_case() {
        let files = [("a.md", "para ^abc\n"), ("b.md", "see [[a#^ABC]]\n")];
        let mut found = refs(&files, "b.md", (0, 7), true);
        found.sort();
        assert_eq!(
            found,
            vec![("a.md".to_string(), 0), ("b.md".to_string(), 0)]
        );
        // From the definition side too.
        let mut back = refs(&files, "a.md", (0, 7), true);
        back.sort();
        assert_eq!(back, vec![("a.md".to_string(), 0), ("b.md".to_string(), 0)]);
    }

    #[test]
    fn references_tell_the_same_file_name_in_two_folders_apart() {
        let files = [
            ("b.md", "# root b\n"),
            ("sub/b.md", "# sub b\n"),
            ("sub/a.md", "[t](b.md)\n[u](../b.md)\n"),
            ("c.md", "[[b]]\n"),
        ];
        let mut root_refs = refs(&files, "c.md", (0, 3), false);
        root_refs.sort();
        assert_eq!(
            root_refs,
            vec![("c.md".to_string(), 0), ("sub/a.md".to_string(), 1)]
        );
        let folder_refs = refs(&files, "sub/a.md", (0, 2), false);
        assert_eq!(folder_refs, vec![("sub/a.md".to_string(), 0)]);
    }

    #[test]
    fn references_from_a_link_that_leaves_the_vault_find_nothing() {
        let files = [
            ("out.md", "# out\n"),
            ("sub/a.md", "[t](../../out.md)\n"),
            ("c.md", "[[out]]\n"),
        ];
        assert!(refs(&files, "sub/a.md", (0, 3), true).is_empty());
        // The escaping link is not a reference to `out.md` either.
        assert_eq!(
            refs(&files, "c.md", (0, 3), false),
            vec![("c.md".to_string(), 0)]
        );
    }

    #[test]
    fn heading_references_follow_the_note_the_link_really_reaches() {
        let files = [
            ("b.md", "# Head\n"),
            ("sub/b.md", "# Head\n"),
            ("sub/a.md", "[t](b.md#Head)\n[u](../b.md#Head)\n"),
        ];
        let found = refs(&files, "sub/a.md", (0, 6), false);
        assert_eq!(found, vec![("sub/a.md".to_string(), 0)]);
        let root = refs(&files, "sub/a.md", (1, 6), false);
        assert_eq!(root, vec![("sub/a.md".to_string(), 1)]);
    }

    // ---- a tag inside a block is the tag, not the block ----

    const TAG_IN_BLOCK: [(&str, &str); 3] = [
        (
            "a.md",
            "# A

first #topic ^blk
",
        ),
        (
            "b.md",
            "#topic here

see [[a#^blk]]
",
        ),
        (
            "c.md",
            "only #topic
",
        ),
    ];

    #[test]
    fn a_tag_in_a_block_line_finds_the_tag_uses() {
        let mut found = refs(&TAG_IN_BLOCK, "a.md", (2, 8), true);
        found.sort();
        assert_eq!(found, vec![at("a.md", 2), at("b.md", 0), at("c.md", 0)]);
    }

    #[test]
    fn the_block_id_of_the_same_line_still_finds_the_block_uses() {
        let mut found = refs(&TAG_IN_BLOCK, "a.md", (2, 16), true);
        found.sort();
        assert_eq!(found, vec![at("a.md", 2), at("b.md", 2)]);
    }
}
