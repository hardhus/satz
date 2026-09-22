use std::borrow::Cow;
use std::cmp::{Ordering, Reverse};

use serde_json::Value;
use tower_lsp_server::ls_types::{
    CompletionItem, CompletionItemKind, CompletionList, CompletionParams, CompletionResponse,
    CompletionTextEdit, Documentation, MarkupContent, MarkupKind, Position, Range, TextEdit,
};

use crate::state::SatzState;

/// What ranking needs to know about a candidate, so the same order is used whether a candidate is
/// a finished item or a light description of one that is built only if it is offered.
trait Ranked {
    fn label(&self) -> Cow<'_, str>;
    /// What the typed query is matched against.
    fn filter_text(&self) -> &str;
    fn detail(&self) -> Option<Cow<'_, str>>;
}

impl Ranked for CompletionItem {
    fn label(&self) -> Cow<'_, str> {
        Cow::Borrowed(&self.label)
    }

    fn filter_text(&self) -> &str {
        self.filter_text.as_deref().unwrap_or(&self.label)
    }

    fn detail(&self) -> Option<Cow<'_, str>> {
        self.detail.as_deref().map(Cow::Borrowed)
    }
}

/// A candidate's rank: the cheap part (match tier, then score) and where it is in the list, which
/// is the order it was found in and decides between candidates that rank the same in every other
/// way. Small on purpose: ranking moves these around, never the candidates themselves.
struct Entry {
    coarse: (u8, Reverse<u32>),
    at: usize,
}

/// An entry whose folded label is worked out: the expensive part of the order.
struct Keyed {
    entry: Entry,
    key: String,
}

impl Keyed {
    fn new<T: Ranked>(candidates: &[T], entry: Entry) -> Self {
        let key = satz_core::fold_key(&candidates[entry.at].label());
        Self { entry, key }
    }

    /// By folded label, then label, then detail, then the order found (a total order).
    fn by_label<T: Ranked>(candidates: &[T], a: &Self, b: &Self) -> Ordering {
        let (x, y) = (&candidates[a.entry.at], &candidates[b.entry.at]);
        a.key
            .cmp(&b.key)
            .then_with(|| x.label().cmp(&y.label()))
            .then_with(|| x.detail().cmp(&y.detail()))
            .then_with(|| a.entry.at.cmp(&b.entry.at))
    }
}

/// The places (in `candidates`) of the best `k`, in order: by `coarse`, and among equal `coarse` by
/// label. Only the entries that can be among the `k` get their label folded: with a query, most
/// fall behind on `coarse` alone.
fn top_k<T: Ranked>(candidates: &[T], mut entries: Vec<Entry>, k: usize) -> Vec<usize> {
    let mut ties = Vec::new();
    if entries.len() > k {
        // Everything ahead of the k-th on `coarse` is in; everything behind is out; the entries
        // equal to it compete for the places that are left.
        entries.select_nth_unstable_by(k - 1, |a, b| a.coarse.cmp(&b.coarse));
        let edge = entries[k - 1].coarse;
        let (ahead, rest): (Vec<_>, Vec<_>) = entries.into_iter().partition(|e| e.coarse < edge);
        entries = ahead;
        ties = rest.into_iter().filter(|e| e.coarse == edge).collect();
    }
    let mut ahead: Vec<Keyed> = entries
        .into_iter()
        .map(|e| Keyed::new(candidates, e))
        .collect();
    ahead.sort_unstable_by(|a, b| {
        a.entry
            .coarse
            .cmp(&b.entry.coarse)
            .then_with(|| Keyed::by_label(candidates, a, b))
    });
    let mut out: Vec<usize> = ahead.into_iter().map(|k| k.entry.at).collect();

    if !ties.is_empty() {
        let places = k - out.len();
        let mut ties: Vec<Keyed> = ties
            .into_iter()
            .map(|e| Keyed::new(candidates, e))
            .collect();
        if ties.len() > places {
            ties.select_nth_unstable_by(places - 1, |a, b| Keyed::by_label(candidates, a, b));
            ties.truncate(places);
        }
        ties.sort_unstable_by(|a, b| Keyed::by_label(candidates, a, b));
        out.extend(ties.into_iter().map(|k| k.entry.at));
    }
    out
}

/// The candidates that are answered, in order, and whether more were left out.
struct Ranking<T> {
    items: Vec<T>,
    incomplete: bool,
}

/// Sorts (by folded label, then detail, so the same request always gives the same list whatever
/// order the index iterates in) and cuts at `limit` (0 = no limit). A cut answer is incomplete, so
/// the client asks again as the user types.
///
/// When the list has to be cut, what the user has typed decides what stays: without that, the cut
/// keeps the first candidates of the alphabet and a note or heading further down (`Nesne`) is
/// never offered, however much of its name is typed.
fn rank<T: Ranked>(candidates: Vec<T>, limit: usize, query: &str) -> Ranking<T> {
    let total = candidates.len();
    let unranked = || -> Vec<Entry> {
        (0..total)
            .map(|at| Entry {
                coarse: (0, Reverse(0)),
                at,
            })
            .collect()
    };

    let (places, incomplete) = if limit == 0 || total <= limit {
        (top_k(&candidates, unranked(), total.max(1)), false)
    } else {
        let query = satz_core::fold_key(query);
        if query.is_empty() {
            (top_k(&candidates, unranked(), limit), true)
        } else {
            let mut ranker = crate::rank::Ranker::new(&query);
            let scored: Vec<Entry> = candidates
                .iter()
                .enumerate()
                .filter_map(|(at, item)| {
                    let text = satz_core::fold_key(item.filter_text());
                    let score = ranker.score(&text)?;
                    let tier = if text == query {
                        0
                    } else if text.starts_with(&query) {
                        1
                    } else {
                        2
                    };
                    Some(Entry {
                        coarse: (tier, Reverse(score)),
                        at,
                    })
                })
                .collect();
            let matched = scored.len();
            (top_k(&candidates, scored, limit), matched > limit)
        }
    };

    let mut slots: Vec<Option<T>> = candidates.into_iter().map(Some).collect();
    Ranking {
        items: places
            .into_iter()
            .filter_map(|at| slots[at].take())
            .collect(),
        incomplete,
    }
}

fn response_of(items: Vec<CompletionItem>, incomplete: bool) -> CompletionResponse {
    if incomplete {
        CompletionResponse::List(CompletionList {
            is_incomplete: true,
            items,
        })
    } else {
        CompletionResponse::Array(items)
    }
}

/// The answer for a list of finished items (see `rank`).
fn respond(items: Vec<CompletionItem>, limit: usize, query: &str) -> CompletionResponse {
    let ranking = rank(items, limit, query);
    response_of(ranking.items, ranking.incomplete)
}

/// A note, alias or heading that could be offered, before anything is built for it: cheap to make,
/// so all of them can be ranked and only the ones that are answered turned into items.
struct Candidate<'a> {
    doc: &'a satz_core::Document,
    hit: Hit<'a>,
}

enum Hit<'a> {
    /// The note itself.
    Title,
    Alias(&'a str),
    /// A heading's text, trimmed (never empty).
    Heading(&'a str),
}

/// What a note is called in the list: its title, or its id when it has none.
fn title_label(doc: &satz_core::Document) -> &str {
    if doc.title != "Untitled" && !doc.title.is_empty() {
        &doc.title
    } else {
        doc.id.as_str()
    }
}

impl Ranked for Candidate<'_> {
    fn label(&self) -> Cow<'_, str> {
        match self.hit {
            Hit::Title => Cow::Borrowed(title_label(self.doc)),
            Hit::Alias(alias) => Cow::Owned(format!("{} (alias)", alias)),
            Hit::Heading(text) => Cow::Borrowed(text),
        }
    }

    fn filter_text(&self) -> &str {
        match self.hit {
            Hit::Title => title_label(self.doc),
            Hit::Alias(text) | Hit::Heading(text) => text,
        }
    }

    fn detail(&self) -> Option<Cow<'_, str>> {
        Some(match self.hit {
            Hit::Title => Cow::Borrowed(self.doc.id.as_str()),
            Hit::Alias(_) => Cow::Owned(format!("Alias for: {}", self.doc.title)),
            Hit::Heading(_) => Cow::Owned(format!("Heading in {}", title_label(self.doc))),
        })
    }
}

impl Candidate<'_> {
    /// The item for a note, alias or heading. `insert_text` is always the document's own
    /// vault-relative path (extension stripped), never its title: a title is free-form prose the
    /// user should be able to reword at any time (this is a book, chapters get retitled) without
    /// silently breaking every wikilink that was inserted by completion -- paths only change via
    /// `rename`, which already rewrites every link (of any style) pointing at the renamed document.
    fn build(&self, range: Range, close_suffix: &str) -> CompletionItem {
        let d = self.doc;
        let path_str = d.path.to_string_lossy().replace('\\', "/");
        let insert_base = path_str.strip_suffix(".md").unwrap_or(&path_str);
        let data = Some(serde_json::json!({ "doc_id": d.id.as_str() }));
        match self.hit {
            Hit::Title => {
                let label = title_label(d);
                CompletionItem {
                    label: label.to_string(),
                    kind: Some(CompletionItemKind::FILE),
                    detail: Some(d.id.as_str().to_string()),
                    text_edit: Some(completion_text_edit(
                        range,
                        format!("{}{}", insert_base, close_suffix),
                    )),
                    filter_text: Some(label.to_string()),
                    data,
                    ..Default::default()
                }
            }
            Hit::Alias(alias) => CompletionItem {
                label: format!("{} (alias)", alias),
                kind: Some(CompletionItemKind::REFERENCE),
                detail: Some(format!("Alias for: {}", d.title)),
                text_edit: Some(completion_text_edit(
                    range,
                    format!("{}{}", alias, close_suffix),
                )),
                filter_text: Some(alias.to_string()),
                data,
                ..Default::default()
            },
            // So e.g. typing "olgu" can directly surface a `## Olgu` heading buried in some other
            // document as `path#Olgu`, without first having to complete to that document and then
            // separately complete `#`. No manual `sort_text` bias here: a short, close-to-exact
            // heading label like "Olgu" already ranks above an unrelated, much longer title in any
            // reasonable client-side fuzzy matcher, so hand-tuning order here would just as likely
            // fight the client's own scoring as help it.
            Hit::Heading(text) => CompletionItem {
                label: text.to_string(),
                kind: Some(CompletionItemKind::FIELD),
                detail: Some(format!("Heading in {}", title_label(d))),
                text_edit: Some(completion_text_edit(
                    range,
                    format!("{}#{}{}", insert_base, text, close_suffix),
                )),
                filter_text: Some(text.to_string()),
                data,
                ..Default::default()
            },
        }
    }
}

/// Builds an explicit replace-range `text_edit` covering `[query_start, cursor)` instead of a
/// bare `insert_text`. Without this, it's up to the client to guess how much of the
/// already-typed query to replace -- ambiguous and, per one field report, inconsistent between
/// a document's first and second wikilink completion in the same session (a stray extra `]]`,
/// or a garbled single-bracket result). An explicit range removes that guesswork entirely.
fn completion_text_edit(range: Range, new_text: String) -> CompletionTextEdit {
    CompletionTextEdit::Edit(TextEdit { range, new_text })
}

pub fn completion(params: CompletionParams, state: &SatzState) -> Option<CompletionResponse> {
    let uri = params.text_document_position.text_document.uri.as_str();
    let pos = params.text_document_position.position;
    tracing::debug!(uri, ?pos, "completion");

    let (open_doc, doc) = state.doc_for_uri(uri)?;

    // Byte-offset/text-scan against the LIVE rope, not `doc.line_index`: `doc` is the
    // debounced (200-500ms) reparse snapshot, but completion re-fires immediately on every
    // `[`/`#`/`^` keystroke, faster than that debounce can settle. Scanning stale text here
    // corrupts the line-prefix/closing-bracket checks below -- e.g. producing a duplicated
    // `]]` when a second wikilink is typed quickly on the same line right after a first one.
    // Only the line the cursor is on is needed (copied once), not the whole document: every
    // decision below looks at that line.
    let line_number = (pos.line as usize).min(open_doc.rope.len_lines().saturating_sub(1));
    let line_text = open_doc.rope.line(line_number).to_string();
    let source: &str = line_text.trim_end_matches(['\r', '\n']);
    let line_start_offset = 0usize;
    // The column is in UTF-16 units; a column past the end means the end of the line, and one in the
    // middle of a surrogate pair stays before that character.
    let mut byte_offset = 0usize;
    let mut units = 0u32;
    for c in source.chars() {
        if units + c.len_utf16() as u32 > pos.character {
            break;
        }
        units += c.len_utf16() as u32;
        byte_offset += c.len_utf8();
    }
    let line_no = line_number as u32;
    let col16 = |byte: usize| source[..byte].encode_utf16().count() as u32;
    let range_at = |start: usize, end: usize| {
        Range::new(
            Position::new(line_no, col16(start)),
            Position::new(line_no, col16(end)),
        )
    };
    let limit = state.config.lsp.completion_limit;

    // Get prefix of the current line up to byte_offset
    let line_prefix = &source[line_start_offset..byte_offset];

    // The rest of the word the cursor is inside (`[[Ol|gu]]`) is part of what is being replaced,
    // or the completion would leave it behind (`[[doc-bgu]]`). It ends at a bracket, `|`, `#` or `^`.
    // Whitespace ends the word too, whichever comes first: text further along the line (`x^2`, a
    // later `#tag`, a table `|`) is not part of what is being completed and must survive.
    let tail = &source[byte_offset..];
    let word_end = byte_offset
        + tail
            .find(|c: char| matches!(c, ']' | '|' | '#' | '^') || c.is_whitespace())
            .unwrap_or(tail.len());
    // Closing brackets after the word: none -> `]]`, one -> the missing `]`, both -> nothing.
    // (The link may continue with `|display` or `#anchor` before its closing `]]`.)
    let after_word = &source[word_end..];
    let closed_later = after_word
        .split("[[")
        .next()
        .is_some_and(|s| s.contains("]]"));
    let close_suffix = if closed_later {
        ""
    } else if after_word.starts_with(']') {
        "]"
    } else {
        "]]"
    };

    // 1. Check for wikilink completion: `[[...`
    // A `[[` that a `]]` has already closed on this line is finished text, not a link being typed.
    let open_bracket = line_prefix
        .rfind("[[")
        .filter(|idx| !line_prefix[idx + 2..].contains("]]"));
    if let Some(open_bracket_idx) = open_bracket {
        let inside_wikilink = &line_prefix[open_bracket_idx + 2..];
        let inside_wikilink_start = line_start_offset + open_bracket_idx + 2;

        // After `|` the user is writing the link's display text: nothing to complete.
        if inside_wikilink.contains('|') {
            return Some(CompletionResponse::Array(vec![]));
        }

        // Check if inside heading or block reference `[[doc#...` or `[[#...`
        if let Some((target_doc_str, heading_or_block)) = inside_wikilink.split_once('#') {
            let heading_or_block_start = inside_wikilink_start + target_doc_str.len() + 1;
            let target_id = if target_doc_str.is_empty() {
                &doc.id
            } else if let Some(resolved) = state.index.resolve_link(target_doc_str) {
                resolved
            } else {
                tracing::debug!(
                    target_doc_str,
                    "completion: returning candidates count=0 (target doc did not resolve)"
                );
                return Some(CompletionResponse::Array(vec![]));
            };

            if let Some(target_doc) = state.index.get_doc(target_id) {
                if let Some(_block_prefix) = heading_or_block.strip_prefix('^') {
                    // Block anchor completion: `[[doc#^...`. The replaced range includes the
                    // typed `^` because every new text starts with its own.
                    let range = range_at(heading_or_block_start, word_end);
                    let items: Vec<CompletionItem> = target_doc
                        .blocks
                        .iter()
                        .map(|b| {
                            let new_text = format!("^{}{}", b.id, close_suffix);
                            CompletionItem {
                                label: format!("^{}", b.id),
                                kind: Some(CompletionItemKind::VARIABLE),
                                detail: Some("Block Anchor".to_string()),
                                text_edit: Some(completion_text_edit(range, new_text)),
                                filter_text: Some(format!("^{}", b.id)),
                                ..Default::default()
                            }
                        })
                        .collect();
                    tracing::debug!(
                        count = items.len(),
                        "completion: returning candidates (block anchors)"
                    );
                    return Some(CompletionResponse::Array(items));
                } else {
                    // Heading completion: `[[doc#...`
                    let range = range_at(heading_or_block_start, word_end);
                    // A link to a heading that appears twice reaches the first one, so the later
                    // copy is not a different target and is not offered.
                    let mut seen_slugs = std::collections::HashSet::new();
                    let mut items: Vec<CompletionItem> = target_doc
                        .headings
                        .iter()
                        .filter(|h| seen_slugs.insert(h.slug.as_str()))
                        .map(|h| {
                            let new_text = format!("{}{}", h.text.trim(), close_suffix);
                            CompletionItem {
                                label: h.text.trim().to_string(),
                                kind: Some(CompletionItemKind::FIELD),
                                detail: Some(format!("Level {} Heading", h.level)),
                                text_edit: Some(completion_text_edit(range, new_text)),
                                ..Default::default()
                            }
                        })
                        .collect();

                    // If query is empty or starts with '^', also suggest blocks
                    if heading_or_block.is_empty() {
                        for b in &target_doc.blocks {
                            let new_text = format!("^{}{}", b.id, close_suffix);
                            items.push(CompletionItem {
                                label: format!("^{}", b.id),
                                kind: Some(CompletionItemKind::VARIABLE),
                                detail: Some("Block Anchor".to_string()),
                                text_edit: Some(completion_text_edit(range, new_text)),
                                filter_text: Some(format!("^{}", b.id)),
                                ..Default::default()
                            });
                        }
                    }

                    tracing::debug!(
                        count = items.len(),
                        "completion: returning candidates (headings/blocks for doc)"
                    );
                    return Some(CompletionResponse::Array(items));
                }
            }
        } else {
            // Document / Note completion
            let range = range_at(inside_wikilink_start, word_end);

            let mut candidates: Vec<Candidate> = Vec::new();
            for d in state.index.documents() {
                candidates.push(Candidate {
                    doc: d,
                    hit: Hit::Title,
                });
                for alias in &d.frontmatter.aliases {
                    candidates.push(Candidate {
                        doc: d,
                        hit: Hit::Alias(alias),
                    });
                }
                for h in &d.headings {
                    let heading_text = h.text.trim();
                    if heading_text.is_empty() {
                        continue;
                    }
                    candidates.push(Candidate {
                        doc: d,
                        hit: Hit::Heading(heading_text),
                    });
                }
            }

            tracing::debug!(
                count = candidates.len(),
                "completion: returning candidates (documents/headings/aliases)"
            );
            let ranking = rank(candidates, limit, inside_wikilink);
            let items = ranking
                .items
                .iter()
                .map(|c| c.build(range, close_suffix))
                .collect();
            return Some(response_of(items, ranking.incomplete));
        }
    }

    // 2. Check for Footnote completion: `[^...`
    if let Some(open_fn_idx) = line_prefix.rfind("[^") {
        let inside_fn = &line_prefix[open_fn_idx + 2..];
        if !inside_fn.contains(']') {
            let range = range_at(line_start_offset + open_fn_idx + 2, byte_offset);
            let items: Vec<CompletionItem> = doc
                .footnotes
                .definitions
                .iter()
                .map(|f| CompletionItem {
                    label: f.label.clone(),
                    kind: Some(CompletionItemKind::REFERENCE),
                    detail: Some("Footnote Definition".to_string()),
                    text_edit: Some(completion_text_edit(range, f.label.clone())),
                    ..Default::default()
                })
                .collect();
            tracing::debug!(
                count = items.len(),
                "completion: returning candidates (footnotes)"
            );
            return Some(CompletionResponse::Array(items));
        }
    }

    // 3. Check for Tag completion: `#...`
    if let Some(hash_idx) = line_prefix.rfind('#') {
        // A tag starts at the line start, after whitespace, or after an opening bracket/quote (the
        // same set the parser accepts), and only tag characters have been typed since the `#`:
        // `# Heading text|` is a heading marker, not a tag being typed.
        let starts_a_tag = line_prefix[..hash_idx].chars().next_back().is_none_or(|c| {
            c.is_whitespace() || matches!(c, '(' | '[' | '{' | '"' | '\'' | '<' | '—' | '–')
        });
        let only_tag_characters = line_prefix[hash_idx + 1..]
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '/'));

        if starts_a_tag && only_tag_characters {
            let range = range_at(line_start_offset + hash_idx + 1, byte_offset);
            let spellings = tag_spellings(state);
            let items: Vec<CompletionItem> = state
                .index
                .all_tags()
                .into_iter()
                .map(|key| {
                    // The index keys tags folded; complete with the spelling the vault uses.
                    let tag_name = spellings.get(key).map_or(key, String::as_str);
                    CompletionItem {
                        label: format!("#{}", tag_name),
                        kind: Some(CompletionItemKind::KEYWORD),
                        detail: Some("Tag".to_string()),
                        filter_text: Some(tag_name.to_string()),
                        text_edit: Some(completion_text_edit(range, tag_name.to_string())),
                        ..Default::default()
                    }
                })
                .collect();
            tracing::debug!(
                count = items.len(),
                "completion: returning candidates (tags)"
            );
            return Some(respond(items, limit, &line_prefix[hash_idx + 1..]));
        }
    }

    None
}

/// For every folded tag key, the spelling used most often across the vault (the alphabetically
/// first one on a tie, so the result never depends on iteration order).
fn tag_spellings(state: &SatzState) -> std::collections::HashMap<String, String> {
    use std::collections::HashMap;
    let mut counts: HashMap<String, HashMap<String, usize>> = HashMap::new();
    for doc in state.index.documents() {
        for tag in &doc.tags {
            let name = tag.name.trim_start_matches('#');
            *counts
                .entry(satz_core::fold_key(name))
                .or_default()
                .entry(name.to_string())
                .or_default() += 1;
        }
    }
    counts
        .into_iter()
        .filter_map(|(key, spellings)| {
            spellings
                .into_iter()
                .max_by(|a, b| a.1.cmp(&b.1).then_with(|| b.0.cmp(&a.0)))
                .map(|(spelling, _)| (key, spelling))
        })
        .collect()
}

pub fn completion_resolve(mut item: CompletionItem, state: &SatzState) -> CompletionItem {
    if let Some(Value::Object(map)) = &item.data
        && let Some(Value::String(doc_id_str)) = map.get("doc_id")
    {
        let doc_id = satz_core::DocId::new(doc_id_str);
        if let Some(target_doc) = state.index.get_doc(&doc_id) {
            let mut value = format!("# {}\n\n", target_doc.title);

            if !target_doc.tags.is_empty() {
                let tags_str: Vec<String> =
                    target_doc.tags.iter().map(|t| t.name.clone()).collect();
                value.push_str(&format!("**Tags:** {}\n\n", tags_str.join(", ")));
            }

            // The note itself, not its frontmatter (which would fill the preview of most notes).
            let source = target_doc.line_index.source();
            let body = target_doc
                .frontmatter_range
                .map_or(source, |range| &source[range.end.min(source.len())..]);
            let mut lines = body.lines().filter(|l| !l.trim().is_empty());
            let preview_lines: Vec<&str> = lines.by_ref().take(5).collect();
            let more = lines.next().is_some();

            value.push_str("```markdown\n");
            value.push_str(&preview_lines.join("\n"));
            if more {
                value.push_str("\n...");
            }
            value.push_str("\n```");

            item.documentation = Some(Documentation::MarkupContent(MarkupContent {
                kind: MarkupKind::Markdown,
                value,
            }));
        }
    }

    item
}

#[cfg(test)]
// Test states are built field by field so each test shows exactly what it sets up.
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use super::*;
    use satz_core::{Index, parse_document};
    use std::path::Path;
    use tower_lsp_server::ls_types::{
        Position, TextDocumentIdentifier, TextDocumentPositionParams,
    };

    /// Test helper: pulls the replacement text out of a completion item's `text_edit`
    /// (completion no longer sets bare `insert_text` -- see `completion_text_edit`).
    fn item_new_text(item: &CompletionItem) -> Option<&str> {
        match &item.text_edit {
            Some(CompletionTextEdit::Edit(edit)) => Some(edit.new_text.as_str()),
            _ => None,
        }
    }

    #[test]
    fn test_wikilink_completion() {
        let rel_a = Path::new("doc-a.md");
        let rel_b = Path::new("doc-b.md");
        let doc_a = parse_document("# Doc A\n\n[[", rel_a);
        let doc_b = parse_document(
            "---\ntitle: Target Note\naliases: [TargetAlias]\n---\n# Note B",
            rel_b,
        );

        let mut state = SatzState::default();
        state.index = Index::build(vec![doc_a.clone(), doc_b]);
        state.set_vault_root(Some(Path::new("").to_path_buf()));

        let uri_str = "file:///doc-a.md";
        state.open_docs.insert(
            uri_str.to_string(),
            crate::state::OpenDocument::new(uri_str, rel_a.to_path_buf(), "# Doc A\n\n[[", 1),
        );

        let params = CompletionParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: uri_str.parse().unwrap(),
                },
                position: Position::new(2, 2), // right after `[[`
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: None,
        };

        let response = completion(params, &state).expect("Completion response expected");
        if let CompletionResponse::Array(items) = response {
            assert!(items.iter().any(|i| i.label == "Target Note"));
            assert!(items.iter().any(|i| i.label.contains("TargetAlias")));
            // Document completions insert the path, not the title, so the link survives a
            // future title edit; the title stays as the (searchable) label only.
            let target_note = items
                .iter()
                .find(|i| i.label == "Target Note")
                .expect("Target Note item");
            assert_eq!(item_new_text(target_note), Some("doc-b]]"));
        } else {
            panic!("Expected CompletionResponse::Array");
        }
    }

    #[test]
    fn test_completion_uses_live_rope_not_stale_index() {
        let rel_a = Path::new("doc-a.md");
        // The indexed snapshot is stale: reparse hasn't caught up to the "]]" the editor
        // already auto-paired in the live buffer, simulating typing faster than the
        // reparse debounce window (200-500ms).
        let stale_doc_a = parse_document("# Doc A\n\n[[Olgu", rel_a);
        let doc_b = parse_document("# Olgu\nContent", Path::new("doc-b.md"));

        let mut state = SatzState::default();
        state.index = Index::build(vec![stale_doc_a, doc_b]);
        state.set_vault_root(Some(Path::new("").to_path_buf()));

        let uri_str = "file:///doc-a.md";
        // Live buffer already has the closing brackets the editor auto-paired.
        let live_content = "# Doc A\n\n[[Olgu]]";
        state.open_docs.insert(
            uri_str.to_string(),
            crate::state::OpenDocument::new(uri_str, rel_a.to_path_buf(), live_content, 1),
        );

        let params = CompletionParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: uri_str.parse().unwrap(),
                },
                position: Position::new(2, 6), // right after "Olgu", before the live "]]"
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: None,
        };

        let response = completion(params, &state).expect("Completion response expected");
        let CompletionResponse::Array(items) = response else {
            panic!("Expected CompletionResponse::Array");
        };
        assert!(!items.is_empty());

        // The live buffer already has closing brackets right after the cursor; completion must
        // not append a second "]]" on top of them. Before the fix, this scanned the stale
        // indexed text (which had no trailing "]]" yet) instead of the live rope, and always
        // appended "]]" -- producing a duplicated "]]]]" once the editor's own auto-pair merged
        // in.
        for item in &items {
            if let Some(text) = item_new_text(item) {
                assert!(
                    !text.ends_with("]]"),
                    "new_text must not append ]] when the live buffer already has \
                     closing brackets after the cursor: {text:?}"
                );
            }
        }

        // The replace range must cover exactly the already-typed query ("Olgu", from right
        // after "[[" to the cursor) -- not the client's own guess. Accepting any item should
        // replace "Olgu" in place, not insert alongside it.
        let any_item = items.first().expect("at least one candidate");
        match &any_item.text_edit {
            Some(CompletionTextEdit::Edit(edit)) => {
                assert_eq!(edit.range.start, Position::new(2, 2));
                assert_eq!(edit.range.end, Position::new(2, 6));
            }
            _ => panic!("expected an explicit text_edit, not a bare insert_text"),
        }
    }

    #[test]
    fn test_flat_wikilink_completion_includes_headings() {
        let rel_a = Path::new("doc-a.md");
        let rel_b = Path::new("tlp/sozluk.md");
        let doc_a = parse_document("# Doc A\n\n[[", rel_a);
        let doc_b = parse_document(
            "---\ntitle: Tractatus Sözlüğü\n---\n# Tractatus Sözlüğü\n\n## Olgu\n\ntext",
            rel_b,
        );

        let mut state = SatzState::default();
        state.index = Index::build(vec![doc_a.clone(), doc_b]);
        state.set_vault_root(Some(Path::new("").to_path_buf()));

        let uri_str = "file:///doc-a.md";
        state.open_docs.insert(
            uri_str.to_string(),
            crate::state::OpenDocument::new(uri_str, rel_a.to_path_buf(), "# Doc A\n\n[[", 1),
        );

        let params = CompletionParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: uri_str.parse().unwrap(),
                },
                position: Position::new(2, 2),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: None,
        };

        let response = completion(params, &state).expect("Completion response expected");
        let CompletionResponse::Array(items) = response else {
            panic!("Expected CompletionResponse::Array");
        };

        // Typing "olgu" should be able to jump straight to the `## Olgu` heading inside
        // tlp/sozluk.md without first completing to the document and then to `#Olgu`.
        let olgu_item = items
            .iter()
            .find(|i| i.label == "Olgu")
            .expect("heading completion item for 'Olgu'");
        assert_eq!(item_new_text(olgu_item), Some("tlp/sozluk#Olgu]]"));

        // The document-level completion for the same file is still path-based.
        assert!(
            items
                .iter()
                .any(|i| i.label == "Tractatus Sözlüğü"
                    && item_new_text(i) == Some("tlp/sozluk]]"))
        );
    }

    #[test]
    fn test_heading_completion() {
        let rel_a = Path::new("doc-a.md");
        let rel_b = Path::new("doc-b.md");
        let doc_a = parse_document("# Doc A\n\n[[doc-b#", rel_a);
        let doc_b = parse_document("# Heading In B\n## Subheading", rel_b);

        let mut state = SatzState::default();
        state.index = Index::build(vec![doc_a.clone(), doc_b]);
        state.set_vault_root(Some(Path::new("").to_path_buf()));

        let uri_str = "file:///doc-a.md";
        state.open_docs.insert(
            uri_str.to_string(),
            crate::state::OpenDocument::new(uri_str, rel_a.to_path_buf(), "# Doc A\n\n[[doc-b#", 1),
        );

        let params = CompletionParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: uri_str.parse().unwrap(),
                },
                position: Position::new(2, 8), // right after `[[doc-b#`
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: None,
        };

        let response = completion(params, &state).expect("Completion response expected");
        if let CompletionResponse::Array(items) = response {
            assert!(items.iter().any(|i| i.label == "Heading In B"));
            assert!(items.iter().any(|i| i.label == "Subheading"));
        } else {
            panic!("Expected CompletionResponse::Array");
        }
    }

    #[test]
    fn test_completion_resolve() {
        let rel_a = Path::new("doc-a.md");
        let doc_a = parse_document(
            "---\ntags: [rust]\n---\n# Doc Title\nLine 1 of content\nLine 2 of content",
            rel_a,
        );

        let mut state = SatzState::default();
        state.index = Index::build(vec![doc_a]);

        let item = CompletionItem {
            label: "Doc Title".to_string(),
            data: Some(serde_json::json!({ "doc_id": "doc-a.md" })),
            ..Default::default()
        };

        let resolved = completion_resolve(item, &state);
        assert!(resolved.documentation.is_some());
        if let Some(Documentation::MarkupContent(m)) = resolved.documentation {
            assert!(m.value.contains("Doc Title"));
            assert!(m.value.contains("Line 1 of content"));
            assert!(m.value.contains("rust"));
        } else {
            panic!("Expected MarkupContent in documentation");
        }
    }

    #[test]
    fn test_block_anchor_completion() {
        let rel_a = Path::new("doc-a.md");
        let rel_b = Path::new("doc-b.md");
        let doc_a = parse_document("# Doc A\n\n[[doc-b#^", rel_a);
        let doc_b = parse_document(
            "Some block text ^my-block-id\nOther text ^other-block",
            rel_b,
        );

        let mut state = SatzState::default();
        state.index = Index::build(vec![doc_a.clone(), doc_b]);
        state.set_vault_root(Some(Path::new("").to_path_buf()));

        let uri_str = "file:///doc-a.md";
        state.open_docs.insert(
            uri_str.to_string(),
            crate::state::OpenDocument::new(
                uri_str,
                rel_a.to_path_buf(),
                "# Doc A\n\n[[doc-b#^",
                1,
            ),
        );

        let params = CompletionParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: uri_str.parse().unwrap(),
                },
                position: Position::new(2, 9), // right after `[[doc-b#^`
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: None,
        };

        let response = completion(params, &state).expect("Completion response expected");
        if let CompletionResponse::Array(items) = response {
            assert!(items.iter().any(|i| i.label == "^my-block-id"));
            assert!(items.iter().any(|i| i.label == "^other-block"));
        } else {
            panic!("Expected CompletionResponse::Array");
        }
    }

    // ---- applied-result harness: complete at `§`, apply the chosen edit, compare the text ----

    const CURSOR: char = '§';

    fn vault_root() -> std::path::PathBuf {
        if cfg!(windows) {
            Path::new("C:\\vault").to_path_buf()
        } else {
            Path::new("/vault").to_path_buf()
        }
    }

    /// Runs completion in `a.md` whose text is `marked` with the cursor at `§`. `b.md` has the
    /// headings Intro/Details, the block `^my-block` and the tags alpha/beta.
    /// Returns the response and the text without the marker.
    fn complete(marked: &str) -> (Option<CompletionResponse>, String) {
        complete_in(
            marked,
            "# Intro\n\nSome text ^my-block\n\n## Details\n\n#alpha #beta\n",
        )
    }

    /// Like `complete`, with the text of the peer note `b.md` chosen by the test.
    fn complete_in(marked: &str, b_text: &str) -> (Option<CompletionResponse>, String) {
        let idx = marked.find(CURSOR).expect("cursor marker");
        let text = marked.replacen(CURSOR, "", 1);
        let before = &marked[..idx];
        let line = before.matches('\n').count() as u32;
        let line_start = before.rfind('\n').map_or(0, |i| i + 1);
        let character = before[line_start..].encode_utf16().count() as u32;

        let mut state = SatzState::default();
        state.index = Index::build(vec![
            parse_document(&text, Path::new("a.md")),
            parse_document(b_text, Path::new("b.md")),
        ]);
        state.set_vault_root(Some(vault_root()));
        let uri = crate::convert::path_to_uri(&vault_root().join("a.md"))
            .unwrap()
            .as_str()
            .to_string();
        state.open_docs.insert(
            uri.clone(),
            crate::state::OpenDocument::new(&uri, vault_root().join("a.md"), &text, 1),
        );
        let params = CompletionParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: uri.parse().unwrap(),
                },
                position: Position::new(line, character),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: None,
        };
        (completion(params, &state), text)
    }

    fn items(marked: &str) -> Vec<CompletionItem> {
        match complete(marked).0 {
            Some(CompletionResponse::Array(items)) => items,
            other => panic!("expected an item array for {marked:?}, got {other:?}"),
        }
    }

    /// The document text after accepting the item labelled `label`.
    fn accept(marked: &str, label: &str) -> String {
        accept_from(complete(marked), marked, label)
    }

    fn accept_from(
        completed: (Option<CompletionResponse>, String),
        marked: &str,
        label: &str,
    ) -> String {
        let (response, text) = completed;
        let Some(CompletionResponse::Array(items)) = response else {
            panic!("no completion items for {marked:?}");
        };
        let item = items.iter().find(|i| i.label == label).unwrap_or_else(|| {
            panic!(
                "no item {label:?} for {marked:?}; have {:?}",
                items.iter().map(|i| &i.label).collect::<Vec<_>>()
            )
        });
        let Some(CompletionTextEdit::Edit(edit)) = &item.text_edit else {
            panic!("item without text edit");
        };
        crate::convert::apply_text_edits(&text, std::slice::from_ref(edit))
    }

    #[test]
    fn block_anchor_completion_yields_a_single_caret() {
        assert_eq!(accept("[[b#^§", "^my-block"), "[[b#^my-block]]");
        assert_eq!(accept("[[b#^my§", "^my-block"), "[[b#^my-block]]");
        assert_eq!(accept("![[b#^§", "^my-block"), "![[b#^my-block]]");
        // Closing brackets that are already there are not duplicated.
        assert_eq!(accept("[[b#^§]]", "^my-block"), "[[b#^my-block]]");
        assert_eq!(accept("x [[b#^§]] y", "^my-block"), "x [[b#^my-block]] y");
    }

    #[test]
    fn heading_completion_replaces_the_typed_query() {
        assert_eq!(accept("[[b#§", "Intro"), "[[b#Intro]]");
        assert_eq!(accept("[[b#In§", "Intro"), "[[b#Intro]]");
        assert_eq!(accept("[[b#§]]", "Details"), "[[b#Details]]");
        assert_eq!(accept("[[b#§", "^my-block"), "[[b#^my-block]]");
    }

    #[test]
    fn a_closed_wikilink_earlier_on_the_line_is_not_the_context() {
        // Tags still work after a finished link.
        assert_eq!(
            accept("see [[b]] and #al§", "#alpha"),
            "see [[b]] and #alpha"
        );
        assert_eq!(accept("[[b#Intro]] #§", "#beta"), "[[b#Intro]] #beta");
        // Nothing to complete in plain text after a finished link.
        assert!(complete("[[b]] bar§").0.is_none());
        assert!(complete("![[b]] and [[b#Intro]] text§").0.is_none());
        // Footnotes still work after a finished link.
        assert_eq!(
            accept("[[b]] text[^§\n\n[^1]: note\n", "1"),
            "[[b]] text[^1\n\n[^1]: note\n"
        );
    }

    #[test]
    fn a_second_and_third_link_on_the_same_line_complete_fully() {
        assert_eq!(accept("[[b]] [[§", "Details"), "[[b]] [[b#Details]]");
        assert_eq!(
            accept("[[b]] [[b]] [[b#In§", "Intro"),
            "[[b]] [[b]] [[b#Intro]]"
        );
        assert_eq!(
            accept("[[b]] and [[b#^§", "^my-block"),
            "[[b]] and [[b#^my-block]]"
        );
    }

    #[test]
    fn nothing_is_offered_inside_the_display_text_part() {
        for marked in ["[[b|My al§", "[[b#Intro|al§", "[[b#^my-block|x§", "![[b|§"] {
            assert!(items(marked).is_empty(), "{marked:?}");
        }
    }

    #[test]
    fn unicode_prefixes_and_crlf_lines_are_positioned_correctly() {
        assert_eq!(accept("Türkçe 🎉 [[b#§", "Intro"), "Türkçe 🎉 [[b#Intro]]");
        assert_eq!(
            accept("Türkçe 🎉 [[b#^§", "^my-block"),
            "Türkçe 🎉 [[b#^my-block]]"
        );
        assert_eq!(
            accept("first line\r\n[[b#^§", "^my-block"),
            "first line\r\n[[b#^my-block]]"
        );
        assert_eq!(
            accept("first\r\n[[b]] #al§\r\nlast", "#alpha"),
            "first\r\n[[b]] #alpha\r\nlast"
        );
    }

    #[test]
    fn unresolved_target_gives_no_items_and_open_brackets_at_line_edges_work() {
        assert!(items("[[missing#§").is_empty());
        assert!(items("[[missing#^§").is_empty());
        assert_eq!(accept("[[§", "Details"), "[[b#Details]]");
        assert_eq!(
            accept("text\n[[§\nnext", "Details"),
            "text\n[[b#Details]]\nnext"
        );
    }

    // ---- tags: only where a tag can be typed, and spelled as the vault spells it ----

    #[test]
    fn a_heading_marker_is_not_a_tag_being_typed() {
        for marked in [
            "# Başlık§",
            "## Başlık§",
            "###### x§",
            "text # not a tag§",
            "# §",
            "#\tword§",
        ] {
            assert!(
                complete(marked).0.is_none(),
                "{marked:?} -> {:?}",
                complete(marked).0
            );
        }
    }

    #[test]
    fn tags_are_still_completed_where_they_can_be_typed() {
        assert_eq!(accept("#§", "#alpha"), "#alpha");
        assert_eq!(accept("#al§", "#alpha"), "#alpha");
        assert_eq!(accept("text #al§", "#alpha"), "text #alpha");
        assert_eq!(accept("## Head #be§", "#beta"), "## Head #beta");
        assert_eq!(accept("- #be§", "#beta"), "- #beta");
        assert_eq!(accept("(#al§", "#alpha"), "(#alpha");
        assert!(items("#§").len() >= 2);
        // A `#` glued to a word is a fragment, not a tag.
        assert!(complete("https://a.b/c#fr§").0.is_none());
    }

    #[test]
    fn a_tag_is_completed_with_the_spelling_the_vault_uses_most() {
        let b = "#Proje #Proje\n\n#proje\n\n#Zed/Alt #zed/alt #zed/alt\n\n#İş\n";
        let done = |marked: &str, label: &str| accept_from(complete_in(marked, b), marked, label);
        assert_eq!(done("#§", "#Proje"), "#Proje");
        assert_eq!(done("#pr§", "#Proje"), "#Proje");
        // The commonest spelling wins, hierarchy included.
        assert_eq!(done("#§", "#zed/alt"), "#zed/alt");
        assert_eq!(done("#§", "#İş"), "#İş");
    }

    #[test]
    fn equally_common_spellings_resolve_deterministically() {
        let b = "#Foo #foo\n";
        let first = accept_from(complete_in("#§", b), "#§", "#Foo");
        assert_eq!(first, "#Foo");
        for _ in 0..5 {
            let labels: Vec<String> = match complete_in("#§", b).0 {
                Some(CompletionResponse::Array(items)) => {
                    items.into_iter().map(|i| i.label).collect()
                }
                other => panic!("{other:?}"),
            };
            assert_eq!(labels, vec!["#Foo".to_string()]);
        }
    }

    // ---- what a completion replaces, in what order, and how many ----

    const MARK: char = '‸';

    /// Completes at the `‸` in `open_text` (a note `open.md`) among `others`, and returns for every
    /// item its label and the text of the note AFTER that item's edit is applied.
    fn complete_marked(
        others: &[(&str, &str)],
        open_text: &str,
        configure: impl FnOnce(&mut SatzState),
    ) -> (Vec<(String, String)>, bool) {
        let marked = open_text;
        let (before, after) = marked.split_once(MARK).expect("a cursor marker");
        let text = format!("{before}{after}");
        let line = before.matches('\n').count() as u32;
        let last_line = before.rsplit('\n').next().unwrap();
        let character = last_line.encode_utf16().count() as u32;

        let mut state = SatzState::default();
        let mut docs: Vec<satz_core::Document> = others
            .iter()
            .map(|(p, t)| parse_document(t, Path::new(p)))
            .collect();
        docs.push(parse_document(&text, Path::new("open.md")));
        state.index = Index::build(docs);
        state.set_vault_root(Some(Path::new("").to_path_buf()));
        state.open_docs.insert(
            "file:///open.md".to_string(),
            crate::state::OpenDocument::new(
                "file:///open.md",
                Path::new("open.md").to_path_buf(),
                text.clone(),
                1,
            ),
        );
        configure(&mut state);

        let params = CompletionParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: "file:///open.md".parse().unwrap(),
                },
                position: Position::new(line, character),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: None,
        };
        let (items, incomplete) = match completion(params, &state) {
            Some(CompletionResponse::Array(items)) => (items, false),
            Some(CompletionResponse::List(list)) => (list.items, list.is_incomplete),
            None => return (Vec::new(), false),
        };
        let applied = items
            .iter()
            .map(|item| {
                let Some(CompletionTextEdit::Edit(edit)) = &item.text_edit else {
                    panic!("every item carries a text edit: {item:?}");
                };
                (
                    item.label.clone(),
                    crate::convert::apply_text_edits(&text, std::slice::from_ref(edit)),
                )
            })
            .collect();
        (applied, incomplete)
    }

    fn edited<'a>(items: &'a [(String, String)], label: &str) -> &'a str {
        &items
            .iter()
            .find(|(l, _)| l == label)
            .unwrap_or_else(|| {
                panic!(
                    "no item {label:?} in {:?}",
                    items.iter().map(|i| &i.0).collect::<Vec<_>>()
                )
            })
            .1
    }

    const TARGET: (&str, &str) = (
        "doc-b.md",
        "---\ntitle: Target\n---\n# Top\n\n## Head\n\ntext ^blk\n",
    );

    #[test]
    fn the_rest_of_the_word_after_the_cursor_is_replaced_not_kept() {
        let (items, _) = complete_marked(&[TARGET], "[[Ta‸rget]] tail", |_| {});
        assert_eq!(edited(&items, "Target"), "[[doc-b]] tail");
        // Without closing brackets: they are added.
        let (items, _) = complete_marked(&[TARGET], "[[Ta‸rget tail", |_| {});
        assert_eq!(edited(&items, "Target"), "[[doc-b]] tail");
        // Cursor at the very end of the word, brackets already there: nothing doubled.
        let (items, _) = complete_marked(&[TARGET], "[[Target‸]]", |_| {});
        assert_eq!(edited(&items, "Target"), "[[doc-b]]");
    }

    #[test]
    fn a_single_closing_bracket_is_completed_to_two() {
        let (items, _) = complete_marked(&[TARGET], "[[Tar‸]", |_| {});
        assert_eq!(edited(&items, "Target"), "[[doc-b]]");
        let (items, _) = complete_marked(&[TARGET], "[[Tar‸]] and [x]", |_| {});
        assert_eq!(edited(&items, "Target"), "[[doc-b]] and [x]");
    }

    #[test]
    fn an_alias_display_text_or_anchor_after_the_word_is_kept() {
        let (items, _) = complete_marked(&[TARGET], "[[Ta‸rget|shown]]", |_| {});
        assert_eq!(edited(&items, "Target"), "[[doc-b|shown]]");
        let (items, _) = complete_marked(&[TARGET], "[[Ta‸rget#Head]]", |_| {});
        assert_eq!(edited(&items, "Target"), "[[doc-b#Head]]");
    }

    #[test]
    fn headings_and_blocks_replace_the_rest_of_their_word_too() {
        let (items, _) = complete_marked(&[TARGET], "[[doc-b#He‸ad]] tail", |_| {});
        assert_eq!(edited(&items, "Head"), "[[doc-b#Head]] tail");
        let (items, _) = complete_marked(&[TARGET], "[[doc-b#^bl‸k]] tail", |_| {});
        assert_eq!(edited(&items, "^blk"), "[[doc-b#^blk]] tail");
        let (items, _) = complete_marked(&[TARGET], "[[doc-b#He‸ad|shown]]", |_| {});
        assert_eq!(edited(&items, "Head"), "[[doc-b#Head|shown]]");
    }

    #[test]
    fn positions_count_utf16_units_and_crlf_lines() {
        let others = [("iş.md", "---\ntitle: İş\n---\n# Başlık\n")];
        let (items, _) = complete_marked(&others, "🦀 çok [[İş‸x]] sonra", |_| {});
        assert_eq!(edited(&items, "İş"), "🦀 çok [[iş]] sonra");
        let (items, _) = complete_marked(&[TARGET], "first\r\nsecond [[Ta‸rget]]\r\nlast", |_| {});
        assert_eq!(
            edited(&items, "Target"),
            "first\r\nsecond [[doc-b]]\r\nlast"
        );
        // A cursor past the end of a line means the end of that line.
        let (items, _) = complete_marked(&[TARGET], "[[Tar‸", |_| {});
        assert_eq!(edited(&items, "Target"), "[[doc-b]]");
    }

    #[test]
    fn nothing_is_offered_outside_a_link_tag_or_footnote() {
        let (items, incomplete) = complete_marked(&[TARGET], "plain words ‸ here", |_| {});
        assert!(items.is_empty() && !incomplete);
        let (items, _) = complete_marked(&[TARGET], "[[done]] and then ‸", |_| {});
        assert!(items.is_empty());
        let (items, _) = complete_marked(&[TARGET], "# Heading ‸", |_| {});
        assert!(items.is_empty());
    }

    fn many_notes(count: usize) -> Vec<(String, String)> {
        (0..count)
            .map(|i| (format!("note-{i:04}.md"), format!("# Note {i:04}\n")))
            .collect()
    }

    #[test]
    fn candidates_come_in_a_fixed_order_whatever_the_index_iteration_order() {
        let notes = many_notes(40);
        let refs: Vec<(&str, &str)> = notes
            .iter()
            .map(|(p, t)| (p.as_str(), t.as_str()))
            .collect();
        let (first, _) = complete_marked(&refs, "[[‸", |_| {});
        let labels: Vec<&str> = first.iter().map(|(l, _)| l.as_str()).collect();
        let mut sorted = labels.clone();
        sorted.sort_by_key(|l| satz_core::fold_key(l));
        assert_eq!(labels, sorted, "sorted by (folded) label");
        for _ in 0..20 {
            let (again, _) = complete_marked(&refs, "[[‸", |_| {});
            assert_eq!(again, first);
        }
    }

    #[test]
    fn a_long_answer_is_cut_at_the_limit_and_marked_incomplete() {
        let notes = many_notes(500);
        let refs: Vec<(&str, &str)> = notes
            .iter()
            .map(|(p, t)| (p.as_str(), t.as_str()))
            .collect();

        let (items, incomplete) = complete_marked(&refs, "[[‸", |_| {});
        assert_eq!(items.len(), 200, "the default limit");
        assert!(incomplete);
        assert_eq!(
            items[0].0, "Note 0000",
            "the cut keeps the first ones in order"
        );

        let (items, incomplete) =
            complete_marked(&refs, "[[‸", |s| s.config.lsp.completion_limit = 30);
        assert_eq!((items.len(), incomplete), (30, true));

        let (items, incomplete) =
            complete_marked(&refs, "[[‸", |s| s.config.lsp.completion_limit = 0);
        assert!(
            items.len() >= 501,
            "no limit: every note and heading, {}",
            items.len()
        );
        assert!(!incomplete);

        let (items, incomplete) =
            complete_marked(&refs, "[[‸", |s| s.config.lsp.completion_limit = 10_000);
        assert!(items.len() > 200 && !incomplete);
    }

    #[test]
    fn a_short_answer_is_a_plain_array_as_before() {
        let (items, incomplete) = complete_marked(&[TARGET], "[[‸", |_| {});
        assert!(!items.is_empty() && !incomplete);
    }

    #[test]
    fn tags_are_sorted_and_limited_like_notes() {
        let tagged: Vec<(String, String)> = (0..300)
            .map(|i| (format!("t{i}.md"), format!("# T\n\n#tag{i:03}\n")))
            .collect();
        let refs: Vec<(&str, &str)> = tagged
            .iter()
            .map(|(p, t)| (p.as_str(), t.as_str()))
            .collect();
        let (items, incomplete) = complete_marked(&refs, "#‸", |_| {});
        assert_eq!(items.len(), 200);
        assert!(incomplete);
        assert_eq!(items[0].0, "#tag000");
    }

    // ---- the preview shows the note, not its frontmatter ----

    /// The text between the code fences of the resolved item's preview.
    fn preview_of(text: &str) -> String {
        let mut state = SatzState::default();
        state.index = Index::build(vec![parse_document(text, Path::new("n.md"))]);
        let item = CompletionItem {
            label: "n".to_string(),
            data: Some(serde_json::json!({ "doc_id": "n.md" })),
            ..Default::default()
        };
        let Some(Documentation::MarkupContent(m)) = completion_resolve(item, &state).documentation
        else {
            panic!("a preview expected");
        };
        let start = m
            .value
            .find(
                "```markdown
",
            )
            .expect("fence")
            + "```markdown
"
            .len();
        let end = m
            .value
            .rfind(
                "
```",
            )
            .expect("closing fence");
        m.value[start..end.max(start)].to_string()
    }

    #[test]
    fn the_preview_skips_the_frontmatter() {
        let text = "---
title: T
date: 2026-01-01
aliases: []
tags: []
author: me
---

# Body
line
";
        assert_eq!(
            preview_of(text),
            "# Body
line"
        );
    }

    #[test]
    fn a_long_body_is_cut_after_five_lines_with_an_ellipsis() {
        let body: String = (1..=9)
            .map(|i| {
                format!(
                    "l{i}
"
                )
            })
            .collect();
        let text = format!(
            "---
title: T
---
{body}"
        );
        assert_eq!(
            preview_of(&text),
            "l1
l2
l3
l4
l5
..."
        );
    }

    #[test]
    fn exactly_five_body_lines_get_no_ellipsis() {
        let text = "---
title: T
---
a
b
c
d
e
";
        assert_eq!(
            preview_of(text),
            "a
b
c
d
e"
        );
    }

    #[test]
    fn a_note_that_is_only_frontmatter_has_an_empty_preview() {
        assert_eq!(
            preview_of(
                "---
title: T
---
"
            ),
            ""
        );
    }

    #[test]
    fn the_preview_skips_crlf_frontmatter_too() {
        assert_eq!(
            preview_of(
                "---
title: T
---

body
"
            ),
            "body"
        );
    }

    #[test]
    fn an_unclosed_frontmatter_fence_is_shown_as_the_text_it_is() {
        assert_eq!(
            preview_of(
                "---
title: T
body
"
            ),
            "---
title: T
body"
        );
    }

    #[test]
    fn equal_headings_of_the_target_are_offered_once_in_document_order() {
        let mut state = SatzState::default();
        state.index = Index::build(vec![
            parse_document("# A\n\n[[b#", Path::new("a.md")),
            parse_document("# B\n\n## Same\n\n## Other\n\n## Same\n", Path::new("b.md")),
        ]);
        state.set_vault_root(Some(Path::new("").to_path_buf()));
        state.open_docs.insert(
            "file:///a.md".to_string(),
            crate::state::OpenDocument::new(
                "file:///a.md",
                Path::new("a.md").to_path_buf(),
                "# A\n\n[[b#",
                1,
            ),
        );
        let params = CompletionParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: "file:///a.md".parse().unwrap(),
                },
                position: Position::new(2, 4),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: None,
        };
        let Some(CompletionResponse::Array(items)) = completion(params, &state) else {
            panic!("an answer expected");
        };
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert_eq!(labels, vec!["B", "Same", "Other"]);
    }

    // ---- a long list is narrowed by what was typed before it is cut (R-01, R-03) ----

    fn alfa_notes(count: usize) -> Vec<(String, String)> {
        (0..count)
            .map(|i| (format!("alfa-{i:04}.md"), format!("# Alfa {i:04}\n")))
            .collect()
    }

    /// 600 notes that sort before everything below, plus a glossary with the headings that are
    /// looked for.
    fn big_vault_with_glossary(glossary: &str) -> Vec<(String, String)> {
        let mut files = alfa_notes(600);
        files.push(("tlp/sozluk.md".to_string(), glossary.to_string()));
        files
    }

    fn as_refs(files: &[(String, String)]) -> Vec<(&str, &str)> {
        files
            .iter()
            .map(|(p, t)| (p.as_str(), t.as_str()))
            .collect()
    }

    const GLOSSARY: &str = "# Sözlük\n\n## Nesne\n\n## Olgu\n\n## Zeytin\n";

    #[test]
    fn a_heading_far_down_the_alphabet_is_found_first_by_what_was_typed() {
        let files = big_vault_with_glossary(GLOSSARY);
        for typed in ["Nesne", "Nes", "nesne", "NESNE", "N"] {
            let (items, incomplete) =
                complete_marked(&as_refs(&files), &format!("[[{typed}‸"), |_| {});
            assert!(
                !items.is_empty(),
                "typed {typed:?}: nothing offered although the heading exists"
            );
            assert_eq!(
                items[0].0,
                "Nesne",
                "typed {typed:?}: {:?}",
                &items[..3.min(items.len())]
            );
            assert_eq!(items[0].1, "[[tlp/sozluk#Nesne]]", "typed {typed:?}");
            assert!(items.len() <= 200 && (incomplete || items.len() < 200));
        }
    }

    #[test]
    fn turkish_capital_and_dotless_letters_find_their_ascii_spellings() {
        let files = big_vault_with_glossary("# Sözlük\n\n## İşlem\n\n## Işık\n");
        for typed in ["işlem", "islem", "İŞLEM", "isik", "ışık"] {
            let (items, _) = complete_marked(&as_refs(&files), &format!("[[{typed}‸"), |_| {});
            let labels: Vec<&str> = items.iter().map(|(l, _)| l.as_str()).collect();
            let wanted = if typed.contains('k') {
                "Işık"
            } else {
                "İşlem"
            };
            assert_eq!(
                labels.first().copied(),
                Some(wanted),
                "typed {typed:?}: {labels:?}"
            );
        }
    }

    #[test]
    fn an_exact_match_comes_before_a_prefix_match_before_a_fuzzy_one() {
        let files = big_vault_with_glossary(
            "# Sözlük\n\n## Bir nesne daha\n\n## Nesneler\n\n## Nesne\n\n## Sadece not\n",
        );
        let (items, _) = complete_marked(&as_refs(&files), "[[nesne‸", |_| {});
        let labels: Vec<&str> = items.iter().map(|(l, _)| l.as_str()).collect();
        assert_eq!(
            &labels[..3],
            ["Nesne", "Nesneler", "Bir nesne daha"],
            "{labels:?}"
        );
        assert!(
            !labels.contains(&"Sadece not"),
            "a heading that does not match is left out"
        );
    }

    #[test]
    fn a_small_list_is_returned_whole_whatever_was_typed() {
        // Nothing has to be cut, so nothing is filtered: the client narrows it as it always did.
        let (items, incomplete) = complete_marked(&[TARGET], "[[zzzz‸", |_| {});
        assert!(!items.is_empty() && !incomplete);
        let files = big_vault_with_glossary(GLOSSARY);
        let (items, incomplete) = complete_marked(&as_refs(&files), "[[Nesne‸", |s| {
            s.config.lsp.completion_limit = 0
        });
        assert!(
            items.len() > 600 && !incomplete,
            "no limit: everything, {}",
            items.len()
        );
    }

    #[test]
    fn every_extra_letter_narrows_the_answer_and_keeps_the_target() {
        let files = big_vault_with_glossary(GLOSSARY);
        let mut last = usize::MAX;
        for typed in ["N", "Ne", "Nes", "Nesn", "Nesne"] {
            let (items, _) = complete_marked(&as_refs(&files), &format!("[[{typed}‸"), |_| {});
            assert!(items.iter().any(|(l, _)| l == "Nesne"), "typed {typed:?}");
            assert!(
                items.len() <= last,
                "typed {typed:?}: {} > {last}",
                items.len()
            );
            last = items.len();
        }
    }

    #[test]
    fn the_answer_for_one_request_is_always_the_same() {
        let files = big_vault_with_glossary(GLOSSARY);
        let first = complete_marked(&as_refs(&files), "[[Ne‸", |_| {});
        for _ in 0..20 {
            assert_eq!(complete_marked(&as_refs(&files), "[[Ne‸", |_| {}), first);
        }
    }

    #[test]
    fn a_tag_far_down_the_alphabet_is_found_by_what_was_typed() {
        let mut files: Vec<(String, String)> = (0..300)
            .map(|i| {
                (
                    format!("t-{i:03}.md"),
                    format!("---\ntags: [tag{i:03}]\n---\n# T\n"),
                )
            })
            .collect();
        files.push((
            "z.md".to_string(),
            "---\ntags: [zulu]\n---\n# Z\n".to_string(),
        ));
        for typed in ["zul", "zulu", "ZUL"] {
            let (items, _) = complete_marked(&as_refs(&files), &format!("#{typed}‸"), |_| {});
            // The tag being typed is in the open note itself and is offered too; the vault's own
            // `zulu` must be among the first few, not lost behind 300 alphabetically earlier tags.
            let position = items.iter().position(|(l, _)| l == "#zulu");
            assert!(
                position.is_some_and(|p| p <= 1),
                "typed {typed:?}: {:?}",
                items.iter().take(3).collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn strange_queries_do_not_panic_and_give_a_sensible_answer() {
        let files = big_vault_with_glossary(GLOSSARY);
        let long = "n".repeat(10_000);
        for typed in [
            "]",
            "|x",
            "a#b",
            "^^^",
            "🦀",
            "ne\u{301}sne",
            long.as_str(),
            "  ",
            "İİİ",
            "$",
            "!x",
            "'q",
        ] {
            let (items, _) = complete_marked(&as_refs(&files), &format!("[[{typed}‸"), |_| {});
            assert!(items.len() <= 601 + 300, "typed {typed:?}: {}", items.len());
        }
    }

    // ---- what stands after the cursor is left alone (R-02) ----

    #[test]
    fn text_after_an_unclosed_link_is_not_swallowed() {
        for (typed, expected) in [
            (
                "Bu bir [[Ta‸ örnek #etiket",
                "Bu bir [[doc-b]] örnek #etiket",
            ),
            ("x^2 [[Ta‸ y", "x^2 [[doc-b]] y"),
            ("| a | [[Ta‸ | b |", "| a | [[doc-b]] | b |"),
            ("[[Ta‸ [md](u) and ]x", "[[doc-b]] [md](u) and ]x"),
            ("[[Ta‸ ^blockid", "[[doc-b]] ^blockid"),
        ] {
            let (items, _) = complete_marked(&[TARGET], typed, |_| {});
            assert_eq!(edited(&items, "Target"), expected, "{typed:?}");
        }
    }

    #[test]
    fn text_after_an_unclosed_heading_or_block_reference_is_not_swallowed() {
        let (items, _) = complete_marked(&[TARGET], "[[doc-b#He‸ text #tag", |_| {});
        assert_eq!(edited(&items, "Head"), "[[doc-b#Head]] text #tag");
        let (items, _) = complete_marked(&[TARGET], "[[doc-b#^bl‸ and ^2", |_| {});
        assert_eq!(edited(&items, "^blk"), "[[doc-b#^blk]] and ^2");
    }

    #[test]
    fn a_word_inside_a_real_link_is_still_replaced_whole() {
        // Unchanged from before: what is left of a word that ends at a bracket, `|` or `#` goes too.
        let (items, _) = complete_marked(&[TARGET], "[[Ta‸rget]] tail", |_| {});
        assert_eq!(edited(&items, "Target"), "[[doc-b]] tail");
        let (items, _) = complete_marked(&[TARGET], "[[Ta‸rget|shown]] x^2", |_| {});
        assert_eq!(edited(&items, "Target"), "[[doc-b|shown]] x^2");
    }

    // ---- the answer must stay what it was: the straightforward versions, kept as the reference ----

    /// `respond` as it was before candidates were ranked first: every item built, sorted with a
    /// comparator that folds both labels on every comparison, then cut.
    fn reference_respond(
        mut items: Vec<CompletionItem>,
        limit: usize,
        query: &str,
    ) -> CompletionResponse {
        let by_label = |a: &CompletionItem, b: &CompletionItem| {
            satz_core::fold_key(&a.label)
                .cmp(&satz_core::fold_key(&b.label))
                .then_with(|| a.label.cmp(&b.label))
                .then_with(|| a.detail.cmp(&b.detail))
        };
        if limit == 0 || items.len() <= limit {
            items.sort_by(by_label);
            return CompletionResponse::Array(items);
        }
        let query = satz_core::fold_key(query);
        if !query.is_empty() {
            let mut ranker = crate::rank::Ranker::new(&query);
            let mut scored: Vec<(u8, u32, CompletionItem)> = items
                .into_iter()
                .filter_map(|item| {
                    let text =
                        satz_core::fold_key(item.filter_text.as_deref().unwrap_or(&item.label));
                    let score = ranker.score(&text)?;
                    let tier = if text == query {
                        0
                    } else if text.starts_with(&query) {
                        1
                    } else {
                        2
                    };
                    Some((tier, score, item))
                })
                .collect();
            scored.sort_by(|a, b| {
                a.0.cmp(&b.0)
                    .then_with(|| b.1.cmp(&a.1))
                    .then_with(|| by_label(&a.2, &b.2))
            });
            items = scored.into_iter().map(|(_, _, item)| item).collect();
        } else {
            items.sort_by(by_label);
        }
        let cut = items.len() > limit;
        items.truncate(limit);
        if cut {
            return CompletionResponse::List(CompletionList {
                is_incomplete: true,
                items,
            });
        }
        CompletionResponse::Array(items)
    }

    /// The items for `[[` + a typed query, built for every note the way they were before.
    fn reference_note_items(state: &SatzState, range: Range) -> Vec<CompletionItem> {
        let close_suffix = "]]";
        let mut items = Vec::new();
        for d in state.index.documents() {
            let title_label = if d.title != "Untitled" && !d.title.is_empty() {
                d.title.clone()
            } else {
                d.id.as_str().to_string()
            };
            let path_str = d.path.to_string_lossy().replace('\\', "/");
            let insert_base = path_str
                .strip_suffix(".md")
                .unwrap_or(&path_str)
                .to_string();
            items.push(CompletionItem {
                label: title_label.clone(),
                kind: Some(CompletionItemKind::FILE),
                detail: Some(d.id.as_str().to_string()),
                text_edit: Some(completion_text_edit(
                    range,
                    format!("{}{}", insert_base, close_suffix),
                )),
                filter_text: Some(title_label.clone()),
                data: Some(serde_json::json!({ "doc_id": d.id.as_str() })),
                ..Default::default()
            });
            for alias in &d.frontmatter.aliases {
                items.push(CompletionItem {
                    label: format!("{} (alias)", alias),
                    kind: Some(CompletionItemKind::REFERENCE),
                    detail: Some(format!("Alias for: {}", d.title)),
                    text_edit: Some(completion_text_edit(
                        range,
                        format!("{}{}", alias, close_suffix),
                    )),
                    filter_text: Some(alias.clone()),
                    data: Some(serde_json::json!({ "doc_id": d.id.as_str() })),
                    ..Default::default()
                });
            }
            for h in &d.headings {
                let heading_text = h.text.trim();
                if heading_text.is_empty() {
                    continue;
                }
                items.push(CompletionItem {
                    label: heading_text.to_string(),
                    kind: Some(CompletionItemKind::FIELD),
                    detail: Some(format!("Heading in {}", title_label)),
                    text_edit: Some(completion_text_edit(
                        range,
                        format!("{}#{}{}", insert_base, heading_text, close_suffix),
                    )),
                    filter_text: Some(heading_text.to_string()),
                    data: Some(serde_json::json!({ "doc_id": d.id.as_str() })),
                    ..Default::default()
                });
            }
        }
        items
    }

    /// The items for `#` + a typed query, built for every tag the way they were before.
    fn reference_tag_items(state: &SatzState, range: Range) -> Vec<CompletionItem> {
        let spellings = tag_spellings(state);
        state
            .index
            .all_tags()
            .into_iter()
            .map(|key| {
                let tag_name = spellings.get(key).map_or(key, String::as_str);
                CompletionItem {
                    label: format!("#{}", tag_name),
                    kind: Some(CompletionItemKind::KEYWORD),
                    detail: Some("Tag".to_string()),
                    filter_text: Some(tag_name.to_string()),
                    text_edit: Some(completion_text_edit(range, tag_name.to_string())),
                    ..Default::default()
                }
            })
            .collect()
    }

    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }

        fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }
    }

    const TITLES: &[&str] = &[
        "Alpha",
        "alpha",
        "İş Notları",
        "istanbul planı",
        "Çeviri",
        "felsefe",
        "felsefe kavram",
        "😀 Emoji",
        "Hub",
        "Sub One",
        "ışık",
        "Okul Notları",
        "Untitled",
        "",
    ];
    const HEADINGS: &[&str] = &[
        "Özet",
        "Detaylar",
        "İstanbul planı",
        "felsefe",
        "Sub One",
        "Ownership",
        "iş",
        "ışık",
        "Alpha",
    ];
    const TAGS: &[&str] = &[
        "rust", "Rust", "RUST", "proje", "Proje", "iş", "İş", "ışık", "a/b",
    ];
    const TYPED: &[&str] = &[
        "",
        "   ",
        "a",
        "i",
        "iş",
        "İş",
        "istanbul",
        "felsefe",
        "felsefe kavram",
        "kavram felsefe",
        "zzzz",
        "ö",
        "😀",
        "^alpha",
        "alpha$",
        "!hub",
        "sub one",
        "Alpha",
        "ownership",
        "a b c d e f g h i j k l m n o p",
    ];

    fn random_note(rng: &mut Rng) -> String {
        let mut text = String::new();
        let title = TITLES[rng.below(TITLES.len())];
        if rng.below(2) == 0 {
            text.push_str("---\n");
            if rng.below(2) == 0 && !title.is_empty() {
                text.push_str(&format!("title: {title}\n"));
            }
            if rng.below(3) == 0 {
                text.push_str(&format!(
                    "aliases: [{}, {}]\n",
                    TITLES[rng.below(TITLES.len())].replace(',', ""),
                    HEADINGS[rng.below(HEADINGS.len())]
                ));
            }
            text.push_str("---\n");
        }
        if rng.below(4) != 0 && !title.is_empty() {
            text.push_str(&format!("# {title}\n\n"));
        }
        for _ in 0..rng.below(5) {
            let level = 1 + rng.below(3);
            text.push_str(&format!(
                "{} {}\n\ntext\n\n",
                "#".repeat(level),
                HEADINGS[rng.below(HEADINGS.len())]
            ));
        }
        if rng.below(3) == 0 {
            text.push_str(&format!(
                "#{} #{}\n",
                TAGS[rng.below(TAGS.len())],
                TAGS[rng.below(TAGS.len())]
            ));
        }
        text
    }

    /// A vault of the given notes plus the open note holding `open_line`; completion is asked at
    /// the end of that line.
    fn state_from(texts: &[String], open_line: &str) -> SatzState {
        let mut docs: Vec<satz_core::Document> = texts
            .iter()
            .enumerate()
            .map(|(i, text)| {
                let path = format!("f{}/note-{i}.md", i % 7);
                parse_document(text, Path::new(&path))
            })
            .collect();
        docs.push(parse_document(open_line, Path::new("open.md")));
        let mut state = SatzState::default();
        state.index = Index::build(docs);
        state.set_vault_root(Some(Path::new("").to_path_buf()));
        state.open_docs.insert(
            "file:///open.md".to_string(),
            crate::state::OpenDocument::new(
                "file:///open.md",
                Path::new("open.md").to_path_buf(),
                open_line,
                1,
            ),
        );
        state
    }

    fn state_with_open_line(notes: usize, rng: &mut Rng, open_line: &str) -> SatzState {
        let texts: Vec<String> = (0..notes).map(|_| random_note(rng)).collect();
        state_from(&texts, open_line)
    }

    fn ask(state: &SatzState, open_line: &str) -> Option<CompletionResponse> {
        completion(
            CompletionParams {
                text_document_position: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier {
                        uri: "file:///open.md".parse().unwrap(),
                    },
                    position: Position::new(0, open_line.encode_utf16().count() as u32),
                },
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
                context: None,
            },
            state,
        )
    }

    fn describe(response: &CompletionResponse) -> String {
        format!("{response:?}")
    }

    fn range_after(prefix_units: usize, typed: &str) -> Range {
        Range::new(
            Position::new(0, prefix_units as u32),
            Position::new(0, (prefix_units + typed.encode_utf16().count()) as u32),
        )
    }

    #[test]
    fn notes_are_offered_exactly_as_the_straightforward_version_offers_them() {
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        let mut cases = 0;
        for round in 0..40 {
            // Small vaults, and vaults with far more candidates than the limits below.
            let notes = if round % 4 == 0 {
                100 + rng.below(80)
            } else {
                1 + rng.below(30)
            };
            for typed in TYPED {
                let line = format!("[[{typed}");
                let mut state = state_with_open_line(notes, &mut rng, &line);
                for limit in [0usize, 1, 5, 30, 200, 100_000] {
                    state.config.lsp.completion_limit = limit;
                    let got = ask(&state, &line).expect("an answer");
                    let items = reference_note_items(&state, range_after(2, typed));
                    let want = reference_respond(items, limit, typed);
                    assert_eq!(
                        describe(&got),
                        describe(&want),
                        "round {round} ({notes} notes), typed {typed:?}, limit {limit}"
                    );
                    cases += 1;
                }
            }
        }
        assert!(cases >= 4000, "{cases} cases");
    }

    #[test]
    fn notes_that_are_alike_keep_the_order_they_were_found_in() {
        // Same title, same alias, same headings: every sort key ties, so the order is the order
        // of production alone (the index's own iteration order).
        for typed in ["", "s", "same", "twin", "part"] {
            let line = format!("[[{typed}");
            let texts: Vec<String> = (0..150)
                .map(|_| {
                    "---
title: Same
aliases: [Twin]
---
# Same

## Part

## Part
"
                    .to_string()
                })
                .collect();
            let mut state = state_from(&texts, &line);
            for limit in [0usize, 7, 100, 200, 100_000] {
                state.config.lsp.completion_limit = limit;
                let got = ask(&state, &line).expect("an answer");
                let want = reference_respond(
                    reference_note_items(&state, range_after(2, typed)),
                    limit,
                    typed,
                );
                assert_eq!(
                    describe(&got),
                    describe(&want),
                    "typed {typed:?}, limit {limit}"
                );
            }
        }
    }

    #[test]
    fn tags_are_offered_exactly_as_the_straightforward_version_offers_them() {
        let mut rng = Rng(0xDEAD_BEEF_CAFE_F00D);
        let mut cases = 0;
        for round in 0..40 {
            let notes = if round % 4 == 0 {
                150 + rng.below(80)
            } else {
                1 + rng.below(30)
            };
            for typed in ["", "r", "ru", "Rust", "pro", "iş", "ıs", "a/", "zzz", "İ"] {
                let line = format!("#{typed}");
                let mut state = state_with_open_line(notes, &mut rng, &line);
                for limit in [0usize, 1, 3, 200, 100_000] {
                    state.config.lsp.completion_limit = limit;
                    let got = ask(&state, &line).expect("an answer");
                    let items = reference_tag_items(&state, range_after(1, typed));
                    let want = reference_respond(items, limit, typed);
                    assert_eq!(
                        describe(&got),
                        describe(&want),
                        "round {round} ({notes} notes), typed {typed:?}, limit {limit}"
                    );
                    cases += 1;
                }
            }
        }
        assert!(cases >= 1500, "{cases} cases");
    }
}
