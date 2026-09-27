use super::*;
use satz_core::{Index, parse_document};
use std::path::Path;
use tower_lsp_server::ls_types::TextDocumentIdentifier;

#[test]
fn test_semantic_tokens_legend() {
    let legend = semantic_tokens_legend();
    assert_eq!(legend.token_types.len(), 7);
    assert_eq!(legend.token_types[0].as_str(), "link");
    assert_eq!(legend.token_types[1].as_str(), "unresolvedLink");
    assert_eq!(legend.token_types[2].as_str(), "tag");
    assert_eq!(legend.token_types[3].as_str(), "heading");
    assert_eq!(legend.token_types[4].as_str(), "embed");
    assert_eq!(legend.token_types[5].as_str(), "blockAnchor");
    assert_eq!(legend.token_types[6].as_str(), "linkDisplay");
}

#[test]
fn test_semantic_tokens_encoding() {
    let text = "# Title\n\n[[doc-b]] and [[missing-doc]] #rust";
    let rel_path = Path::new("doc-a.md");
    let doc_a = parse_document(text, rel_path);
    let doc_b = parse_document("# Doc B", Path::new("doc-b.md"));

    let mut state = SatzState::default();
    state.index = Index::build(vec![doc_a, doc_b]);
    state.set_vault_root(Some(Path::new("").to_path_buf()));
    state.open_docs.insert(
        "file:///doc-a.md".to_string(),
        crate::state::OpenDocument::new("file:///doc-a.md", rel_path.to_path_buf(), text, 1),
    );

    let params = SemanticTokensParams {
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
        text_document: TextDocumentIdentifier {
            uri: "file:///doc-a.md".parse().unwrap(),
        },
    };

    let result = semantic_tokens_full(params, &state).expect("Tokens result expected");
    if let SemanticTokensResult::Tokens(tokens) = result {
        assert_eq!(tokens.data.len(), 4);

        // Token 0: "# Title" heading (line 0, col 0, len 7, type 3)
        assert_eq!(tokens.data[0].delta_line, 0);
        assert_eq!(tokens.data[0].delta_start, 0);
        assert_eq!(tokens.data[0].length, 7);
        assert_eq!(tokens.data[0].token_type, 3);

        // Token 1: "[[doc-b]]" resolved link (line 2, col 0, len 9, type 0)
        assert_eq!(tokens.data[1].delta_line, 2);
        assert_eq!(tokens.data[1].delta_start, 0);
        assert_eq!(tokens.data[1].length, 9);
        assert_eq!(tokens.data[1].token_type, 0);

        // Token 2: "[[missing-doc]]" unresolved link (line 2, col 14, len 15, type 1)
        assert_eq!(tokens.data[2].delta_line, 0);
        assert_eq!(tokens.data[2].delta_start, 14); // 14 - 0
        assert_eq!(tokens.data[2].length, 15);
        assert_eq!(tokens.data[2].token_type, 1);

        // Token 3: "#rust" tag (line 2, col 30, len 5, type 2)
        assert_eq!(tokens.data[3].delta_line, 0);
        assert_eq!(tokens.data[3].delta_start, 16); // 30 - 14 = 16
        assert_eq!(tokens.data[3].length, 5);
        assert_eq!(tokens.data[3].token_type, 2);
    } else {
        panic!("Expected Tokens");
    }
}

#[test]
fn test_semantic_tokens_anchor_missing_unresolved() {
    let text = "[[doc-b#NonexistentHeading]]";
    let rel_path = Path::new("doc-a.md");
    let doc_a = parse_document(text, rel_path);
    let doc_b = parse_document("# Doc B", Path::new("doc-b.md"));

    let mut state = SatzState::default();
    state.index = Index::build(vec![doc_a, doc_b]);
    state.set_vault_root(Some(Path::new("").to_path_buf()));
    state.open_docs.insert(
        "file:///doc-a.md".to_string(),
        crate::state::OpenDocument::new("file:///doc-a.md", rel_path.to_path_buf(), text, 1),
    );

    let params = SemanticTokensParams {
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
        text_document: TextDocumentIdentifier {
            uri: "file:///doc-a.md".parse().unwrap(),
        },
    };

    let result = semantic_tokens_full(params, &state).expect("Tokens result expected");
    if let SemanticTokensResult::Tokens(tokens) = result {
        assert_eq!(tokens.data.len(), 1);
        // AnchorMissing should have token_type 1 (unresolvedLink)
        assert_eq!(tokens.data[0].token_type, 1);
    } else {
        panic!("Expected Tokens");
    }
}

/// Builds a single-document `SatzState` (plus an empty `doc-b.md` peer so links to it
/// resolve) and runs `semantic_tokens_full` against it, returning the raw token data.
fn run_tokens(text: &str, config: satz_core::VaultConfig) -> Vec<SemanticToken> {
    let rel_path = Path::new("doc-a.md");
    let doc_a = parse_document(text, rel_path);
    let doc_b = parse_document("# Doc B", Path::new("doc-b.md"));

    let mut state = SatzState::default();
    state.index = Index::build(vec![doc_a, doc_b]);
    state.config = config;
    state.set_vault_root(Some(Path::new("").to_path_buf()));
    state.open_docs.insert(
        "file:///doc-a.md".to_string(),
        crate::state::OpenDocument::new("file:///doc-a.md", rel_path.to_path_buf(), text, 1),
    );

    let params = SemanticTokensParams {
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
        text_document: TextDocumentIdentifier {
            uri: "file:///doc-a.md".parse().unwrap(),
        },
    };

    match semantic_tokens_full(params, &state).expect("Tokens result expected") {
        SemanticTokensResult::Tokens(tokens) => tokens.data,
        _ => panic!("Expected Tokens"),
    }
}

#[test]
fn test_wikilink_display_split_default_on() {
    let data = run_tokens("[[doc-b|Alias]]", satz_core::VaultConfig::default());
    assert_eq!(data.len(), 2);
    assert_eq!(data[0].token_type, 0); // "[[doc-b|" — resolved target
    assert_eq!(data[0].length, 8);
    assert_eq!(data[1].token_type, 6); // "Alias]]" — linkDisplay
    assert_eq!(data[1].length, 7);
}

#[test]
fn test_wikilink_display_split_disabled_via_config() {
    let mut config = satz_core::VaultConfig::default();
    config.lsp.semantic_tokens.split_link_display = false;
    let data = run_tokens("[[doc-b|Alias]]", config);
    assert_eq!(data.len(), 1);
    assert_eq!(data[0].token_type, 0);
    assert_eq!(data[0].length, 15);
}

#[test]
fn test_wikilink_without_alias_never_split() {
    let data = run_tokens("[[doc-b]]", satz_core::VaultConfig::default());
    assert_eq!(data.len(), 1);
    assert_eq!(data[0].token_type, 0);
    assert_eq!(data[0].length, 9);
}

#[test]
fn test_embed_display_split() {
    let data = run_tokens("![[doc-b|Alias]]", satz_core::VaultConfig::default());
    assert_eq!(data.len(), 2);
    assert_eq!(data[0].token_type, 4); // "![[doc-b|" — embed
    assert_eq!(data[0].length, 9);
    assert_eq!(data[1].token_type, 6); // "Alias]]" — linkDisplay
    assert_eq!(data[1].length, 7);
}

#[test]
fn test_footnote_resolved_and_unresolved() {
    // `[^a]` has a matching definition and is a real `LinkKind::Footnote` (pulldown-cmark
    // recognized it) -- `[^b]` doesn't, so it's only caught by the manual scan feeding
    // `doc.broken_footnote_refs`. Both should get colored, but differently.
    let text = "Ref one [^a] and ref two [^b].\n\n[^a]: Definition A.\n";
    let data = run_tokens(text, satz_core::VaultConfig::default());
    assert_eq!(data.len(), 2);
    assert_eq!(data[0].token_type, 0); // [^a] resolved
    assert_eq!(data[1].token_type, 1); // [^b] broken
}

// ---- tokens never overlap, and never span lines ----

/// Absolute `(line, start, length, type)` of every token of `text` (a note `doc-a.md` next to
/// an existing `doc-b.md`, so `[[doc-b]]` resolves).
fn decoded(text: &str) -> Vec<(u32, u32, u32, u32)> {
    let rel_path = Path::new("doc-a.md");
    let mut state = SatzState::default();
    state.index = Index::build(vec![
        parse_document(text, rel_path),
        parse_document("# Doc B", Path::new("doc-b.md")),
    ]);
    state.set_vault_root(Some(Path::new("").to_path_buf()));
    state.open_docs.insert(
        "file:///doc-a.md".to_string(),
        crate::state::OpenDocument::new("file:///doc-a.md", rel_path.to_path_buf(), text, 1),
    );
    let params = SemanticTokensParams {
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
        text_document: TextDocumentIdentifier {
            uri: "file:///doc-a.md".parse().unwrap(),
        },
    };
    let Some(SemanticTokensResult::Tokens(tokens)) = semantic_tokens_full(params, &state) else {
        panic!("tokens expected");
    };
    let (mut line, mut start) = (0u32, 0u32);
    tokens
        .data
        .iter()
        .map(|t| {
            line += t.delta_line;
            start = if t.delta_line == 0 {
                start + t.delta_start
            } else {
                t.delta_start
            };
            (line, start, t.length, t.token_type)
        })
        .collect()
}

#[test]
fn a_heading_is_split_around_the_tokens_inside_it() {
    assert_eq!(
        decoded("## See [[doc-b]] #tag"),
        vec![(0, 0, 7, 3), (0, 7, 9, 0), (0, 17, 4, 2)]
    );
    assert_eq!(decoded("## Title ^blk"), vec![(0, 0, 9, 3), (0, 9, 4, 5)]);
    assert_eq!(
        decoded("## A [[doc-b]] end"),
        vec![(0, 0, 5, 3), (0, 5, 9, 0), (0, 14, 4, 3)]
    );
    // Two tokens inside one heading.
    assert_eq!(
        decoded("# [[doc-b]] and [[nope]]"),
        vec![(0, 0, 2, 3), (0, 2, 9, 0), (0, 11, 5, 3), (0, 16, 8, 1)]
    );
}

#[test]
fn utf16_lengths_are_used_for_the_split_pieces() {
    // "## Türkçe 🦀 " is 13 UTF-16 units (the crab is a surrogate pair).
    assert_eq!(
        decoded("## Türkçe 🦀 [[doc-b]]"),
        vec![(0, 0, 13, 3), (0, 13, 9, 0)]
    );
}

#[test]
fn a_link_wrapping_across_lines_is_coloured_on_every_line() {
    assert_eq!(
        decoded("[a\nb](doc-b.md) tail"),
        vec![(0, 0, 2, 0), (1, 0, 12, 0)]
    );
    assert_eq!(
        decoded("[a\r\nb](doc-b.md) tail"),
        vec![(0, 0, 2, 0), (1, 0, 12, 0)]
    );
    assert_eq!(
        decoded("x [one\ntwo\nthree](doc-b.md)"),
        vec![(0, 2, 4, 0), (1, 0, 3, 0), (2, 0, 16, 0)]
    );
}

#[test]
fn tokens_are_strictly_ordered_and_never_overlap() {
    for text in [
        "## See [[doc-b]] #tag ^blk\n\n[a\nb](doc-b.md) #t [[nope|x]] [^1]\n\n[^1]: n\n",
        "# [[doc-b]][[doc-b]]#a#b\n",
        "### ![[doc-b]] ^x\r\n\r\n#tag [[doc-b#h]]\r\n",
        "Setext [[doc-b]] #t\n=====\n",
    ] {
        let tokens = decoded(text);
        for pair in tokens.windows(2) {
            let (l1, s1, n1, _) = pair[0];
            let (l2, s2, _, _) = pair[1];
            assert!(
                l2 > l1 || (l2 == l1 && s2 >= s1 + n1),
                "overlap or misorder {pair:?} in {text:?}: {tokens:?}"
            );
        }
        assert!(tokens.iter().all(|t| t.2 > 0), "{text:?}");
    }
}

#[test]
fn external_links_are_not_painted_as_broken_links() {
    assert_eq!(
        decoded(
            "[m](mailto:a@b.c) [w](https://example.com/x) <https://e.org>
"
        ),
        vec![],
        "a link that leaves the vault is neither resolved nor unresolved"
    );
    // Next to real links they change nothing about those.
    assert_eq!(
        decoded(
            "[m](mailto:a@b.c) [[doc-b]] [[nope]]
"
        ),
        vec![(0, 18, 9, 0), (0, 28, 8, 1)]
    );
}

#[test]
fn columns_after_a_tag_on_a_crlf_line_match_the_lf_line() {
    let lf = decoded(
        "#tag [[doc-b]] [[nope]]

[[doc-b]]
",
    );
    let crlf = decoded(
        "#tag [[doc-b]] [[nope]]

[[doc-b]]
",
    );
    assert_eq!(lf, crlf);
    assert_eq!(lf.len(), 4);
}

/// The type of the token that starts a note made of `link` alone (`doc-b.md` is next to it).
fn type_of_the_first_token(link: &str) -> u32 {
    let data = decoded(&format!("{link}\n"));
    let first = data
        .iter()
        .find(|t| t.0 == 0 && t.1 == 0)
        .unwrap_or_else(|| panic!("no token at the start of {link}: {data:?}"));
    first.3
}

#[test]
fn a_markdown_link_to_a_real_anchor_that_is_no_heading_is_coloured_as_resolved() {
    // The diagnostics do not call `#top` broken; nor do the colours. A wikilink has no such
    // rule, and a fragment that is nothing is still an unresolved link.
    for (link, expected) in [
        ("[t](doc-b.md#top)", 0),
        ("[t](doc-b.md#TOP)", 0),
        ("[t](#top)", 0),
        ("[t](doc-b.md#nothing)", 1),
        ("[t](#nothing)", 1),
        ("[[doc-b#top]]", 1),
        ("[t](ghost.md#top)", 1),
    ] {
        assert_eq!(type_of_the_first_token(link), expected, "{link}");
    }
}

#[test]
fn an_embed_is_coloured_by_what_it_resolves_to() {
    // `embed` (4) for an embed of something that is there, `unresolvedLink` (1) for one that
    // is not (the diagnostic and the hint say so too), and an embed of an address elsewhere
    // has nothing to resolve.
    for (link, expected) in [
        ("![[doc-b]]", 4),
        ("![[doc-b#Doc B]]", 4),
        ("![[doc-b|300]]", 4),
        ("![[ghost]]", 1),
        ("![[pic.png]]", 1),
        ("![[doc-b#nothing]]", 1),
        ("![[#nothing]]", 1),
        ("![[https://example.com/x.png]]", 4),
    ] {
        assert_eq!(type_of_the_first_token(link), expected, "{link}");
    }
}
