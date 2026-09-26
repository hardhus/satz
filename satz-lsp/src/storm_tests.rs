//! What a burst of file events does to the index and to the client (5.1): whatever way the events
//! are applied, the index ends up as the disk says, open notes stay the editor's, an event that
//! comes before the first indexing is done is not lost, and the client is told what it needs to
//! be told.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use satz_core::{DocId, Document, GitignoreMode, Index, LinkResolution};
use tokio::sync::{RwLock, mpsc};
use tower_lsp_server::Client;

use crate::backend::Backend;
use crate::backend::tests::{connected_backend, count_of, sent_meanwhile};
use crate::state::SatzState;
use crate::watcher::{process_batch, run_debounce_loop};

// ---- how the events of one debounce window are applied (the one place that changes in 5.1) ----

/// Applies the paths that came out of one debounce window. The paths to try again (the first
/// indexing was not finished) come back.
pub(crate) async fn run_ready_paths(
    paths: &[PathBuf],
    vault: &Path,
    state: &Arc<RwLock<SatzState>>,
    client: &Client,
) -> Vec<PathBuf> {
    process_batch(paths, vault, state, client).await
}

/// How many times the client is asked to fetch diagnostics again for a window in which `changed`
/// events changed the index (or the configuration).
fn refreshes_for(changed: usize) -> usize {
    usize::from(changed > 0)
}

/// How many `publishDiagnostics` each open note is sent for such a window.
fn publishes_for(changed: usize) -> usize {
    usize::from(changed > 0)
}

// ---- small helpers ----

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn chance(&mut self, num: usize, den: usize) -> bool {
        self.below(den) < num
    }
    fn pick<'a>(&mut self, of: &[&'a str]) -> &'a str {
        of[self.below(of.len())]
    }
}

/// A folder that is removed with the test.
pub(crate) struct Dir(pub(crate) PathBuf);

impl Dir {
    pub(crate) fn new(tag: &str) -> Self {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "satz-storm-{}-{}-{}",
            std::process::id(),
            tag,
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
    /// The path of `rel` (`/`-separated) the way the OS spells it, which is how it is reported.
    pub(crate) fn path(&self, rel: &str) -> PathBuf {
        rel.split('/')
            .fold(self.0.clone(), |path, part| path.join(part))
    }
    pub(crate) fn write(&self, rel: &str, text: &str) -> PathBuf {
        let path = self.path(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, text).unwrap();
        path
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A state whose index is what the folder holds now.
pub(crate) fn state_of(dir: &Path) -> SatzState {
    let mut state = SatzState::default();
    state.set_vault_root(Some(dir.to_path_buf()));
    state.index = Index::build(satz_core::walk_vault_with(dir, GitignoreMode::default()).unwrap());
    state.set_indexing_complete(true);
    state
}

/// Everything the index answers, in a form that can be compared: the notes, what each link
/// resolves to, who links to whom, the tags, the orphans, the counts.
pub(crate) fn fingerprint(index: &Index) -> String {
    let mut docs: Vec<&Document> = index.documents().collect();
    docs.sort_by(|a, b| a.id.as_str().cmp(b.id.as_str()));
    let mut lines = Vec::new();
    for doc in docs {
        lines.push(format!(
            "doc {} title={:?} hash={} path={:?} tags={:?}",
            doc.id, doc.title, doc.content_hash, doc.path, doc.tags
        ));
        for link in &doc.links {
            let resolved = match index.resolve_link_full(link, Some(doc)) {
                LinkResolution::Resolved { doc, anchor } => {
                    format!("ok {} {:?}", doc.id, anchor)
                }
                LinkResolution::AnchorMissing { doc } => format!("anchor missing in {}", doc.id),
                LinkResolution::DocMissing => "missing".to_string(),
            };
            lines.push(format!("  link {:?} -> {resolved}", link.target_doc));
        }
        let mut back: Vec<&str> = index.backlinks_of(&doc.id).map(DocId::as_str).collect();
        back.sort();
        lines.push(format!("  backlinks {back:?}"));
    }
    let mut tags = index.all_tags();
    tags.sort();
    for tag in tags {
        let mut with: Vec<&str> = index.docs_with_tag(tag).map(|d| d.id.as_str()).collect();
        with.sort();
        lines.push(format!("tag {tag:?} {with:?}"));
    }
    let mut orphans: Vec<&str> = index.orphan_docs().map(|d| d.id.as_str()).collect();
    orphans.sort();
    lines.push(format!("orphans {orphans:?}"));
    lines.push(format!(
        "counts {} {} {}",
        index.doc_count(),
        index.total_links(),
        index.broken_link_count()
    ));
    lines.push(format!("{:?}", index.stats()));
    lines.join("\n")
}

pub(crate) fn ids(state: &SatzState) -> Vec<String> {
    let mut ids: Vec<String> = state
        .index
        .documents()
        .map(|d| d.id.as_str().to_string())
        .collect();
    ids.sort();
    ids
}

fn title_of(state: &SatzState, id: &str) -> Option<String> {
    state
        .index
        .get_doc(&DocId::new(id))
        .map(|d| d.title.clone())
}

/// Waits (for real, whatever the clock does) until `done` holds: the blocking work a paused clock
/// does not wait for.
async fn until(what: &str, mut done: impl FnMut() -> bool) {
    for _ in 0..2000 {
        if done() {
            return;
        }
        std::thread::sleep(Duration::from_millis(2));
        tokio::task::yield_now().await;
    }
    panic!("timed out waiting for {what}");
}

/// Lets what is already running finish (a paused clock jumps ahead without waiting for the
/// blocking threads).
pub(crate) async fn settle() {
    for _ in 0..5 {
        std::thread::sleep(Duration::from_millis(15));
        tokio::task::yield_now().await;
    }
}

/// A backend for a vault in `dir` whose open notes are `open` (file names inside it), for a client
/// that pushes diagnostics or one that pulls them.
pub(crate) async fn backend_for(
    dir: &Path,
    open: &[&str],
    pull: bool,
) -> (
    Arc<Backend>,
    tokio::io::BufReader<tokio::io::ReadHalf<tokio::io::DuplexStream>>,
) {
    let (backend, mut from_server) = connected_backend().await;
    {
        let mut state = backend.state.write().await;
        state.open_docs.clear();
        state.index = Index::default();
        state.set_vault_root(Some(dir.to_path_buf()));
        state.set_indexing_complete(true);
        state.client_supports_pull_diagnostics = pull;
        for name in open {
            let text =
                std::fs::read_to_string(dir.join(name)).unwrap_or_else(|_| format!("# {name}\n"));
            state.open_document(&format!("file:///vault/{name}"), &text, &dir.join(name), 1);
        }
    }
    // What the handshake and the opening sent is not what the tests look at.
    let _ = sent_meanwhile(&mut from_server, Duration::from_millis(300)).await;
    (backend, from_server)
}

pub(crate) fn uri_of(name: &str) -> String {
    format!("file:///vault/{name}")
}

// ---- C1: whatever the events, the index ends up as the disk says ----

const NAMES: [&str; 8] = [
    "alpha", "Beta", "gamma", "Delta", "épsilon", "zeta", "Eta", "theta",
];
const TAGS: [&str; 4] = ["one", "two", "Three", "drei"];
const FOLDERS: [&str; 4] = ["", "sub", "sub/deep", "Other"];

fn note_text(rng: &mut Rng) -> String {
    let mut text = String::new();
    if rng.chance(1, 4) {
        text.push_str(&format!(
            "---\naliases: [{}]\ntags: [{}]\n---\n",
            rng.pick(&NAMES),
            rng.pick(&TAGS)
        ));
    }
    text.push_str(&format!("# {}\n\n", rng.pick(&NAMES)));
    for _ in 0..rng.below(4) {
        let target = rng.pick(&NAMES);
        text.push_str(&format!(
            "see [[{target}]] and [[{}#H]] #{}\n",
            rng.pick(&["sub/alpha", "e\u{301}psilon", "nowhere", "Beta"]),
            rng.pick(&TAGS)
        ));
    }
    text
}

fn note_path(rng: &mut Rng) -> String {
    let folder = rng.pick(&FOLDERS);
    let name = rng.pick(&NAMES);
    if folder.is_empty() {
        format!("{name}.md")
    } else {
        format!("{folder}/{name}.md")
    }
}

/// The `.md` files under `dir` that can be reported (relative, `/`-separated).
fn notes_under(dir: &Path) -> Vec<String> {
    let mut found = Vec::new();
    fn visit(dir: &Path, root: &Path, found: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            // What the file system thread never reports: inside a hidden folder.
            if entry.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            if path.is_dir() {
                visit(&path, root, found);
            } else if path.extension().is_some_and(|e| e == "md") {
                let rel = path.strip_prefix(root).unwrap();
                found.push(rel.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    visit(dir, dir, &mut found);
    found.sort();
    found
}

/// Changes the folder the way a file system does when a tool works on it, and returns the paths
/// the OS would report (a folder that appears, moves or disappears is reported by its own path).
///
/// A folder that went away (deleted, or renamed) is not made again in the same burst: the watcher
/// takes a folder event for a folder the index holds notes of as "nothing new" (see
/// `folder_event_is_news`), so the notes that left with the old folder would stay indexed. A tool
/// that puts notes into a folder reports them one by one, so nor is a folder that is already
/// there "moved in".
fn storm(dir: &Dir, rng: &mut Rng) -> BTreeSet<PathBuf> {
    let mut events = BTreeSet::new();
    let mut retired: Vec<String> = Vec::new();
    let usable = |retired: &[String], rel: &str| {
        !retired
            .iter()
            .any(|r| rel == r || rel.starts_with(&format!("{r}/")))
    };
    for _ in 0..3 + rng.below(10) {
        let existing = notes_under(&dir.0);
        match rng.below(9) {
            // a note changes
            0 | 1 if !existing.is_empty() => {
                let rel = existing[rng.below(existing.len())].clone();
                events.insert(dir.write(&rel, &note_text(rng)));
            }
            // a note is deleted
            2 if !existing.is_empty() => {
                let rel = existing[rng.below(existing.len())].clone();
                std::fs::remove_file(dir.path(&rel)).unwrap();
                events.insert(dir.path(&rel));
            }
            // a note appears
            3 => {
                let rel = note_path(rng);
                if usable(&retired, &rel) {
                    events.insert(dir.write(&rel, &note_text(rng)));
                }
            }
            // a folder with notes is moved in (only its own path is reported)
            4 => {
                let folder = format!("moved{}", rng.below(3));
                if dir.path(&folder).exists() || !usable(&retired, &folder) {
                    continue;
                }
                for i in 0..1 + rng.below(4) {
                    dir.write(
                        &format!("{folder}/{}{i}.md", rng.pick(&NAMES)),
                        &note_text(rng),
                    );
                }
                if rng.chance(1, 2) {
                    dir.write(&format!("{folder}/.git/hidden.md"), "# hidden\n");
                }
                events.insert(dir.path(&folder));
            }
            // a folder is deleted (only its own path is reported)
            5 => {
                let folder = rng.pick(&["sub", "Other", "moved0", "sub/deep"]);
                if dir.path(folder).is_dir() {
                    std::fs::remove_dir_all(dir.path(folder)).unwrap();
                    events.insert(dir.path(folder));
                    retired.push(folder.to_string());
                }
            }
            // a folder is renamed: the old and the new path are reported
            6 => {
                let (from, to) = (rng.pick(&["sub", "Other"]), rng.pick(&["renamed", "sub2"]));
                if dir.path(from).is_dir() && !dir.path(to).exists() && usable(&retired, to) {
                    std::fs::rename(dir.path(from), dir.path(to)).unwrap();
                    events.insert(dir.path(from));
                    events.insert(dir.path(to));
                    retired.push(from.to_string());
                }
            }
            // something that is not a note, and a path that is not there at all
            7 => {
                events.insert(dir.write("picture.png", "not a note"));
                events.insert(dir.path("never/existed.md"));
            }
            // a folder that is already known is reported (saving a note does that on some systems)
            _ => {
                if dir.path("sub").is_dir() {
                    events.insert(dir.path("sub"));
                }
            }
        }
    }
    events
}

#[tokio::test]
async fn after_any_burst_of_events_the_index_is_what_the_disk_says() {
    let (backend, _from_server) = connected_backend().await;
    let mut rng = Rng(0x5EED_0051_D00D_F00D);
    let (mut rounds_with_folder_events, mut changed_rounds) = (0, 0);
    for round in 0..150 {
        let dir = Dir::new("c1");
        for _ in 0..6 + rng.below(9) {
            dir.write(&note_path(&mut rng), &note_text(&mut rng));
        }
        *backend.state.write().await = state_of(&dir.0);

        let events: Vec<PathBuf> = storm(&dir, &mut rng).into_iter().collect();
        rounds_with_folder_events += usize::from(events.iter().any(|p| p.is_dir()));
        let again = run_ready_paths(&events, &dir.0, &backend.state, &backend.client).await;
        assert!(again.is_empty(), "round {round}");

        let expected =
            Index::build(satz_core::walk_vault_with(&dir.0, GitignoreMode::default()).unwrap());
        let state = backend.state.read().await;
        changed_rounds += usize::from(!events.is_empty());
        let (got, want) = (fingerprint(&state.index), fingerprint(&expected));
        if got != want {
            let (got, want): (Vec<&str>, Vec<&str>) =
                (got.lines().collect(), want.lines().collect());
            let only_got: Vec<&&str> = got.iter().filter(|l| !want.contains(l)).collect();
            let only_want: Vec<&&str> = want.iter().filter(|l| !got.contains(l)).collect();
            panic!(
                "round {round}: events {events:?}
only in the index: {only_got:#?}
only on the disk: {only_want:#?}"
            );
        }
    }
    assert!(
        changed_rounds > 120 && rounds_with_folder_events > 50,
        "{changed_rounds} rounds with events, {rounds_with_folder_events} with a folder event"
    );
}

// ---- C2: an open note belongs to the editor ----

#[tokio::test]
async fn an_open_note_is_not_overwritten_by_its_file_and_stays_when_the_file_goes() {
    let dir = Dir::new("c2-open");
    dir.write("a.md", "# Disk\n");
    dir.write("b.md", "# B\n");
    let (backend, mut from_server) = backend_for(&dir.0, &[], false).await;
    {
        let mut state = backend.state.write().await;
        state.index =
            Index::build(satz_core::walk_vault_with(&dir.0, GitignoreMode::default()).unwrap());
        state.open_document(&uri_of("a.md"), "# Buffer\n", &dir.path("a.md"), 1);
    }
    let _ = sent_meanwhile(&mut from_server, Duration::from_millis(300)).await;

    // The file changes on disk, next to a closed note that changes too.
    dir.write("a.md", "# Changed on disk\n");
    dir.write("b.md", "# B changed\n");
    let events = [dir.path("a.md"), dir.path("b.md")];
    run_ready_paths(&events, &dir.0, &backend.state, &backend.client).await;
    {
        let state = backend.state.read().await;
        assert_eq!(title_of(&state, "a.md").as_deref(), Some("Buffer"));
        assert_eq!(title_of(&state, "b.md").as_deref(), Some("B changed"));
    }

    // The file is deleted: the note stays, the closed one goes.
    std::fs::remove_file(dir.path("a.md")).unwrap();
    std::fs::remove_file(dir.path("b.md")).unwrap();
    run_ready_paths(&events, &dir.0, &backend.state, &backend.client).await;
    let state = backend.state.read().await;
    assert_eq!(ids(&state), vec!["a.md"]);
    assert_eq!(title_of(&state, "a.md").as_deref(), Some("Buffer"));
}

#[tokio::test]
async fn a_folder_that_goes_takes_its_closed_notes_and_leaves_the_open_ones() {
    let dir = Dir::new("c2-folder");
    for name in [
        "keep.md",
        "sub/open.md",
        "sub/closed.md",
        "sub/deep/inner.md",
    ] {
        dir.write(name, &format!("# {name}\n"));
    }
    let (backend, mut from_server) = backend_for(&dir.0, &[], false).await;
    {
        let mut state = backend.state.write().await;
        state.index =
            Index::build(satz_core::walk_vault_with(&dir.0, GitignoreMode::default()).unwrap());
        state.open_document(
            &uri_of("sub/open.md"),
            "# open buffer\n",
            &dir.path("sub/open.md"),
            1,
        );
    }
    let _ = sent_meanwhile(&mut from_server, Duration::from_millis(300)).await;

    std::fs::remove_dir_all(dir.path("sub")).unwrap();
    run_ready_paths(&[dir.path("sub")], &dir.0, &backend.state, &backend.client).await;
    let state = backend.state.read().await;
    assert_eq!(ids(&state), vec!["keep.md", "sub/open.md"]);
    assert_eq!(
        title_of(&state, "sub/open.md").as_deref(),
        Some("open buffer")
    );
}

#[tokio::test]
async fn an_event_path_spelled_with_another_case_or_separator_still_finds_the_open_note() {
    let dir = Dir::new("c2-spelling");
    dir.write("Sub/a.md", "# Disk\n");
    let (backend, mut from_server) = backend_for(&dir.0, &[], false).await;
    {
        let mut state = backend.state.write().await;
        state.index = Index::default();
        state.open_document(&uri_of("Sub/a.md"), "# Buffer\n", &dir.path("Sub/a.md"), 1);
    }
    let _ = sent_meanwhile(&mut from_server, Duration::from_millis(300)).await;

    let respelled = [
        dir.0.join("sub").join("A.md"),
        PathBuf::from(dir.0.to_string_lossy().replace('\\', "/")).join("Sub/a.md"),
    ];
    run_ready_paths(&respelled, &dir.0, &backend.state, &backend.client).await;
    let state = backend.state.read().await;
    assert_eq!(ids(&state), vec!["Sub/a.md"]);
    assert_eq!(title_of(&state, "Sub/a.md").as_deref(), Some("Buffer"));
}

// ---- C4: what the client is sent ----

/// What the client is sent for a window in which `k` new notes appear, with the open notes
/// `open`: `(publishDiagnostics for the open notes, workspace/diagnostic/refresh)`.
async fn sent_for_new_notes(k: usize, open: &[&str], pull: bool) -> (Vec<usize>, usize) {
    let dir = Dir::new("c4");
    for name in open {
        dir.write(name, &format!("# {name}\n\n[[n0]]\n"));
    }
    let (backend, mut from_server) = backend_for(&dir.0, open, pull).await;
    let events: Vec<PathBuf> = (0..k)
        .map(|i| dir.write(&format!("n{i}.md"), &format!("# N{i}\n")))
        .collect();
    let again = run_ready_paths(&events, &dir.0, &backend.state, &backend.client).await;
    assert!(again.is_empty());
    let sent = sent_meanwhile(&mut from_server, Duration::from_millis(500)).await;
    let published = open
        .iter()
        .map(|name| {
            count_of(
                &sent,
                "textDocument/publishDiagnostics",
                Some(&uri_of(name)),
            )
        })
        .collect();
    (
        published,
        count_of(&sent, "workspace/diagnostic/refresh", None),
    )
}

#[tokio::test(start_paused = true)]
async fn a_push_client_is_sent_the_open_notes_for_a_window_of_new_notes() {
    let open = ["a.md", "b.md", "c.md"];
    for k in [1, 7, 40] {
        let (published, refreshes) = sent_for_new_notes(k, &open, false).await;
        assert_eq!(published, vec![publishes_for(k); 3], "k = {k}");
        assert_eq!(refreshes, 0, "a push client is not asked to pull");
    }
}

#[tokio::test(start_paused = true)]
async fn a_pull_client_is_asked_to_fetch_again_for_a_window_of_new_notes() {
    let open = ["a.md", "b.md", "c.md"];
    for k in [1, 7, 40] {
        let (published, refreshes) = sent_for_new_notes(k, &open, true).await;
        assert_eq!(
            published,
            vec![0; 3],
            "a pull client is not sent diagnostics"
        );
        assert_eq!(refreshes, refreshes_for(k), "k = {k}");
    }
}

#[tokio::test(start_paused = true)]
async fn events_that_change_nothing_send_nothing() {
    for pull in [false, true] {
        let dir = Dir::new("c4-nothing");
        dir.write("a.md", "# A\n");
        dir.write("sub/b.md", "# B\n");
        let (backend, mut from_server) = backend_for(&dir.0, &["a.md"], pull).await;
        {
            let mut state = backend.state.write().await;
            state.index =
                Index::build(satz_core::walk_vault_with(&dir.0, GitignoreMode::default()).unwrap());
            state.open_document(&uri_of("a.md"), "# A\n", &dir.path("a.md"), 1);
        }
        let _ = sent_meanwhile(&mut from_server, Duration::from_millis(300)).await;

        let events = [
            dir.write("picture.png", "x"),
            dir.path("never/existed.txt"),
            dir.path("sub"),
            dir.path("a.md"),
        ];
        run_ready_paths(&events, &dir.0, &backend.state, &backend.client).await;
        let sent = sent_meanwhile(&mut from_server, Duration::from_millis(500)).await;
        assert!(sent.is_empty(), "pull = {pull}: {sent:?}");
    }
}

/// The window's worth of publishes for a note that is gone but was never in the index.
fn publishes_for_an_unknown_delete() -> usize {
    0
}

#[tokio::test(start_paused = true)]
async fn a_note_that_was_never_indexed_and_is_gone_leaves_the_index_alone() {
    let dir = Dir::new("c4-unknown");
    dir.write(
        "a.md", "# A
",
    );
    let (backend, mut from_server) = backend_for(&dir.0, &["a.md"], false).await;
    let before = fingerprint(&backend.state.read().await.index);
    run_ready_paths(
        &[dir.path("never/existed.md")],
        &dir.0,
        &backend.state,
        &backend.client,
    )
    .await;
    assert_eq!(fingerprint(&backend.state.read().await.index), before);
    let sent = sent_meanwhile(&mut from_server, Duration::from_millis(500)).await;
    assert_eq!(
        count_of(&sent, "textDocument/publishDiagnostics", None),
        publishes_for_an_unknown_delete(),
        "{sent:?}"
    );
}

// ---- C5: the configuration file, alone and in a window with notes ----

#[tokio::test(start_paused = true)]
async fn the_configuration_file_is_reloaded_and_the_user_told_once() {
    let dir = Dir::new("c5");
    dir.write("a.md", "# A\n");
    let (backend, mut from_server) = backend_for(&dir.0, &["a.md"], true).await;
    let config = dir.path(".satz.toml");
    let mut new_notes = 0;
    let mut window = |dir: &Dir, n: usize| -> Vec<PathBuf> {
        (0..n)
            .map(|_| {
                new_notes += 1;
                dir.write(&format!("n{new_notes}.md"), "# N\n")
            })
            .collect()
    };

    // Valid: reloaded, logged, nothing shown; the notes next to it are indexed.
    dir.write(".satz.toml", "[hover]\npreview_lines = 4\n");
    let mut events = window(&dir, 3);
    events.push(config.clone());
    run_ready_paths(&events, &dir.0, &backend.state, &backend.client).await;
    let sent = sent_meanwhile(&mut from_server, Duration::from_millis(500)).await;
    assert_eq!(backend.state.read().await.config.hover.preview_lines, 4);
    assert_eq!(count_of(&sent, "window/showMessage", None), 0, "{sent:?}");
    assert!(count_of(&sent, "window/logMessage", None) >= 1, "{sent:?}");
    assert_eq!(
        ids(&*backend.state.read().await).len(),
        4,
        "a.md and the three new notes"
    );
    assert_eq!(
        count_of(&sent, "workspace/diagnostic/refresh", None),
        refreshes_for(4)
    );

    // With a typo'd key: the rest applies, the typo is shown once, and the open notes are told
    // even though no note came with it.
    dir.write(".satz.toml", "[hover]\npreview_lines = 5\nbogus = 1\n");
    run_ready_paths(
        std::slice::from_ref(&config),
        &dir.0,
        &backend.state,
        &backend.client,
    )
    .await;
    let sent = sent_meanwhile(&mut from_server, Duration::from_millis(500)).await;
    assert_eq!(backend.state.read().await.config.hover.preview_lines, 5);
    assert_eq!(count_of(&sent, "window/showMessage", None), 1, "{sent:?}");
    assert_eq!(
        count_of(&sent, "workspace/diagnostic/refresh", None),
        refreshes_for(1),
        "{sent:?}"
    );

    // Broken: the previous settings stay, the error is shown once.
    dir.write(".satz.toml", "[hover\npreview_lines = 9\n");
    run_ready_paths(
        std::slice::from_ref(&config),
        &dir.0,
        &backend.state,
        &backend.client,
    )
    .await;
    let sent = sent_meanwhile(&mut from_server, Duration::from_millis(500)).await;
    {
        let state = backend.state.read().await;
        assert_eq!(state.config.hover.preview_lines, 5);
        assert!(state.config_error.is_some());
    }
    assert_eq!(count_of(&sent, "window/showMessage", None), 1, "{sent:?}");

    // Removed: back to the defaults, no message to show.
    std::fs::remove_file(&config).unwrap();
    run_ready_paths(
        std::slice::from_ref(&config),
        &dir.0,
        &backend.state,
        &backend.client,
    )
    .await;
    let sent = sent_meanwhile(&mut from_server, Duration::from_millis(500)).await;
    {
        let state = backend.state.read().await;
        assert_eq!(
            state.config.hover.preview_lines,
            VaultDefaults::preview_lines()
        );
        assert!(state.config_error.is_none());
    }
    assert_eq!(count_of(&sent, "window/showMessage", None), 0, "{sent:?}");
    assert!(count_of(&sent, "window/logMessage", None) >= 1, "{sent:?}");

    // A change of `vault.gitignore` asks for a restart: one message to show, and only for that.
    dir.write(".satz.toml", "[vault]\ngitignore = \"always\"\n");
    run_ready_paths(
        std::slice::from_ref(&config),
        &dir.0,
        &backend.state,
        &backend.client,
    )
    .await;
    let sent = sent_meanwhile(&mut from_server, Duration::from_millis(500)).await;
    assert_eq!(count_of(&sent, "window/showMessage", None), 1, "{sent:?}");
    dir.write(
        ".satz.toml",
        "[vault]\ngitignore = \"always\"\n[hover]\npreview_lines = 6\n",
    );
    run_ready_paths(
        std::slice::from_ref(&config),
        &dir.0,
        &backend.state,
        &backend.client,
    )
    .await;
    let sent = sent_meanwhile(&mut from_server, Duration::from_millis(500)).await;
    assert_eq!(count_of(&sent, "window/showMessage", None), 0, "{sent:?}");
}

/// The settings the configuration has when there is no file.
struct VaultDefaults;

impl VaultDefaults {
    fn preview_lines() -> usize {
        satz_core::VaultConfig::default().hover.preview_lines
    }
}

// ---- C3 and C7: the loop that debounces, defers and applies ----

struct Loop {
    tx: mpsc::UnboundedSender<PathBuf>,
    task: tokio::task::JoinHandle<()>,
}

fn start_loop(dir: &Path, backend: &Backend) -> Loop {
    let (tx, rx) = mpsc::unbounded_channel();
    let task = tokio::spawn(run_debounce_loop(
        rx,
        dir.to_path_buf(),
        backend.state.clone(),
        backend.client.clone(),
        Duration::from_millis(200),
        Duration::from_millis(50),
    ));
    Loop { tx, task }
}

async fn published(from_server: &mut (impl tokio::io::AsyncBufRead + Unpin), name: &str) -> usize {
    let sent = sent_meanwhile(from_server, Duration::from_millis(100)).await;
    count_of(
        &sent,
        "textDocument/publishDiagnostics",
        Some(&uri_of(name)),
    )
}

fn indexed(backend: &Backend, id: &str) -> bool {
    backend
        .state
        .try_read()
        .is_ok_and(|s| s.index.get_doc(&DocId::new(id)).is_some())
}

#[tokio::test(start_paused = true)]
async fn events_of_one_path_inside_the_window_are_one_piece_of_work() {
    let dir = Dir::new("c7-window");
    dir.write("a.md", "# A\n");
    let (backend, mut from_server) = backend_for(&dir.0, &["a.md"], false).await;
    let lp = start_loop(&dir.0, &backend);

    // Five events, 100 ms apart: the window starts again with each, so nothing is applied yet.
    for i in 0..5 {
        dir.write("new.md", &format!("# New {i}\n"));
        lp.tx.send(dir.path("new.md")).unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    settle().await;
    assert!(
        !indexed(&backend, "new.md"),
        "applied before the window was over"
    );

    tokio::time::sleep(Duration::from_millis(400)).await;
    until("the note to be indexed", || indexed(&backend, "new.md")).await;
    settle().await;
    assert_eq!(
        title_of(&*backend.state.read().await, "new.md").as_deref(),
        Some("New 4")
    );
    assert_eq!(published(&mut from_server, "a.md").await, 1);
}

#[tokio::test(start_paused = true)]
async fn windows_that_are_apart_are_applied_apart() {
    let dir = Dir::new("c7-apart");
    dir.write("a.md", "# A\n");
    let (backend, mut from_server) = backend_for(&dir.0, &["a.md"], false).await;
    let lp = start_loop(&dir.0, &backend);

    for name in ["one.md", "two.md"] {
        dir.write(name, "# X\n");
        lp.tx.send(dir.path(name)).unwrap();
        tokio::time::sleep(Duration::from_millis(1000)).await;
        until(name, || indexed(&backend, name)).await;
        settle().await;
    }
    assert_eq!(
        published(&mut from_server, "a.md").await,
        publishes_for(1) * 2
    );
}

#[tokio::test(start_paused = true)]
async fn a_burst_of_new_notes_through_the_loop_tells_the_client_what_a_window_is_worth() {
    let dir = Dir::new("c7-burst");
    dir.write("a.md", "# A\n");
    let (backend, mut from_server) = backend_for(&dir.0, &["a.md"], false).await;
    let lp = start_loop(&dir.0, &backend);

    for i in 0..30 {
        dir.write(&format!("n{i}.md"), "# N\n");
        lp.tx.send(dir.path(&format!("n{i}.md"))).unwrap();
    }
    tokio::time::sleep(Duration::from_millis(1000)).await;
    until("all the notes", || indexed(&backend, "n29.md")).await;
    settle().await;
    assert_eq!(ids(&*backend.state.read().await).len(), 31);
    assert_eq!(published(&mut from_server, "a.md").await, publishes_for(30));
}

#[tokio::test(start_paused = true)]
async fn events_before_the_first_indexing_is_done_wait_for_it_and_are_not_lost() {
    let dir = Dir::new("c3");
    dir.write("a.md", "# A\n");
    let (backend, mut from_server) = backend_for(&dir.0, &["a.md"], false).await;
    backend.state.write().await.set_indexing_complete(false);
    let lp = start_loop(&dir.0, &backend);

    let names: Vec<String> = (0..5).map(|i| format!("n{i}.md")).collect();
    for name in &names {
        dir.write(name, "# N\n");
        lp.tx.send(dir.path(name)).unwrap();
    }
    // Far more than the window, several times over: nothing is applied, nothing is sent.
    tokio::time::sleep(Duration::from_secs(5)).await;
    settle().await;
    assert_eq!(ids(&*backend.state.read().await), vec!["a.md"]);
    assert!(
        sent_meanwhile(&mut from_server, Duration::from_millis(100))
            .await
            .is_empty()
    );

    backend.state.write().await.set_indexing_complete(true);
    tokio::time::sleep(Duration::from_secs(1)).await;
    until("the notes", || names.iter().all(|n| indexed(&backend, n))).await;
    settle().await;
    assert_eq!(ids(&*backend.state.read().await).len(), 6);
    assert_eq!(published(&mut from_server, "a.md").await, publishes_for(5));
}

#[tokio::test(start_paused = true)]
async fn the_loop_ends_when_the_channel_is_closed() {
    let dir = Dir::new("c7-end");
    dir.write("a.md", "# A\n");
    let (backend, _from_server) = backend_for(&dir.0, &["a.md"], false).await;
    let lp = start_loop(&dir.0, &backend);
    dir.write("late.md", "# Late\n");
    lp.tx.send(dir.path("late.md")).unwrap();
    drop(lp.tx);
    tokio::time::timeout(Duration::from_secs(60), lp.task)
        .await
        .expect("the loop must end once nobody can send")
        .unwrap();
}
