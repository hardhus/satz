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
        // `d.id` is exactly `d.path`, spelled with `/`: the same string `DocId::from_path` builds.
        let insert_base = d.id.as_str().strip_suffix(".md").unwrap_or(d.id.as_str());
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

/// Where the cursor is on its line, and the text around it: the ground every decision about what
/// to offer stands on. Text and numbers only; nothing is built here.
struct Cursor<'a> {
    /// The cursor's line, without its line end.
    source: &'a str,
    /// The cursor's line number in the note.
    line: u32,
    /// The cursor's byte offset in `source`.
    at: usize,
}

impl<'a> Cursor<'a> {
    /// The column is in UTF-16 units; a column past the end means the end of the line, and one in
    /// the middle of a surrogate pair stays before that character.
    fn new(source: &'a str, line: u32, column: u32) -> Self {
        let mut at = 0usize;
        let mut units = 0u32;
        for c in source.chars() {
            if units + c.len_utf16() as u32 > column {
                break;
            }
            units += c.len_utf16() as u32;
            at += c.len_utf8();
        }
        Self { source, line, at }
    }

    /// The line up to the cursor.
    fn prefix(&self) -> &'a str {
        &self.source[..self.at]
    }

    /// Where the word the cursor is inside ends (`[[Ol|gu]]`): what follows the cursor up to it is
    /// part of what is replaced, or the completion would leave it behind (`[[doc-bgu]]`). It ends
    /// at a bracket, `|`, `#` or `^`. Whitespace ends the word too, whichever comes first: text
    /// further along the line (`x^2`, a later `#tag`, a table `|`) is not part of what is being
    /// completed and must survive.
    fn word_end(&self) -> usize {
        let tail = &self.source[self.at..];
        self.at
            + tail
                .find(|c: char| matches!(c, ']' | '|' | '#' | '^') || c.is_whitespace())
                .unwrap_or(tail.len())
    }

    /// The closing brackets a link needs after the word: none -> `]]`, one -> the missing `]`,
    /// both -> nothing. (The link may continue with `|display` or `#anchor` before its closing
    /// `]]`.)
    fn close_suffix(&self) -> &'static str {
        let after_word = &self.source[self.word_end()..];
        let closed_later = after_word
            .split("[[")
            .next()
            .is_some_and(|s| s.contains("]]"));
        if closed_later {
            ""
        } else if after_word.starts_with(']') {
            "]"
        } else {
            "]]"
        }
    }

    /// The range between two byte offsets of the line, in the client's UTF-16 columns.
    fn range(&self, start: usize, end: usize) -> Range {
        let column = |byte: usize| self.source[..byte].encode_utf16().count() as u32;
        Range::new(
            Position::new(self.line, column(start)),
            Position::new(self.line, column(end)),
        )
    }
}

/// What the cursor is in the middle of writing: where it is decided which candidates are offered.
/// Data only; the offsets are bytes of the cursor's line, where the text being replaced starts.
#[derive(Debug, PartialEq)]
enum CompletionContext<'a> {
    /// `[[note|`: after the `|` the link's display text is written; nothing is offered.
    DisplayText,
    /// `[[typed`: a note, an alias or a heading of any note.
    Note { typed: &'a str, start: usize },
    /// `[[target#typed`: a heading of `target` (empty: this note), and its blocks while nothing
    /// has been typed.
    Heading {
        target: &'a str,
        typed: &'a str,
        start: usize,
    },
    /// `[[target#^typed`: a block of `target` (empty: this note). The replaced text includes the
    /// typed `^`, because every new text starts with its own.
    Block { target: &'a str, start: usize },
    /// `[^label`: a footnote definition.
    Footnote { start: usize },
    /// `#typed`: a tag.
    Tag { typed: &'a str, start: usize },
}

/// The contexts the cursor can be in, the most specific first. The first one that can answer does;
/// a later one is only asked when the ones before it had nothing to say.
fn contexts<'a>(cursor: &Cursor<'a>) -> Vec<CompletionContext<'a>> {
    let prefix = cursor.prefix();
    let mut found = Vec::new();

    // A `[[` that a `]]` has already closed on this line is finished text, not a link being typed.
    let open_bracket = prefix
        .rfind("[[")
        .filter(|idx| !prefix[idx + 2..].contains("]]"));
    if let Some(open_bracket_idx) = open_bracket {
        let inside = &prefix[open_bracket_idx + 2..];
        let start = open_bracket_idx + 2;
        found.push(if inside.contains('|') {
            CompletionContext::DisplayText
        } else if let Some((target, anchor)) = inside.split_once('#') {
            let anchor_start = start + target.len() + 1;
            if anchor.starts_with('^') {
                CompletionContext::Block {
                    target,
                    start: anchor_start,
                }
            } else {
                CompletionContext::Heading {
                    target,
                    typed: anchor,
                    start: anchor_start,
                }
            }
        } else {
            CompletionContext::Note {
                typed: inside,
                start,
            }
        });
    }

    if let Some(open_footnote) = prefix.rfind("[^")
        && !prefix[open_footnote + 2..].contains(']')
    {
        found.push(CompletionContext::Footnote {
            start: open_footnote + 2,
        });
    }

    if let Some(hash) = prefix.rfind('#') {
        // A tag starts at the line start, after whitespace, or after an opening bracket/quote (the
        // same set the parser accepts), and only tag characters have been typed since the `#`:
        // `# Heading text|` is a heading marker, not a tag being typed.
        let starts_a_tag = prefix[..hash].chars().next_back().is_none_or(|c| {
            c.is_whitespace() || matches!(c, '(' | '[' | '{' | '"' | '\'' | '<' | '—' | '–')
        });
        let only_tag_characters = prefix[hash + 1..]
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '/'));
        if starts_a_tag && only_tag_characters {
            found.push(CompletionContext::Tag {
                typed: &prefix[hash + 1..],
                start: hash + 1,
            });
        }
    }

    found
}

/// The note a `[[target#…` refers to.
enum Target<'s> {
    Found(&'s satz_core::Document),
    /// No note is called that: nothing can be offered.
    NotResolved,
    /// The name resolves, but the index has no such note.
    NotIndexed,
}

/// `target` is empty for a reference to the note the cursor is in.
fn target_of<'s>(
    state: &'s SatzState,
    current: &'s satz_core::Document,
    target: &str,
) -> Target<'s> {
    let id = if target.is_empty() {
        &current.id
    } else if let Some(resolved) = state.index.resolve_link(target) {
        resolved
    } else {
        tracing::debug!(
            target,
            "completion: returning candidates count=0 (target doc did not resolve)"
        );
        return Target::NotResolved;
    };
    match state.index.get_doc(id) {
        Some(doc) => Target::Found(doc),
        None => Target::NotIndexed,
    }
}

/// The items for one context; `None` when it has nothing to say, and the next context is asked.
fn answer(
    context: &CompletionContext,
    cursor: &Cursor,
    state: &SatzState,
    current: &satz_core::Document,
) -> Option<CompletionResponse> {
    let limit = state.config.lsp.completion_limit;
    match *context {
        // After `|` the user is writing the link's display text: nothing to complete.
        CompletionContext::DisplayText => Some(CompletionResponse::Array(vec![])),
        CompletionContext::Note { typed, start } => {
            Some(note_items(state, cursor, typed, start, limit))
        }
        CompletionContext::Heading {
            target,
            typed,
            start,
        } => match target_of(state, current, target) {
            Target::Found(doc) => Some(heading_items(doc, cursor, typed, start)),
            Target::NotResolved => Some(CompletionResponse::Array(vec![])),
            Target::NotIndexed => None,
        },
        CompletionContext::Block { target, start } => match target_of(state, current, target) {
            Target::Found(doc) => Some(block_items(doc, cursor, start)),
            Target::NotResolved => Some(CompletionResponse::Array(vec![])),
            Target::NotIndexed => None,
        },
        CompletionContext::Footnote { start } => Some(footnote_items(current, cursor, start)),
        CompletionContext::Tag { typed, start } => {
            Some(tag_items(state, cursor, typed, start, limit))
        }
    }
}

/// Every note, alias and heading of the vault that matches what was typed.
fn note_items(
    state: &SatzState,
    cursor: &Cursor,
    typed: &str,
    start: usize,
    limit: usize,
) -> CompletionResponse {
    let range = cursor.range(start, cursor.word_end());
    let close_suffix = cursor.close_suffix();

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
    let ranking = rank(candidates, limit, typed);
    let items = ranking
        .items
        .iter()
        .map(|c| c.build(range, close_suffix))
        .collect();
    response_of(items, ranking.incomplete)
}

fn block_item(block_id: &str, range: Range, close_suffix: &str) -> CompletionItem {
    CompletionItem {
        label: format!("^{block_id}"),
        kind: Some(CompletionItemKind::VARIABLE),
        detail: Some("Block Anchor".to_string()),
        text_edit: Some(completion_text_edit(
            range,
            format!("^{block_id}{close_suffix}"),
        )),
        filter_text: Some(format!("^{block_id}")),
        ..Default::default()
    }
}

/// The blocks of `target`: `[[doc#^...`.
fn block_items(target: &satz_core::Document, cursor: &Cursor, start: usize) -> CompletionResponse {
    let range = cursor.range(start, cursor.word_end());
    let close_suffix = cursor.close_suffix();
    let items: Vec<CompletionItem> = target
        .blocks
        .iter()
        .map(|b| block_item(&b.id, range, close_suffix))
        .collect();
    tracing::debug!(
        count = items.len(),
        "completion: returning candidates (block anchors)"
    );
    CompletionResponse::Array(items)
}

/// The headings of `target`, and its blocks while nothing has been typed: `[[doc#...`.
fn heading_items(
    target: &satz_core::Document,
    cursor: &Cursor,
    typed: &str,
    start: usize,
) -> CompletionResponse {
    let range = cursor.range(start, cursor.word_end());
    let close_suffix = cursor.close_suffix();
    // A link to a heading that appears twice reaches the first one, so the later copy is not a
    // different target and is not offered.
    let mut seen_slugs = std::collections::HashSet::new();
    let mut items: Vec<CompletionItem> = target
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

    // If query is empty, also suggest blocks
    if typed.is_empty() {
        items.extend(
            target
                .blocks
                .iter()
                .map(|b| block_item(&b.id, range, close_suffix)),
        );
    }

    tracing::debug!(
        count = items.len(),
        "completion: returning candidates (headings/blocks for doc)"
    );
    CompletionResponse::Array(items)
}

/// The footnote definitions of the note the cursor is in: `[^...`.
fn footnote_items(
    current: &satz_core::Document,
    cursor: &Cursor,
    start: usize,
) -> CompletionResponse {
    let range = cursor.range(start, cursor.at);
    let items: Vec<CompletionItem> = current
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
    CompletionResponse::Array(items)
}

/// Every tag of the vault: `#...`.
fn tag_items(
    state: &SatzState,
    cursor: &Cursor,
    typed: &str,
    start: usize,
    limit: usize,
) -> CompletionResponse {
    let range = cursor.range(start, cursor.at);
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
    respond(items, limit, typed)
}

pub fn completion(params: CompletionParams, state: &SatzState) -> Option<CompletionResponse> {
    let uri = params.text_document_position.text_document.uri.as_str();
    let pos = params.text_document_position.position;
    tracing::debug!(uri, ?pos, "completion");

    let (open_doc, doc) = state.doc_for_uri(uri)?;

    // Byte-offset/text-scan against the LIVE rope, not `doc.line_index`: `doc` is the
    // debounced (200-500ms) reparse snapshot, but completion re-fires immediately on every
    // `[`/`#`/`^` keystroke, faster than that debounce can settle. Scanning stale text here
    // corrupts the line-prefix/closing-bracket checks -- e.g. producing a duplicated
    // `]]` when a second wikilink is typed quickly on the same line right after a first one.
    // Only the line the cursor is on is needed (copied once), not the whole document: every
    // decision looks at that line.
    let line_number = (pos.line as usize).min(open_doc.rope.len_lines().saturating_sub(1));
    let line_text = open_doc.rope.line(line_number).to_string();
    let source: &str = line_text.trim_end_matches(['\r', '\n']);
    let cursor = Cursor::new(source, line_number as u32, pos.character);

    contexts(&cursor)
        .iter()
        .find_map(|context| answer(context, &cursor, state, doc))
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
mod tests;
