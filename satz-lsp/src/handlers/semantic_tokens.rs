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

                let token_type = match crate::handlers::diagnostics::as_the_user_sees_it(
                    link,
                    state
                        .index
                        .resolve_link_full_with_config(link, Some(doc), Some(&state.config)),
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
                // An embed of something that is there has its own colour; one that is not is as
                // broken as a link (the diagnostic and the hint say so too).
                let token_type = if satz_core::model::link::is_external_target(&link.target_doc) {
                    4
                } else {
                    match state.index.resolve_link_full_with_config(
                        link,
                        Some(doc),
                        Some(&state.config),
                    ) {
                        satz_core::LinkResolution::Resolved { .. } => 4,
                        satz_core::LinkResolution::AnchorMissing { .. }
                        | satz_core::LinkResolution::DocMissing => 1,
                    }
                };
                push_link_tokens(
                    &mut raw_tokens,
                    source,
                    link,
                    token_type,
                    split_link_display,
                );
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
mod tests;
