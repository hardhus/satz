//! What `satz.formatWorkspace` returns, checked against the formatter itself (5.2): whichever way the
//! vault is worked through, the changes are the notes that formatting changes, the edits of each
//! turn its text into what `format_document` makes of it, an open note is formatted from its live
//! buffer and carries its version, and what is remembered makes the next call cheaper without
//! changing its answer.

use std::path::{Path, PathBuf};

use ropey::Rope;
use satz_core::{Index, parse_document};

use crate::convert::{apply_text_edits, path_to_uri};
use crate::handlers::execute_command::{FormatWorkspaceResult, compute_format_changes};
use crate::state::{FormatCache, SatzState};

pub(crate) struct Rng(pub u64);

impl Rng {
    pub(crate) fn below(&mut self, n: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % n as u64) as usize
    }
    pub(crate) fn pick<'a>(&mut self, of: &[&'a str]) -> &'a str {
        of[self.below(of.len())]
    }
}

pub(crate) fn root() -> PathBuf {
    if cfg!(windows) {
        PathBuf::from("C:\\vault")
    } else {
        PathBuf::from("/vault")
    }
}

/// A note that formatting may or may not change: the things the formatter takes away or puts right
/// (trailing spaces, runs of blank lines, list markers, headings glued to text, line endings), and
/// things it leaves alone (frontmatter, code, tables).
pub(crate) fn note_text(rng: &mut Rng, i: usize, tidy: bool) -> String {
    let pieces = [
        "A paragraph of plain prose with a [[link]] in it.",
        "Trailing spaces here   ",
        "* item one\n* item two",
        "- item one\n- item two   ",
        "## A heading\nglued text",
        "```rust\nlet x   =  1;\n```",
        "| a | b |\n|---|---|\n| 1 | 2 |",
        "> a quote   \n> goes on",
        "1. first\n2. second",
        "Some **bold** and _italic_ words.",
    ];
    let mut text = String::new();
    if rng.below(5) == 0 {
        text.push_str(&format!("---\ntitle: Note {i}\ntags: [a, b]\n---\n"));
    }
    text.push_str(&format!("# Note {i}\n"));
    for _ in 0..1 + rng.below(6) {
        let gap = if rng.below(3) == 0 {
            "\n\n\n\n"
        } else {
            "\n\n"
        };
        text.push_str(gap);
        text.push_str(rng.pick(&pieces));
    }
    text.push('\n');
    if rng.below(6) == 0 {
        text = text.replace('\n', "\r\n");
    }
    if rng.below(12) == 0 {
        text.insert(0, '\u{feff}');
    }
    if tidy {
        satz_core::formatter::format_document(&text, &satz_core::VaultConfig::default().formatter)
    } else {
        text
    }
}

/// A vault of `n` notes (a third of them already formatted); `open` of them are open in the
/// editor, and half of those have a live buffer that is not what the index holds (the debounced
/// reparse has not caught up).
pub(crate) fn vault(rng: &mut Rng, n: usize, open: usize) -> SatzState {
    let mut state = SatzState::default();
    state.set_vault_root(Some(root()));
    let docs = (0..n)
        .map(|i| {
            let tidy = rng.below(3) == 0;
            parse_document(
                &note_text(rng, i, tidy),
                Path::new(&format!("dir{}/n{i}.md", i % 7)),
            )
        })
        .collect();
    state.index = Index::build(docs);
    state.format_cache = FormatCache::new(n * 2);
    for i in (0..n).step_by((n / open.max(1)).max(1)).take(open) {
        let path = root()
            .join(format!("dir{}", i % 7))
            .join(format!("n{i}.md"));
        let indexed = state
            .index
            .get_doc(&satz_core::DocId::new(format!("dir{}/n{i}.md", i % 7)))
            .map(|d| d.line_index.source().to_string())
            .unwrap();
        let uri = format!("file:///editor/n{i}.md");
        state.open_document(&uri, &indexed, &path, 3);
        if rng.below(2) == 0 {
            // The user typed since: the buffer is ahead of the index.
            let tidy = rng.below(2) == 0;
            let live = note_text(rng, i + 1000, tidy);
            let open = state.open_docs.get_mut(&uri).unwrap();
            open.rope = Rope::from_str(&live);
            open.version = 4 + rng.below(5) as i32;
        }
    }
    state
}

/// What formatting says about one note now: its text (the live buffer if it is open) and what
/// `format_document` makes of it.
pub(crate) struct Expected {
    pub(crate) uri: String,
    pub(crate) source: String,
    pub(crate) formatted: String,
    pub(crate) version: Option<i32>,
}

pub(crate) fn expected(state: &SatzState) -> Vec<Expected> {
    let mut all = Vec::new();
    for doc in state.index.documents() {
        let open = state.open_doc_for_path(&doc.path).map(|(_, o)| o);
        let source = match open {
            Some(open) => open.rope.to_string(),
            None => doc.line_index.source().to_string(),
        };
        let formatted = satz_core::formatter::format_document(&source, &state.config.formatter);
        let uri = match open {
            Some(open) => open.uri.clone(),
            None => path_to_uri(&root().join(&doc.path)).unwrap().to_string(),
        };
        all.push(Expected {
            uri,
            source,
            formatted,
            version: open.map(|o| o.version),
        });
    }
    all.sort_by(|a, b| a.uri.cmp(&b.uri));
    all
}

/// Checks a result against what the formatter says: which notes change, what their edits turn them
/// into, which version they carry, and that everything learned is a fact about a text.
/// Returns how many notes change.
pub(crate) fn check(state: &SatzState, result: &FormatWorkspaceResult, what: &str) -> usize {
    let wanted = expected(state);
    let mut changed: Vec<&Expected> = wanted.iter().filter(|e| e.formatted != e.source).collect();
    changed.sort_by(|a, b| a.uri.cmp(&b.uri));

    let mut got: Vec<&crate::handlers::execute_command::FormatChange> =
        result.changes.iter().collect();
    got.sort_by(|a, b| a.uri.as_str().cmp(b.uri.as_str()));
    assert_eq!(
        got.iter().map(|c| c.uri.as_str()).collect::<Vec<_>>(),
        changed.iter().map(|e| e.uri.as_str()).collect::<Vec<_>>(),
        "{what}: which notes change"
    );
    for (change, want) in got.iter().zip(&changed) {
        assert_eq!(
            apply_text_edits(&want.source, &change.edits),
            want.formatted,
            "{what}: the edits of {}",
            want.uri
        );
        assert_eq!(
            change.version, want.version,
            "{what}: version of {}",
            want.uri
        );
    }
    // What is remembered is true of the text it is about.
    for update in &result.cache_updates {
        match update {
            crate::state::CacheUpdate::Unchanged(hash) => {
                assert!(
                    wanted
                        .iter()
                        .any(|e| e.formatted == e.source
                            && satz_core::content_hash(&e.source) == *hash),
                    "{what}: an 'already formatted' text that is not"
                );
            }
            crate::state::CacheUpdate::Formatted(hash, text) => {
                assert!(
                    wanted.iter().any(
                        |e| satz_core::content_hash(&e.source) == *hash && &e.formatted == text
                    ),
                    "{what}: a formatted text that is not what formatting makes"
                );
            }
        }
    }
    got.len()
}

#[test]
fn what_the_workspace_format_returns_is_what_the_formatter_says() {
    let mut rng = Rng(0x5252_0000_0000_0001);
    let (mut changed_notes, mut open_notes, mut rounds) = (0, 0, 0);
    for round in 0..120 {
        let n = 1 + rng.below(40);
        let open = rng.below(6).min(n);
        let mut state = vault(&mut rng, n, open);
        open_notes += state.open_docs.len();

        // Cold cache.
        let cold = compute_format_changes(&state);
        changed_notes += check(&state, &cold, &format!("round {round}, cold"));

        // What was learned makes the next call do no formatting, and answers the same.
        let learned = cold.cache_updates.len();
        state.apply_format_cache_updates(cold.cache_updates);
        let warm = compute_format_changes(&state);
        check(&state, &warm, &format!("round {round}, warm"));
        assert_eq!(warm.changes.len(), cold.changes.len(), "round {round}");
        assert!(
            warm.cache_updates.is_empty(),
            "round {round}: {learned} learned, and still work to do"
        );
        rounds += 1;
    }
    assert!(
        changed_notes > 400 && open_notes > 100,
        "{changed_notes} changed, {open_notes} open ({rounds} rounds)"
    );
}

#[test]
fn a_cache_that_is_full_or_too_small_changes_nothing_in_the_answer() {
    let mut rng = Rng(0x5252_0000_0000_0002);
    for round in 0..40 {
        let n = 2 + rng.below(30);
        let open = rng.below(4).min(n);
        let mut state = vault(&mut rng, n, open);
        state.format_cache = FormatCache::new(rng.below(6));
        for call in 0..3 {
            let result = compute_format_changes(&state);
            check(&state, &result, &format!("round {round}, call {call}"));
            state.apply_format_cache_updates(result.cache_updates);
        }
    }
}

#[test]
fn a_disabled_formatter_or_an_unusable_configuration_formats_nothing() {
    let mut rng = Rng(0x5252_0000_0000_0003);
    let mut state = vault(&mut rng, 12, 2);
    assert!(!compute_format_changes(&state).changes.is_empty());

    state.config.formatter.enabled = false;
    let off = compute_format_changes(&state);
    assert!(off.changes.is_empty() && off.cache_updates.is_empty());

    state.config.formatter.enabled = true;
    state.config_error = Some("broken".to_string());
    let broken = compute_format_changes(&state);
    assert!(broken.changes.is_empty() && broken.cache_updates.is_empty());
}

#[test]
fn an_empty_vault_has_nothing_to_format() {
    let mut state = SatzState::default();
    state.set_vault_root(Some(root()));
    let result = compute_format_changes(&state);
    assert!(result.changes.is_empty() && result.cache_updates.is_empty());
}

// ---- the vault formatted off the state lock, a slice at a time ----

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use tokio::sync::RwLock;

use crate::handlers::execute_command::{
    FormatChange, Formatter, format_workspace, format_workspace_with,
};

/// What a result comes to, in a form that can be compared: the URIs in order, with the edits and the
/// version each carries.
fn shape(result: &FormatWorkspaceResult) -> Vec<String> {
    let mut all: Vec<String> = result
        .changes
        .iter()
        .map(|c: &FormatChange| format!("{} v{:?} {:?}", c.uri.as_str(), c.version, c.edits))
        .collect();
    all.sort();
    all
}

fn shared(state: SatzState) -> Arc<RwLock<SatzState>> {
    Arc::new(RwLock::new(state))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_vault_formatted_off_the_lock_is_what_the_formatter_says() {
    let mut rng = Rng(0x5252_0000_0000_0011);
    let (mut changed_notes, mut open_notes) = (0, 0);
    for round in 0..60 {
        let n = 1 + rng.below(60);
        let open = rng.below(6).min(n);
        let state = shared(vault(&mut rng, n, open));
        open_notes += state.read().await.open_docs.len();

        let cold = format_workspace(&state).await;
        changed_notes += check(&*state.read().await, &cold, &format!("round {round}, cold"));
        // The same notes change as when the vault is worked through under the lock.
        assert_eq!(
            shape(&cold),
            shape(&compute_format_changes(&*state.read().await)),
            "round {round}"
        );
        // The changes come in the order of their URIs.
        let uris: Vec<&str> = cold.changes.iter().map(|c| c.uri.as_str()).collect();
        assert!(
            uris.windows(2).all(|w| w[0] <= w[1]),
            "round {round}: {uris:?}"
        );

        let learned = cold.cache_updates.len();
        let changes = cold.changes.len();
        state
            .write()
            .await
            .apply_format_cache_updates(cold.cache_updates);
        let warm = format_workspace(&state).await;
        check(&*state.read().await, &warm, &format!("round {round}, warm"));
        assert_eq!(warm.changes.len(), changes, "round {round}");
        assert!(
            warm.cache_updates.is_empty(),
            "round {round}: {learned} learned, and still work to do"
        );
    }
    assert!(
        changed_notes > 400 && open_notes > 60,
        "{changed_notes} changed, {open_notes} open"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_size_of_a_slice_does_not_change_the_answer() {
    let mut rng = Rng(0x5252_0000_0000_0012);
    for round in 0..12 {
        let n = 5 + rng.below(40);
        let open = rng.below(5).min(n);
        let state = shared(vault(&mut rng, n, open));
        let want = shape(&compute_format_changes(&*state.read().await));
        let learned = compute_format_changes(&*state.read().await)
            .cache_updates
            .len();
        for chunk in [0, 1, 2, 3, 7, n - 1, n, n + 1, 512] {
            let formatter: Arc<Formatter> = Arc::new(satz_core::formatter::format_document);
            let result = format_workspace_with(&state, chunk, formatter).await;
            assert_eq!(
                shape(&result),
                want,
                "round {round}, slices of {chunk} of {n}"
            );
            // Every text that was formatted is learned once.
            assert_eq!(
                result.cache_updates.len(),
                learned,
                "round {round}, slices of {chunk}"
            );
        }
    }
}

/// `format_workspace_with`, formatting with `format_document` but first (once) doing `during` to
/// the state, as an edit typed while the vault is being formatted would.
async fn format_while(
    state: &Arc<RwLock<SatzState>>,
    chunk: usize,
    during: impl Fn(&mut SatzState) + Send + Sync + 'static,
) -> FormatWorkspaceResult {
    let fired = AtomicBool::new(false);
    let probe = state.clone();
    let formatter: Arc<Formatter> = Arc::new(move |text, config| {
        if !fired.swap(true, Ordering::SeqCst) {
            let mut state = probe
                .try_write()
                .expect("the state must not be held while formatting");
            during(&mut state);
        }
        satz_core::formatter::format_document(text, config)
    });
    format_workspace_with(state, chunk, formatter).await
}

/// Six dirty notes, `n0`..`n5`; `n1` and `n2` are open (their buffers are what the index holds).
fn six_notes() -> Arc<RwLock<SatzState>> {
    shared(six_notes_state())
}

fn six_notes_state() -> SatzState {
    let mut state = SatzState::default();
    state.set_vault_root(Some(root()));
    let dirty = |i: usize| format!("# Note {i}\n\n\n\nsome text   \n");
    state.index = Index::build(
        (0..6)
            .map(|i| parse_document(&dirty(i), Path::new(&format!("n{i}.md"))))
            .collect(),
    );
    state.format_cache = FormatCache::new(100);
    for i in [1, 2] {
        state.open_document(
            &format!("file:///editor/n{i}.md"),
            &dirty(i),
            &root().join(format!("n{i}.md")),
            7,
        );
    }
    state
}

fn names(result: &FormatWorkspaceResult) -> Vec<String> {
    let mut all: Vec<String> = result
        .changes
        .iter()
        .map(|c| {
            let uri = c.uri.as_str();
            uri.rsplit('/').next().unwrap_or(uri).to_string()
        })
        .collect();
    all.sort();
    all
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_state_is_not_held_while_the_notes_are_formatted() {
    let state = six_notes();
    let calls = Arc::new(AtomicUsize::new(0));
    let free = Arc::new(AtomicBool::new(true));
    let one_at_a_time = Arc::new(std::sync::Mutex::new(()));
    let (probe, seen, all_free) = (state.clone(), calls.clone(), free.clone());
    let formatter: Arc<Formatter> = Arc::new(move |text, config| {
        seen.fetch_add(1, Ordering::SeqCst);
        // Nobody reads it, nobody writes it: a request or an edit would get in. (The notes are
        // formatted on several threads at once; one probe at a time, or they would hold the state
        // against each other.)
        let _turn = one_at_a_time.lock().unwrap();
        let writable = probe.try_write().is_ok();
        let readable = probe.try_read().is_ok();
        if !(writable && readable) {
            all_free.store(false, Ordering::SeqCst);
        }
        satz_core::formatter::format_document(text, config)
    });
    for chunk in [1, 2, 100] {
        calls.store(0, Ordering::SeqCst);
        let result = format_workspace_with(&state, chunk, formatter.clone()).await;
        assert_eq!(result.changes.len(), 6, "slices of {chunk}");
        assert_eq!(calls.load(Ordering::SeqCst), 6, "slices of {chunk}");
    }
    assert!(
        free.load(Ordering::SeqCst),
        "the state was held while a note was formatted"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_note_typed_in_while_the_vault_is_formatted_is_left_out_and_the_rest_is_not() {
    let state = six_notes();
    let result = format_while(&state, 100, |state| {
        let open = state.open_docs.get_mut("file:///editor/n1.md").unwrap();
        open.rope = Rope::from_str("# Note 1\n\n\n\ntyped since   \n");
        open.version = 8;
    })
    .await;
    assert_eq!(
        names(&result),
        vec!["n0.md", "n2.md", "n3.md", "n4.md", "n5.md"]
    );
    // The edits that are left are for the version they were computed against.
    let n2 = result
        .changes
        .iter()
        .find(|c| c.uri.as_str().ends_with("n2.md"))
        .unwrap();
    assert_eq!(n2.version, Some(7));
    // What was learned about the text it was computed from is still true, and kept.
    let old = satz_core::content_hash("# Note 1\n\n\n\nsome text   \n");
    assert!(
        result
            .cache_updates
            .iter()
            .any(|u| matches!(u, crate::state::CacheUpdate::Formatted(h, _) if *h == old)),
        "{:?}",
        result.cache_updates
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_note_that_is_closed_changed_removed_or_opened_meanwhile_is_left_out() {
    type Change = fn(&mut SatzState);
    let cases: [(&str, &str, Change); 4] = [
        ("closed", "n1.md", |s| {
            s.open_docs.remove("file:///editor/n1.md");
        }),
        ("changed on disk", "n3.md", |s| {
            s.index.replace_doc(parse_document(
                "# Note 3\n\n\n\nchanged   \n",
                Path::new("n3.md"),
            ));
        }),
        ("removed", "n4.md", |s| {
            s.index.remove_doc(&satz_core::DocId::new("n4.md"))
        }),
        ("opened", "n5.md", |s| {
            s.open_document(
                "file:///editor/n5.md",
                "# Note 5\n\n\n\nsome text   \n",
                &root().join("n5.md"),
                1,
            );
        }),
    ];
    for (what, left_out, during) in cases {
        let state = six_notes();
        let result = format_while(&state, 100, during).await;
        let got = names(&result);
        assert!(!got.contains(&left_out.to_string()), "{what}: {got:?}");
        // Nothing else is lost: the other five are formatted.
        assert_eq!(got.len(), 5, "{what}: {got:?}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_change_that_touches_no_note_leaves_every_change_in() {
    let state = six_notes();
    let result = format_while(&state, 100, |state| {
        state
            .index
            .replace_doc(parse_document("# Other\n", Path::new("other.md")));
        state.config.hover.preview_lines = 9;
    })
    .await;
    assert_eq!(result.changes.len(), 6);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_disabled_formatter_an_unusable_configuration_or_no_notes_format_nothing_off_the_lock() {
    let state = six_notes();
    state.write().await.config.formatter.enabled = false;
    let off = format_workspace(&state).await;
    assert!(off.changes.is_empty() && off.cache_updates.is_empty());

    let state = six_notes();
    state.write().await.config_error = Some("broken".to_string());
    let broken = format_workspace(&state).await;
    assert!(broken.changes.is_empty() && broken.cache_updates.is_empty());

    let mut empty = SatzState::default();
    empty.set_vault_root(Some(root()));
    let none = format_workspace(&shared(empty)).await;
    assert!(none.changes.is_empty() && none.cache_updates.is_empty());
}

// ---- the command, end to end ----

use crate::backend::tests::{connected_backend_io, read_frame, write_frame};
use tower_lsp_server::LanguageServer;
use tower_lsp_server::ls_types::ExecuteCommandParams;

/// A client that answers `workspace/applyEdit` with `applied` and keeps what it was asked to apply.
fn client_that_applies(
    mut from_server: tokio::io::BufReader<tokio::io::ReadHalf<tokio::io::DuplexStream>>,
    mut to_server: tokio::io::WriteHalf<tokio::io::DuplexStream>,
    applied: bool,
) -> Arc<std::sync::Mutex<Vec<serde_json::Value>>> {
    let asked: Arc<std::sync::Mutex<Vec<serde_json::Value>>> = Arc::default();
    let seen = asked.clone();
    tokio::spawn(async move {
        while let Some(frame) = read_frame(&mut from_server).await {
            let (Some(method), Some(id)) = (frame["method"].as_str(), frame.get("id")) else {
                continue;
            };
            let result = if method == "workspace/applyEdit" {
                seen.lock().unwrap().push(frame["params"].clone());
                serde_json::json!({"applied": applied})
            } else {
                serde_json::Value::Null
            };
            write_frame(
                &mut to_server,
                serde_json::json!({"jsonrpc": "2.0", "id": id, "result": result}),
            )
            .await;
        }
    });
    asked
}

async fn run_format_command(
    dirty: bool,
    applied: bool,
) -> (Option<serde_json::Value>, Vec<serde_json::Value>) {
    let (backend, from_server, to_server) = connected_backend_io().await;
    let asked = client_that_applies(from_server, to_server, applied);
    {
        let mut fresh = six_notes_state();
        if !dirty {
            // Everything formatted as it is now.
            let docs: Vec<satz_core::Document> = fresh
                .index
                .documents()
                .map(|d| {
                    let text = satz_core::formatter::format_document(
                        d.line_index.source(),
                        &fresh.config.formatter,
                    );
                    parse_document(&text, &d.path)
                })
                .collect();
            fresh.index = Index::build(docs);
            fresh.open_docs.clear();
        }
        fresh.client_supports_document_changes = true;
        fresh.set_indexing_complete(true);
        *backend.state.write().await = fresh;
    }
    let answer = backend
        .execute_command(ExecuteCommandParams {
            command: "satz.formatWorkspace".to_string(),
            arguments: Vec::new(),
            work_done_progress_params: Default::default(),
        })
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let asked = asked.lock().unwrap().clone();
    (answer, asked)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_command_sends_the_edits_and_says_how_many_notes_it_formatted() {
    let (answer, asked) = run_format_command(true, true).await;
    let formatted = answer.unwrap()["formatted"].as_u64().unwrap();
    assert!(formatted > 0);
    assert_eq!(asked.len(), 1, "one applyEdit");
    let edits = asked[0]["edit"]["documentChanges"]
        .as_array()
        .expect("versioned edits");
    assert_eq!(edits.len() as u64, formatted);
    // An open note names the version it was computed against; a file on disk names none.
    assert!(
        edits
            .iter()
            .any(|e| e["textDocument"]["version"].is_number())
    );
    assert!(edits.iter().any(|e| e["textDocument"]["version"].is_null()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_vault_that_is_formatted_already_sends_nothing() {
    let (answer, asked) = run_format_command(false, true).await;
    assert_eq!(answer.unwrap()["formatted"].as_u64(), Some(0));
    assert!(asked.is_empty(), "{asked:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_client_that_does_not_apply_the_edit_is_told_nothing_was_formatted() {
    let (answer, asked) = run_format_command(true, false).await;
    assert_eq!(answer.unwrap()["formatted"].as_u64(), Some(0));
    assert_eq!(asked.len(), 1);
}
