// Test states are built field by field so each test shows exactly what it sets up.
#![allow(clippy::field_reassign_with_default)]

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
