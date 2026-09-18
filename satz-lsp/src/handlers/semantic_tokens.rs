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

/// Computes SemanticTokens for links, tags, headings, and block anchors across the full document.
pub fn semantic_tokens_full(
    params: SemanticTokensParams,
    state: &SatzState,
) -> Option<SemanticTokensResult> {
    let uri = params.text_document.uri.as_str();
    tracing::debug!(uri, "semantic_tokens_full");
    let open_doc = state.open_docs.get(uri)?;
    let rel_path =
        crate::state::SatzState::get_rel_path(&open_doc.path, state.vault_root.as_deref());
    let rel_path_str = rel_path.to_string_lossy().replace('\\', "/");
    let doc_id = satz_core::DocId::new(&rel_path_str);
    let doc = state.index.get_doc(&doc_id)?;

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
                if link.target_doc.starts_with("http://") || link.target_doc.starts_with("https://")
                {
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
            // A `LinkKind::Footnote` only ever exists here for a `[^label]` reference that
            // already has a matching definition -- pulldown-cmark leaves an undefined reference
            // as plain text with no event at all, so there's no "unresolved" case reachable to
            // distinguish; every one gets the plain `link` color instead of no color at all.
            LinkKind::Footnote => {
                raw_tokens.push(RawToken {
                    range: link.range,
                    token_type: 0,
                });
            }
        }
    }

    // 4. Block anchors (type 5)
    for block in &doc.blocks {
        raw_tokens.push(RawToken {
            range: block.range,
            token_type: 5,
        });
    }

    // Sort tokens by start byte offset
    raw_tokens.sort_by_key(|t| t.range.start);

    let mut semantic_tokens: Vec<SemanticToken> = Vec::with_capacity(raw_tokens.len());
    let mut prev_line = 0u32;
    let mut prev_start = 0u32;

    for raw in raw_tokens {
        if raw.range.is_empty() {
            continue;
        }

        let start_pos = doc.line_index.byte_to_position(raw.range.start);
        // Trim trailing newline or CRLF from the token range
        let mut end_byte = raw.range.end;
        while end_byte > raw.range.start {
            let b = doc.line_index.source().as_bytes().get(end_byte - 1);
            if b == Some(&b'\n') || b == Some(&b'\r') {
                end_byte -= 1;
            } else {
                break;
            }
        }
        let end_pos = doc.line_index.byte_to_position(end_byte);

        let line = start_pos.line;
        let start_char = start_pos.character;
        let length = if end_pos.line == line {
            end_pos.character.saturating_sub(start_char)
        } else {
            1
        };

        if length == 0 {
            continue;
        }

        let delta_line = line.saturating_sub(prev_line);
        let delta_start = if delta_line == 0 {
            start_char.saturating_sub(prev_start)
        } else {
            start_char
        };

        semantic_tokens.push(SemanticToken {
            delta_line,
            delta_start,
            length,
            token_type: raw.token_type,
            token_modifiers_bitset: 0,
        });

        prev_line = line;
        prev_start = start_char;
    }

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
    fn test_footnote_reference_gets_link_color() {
        // A `[^b]` with no matching `[^b]: ...` definition isn't parsed as a footnote at all by
        // pulldown-cmark (it's left as literal text, no event fired) -- only a reference that
        // already resolves ever shows up as a `LinkKind::Footnote` link to color here.
        let text = "Ref one [^a] and ref two [^b].\n\n[^a]: Definition A.\n";
        let data = run_tokens(text, satz_core::VaultConfig::default());
        assert_eq!(data.len(), 1);
        assert_eq!(data[0].token_type, 0);
    }
}
