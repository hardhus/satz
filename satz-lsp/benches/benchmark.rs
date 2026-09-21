//! Benchmark of the language server's handlers on a large synthetic vault (10,000 notes):
//! `cargo bench -p satz-lsp --bench benchmark [-- --quick] [-- --check]`.
//!
//! What is measured is the handler itself: a request's work on a `SatzState`, without the JSON-RPC
//! transport or the async runtime. The measuring code (rounds, best and median, families of sizes
//! with the exponent of the growth) is shared with the satz-core benchmark; read its header for how
//! to trust the numbers on a machine that is busy with other things. In short: compare runs by
//! `min`, and believe the exponent and ratios more than any single time.
//!
//! The labels `comfortable` / `borderline` / `slow` next to the handlers that run on every key or
//! cursor move are only a rough feel (16 ms is a frame, 50 ms is where typing starts to feel
//! behind); they are not thresholds and never flag anything.

#[path = "../../satz-core/benches/support/mod.rs"]
mod support;

use std::collections::HashMap;
use std::hint::black_box;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use satz_core::{Document, Index, parse_document};
use satz_lsp::convert::path_to_uri;
use satz_lsp::handlers::completion::completion;
use satz_lsp::handlers::diagnostics::{compute_diagnostics, pull_workspace_report};
use satz_lsp::handlers::document_highlight::document_highlight;
use satz_lsp::handlers::execute_command::compute_format_changes;
use satz_lsp::handlers::references::find_references;
use satz_lsp::handlers::workspace_symbol::workspace_symbol;
use satz_lsp::state::{OpenDocument, SatzState};
use support::*;
use tower_lsp_server::ls_types as lsp;

// ---- the synthetic vault --------------------------------------------------------------------

const WORDS: &[&str] = &[
    "felsefe",
    "kavram",
    "şiir",
    "yazılım",
    "tasarım",
    "müzik",
    "tarih",
    "bilim",
    "özgürlük",
    "çeviri",
    "algoritma",
    "proje",
    "ağaç",
    "göl",
    "ışık",
    "dünya",
    "sistem",
    "dil",
    "zihin",
    "toplum",
    "history",
    "design",
    "garden",
    "network",
    "memory",
    "language",
    "music",
    "science",
    "notes",
    "daily",
    "kitap",
    "sanat",
    "ekonomi",
    "doğa",
    "şehir",
    "yolculuk",
    "okul",
    "hukuk",
    "mimari",
    "fizik",
];

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

fn root() -> PathBuf {
    if cfg!(windows) {
        PathBuf::from("C:\\vault")
    } else {
        PathBuf::from("/vault")
    }
}

/// The title of note `i` (a pair of words and the number, so that titles are distinct).
fn title(i: usize) -> String {
    format!(
        "{} {} {i}",
        WORDS[i % WORDS.len()],
        WORDS[(i / 7 + 3) % WORDS.len()]
    )
}

fn note_path(i: usize) -> String {
    format!("f{}/note-{i}.md", i % 40)
}

/// Notes with titles, aliases, hierarchical tags, headings, block ids, wikilinks (plain, aliased,
/// by path with a heading), relative Markdown links and a few broken links; every 20th note links
/// to `Hub`.
fn note_text(i: usize, total: usize, rng: &mut Rng) -> String {
    let mut text = String::from("---\n");
    if i.is_multiple_of(3) {
        text.push_str(&format!(
            "aliases: [Kısa {} {i}]\n",
            WORDS[rng.below(WORDS.len())]
        ));
    }
    text.push_str(&format!(
        "tags: [t{}/{}, proje/{}, {}]\n---\n\n",
        rng.below(30),
        rng.below(10),
        rng.below(10),
        ["kavram", "felsefe", "taslak", "arşiv"][rng.below(4)]
    ));
    text.push_str(&format!("# {}\n\n## Özet\n\n", title(i)));
    for _ in 0..3 + rng.below(5) {
        let j = rng.below(total);
        match rng.below(5) {
            0 => text.push_str(&format!("Bkz. [[{}]] ve daha fazlası. ", title(j))),
            1 => text.push_str(&format!("Bkz. [[{}|takma ad]]. ", title(j))),
            2 => text.push_str(&format!("Bkz. [[{}#Özet]]. ", title(j))),
            3 => text.push_str(&format!("Bkz. [not](../f{}/note-{j}.md). ", j % 40)),
            _ => text.push_str(&format!("Bkz. [[f{}/note-{j}#Özet]]. ", j % 40)),
        }
    }
    if rng.below(20) == 0 {
        text.push_str(&format!("Bkz. [[Olmayan not {i}]]. "));
    }
    if i.is_multiple_of(20) {
        text.push_str("Bkz. [[Hub]]. ");
    }
    text.push_str(&format!("\n\n## Detaylar\n\nSon söz ^b{i}\n"));
    text
}

/// The `total` notes, `Hub` and `open.md` (the note the requests are made in), parsed.
fn vault_docs(total: usize, open_text: &str) -> Vec<Document> {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut docs: Vec<Document> = (0..total)
        .map(|i| parse_document(&note_text(i, total, &mut rng), &PathBuf::from(note_path(i))))
        .collect();
    docs.push(parse_document(
        "# Hub\n\nThe hub.\n",
        &PathBuf::from("hub.md"),
    ));
    docs.push(parse_document(open_text, &PathBuf::from("open.md")));
    docs
}

// ---- the note the requests are made in ------------------------------------------------------

/// Where the cursor is in each scene; the note's lines are described by `open_text`.
struct Scenes {
    text: String,
    uri: String,
}

impl Scenes {
    fn new() -> Self {
        let text = format!(
            "# Open note\n\
             \n\
             Start: [[\n\
             Short: [[fel\n\
             Two: [[felsefe yaz\n\
             Heading: [[{t0}#\n\
             Tag all: #\n\
             Tag prefix: #pro\n\
             Block: [[{t0}#^\n\
             Links: [[{t1}]] and [[Olmayan bağlantı]] and #kavram\n\
             ## Highlight heading\n\
             Hub: [[Hub]]\n\
             Plain: [[{t5}]]\n",
            t0 = title(0),
            t1 = title(1),
            t5 = title(5)
        );
        let uri = path_to_uri(&root().join("open.md"))
            .expect("a URI for the open note")
            .as_str()
            .to_string();
        Self { text, uri }
    }

    /// The position after `needle` on the line that starts with `line_start`.
    fn after(&self, line_start: &str, needle: &str) -> lsp::Position {
        let (line, text) = self
            .text
            .lines()
            .enumerate()
            .find(|(_, l)| l.starts_with(line_start))
            .expect("a line of the scene");
        let at = text.find(needle).expect("the needle in the line") + needle.len();
        lsp::Position::new(line as u32, text[..at].encode_utf16().count() as u32)
    }

    fn id(&self) -> lsp::TextDocumentIdentifier {
        lsp::TextDocumentIdentifier {
            uri: self.uri.parse().expect("a URI"),
        }
    }

    fn completion(&self, position: lsp::Position) -> lsp::CompletionParams {
        lsp::CompletionParams {
            text_document_position: lsp::TextDocumentPositionParams {
                text_document: self.id(),
                position,
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: None,
        }
    }

    fn highlight(&self, position: lsp::Position) -> lsp::DocumentHighlightParams {
        lsp::DocumentHighlightParams {
            text_document_position_params: lsp::TextDocumentPositionParams {
                text_document: self.id(),
                position,
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        }
    }

    fn references(&self, position: lsp::Position) -> lsp::ReferenceParams {
        lsp::ReferenceParams {
            text_document_position: lsp::TextDocumentPositionParams {
                text_document: self.id(),
                position,
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: lsp::ReferenceContext {
                include_declaration: true,
            },
        }
    }
}

fn state_for(docs: Vec<Document>, scenes: &Scenes) -> SatzState {
    let mut state = SatzState::default();
    state.index = Index::build(docs);
    state.set_vault_root(Some(root()));
    state.set_indexing_complete(true);
    state.open_docs.insert(
        scenes.uri.clone(),
        OpenDocument::new(&scenes.uri, root().join("open.md"), &scenes.text, 1),
    );
    state
}

fn feel(d: Duration) -> &'static str {
    if d <= Duration::from_millis(16) {
        "comfortable"
    } else if d <= Duration::from_millis(50) {
        "borderline"
    } else {
        "SLOW (over 50 ms)"
    }
}

fn completion_count(response: &Option<lsp::CompletionResponse>) -> (usize, bool) {
    match response {
        Some(lsp::CompletionResponse::Array(items)) => (items.len(), false),
        Some(lsp::CompletionResponse::List(list)) => (list.items.len(), list.is_incomplete),
        None => (0, false),
    }
}

fn symbol_count(response: &Option<lsp::WorkspaceSymbolResponse>) -> usize {
    match response {
        Some(lsp::WorkspaceSymbolResponse::Flat(symbols)) => symbols.len(),
        Some(lsp::WorkspaceSymbolResponse::Nested(symbols)) => symbols.len(),
        None => 0,
    }
}

// ---- the benchmark --------------------------------------------------------------------------

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let quick = args.iter().any(|a| a == "--quick");
    let check = args.iter().any(|a| a == "--check");
    let settings = Settings {
        rounds: if quick { 3 } else { 9 },
        shrink: if quick { 2 } else { 1 },
    };
    let mut flagged: Vec<String> = Vec::new();
    check_the_harness();

    println!("================ SATZ LANGUAGE SERVER BENCHMARK ================");
    if cfg!(debug_assertions) {
        println!("!!! DEBUG BUILD: these numbers mean nothing. Use `cargo bench`. !!!");
    }
    println!(
        "{} rounds{}; compare runs by `min`; {} cores",
        settings.rounds,
        if quick { " (--quick)" } else { "" },
        std::thread::available_parallelism().map_or(1, |n| n.get())
    );

    let scenes = Scenes::new();
    let total = settings.size(10_000);
    let started = Instant::now();
    let docs = vault_docs(total, &scenes.text);
    println!(
        "\nsynthetic vault: {total} notes + Hub + the open note, {:.1} MB of text, built in {:.1} s",
        docs.iter()
            .map(|d| d.line_index.source().len())
            .sum::<usize>() as f64
            / 1_048_576.0,
        started.elapsed().as_secs_f64()
    );
    let state = state_for(docs.clone(), &scenes);

    requests_on_every_key(&settings, &scenes, &state);
    requests_now_and_then(&settings, &scenes, &state);
    the_index(&settings, &docs);
    scaling(&settings, &scenes, &mut flagged);

    println!("\n============================================================");
    if flagged.is_empty() {
        println!("nothing flagged");
    } else {
        println!("FLAGGED ({}):", flagged.len());
        for line in &flagged {
            println!("  - {line}");
        }
        if check {
            std::process::exit(1);
        }
    }
}

/// What runs on every key or cursor move.
fn requests_on_every_key(settings: &Settings, scenes: &Scenes, state: &SatzState) {
    println!("\n--- requests made on every key or cursor move (the large vault) ---");
    let rounds = settings.rounds;

    // (what, where the cursor is, at least how many items a vault this size must give)
    let completions: &[(&str, lsp::Position, usize)] = &[
        (
            "completion: `[[` (every note)",
            scenes.after("Start:", "[["),
            100,
        ),
        ("completion: `[[fel`", scenes.after("Short:", "[[fel"), 20),
        (
            "completion: `[[felsefe yaz`",
            scenes.after("Two:", "[[felsefe yaz"),
            5,
        ),
        (
            "completion: heading, `[[<title>#`",
            scenes.after("Heading:", "#"),
            1,
        ),
        ("completion: tag, `#`", scenes.after("Tag all:", "#"), 100),
        (
            "completion: tag, `#pro`",
            scenes.after("Tag prefix:", "#pro"),
            5,
        ),
        (
            "completion: block, `[[<title>#^`",
            scenes.after("Block:", "#^"),
            1,
        ),
    ];
    for (what, position, at_least) in completions {
        let params = scenes.completion(*position);
        let (count, incomplete) = completion_count(&completion(params.clone(), state));
        assert!(
            count >= *at_least,
            "{what}: {count} items, expected at least {at_least}"
        );
        let sample = measure(rounds, || params.clone(), |p| completion(p, state));
        row(
            what,
            &sample,
            &format!(
                "{count} items{} - {}",
                if incomplete { " (cut)" } else { "" },
                feel(sample.median)
            ),
        );
    }

    for (what, query) in [
        ("workspace_symbol: empty query", ""),
        ("workspace_symbol: `f`", "f"),
        ("workspace_symbol: `felsefe`", "felsefe"),
        ("workspace_symbol: `felsefe yazılım`", "felsefe yazılım"),
        ("workspace_symbol: `tag:kavram`", "tag:kavram"),
    ] {
        let params = lsp::WorkspaceSymbolParams {
            query: query.to_string(),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };
        let count = symbol_count(&workspace_symbol(params.clone(), state));
        assert!(count > 0 && count <= 100, "{what}: {count} symbols");
        let sample = measure(rounds, || params.clone(), |p| workspace_symbol(p, state));
        row(
            what,
            &sample,
            &format!("{count} symbols - {}", feel(sample.median)),
        );
    }

    let highlights: &[(&str, lsp::Position)] = &[
        (
            "document_highlight: on a resolved link",
            scenes.after("Links:", "[["),
        ),
        (
            "document_highlight: on a broken link",
            scenes.after("Links:", "[[Olmayan"),
        ),
        (
            "document_highlight: on a tag",
            scenes.after("Links:", "#kav"),
        ),
        (
            "document_highlight: on a heading",
            scenes.after("## Highlight", "Highl"),
        ),
    ];
    for (what, position) in highlights {
        let params = scenes.highlight(*position);
        let found = document_highlight(params.clone(), state).map_or(0, |h| h.len());
        assert!(found > 0, "{what}: nothing highlighted");
        let sample = measure(rounds, || params.clone(), |p| document_highlight(p, state));
        row(
            what,
            &sample,
            &format!("{found} highlights - {}", feel(sample.median)),
        );
    }
}

/// What runs when the user asks for it, or when something changed.
fn requests_now_and_then(settings: &Settings, scenes: &Scenes, state: &SatzState) {
    println!("\n--- requests made now and then (the large vault) ---");
    let rounds = settings.rounds;

    for (what, position, at_least) in [
        (
            "references: the hub (every 20th note links to it)",
            scenes.after("Hub:", "[[Hub"),
            50,
        ),
        (
            "references: an ordinary note",
            scenes.after("Plain:", "[["),
            1,
        ),
    ] {
        let params = scenes.references(position);
        let found = find_references(params.clone(), state).map_or(0, |l| l.len());
        assert!(found >= at_least, "{what}: {found} locations");
        let sample = measure(rounds, || params.clone(), |p| find_references(p, state));
        row(
            what,
            &sample,
            &format!("{found} locations - {}", feel(sample.median)),
        );
    }

    let typical = state
        .index
        .get_doc(&satz_core::DocId::new(note_path(1234)))
        .expect("a note of the vault");
    let one = measure(
        rounds,
        || (),
        |()| compute_diagnostics(typical, &state.index, &state.config),
    );
    row("diagnostics: one ordinary note", &one, "");

    let previous = HashMap::new();
    let report = pull_workspace_report(&previous, state);
    assert_eq!(report.len(), state.index.doc_count());
    let whole = measure(
        rounds.min(5),
        || (),
        |()| pull_workspace_report(&previous, state),
    );
    row(
        &format!(
            "diagnostics: the whole vault, first pull ({} notes)",
            report.len()
        ),
        &whole,
        &format!(
            "{:.1} us/note",
            whole.min.as_secs_f64() * 1e6 / report.len() as f64
        ),
    );
    let known: HashMap<String, String> = report
        .iter()
        .filter_map(|item| match item {
            lsp::WorkspaceDocumentDiagnosticReport::Full(full) => Some((
                full.uri.as_str().to_string(),
                full.full_document_diagnostic_report.result_id.clone()?,
            )),
            lsp::WorkspaceDocumentDiagnosticReport::Unchanged(_) => None,
        })
        .collect();
    let again = pull_workspace_report(&known, state);
    assert!(
        again
            .iter()
            .all(|i| matches!(i, lsp::WorkspaceDocumentDiagnosticReport::Unchanged(_))),
        "with the ids of the last pull nothing is recomputed"
    );
    let unchanged = measure(
        rounds.min(5),
        || (),
        |()| pull_workspace_report(&known, state),
    );
    row(
        "diagnostics: the whole vault, nothing changed since",
        &unchanged,
        "",
    );

    let formatted = compute_format_changes(state);
    let cold = measure(rounds.min(5), || (), |()| compute_format_changes(state));
    row(
        "satz.formatWorkspace: every note, cold cache",
        &cold,
        &format!(
            "{} notes to change, {:.1} us/note",
            formatted.changes.len(),
            cold.min.as_secs_f64() * 1e6 / state.index.doc_count() as f64
        ),
    );
}

/// The index under edits (what the debounced reparse and the file watcher do to it).
fn the_index(settings: &Settings, docs: &[Document]) {
    println!("\n--- the index (the large vault) ---");
    let rounds = settings.rounds;
    let build = measure(rounds.min(5), || docs.to_vec(), Index::build);
    row(
        "Index::build (clone not timed)",
        &build,
        &format!(
            "{:.1} us/note",
            build.min.as_secs_f64() * 1e6 / docs.len() as f64
        ),
    );

    let mut index = Index::build(docs.to_vec());
    let path = PathBuf::from(note_path(1234));
    let typing = measure(
        rounds,
        {
            let mut counter = 0;
            move || {
                counter += 1;
                format!(
                    "{}\nmore words {counter}\n",
                    note_text(1234, docs.len(), &mut Rng(7))
                )
            }
        },
        |text| index.replace_doc(parse_document(&text, &path)),
    );
    row("replace_doc: ordinary typing (parse included)", &typing, "");

    let retitle = measure(
        rounds.min(5),
        {
            let mut counter = 0;
            move || {
                counter += 1;
                format!("# Yeni başlık {counter}\n\nA new title in the first heading.\n")
            }
        },
        |text| index.replace_doc(parse_document(&text, &path)),
    );
    row(
        "replace_doc: the title changes (parse included)",
        &retitle,
        &format!(
            "{:.0}x typing",
            retitle.min.as_secs_f64() / typing.min.as_secs_f64().max(1e-12)
        ),
    );

    let added = measure(
        rounds.min(5),
        {
            let mut counter = 0;
            move || {
                counter += 1;
                (
                    PathBuf::from(format!("new/note-{counter}.md")),
                    format!("# Brand new {counter}\n"),
                )
            }
        },
        |(path, text)| index.replace_doc(parse_document(&text, &path)),
    );
    row(
        "replace_doc: a note that is new (parse included)",
        &added,
        "",
    );

    let removed = measure(
        rounds.min(5),
        {
            let mut counter = 0;
            move || {
                counter += 1;
                counter
            }
        },
        |k: usize| index.remove_doc(&satz_core::DocId::new(note_path(k * 3 % docs.len()))),
    );
    row("remove_doc: one note", &removed, "");
}

/// Does the cost of one request grow in proportion to the vault?
fn scaling(settings: &Settings, scenes: &Scenes, flagged: &mut Vec<String>) {
    println!("\n--- does the cost grow in proportion to the size of the vault? ---");
    let sizes: Vec<usize> = [2500, 5000, 10_000]
        .iter()
        .map(|&n| settings.size(n))
        .collect();
    let mut states: Vec<SatzState> = sizes
        .iter()
        .map(|&n| state_for(vault_docs(n, &scenes.text), scenes))
        .collect();
    let notes: Vec<usize> = states.iter().map(|s| s.index.doc_count()).collect();
    let rounds = settings.rounds;

    let params = scenes.completion(scenes.after("Start:", "[["));
    scaling_family(
        "completion: `[[` (every note); bytes column = notes",
        &sizes,
        &notes,
        rounds,
        None,
        flagged,
        |i| {
            let p = params.clone();
            let started = Instant::now();
            black_box(completion(p, &states[i]));
            started.elapsed()
        },
    );
    let query = lsp::WorkspaceSymbolParams {
        query: String::new(),
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
    };
    scaling_family(
        "workspace_symbol: empty query; bytes column = notes",
        &sizes,
        &notes,
        rounds,
        None,
        flagged,
        |i| {
            let p = query.clone();
            let started = Instant::now();
            black_box(workspace_symbol(p, &states[i]));
            started.elapsed()
        },
    );
    let mut counter = 0;
    scaling_family(
        "replace_doc, the title changes (parse included); bytes column = notes",
        &sizes,
        &notes,
        rounds.min(5),
        None,
        flagged,
        |i| {
            counter += 1;
            let text = format!("# Yeni başlık {counter}\n\nA new title.\n");
            let started = Instant::now();
            states[i]
                .index
                .replace_doc(parse_document(&text, &PathBuf::from(note_path(1234))));
            started.elapsed()
        },
    );
    scaling_family(
        "diagnostics: the whole vault, first pull; bytes column = notes",
        &sizes,
        &notes,
        rounds.min(3),
        None,
        flagged,
        |i| {
            let none = HashMap::new();
            let started = Instant::now();
            black_box(pull_workspace_report(&none, &states[i]));
            started.elapsed()
        },
    );
}
