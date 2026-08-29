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

/// Computes SemanticTokens for links, tags, and headings across the full document.
pub fn semantic_tokens_full(
    params: SemanticTokensParams,
    state: &SatzState,
) -> Option<SemanticTokensResult> {
    let uri = params.text_document.uri.as_str();
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

    // 3. Links (type 0 for resolved, 1 for unresolved, 4 for embed)
    for link in &doc.links {
        match link.kind {
            LinkKind::WikiLink | LinkKind::Markdown => {
                if link.target_doc.starts_with("http://") || link.target_doc.starts_with("https://")
                {
                    continue;
                }

                let token_type = match state.index.resolve_link_full(link, Some(doc)) {
                    satz_core::LinkResolution::Resolved { .. } => 0,
                    satz_core::LinkResolution::AnchorMissing { .. }
                    | satz_core::LinkResolution::DocMissing => 1,
                };
                raw_tokens.push(RawToken {
                    range: link.range,
                    token_type,
                });
            }
            LinkKind::Embed => {
                raw_tokens.push(RawToken {
                    range: link.range,
                    token_type: 4,
                });
            }
            LinkKind::Footnote => {}
        }
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
        assert_eq!(legend.token_types.len(), 5);
        assert_eq!(legend.token_types[0].as_str(), "link");
        assert_eq!(legend.token_types[1].as_str(), "unresolvedLink");
        assert_eq!(legend.token_types[2].as_str(), "tag");
        assert_eq!(legend.token_types[3].as_str(), "heading");
        assert_eq!(legend.token_types[4].as_str(), "embed");
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
}
