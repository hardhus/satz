use crate::state::SatzState;
use satz_core::LinkKind;
use tower_lsp_server::ls_types::{
    SemanticToken, SemanticTokenType, SemanticTokens, SemanticTokensLegend, SemanticTokensParams,
    SemanticTokensResult,
};

pub const TOKEN_TYPES: &[&str] = &[
    "link",           // 0 - resolved link
    "unresolvedLink", // 1 - broken / unresolved link
    "tag",            // 2 - #tag
    "heading",        // 3 - heading
    "embed",          // 4 - ![[embed]]
    "blockAnchor",    // 5 - ^block-anchor
    "linkDisplay",    // 6 - the `|display` part of a [[target|display]] link, when split
];

pub fn semantic_tokens_legend() -> SemanticTokensLegend {
    SemanticTokensLegend {
        token_types: TOKEN_TYPES
            .iter()
            .map(|t| SemanticTokenType::new(t))
            .collect(),
        token_modifiers: vec![],
    }
}

struct RawToken {
    range: satz_core::ByteRange,
    token_type: u32,
}

/// Pushes one token for `link.range`, or two if `split` is true and the link has a `|display`
/// part: the first (of `target_token_type`) covers `[[target#heading|` (pipe included), the
/// second (type 6, `linkDisplay`) covers `display]]`. Splits on the FIRST `|` byte in the link's
/// source text, matching `inline_scan::split_once('|')`'s own target/display split.
fn push_link_tokens(
    raw_tokens: &mut Vec<RawToken>,
    source: &str,
    link: &satz_core::Link,
    target_token_type: u32,
    split: bool,
) {
    if split
        && link.display.is_some()
        && let Some(pipe_offset) = source[link.range.start..link.range.end].find('|')
    {
        let split_point = link.range.start + pipe_offset + 1;
        raw_tokens.push(RawToken {
            range: satz_core::ByteRange::new(link.range.start, split_point),
            token_type: target_token_type,
        });
        raw_tokens.push(RawToken {
            range: satz_core::ByteRange::new(split_point, link.range.end),
            token_type: 6,
        });
        return;
    }
    raw_tokens.push(RawToken {
        range: link.range,
        token_type: target_token_type,
    });
}

/// A token on a single line, in LSP coordinates (UTF-16 columns).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AbsToken {
    line: u32,
    start: u32,
    length: u32,
    token_type: u32,
}

/// Turns raw byte-range tokens into non-overlapping, single-line, sorted tokens.
///
/// The LSP forbids overlapping tokens (unless the client opts in) and a token can't span lines:
/// - a range is cut at every line break (the break itself is never coloured), so a link that
///   wraps is coloured on each of its lines;
/// - where tokens overlap, the shorter one wins and the longer one is cut around it, so a
///   heading keeps its colour on the words but yields to the link/tag/anchor inside it. Between
///   equally long ones the one listed first wins. A leftover piece that is only whitespace is
///   dropped.
fn layout_tokens(raw: Vec<RawToken>, line_index: &satz_core::LineIndex) -> Vec<AbsToken> {
    let source = line_index.source();

    // 1. Cut into per-line pieces: (start, end, type, order).
    let mut pieces: Vec<(usize, usize, u32, usize)> = Vec::new();
    for (order, token) in raw.iter().enumerate() {
        let end = token.range.end.min(source.len());
        let mut start = token.range.start.min(end);
        while start < end {
            let line_end = source[start..end].find('\n').map_or(end, |i| start + i);
            let piece_end = source[start..line_end].trim_end_matches('\r').len() + start;
            if piece_end > start {
                pieces.push((start, piece_end, token.token_type, order));
            }
            start = line_end + 1;
        }
    }

    // 2. Shorter first; each piece takes the parts of its range no shorter piece already holds.
    pieces.sort_by_key(|&(start, end, _, order)| (end - start, order));
    let mut occupied: Vec<(usize, usize)> = Vec::new(); // disjoint, sorted by start
    let mut placed: Vec<(usize, usize, u32)> = Vec::new();
    for (start, end, token_type, _) in pieces {
        let mut cursor = start;
        let mut fragments: Vec<(usize, usize)> = Vec::new();
        let first = occupied.partition_point(|&(_, occ_end)| occ_end <= start);
        for &(occ_start, occ_end) in &occupied[first..] {
            if occ_start >= end {
                break;
            }
            if occ_start > cursor {
                fragments.push((cursor, occ_start));
            }
            cursor = cursor.max(occ_end);
        }
        if cursor < end {
            fragments.push((cursor, end));
        }
        for (frag_start, frag_end) in fragments {
            let was_cut = (frag_start, frag_end) != (start, end);
            if was_cut && source[frag_start..frag_end].trim().is_empty() {
                continue;
            }
            let at = occupied.partition_point(|&(s, _)| s < frag_start);
            occupied.insert(at, (frag_start, frag_end));
            placed.push((frag_start, frag_end, token_type));
        }
    }

    // 3. Sorted, in LSP coordinates.
    placed.sort_by_key(|&(start, _, _)| start);
    placed
        .into_iter()
        .filter_map(|(start, end, token_type)| {
            let from = line_index.byte_to_position(start);
            let to = line_index.byte_to_position(end);
            debug_assert_eq!(from.line, to.line, "pieces are single-line");
            let length = to.character.saturating_sub(from.character);
            (length > 0).then_some(AbsToken {
                line: from.line,
                start: from.character,
                length,
                token_type,
            })
        })
        .collect()
}

/// Delta-encodes sorted, non-overlapping tokens as the LSP wants them.
fn encode_tokens(tokens: Vec<AbsToken>) -> Vec<SemanticToken> {
    let mut out = Vec::with_capacity(tokens.len());
    let (mut prev_line, mut prev_start) = (0u32, 0u32);
    for token in tokens {
        debug_assert!(
            token.line > prev_line || (token.line == prev_line && token.start >= prev_start),
            "tokens must be sorted"
        );
        let delta_line = token.line - prev_line;
        let delta_start = if delta_line == 0 {
            token.start - prev_start
        } else {
            token.start
        };
        out.push(SemanticToken {
            delta_line,
            delta_start,
            length: token.length,
            token_type: token.token_type,
            token_modifiers_bitset: 0,
        });
        prev_line = token.line;
        prev_start = token.start;
    }
    out
}

/// Computes SemanticTokens for links, tags, headings, and block anchors across the full document.
pub fn semantic_tokens_full(
    params: SemanticTokensParams,
    state: &SatzState,
) -> Option<SemanticTokensResult> {
    let uri = params.text_document.uri.as_str();
    tracing::debug!(uri, "semantic_tokens_full");
    let (_, doc) = state.doc_for_uri(uri)?;

    let mut raw_tokens: Vec<RawToken> = Vec::new();

    // 1. Headings (type 3)
    for heading in &doc.headings {
        raw_tokens.push(RawToken {
            range: heading.range,
            token_type: 3,
        });
    }

    // 2. Tags (type 2)
    for tag in &doc.tags {
        raw_tokens.push(RawToken {
            range: tag.range,
            token_type: 2,
        });
    }

    // 3. Links (type 0 for resolved, 1 for unresolved, 4 for embed, 6 for a split `|display`)
    let split_link_display = state.config.lsp.semantic_tokens.split_link_display;
    let source = doc.line_index.source();
    for link in &doc.links {
        match link.kind {
            LinkKind::WikiLink | LinkKind::Markdown => {
                if satz_core::model::link::is_external_target(&link.target_doc) {
                    continue;
                }

                let token_type = match state.index.resolve_link_full_with_config(
                    link,
                    Some(doc),
                    Some(&state.config),
                ) {
                    satz_core::LinkResolution::Resolved { .. } => 0,
                    satz_core::LinkResolution::AnchorMissing { .. }
                    | satz_core::LinkResolution::DocMissing => 1,
                };
                push_link_tokens(
                    &mut raw_tokens,
                    source,
                    link,
                    token_type,
                    link.kind == LinkKind::WikiLink && split_link_display,
                );
            }
            LinkKind::Embed => {
                push_link_tokens(&mut raw_tokens, source, link, 4, split_link_display);
            }
            // A `LinkKind::Footnote` in `doc.links` only ever exists for a `[^label]` reference
            // that already has a matching definition (pulldown-cmark leaves an undefined
            // reference as plain text with no event at all) -- so this is always "resolved".
            LinkKind::Footnote => {
                raw_tokens.push(RawToken {
                    range: link.range,
                    token_type: 0,
                });
            }
        }
    }

    // 3b. Broken footnote references (type 1) -- found by a manual text scan independent of
    // pulldown-cmark, since an undefined `[^label]` never becomes a `LinkKind::Footnote` link.
    for link in &doc.broken_footnote_refs {
        raw_tokens.push(RawToken {
            range: link.range,
            token_type: 1,
        });
    }

    // 4. Block anchors (type 5)
    for block in &doc.blocks {
        raw_tokens.push(RawToken {
            range: block.range,
            token_type: 5,
        });
    }

    let semantic_tokens = encode_tokens(layout_tokens(raw_tokens, &doc.line_index));

    Some(SemanticTokensResult::Tokens(SemanticTokens {
        result_id: None,
        data: semantic_tokens,
    }))
}

#[cfg(test)]
mod tests {
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

        let mut state = SatzState {
            index: Index::build(vec![doc_a, doc_b]),
            vault_root: Some(Path::new("").to_path_buf()),
            ..Default::default()
        };
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

        let mut state = SatzState {
            index: Index::build(vec![doc_a, doc_b]),
            vault_root: Some(Path::new("").to_path_buf()),
            ..Default::default()
        };
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

        let mut state = SatzState {
            index: Index::build(vec![doc_a, doc_b]),
            vault_root: Some(Path::new("").to_path_buf()),
            config,
            ..Default::default()
        };
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
        let mut state = SatzState {
            index: Index::build(vec![
                parse_document(text, rel_path),
                parse_document("# Doc B", Path::new("doc-b.md")),
            ]),
            vault_root: Some(Path::new("").to_path_buf()),
            ..Default::default()
        };
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
        let Some(SemanticTokensResult::Tokens(tokens)) = semantic_tokens_full(params, &state)
        else {
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
}
