use std::collections::HashMap;

use tower_lsp_server::ls_types::{
    Location, Position, Range, SymbolInformation, SymbolKind, Uri, WorkspaceSymbolParams,
    WorkspaceSymbolResponse,
};

use crate::convert::{byte_range_to_lsp, path_to_uri};
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
            .or_insert_with(|| {
                let doc_path = match state.vault_root() {
                    Some(root) if !doc.path.is_absolute() => root.join(&doc.path),
                    _ => doc.path.clone(),
                };
                path_to_uri(&doc_path)
            })
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
// Test states are built field by field so each test shows exactly what it sets up.
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use super::*;
    use satz_core::{Index, parse_document};
    use std::path::Path;

    #[test]
    fn test_workspace_symbol_search() {
        let rel_a = Path::new("doc-a.md");
        let rel_b = Path::new("doc-b.md");
        let doc_a = parse_document(
            "---\ntags: [philosophy]\n---\n# Wittgenstein Tractatus",
            rel_a,
        );
        let doc_b = parse_document("# Rust Programming\n## Ownership and Lifetimes", rel_b);

        let mut state = SatzState::default();
        state.index = Index::build(vec![doc_a, doc_b]);
        state.set_vault_root(Some(if cfg!(windows) {
            Path::new("C:\\").to_path_buf()
        } else {
            Path::new("/").to_path_buf()
        }));

        // 1. General search for "Wittgen"
        let params = WorkspaceSymbolParams {
            query: "Wittgen".to_string(),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };

        let response = workspace_symbol(params, &state).expect("Response expected");
        if let WorkspaceSymbolResponse::Flat(symbols) = response {
            assert!(symbols.iter().any(|s| s.name.contains("Wittgenstein")));
        } else {
            panic!("Expected flat symbols response");
        }

        // 2. Tag filtered search for "tag:philosophy Tract"
        let params_tag = WorkspaceSymbolParams {
            query: "tag:philosophy Tract".to_string(),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };

        let response_tag = workspace_symbol(params_tag, &state).expect("Response expected");
        if let WorkspaceSymbolResponse::Flat(symbols) = response_tag {
            // The note itself; its H1 has the same text and is not listed a second time.
            assert_eq!(symbols.len(), 1);
            assert!(symbols.iter().all(|s| s.name.contains("Wittgenstein")));
        } else {
            panic!("Expected flat symbols response");
        }
    }

    fn symbols_for(files: &[(&str, &str)], query: &str) -> Vec<(String, SymbolKind)> {
        let mut state = SatzState::default();
        state.index = Index::build(
            files
                .iter()
                .map(|(path, text)| parse_document(text, Path::new(path)))
                .collect(),
        );
        state.set_vault_root(Some(if cfg!(windows) {
            Path::new("C:\\").to_path_buf()
        } else {
            Path::new("/").to_path_buf()
        }));
        let params = WorkspaceSymbolParams {
            query: query.to_string(),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };
        let Some(WorkspaceSymbolResponse::Flat(symbols)) = workspace_symbol(params, &state) else {
            return Vec::new();
        };
        let mut found: Vec<(String, SymbolKind)> =
            symbols.into_iter().map(|s| (s.name, s.kind)).collect();
        found.sort_by(|a, b| a.0.cmp(&b.0));
        found
    }

    #[test]
    fn a_notes_own_h1_is_not_listed_twice() {
        let found = symbols_for(&[("a.md", "# Alpha Doc\n\n## Sub One\n")], "");
        assert_eq!(
            found,
            vec![
                ("Alpha Doc".to_string(), SymbolKind::FILE),
                ("Sub One".to_string(), SymbolKind::STRING),
            ]
        );
    }

    #[test]
    fn a_heading_that_differs_from_the_title_is_still_listed() {
        let found = symbols_for(
            &[("a.md", "---\ntitle: Front Title\n---\n# Heading One\n")],
            "",
        );
        assert_eq!(
            found,
            vec![
                ("Front Title".to_string(), SymbolKind::FILE),
                ("Heading One".to_string(), SymbolKind::STRING),
            ]
        );
    }

    #[test]
    fn aliases_have_their_own_symbol_kind() {
        let found = symbols_for(
            &[("a.md", "---\naliases: [short]\n---\n# Alpha\n")],
            "short",
        );
        assert_eq!(found, vec![("short (alias)".to_string(), SymbolKind::KEY)]);
    }

    #[test]
    fn a_note_without_any_title_is_listed_by_its_file_name() {
        let found = symbols_for(&[("plain-note.md", "just some text\n")], "plain");
        assert_eq!(found, vec![("plain-note".to_string(), SymbolKind::FILE)]);
    }

    #[test]
    fn turkish_capital_i_headings_are_found_with_a_lowercase_query() {
        let found = symbols_for(&[("a.md", "# İş Notları\n\n## İstanbul planı\n")], "iş");
        assert!(found.iter().any(|(n, _)| n == "İş Notları"), "{found:?}");
        let found = symbols_for(&[("a.md", "# X\n\n## İstanbul planı\n")], "istanbul");
        assert!(
            found.iter().any(|(n, _)| n == "İstanbul planı"),
            "{found:?}"
        );
    }

    // ---- the answer must stay what it was: the straightforward version, kept as the reference ----

    /// The algorithm as it was before the answer was built lazily: a URI and a `Location` for every
    /// note, a `SymbolInformation` for every match, everything sorted (stably), the first 100 kept.
    fn reference_workspace_symbol(
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

        let mut ranker = Ranker::new(search_query);
        let mut scored_symbols: Vec<(u32, SymbolInformation)> = Vec::new();

        for doc in candidate_docs {
            let doc_path = match state.vault_root() {
                Some(root) if !doc.path.is_absolute() => root.join(&doc.path),
                _ => doc.path.clone(),
            };
            let doc_uri = match path_to_uri(&doc_path) {
                Some(u) => u,
                None => continue,
            };

            let default_location = Location::new(
                doc_uri.clone(),
                Range::new(Position::new(0, 0), Position::new(0, 0)),
            );

            // 1. Match Doc Title
            let title_name = if !doc.title.is_empty() {
                doc.title.clone()
            } else {
                doc.id.as_str().to_string()
            };

            if let Some(score) = ranker.score(&title_name) {
                // The LSP type marks this field `#[deprecated]` but the protocol still requires it.
                #[allow(deprecated)]
                scored_symbols.push((
                    score,
                    SymbolInformation {
                        name: title_name,
                        kind: SymbolKind::FILE,
                        tags: None,
                        deprecated: None,
                        location: default_location.clone(),
                        container_name: None,
                    },
                ));
            }

            // 2. Match Doc Aliases
            for alias in &doc.frontmatter.aliases {
                if let Some(score) = ranker.score(alias) {
                    // The LSP type marks this field `#[deprecated]` but the protocol still requires it.
                    #[allow(deprecated)]
                    scored_symbols.push((
                        score,
                        SymbolInformation {
                            name: format!("{} (alias)", alias),
                            kind: SymbolKind::KEY,
                            tags: None,
                            deprecated: None,
                            location: default_location.clone(),
                            container_name: Some(doc.title.clone()),
                        },
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
                    let range = byte_range_to_lsp(heading.range, &doc.line_index);
                    // The LSP type marks this field `#[deprecated]` but the protocol still requires it.
                    #[allow(deprecated)]
                    scored_symbols.push((
                        score,
                        SymbolInformation {
                            name: heading_text.to_string(),
                            kind: SymbolKind::STRING,
                            tags: None,
                            deprecated: None,
                            location: Location::new(doc_uri.clone(), range),
                            container_name: Some(doc.title.clone()),
                        },
                    ));
                }
            }
        }

        // Sort by match score descending
        scored_symbols.sort_by_key(|a| std::cmp::Reverse(a.0));

        let results: Vec<SymbolInformation> = scored_symbols
            .into_iter()
            .take(100)
            .map(|(_, sym)| sym)
            .collect();

        Some(WorkspaceSymbolResponse::Flat(results))
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

    fn a_root() -> std::path::PathBuf {
        if cfg!(windows) {
            Path::new("C:\\").to_path_buf()
        } else {
            Path::new("/").to_path_buf()
        }
    }

    fn absolute(rel: &str) -> String {
        if cfg!(windows) {
            format!("C:\\abs\\{rel}")
        } else {
            format!("/abs/{rel}")
        }
    }

    fn params(query: &str) -> WorkspaceSymbolParams {
        WorkspaceSymbolParams {
            query: query.to_string(),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        }
    }

    /// Everything about the answer, in order, as text (names, kinds, containers, places).
    fn answer(response: Option<WorkspaceSymbolResponse>) -> Vec<String> {
        match response {
            Some(WorkspaceSymbolResponse::Flat(symbols)) => {
                symbols.iter().map(|s| format!("{s:?}")).collect()
            }
            other => panic!("expected a flat answer, got {other:?}"),
        }
    }

    const TITLES: &[&str] = &[
        "Alpha",
        "İş Notları",
        "istanbul planı",
        "Çeviri",
        "felsefe",
        "😀 Emoji",
        "Hub",
        "Sub One",
        "ışık",
        "Okul Notları",
        "felsefe kavram",
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
    ];

    /// A random note: front matter (title, aliases, tags), then headings of every level.
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
            if rng.below(2) == 0 {
                text.push_str(&format!(
                    "tags: [{}]\n",
                    ["x", "y", "philosophy"][rng.below(3)]
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
        text
    }

    const QUERIES: &[&str] = &[
        "",
        "   ",
        "a",
        "i",
        "iş",
        "istanbul",
        "felsefe",
        "felsefe kavram",
        "zzzz",
        "ö",
        "tag:x",
        "tag:x iş",
        "tag:philosophy",
        "tag:",
        "tag:nope",
        "😀",
        "not",
        "sub one",
    ];

    fn state_of(docs: Vec<satz_core::Document>, root: Option<std::path::PathBuf>) -> SatzState {
        let mut state = SatzState::default();
        state.index = Index::build(docs);
        state.set_vault_root(root);
        state
    }

    /// Asserts that both give the same answer; returns how many symbols it has.
    fn same_answer(state: &SatzState, query: &str, context: &str) -> usize {
        let now = answer(workspace_symbol(params(query), state));
        assert_eq!(
            now,
            answer(reference_workspace_symbol(params(query), state)),
            "{context}, query {query:?}"
        );
        now.len()
    }

    #[test]
    fn the_answer_is_the_same_as_the_straightforward_versions_on_random_vaults() {
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        let (mut cases, mut with_symbols, mut cut_at_a_hundred) = (0, 0, 0);
        for round in 0..120 {
            // Small vaults, and vaults with far more than 100 possible symbols.
            let notes = if round % 4 == 0 {
                120 + rng.below(80)
            } else {
                1 + rng.below(25)
            };
            let docs: Vec<satz_core::Document> = (0..notes)
                .map(|i| {
                    let path = format!("f{}/note-{i}.md", i % 7);
                    parse_document(&random_note(&mut rng), Path::new(&path))
                })
                .collect();
            let state = state_of(docs, Some(a_root()));
            for query in QUERIES {
                let found = same_answer(&state, query, &format!("vault {round} ({notes} notes)"));
                cases += 1;
                with_symbols += usize::from(found > 0);
                cut_at_a_hundred += usize::from(found == 100);
            }
        }
        assert!(cases >= 400, "{cases} cases");
        // The cases are not all empty answers, and many are cut at a hundred.
        assert!(
            with_symbols > cases / 2,
            "{with_symbols} of {cases} with symbols"
        );
        assert!(
            cut_at_a_hundred >= 20,
            "{cut_at_a_hundred} cut at a hundred"
        );
    }

    #[test]
    fn notes_with_the_same_title_keep_their_order_by_id_and_position() {
        // Every note scores the same for the empty query, and 150 notes make 300+ symbols: which
        // hundred are returned, and in what order, is decided by the order of production alone.
        let docs: Vec<satz_core::Document> = (0..150)
            .map(|i| {
                parse_document(
                    "---\naliases: [Same]\n---\n# Same title\n\n## Same heading\n",
                    Path::new(&format!("dir{}/n{i:03}.md", i % 5)),
                )
            })
            .collect();
        let state = state_of(docs, Some(a_root()));
        for query in ["", "same", "s", "same title"] {
            same_answer(&state, query, "150 notes alike");
        }
        assert_eq!(answer(workspace_symbol(params(""), &state)).len(), 100);
    }

    #[test]
    fn a_note_without_a_place_gives_no_symbols_and_the_hundred_are_made_up_from_the_rest() {
        // Without a vault root a relative path has no URI: such a note is not listed at all.
        let relative_only: Vec<satz_core::Document> = (0..10)
            .map(|i| parse_document("# Alpha\n", Path::new(&format!("n{i}.md"))))
            .collect();
        let state = state_of(relative_only, None);
        assert!(answer(workspace_symbol(params(""), &state)).is_empty());
        same_answer(&state, "", "relative paths, no vault root");

        // A few notes with an absolute path among many without one: whatever the many would
        // have taken up of the hundred is made up from the notes that do have a place.
        let mut rng = Rng(42);
        for many in [5usize, 40, 150] {
            let mut docs: Vec<satz_core::Document> = (0..many)
                .map(|i| parse_document(&random_note(&mut rng), Path::new(&format!("rel/n{i}.md"))))
                .collect();
            for i in 0..6 {
                docs.push(parse_document(
                    &random_note(&mut rng),
                    Path::new(&absolute(&format!("a{i}.md"))),
                ));
            }
            let state = state_of(docs, None);
            for query in QUERIES {
                same_answer(
                    &state,
                    query,
                    &format!("{many} relative and 6 absolute notes"),
                );
            }
            assert!(
                !answer(workspace_symbol(params(""), &state)).is_empty(),
                "the absolute notes are listed"
            );
        }
    }
}
