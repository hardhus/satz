use std::collections::HashMap;

use tower_lsp_server::ls_types::{
    Location, Position, Range, SymbolInformation, SymbolKind, Uri, WorkspaceSymbolParams,
    WorkspaceSymbolResponse,
};

use crate::convert::byte_range_to_lsp;
use crate::rank::Ranker;
use crate::state::SatzState;

pub fn workspace_symbol(
    params: WorkspaceSymbolParams,
    state: &SatzState,
) -> Option<WorkspaceSymbolResponse> {
    let raw_query = params.query.trim();
    tracing::debug!(query = raw_query, "workspace_symbol");

    // Check for `tag:tagname query` prefix
    let (tag_filter, search_query) = if let Some(rest) = raw_query.strip_prefix("tag:") {
        if let Some((tag, rest_q)) = rest.split_once(' ') {
            (Some(tag.trim()), rest_q.trim())
        } else {
            (Some(rest.trim()), "")
        }
    } else {
        (None, raw_query)
    };

    let mut candidate_docs: Vec<&satz_core::Document> = if let Some(tag) = tag_filter {
        state.index.docs_with_tag(tag).collect()
    } else {
        state.index.documents().collect()
    };

    // Sort documents by path for stable ordering
    candidate_docs.sort_by(|a, b| a.id.as_str().cmp(b.id.as_str()));

    // What matches is collected first, cheaply (no URI, no strings, no `Location`): a vault of ten
    // thousand notes has tens of thousands of possible symbols, and only the best hundred are
    // answered. Only those are built.
    let mut ranker = Ranker::new(search_query);
    let mut candidates: Vec<Candidate> = Vec::new();
    for doc in candidate_docs {
        // 1. Match Doc Title
        if let Some(score) = ranker.score(title_of(doc)) {
            candidates.push(Candidate::new(score, candidates.len(), doc, Hit::Title));
        }

        // 2. Match Doc Aliases
        for alias in &doc.frontmatter.aliases {
            if let Some(score) = ranker.score(alias) {
                candidates.push(Candidate::new(
                    score,
                    candidates.len(),
                    doc,
                    Hit::Alias(alias),
                ));
            }
        }

        // 3. Match Headings
        for heading in &doc.headings {
            let heading_text = heading.text.trim();
            // The note's own top heading is already listed as the note (its title).
            if heading.level == 1 && heading_text == doc.title {
                continue;
            }
            if let Some(score) = ranker.score(heading_text) {
                candidates.push(Candidate::new(
                    score,
                    candidates.len(),
                    doc,
                    Hit::Heading(heading),
                ));
            }
        }
    }

    // The best hundred by match score (higher first), equal scores in the order they were found:
    // by note, then title, aliases, headings.
    let total = candidates.len();
    if total > LIMIT {
        candidates.select_nth_unstable_by(LIMIT - 1, by_rank);
    }
    let best = total.min(LIMIT);
    candidates[..best].sort_unstable_by(by_rank);

    let mut places = Places::new(state);
    let mut results: Vec<SymbolInformation> = Vec::with_capacity(best);
    let mut without_a_place = false;
    for candidate in &candidates[..best] {
        match places.build(candidate) {
            Some(symbol) => results.push(symbol),
            None => without_a_place = true,
        }
    }
    if without_a_place && total > LIMIT {
        // A note whose file has no URI is not listed at all, so the hundred are made up from the
        // matches behind the first hundred, in order.
        candidates.sort_unstable_by(by_rank);
        results.clear();
        for candidate in &candidates {
            if results.len() == LIMIT {
                break;
            }
            if let Some(symbol) = places.build(candidate) {
                results.push(symbol);
            }
        }
    }

    Some(WorkspaceSymbolResponse::Flat(results))
}

/// The most symbols one answer carries.
const LIMIT: usize = 100;

/// What matched, before anything is built for it.
enum Hit<'a> {
    /// The note itself, by its title (or its file name when it has none).
    Title,
    Alias(&'a str),
    Heading(&'a satz_core::Heading),
}

/// A match: cheap to make, so all of them can be kept and only the winners built.
struct Candidate<'a> {
    score: u32,
    /// The order in which matches were found; decides between equal scores.
    seq: usize,
    doc: &'a satz_core::Document,
    hit: Hit<'a>,
}

impl<'a> Candidate<'a> {
    fn new(score: u32, seq: usize, doc: &'a satz_core::Document, hit: Hit<'a>) -> Self {
        Self {
            score,
            seq,
            doc,
            hit,
        }
    }
}

/// Better matches first; equal scores in the order they were found (a total order).
fn by_rank(a: &Candidate, b: &Candidate) -> std::cmp::Ordering {
    b.score.cmp(&a.score).then(a.seq.cmp(&b.seq))
}

fn title_of(doc: &satz_core::Document) -> &str {
    if doc.title.is_empty() {
        doc.id.as_str()
    } else {
        &doc.title
    }
}

/// Builds the answer's symbols, with the URI of each note worked out once.
struct Places<'a> {
    state: &'a SatzState,
    uris: HashMap<&'a str, Option<Uri>>,
}

impl<'a> Places<'a> {
    fn new(state: &'a SatzState) -> Self {
        Self {
            state,
            uris: HashMap::new(),
        }
    }

    /// `None` for a note whose file has no URI (it is not listed at all).
    fn uri_of(&mut self, doc: &'a satz_core::Document) -> Option<Uri> {
        let state = self.state;
        self.uris
            .entry(doc.id.as_str())
            .or_insert_with(|| state.doc_uri(doc))
            .clone()
    }

    #[allow(deprecated)] // The LSP type marks `deprecated` as such but the protocol still requires it.
    fn build(&mut self, candidate: &Candidate<'a>) -> Option<SymbolInformation> {
        let doc = candidate.doc;
        let uri = self.uri_of(doc)?;
        let start_of_note = Range::new(Position::new(0, 0), Position::new(0, 0));
        Some(match candidate.hit {
            Hit::Title => SymbolInformation {
                name: title_of(doc).to_string(),
                kind: SymbolKind::FILE,
                tags: None,
                deprecated: None,
                location: Location::new(uri, start_of_note),
                container_name: None,
            },
            Hit::Alias(alias) => SymbolInformation {
                name: format!("{} (alias)", alias),
                kind: SymbolKind::KEY,
                tags: None,
                deprecated: None,
                location: Location::new(uri, start_of_note),
                container_name: Some(doc.title.clone()),
            },
            Hit::Heading(heading) => SymbolInformation {
                name: heading.text.trim().to_string(),
                kind: SymbolKind::STRING,
                tags: None,
                deprecated: None,
                location: Location::new(uri, byte_range_to_lsp(heading.range, &doc.line_index)),
                container_name: Some(doc.title.clone()),
            },
        })
    }
}

#[cfg(test)]
mod tests;
