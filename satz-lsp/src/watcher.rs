use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use rayon::prelude::*;
use tokio::sync::{RwLock, mpsc};
use tokio::time::Instant;
use tower_lsp_server::Client;

use crate::state::SatzState;

/// Lets the watcher of a vault be stopped: the file system thread ends and the debounce task with it.
#[derive(Debug, Clone, Default)]
pub struct WatcherHandle {
    stop: Arc<std::sync::atomic::AtomicBool>,
}

impl WatcherHandle {
    /// Asks the watcher to end. Harmless when called again or after it has ended.
    pub fn stop(&self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn is_stopped(&self) -> bool {
        self.stop.load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// The thread that owns the `notify` watcher: it sends every relevant path it sees to `tx` and
/// ends -- dropping the watcher and `tx` -- once the handle is stopped.
pub(crate) fn spawn_notify_thread(
    vault_root: PathBuf,
    tx: mpsc::UnboundedSender<PathBuf>,
    handle: WatcherHandle,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        if handle.is_stopped() {
            return;
        }
        let (event_tx, event_rx) = std::sync::mpsc::channel();
        let mut watcher = match RecommendedWatcher::new(
            event_tx,
            notify::Config::default().with_poll_interval(Duration::from_millis(500)),
        ) {
            Ok(w) => w,
            Err(e) => {
                tracing::error!("Failed to create file watcher: {}", e);
                return;
            }
        };

        if let Err(e) = watcher.watch(&vault_root, RecursiveMode::Recursive) {
            tracing::error!("Failed to watch vault root {}: {}", vault_root.display(), e);
            return;
        }
        tracing::debug!(vault_root = %vault_root.display(), "watcher: now watching");

        while !handle.is_stopped() {
            let res = match event_rx.recv_timeout(Duration::from_millis(100)) {
                Ok(res) => res,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            };
            match res {
                Ok(Event { paths, kind, .. }) => {
                    tracing::trace!(?kind, ?paths, "watcher: raw fs event");
                    if matches!(
                        kind,
                        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
                    ) {
                        for path in paths {
                            if is_relevant_path(&path, &vault_root) {
                                tracing::debug!(?path, "watcher: queued relevant change");
                                let _ = tx.send(path);
                            }
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("Watch error: {}", e);
                }
            }
        }
    })
}

/// Spawns a background task that watches `vault_root` for `.md` file changes. The returned handle
/// stops it.
pub fn spawn_watcher(
    vault_root: PathBuf,
    state: Arc<RwLock<SatzState>>,
    client: Client,
) -> WatcherHandle {
    tracing::debug!(vault_root = %vault_root.display(), "watcher: spawning");
    let handle = WatcherHandle::default();
    let (tx, rx) = mpsc::unbounded_channel::<PathBuf>();

    // 1. The file system thread
    spawn_notify_thread(vault_root.clone(), tx, handle.clone());

    // 2. Debounce and process events in tokio runtime
    tokio::spawn(run_debounce_loop(
        rx,
        vault_root,
        state,
        client,
        DEBOUNCE_WINDOW,
        DEBOUNCE_TICK,
    ));
    handle
}

/// How long a path must be quiet before its event is applied.
const DEBOUNCE_WINDOW: Duration = Duration::from_millis(200);
/// How often the waiting paths are looked at.
const DEBOUNCE_TICK: Duration = Duration::from_millis(50);

/// Collects the paths `rx` delivers and applies each once it has been quiet for `window`, looking
/// at the waiting paths every `tick`. Ends when `rx` is closed (the file system thread has ended:
/// stopped, or it could not watch).
pub(crate) async fn run_debounce_loop(
    mut rx: mpsc::UnboundedReceiver<PathBuf>,
    vault_root: PathBuf,
    state: Arc<RwLock<SatzState>>,
    client: Client,
    window: Duration,
    tick: Duration,
) {
    let mut pending = Debouncer::default();

    loop {
        tokio::select! {
            message = rx.recv() => match message {
                Some(path) => pending.push(path, Instant::now()),
                None => break,
            },
            _ = tokio::time::sleep(tick), if !pending.is_empty() => {
                let now = Instant::now();
                let ready = pending.take_ready(now, window);
                // A change that arrives before the first index is complete waits for it.
                for path in process_batch(&ready, &vault_root, &state, &client).await {
                    pending.push(path, Instant::now());
                }
            }
        }
    }
}

/// Collects file system events and hands each path out once it has been quiet for a while, so a
/// burst of events for one file (an editor saving) is one piece of work.
#[derive(Default)]
pub(crate) struct Debouncer {
    pending: HashMap<PathBuf, Instant>,
}

impl Debouncer {
    /// Records an event; a path that is already waiting starts its wait again.
    pub(crate) fn push(&mut self, path: PathBuf, now: Instant) {
        self.pending.insert(path, now);
    }

    /// Removes and returns (in path order) the paths whose last event is at least `window` old.
    pub(crate) fn take_ready(&mut self, now: Instant, window: Duration) -> Vec<PathBuf> {
        let mut ready: Vec<PathBuf> = self
            .pending
            .iter()
            .filter(|(_, time)| now.duration_since(**time) >= window)
            .map(|(path, _)| path.clone())
            .collect();
        ready.sort();
        for path in &ready {
            self.pending.remove(path);
        }
        ready
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }
}

/// Applies one debounced event. `true`: the first indexing is not finished yet, so the event was
/// not applied and must be queued again.
#[cfg(test)]
pub(crate) async fn process_file_event(
    path: &Path,
    vault_root: &Path,
    state: &Arc<RwLock<SatzState>>,
    client: &Client,
) -> bool {
    !process_batch(&[path.to_path_buf()], vault_root, state, client)
        .await
        .is_empty()
}

/// Applies the events of one debounce window together: what the disk says about all of them is read
/// (on every core), put into the index under ONE write lock, with the derived tables rebuilt once
/// at most, and the open notes are told once, however many events there were.
///
/// The paths that could not be applied yet (the first indexing is not finished) come back, to be
/// queued again.
pub(crate) async fn process_batch(
    paths: &[PathBuf],
    vault_root: &Path,
    state: &Arc<RwLock<SatzState>>,
    client: &Client,
) -> Vec<PathBuf> {
    tracing::debug!(count = paths.len(), "watcher: processing debounced events");
    if paths.is_empty() {
        return Vec::new();
    }
    if !state.read().await.is_indexing_complete() {
        return paths.to_vec();
    }

    // The configuration first: what is read next is read with the settings it brings.
    let (config, notes): (Vec<&PathBuf>, Vec<&PathBuf>) = paths
        .iter()
        .partition(|path| is_config_file(path, vault_root));
    let mut refresh = false;
    if !config.is_empty() {
        reload_config_and_tell(state, client, vault_root).await;
        refresh = true;
    }
    let mut again = Vec::new();
    if !notes.is_empty() {
        let change = apply_disk_changes(&notes, vault_root, state).await;
        if change == FsChange::Deferred {
            again = notes.into_iter().cloned().collect();
        }
        refresh |= fs_change_needs_refresh(&change);
    }

    if refresh {
        crate::backend::refresh_open_documents(client, state).await;
    }
    again
}

/// Re-reads `.satz.toml` and tells the user what came of it.
async fn reload_config_and_tell(
    state: &Arc<RwLock<SatzState>>,
    client: &Client,
    vault_root: &Path,
) {
    use tower_lsp_server::ls_types::MessageType;

    let (outcome, warnings, restart_notice) = {
        let mut s = state.write().await;
        let before = s.config.gitignore_mode();
        let outcome = reload_config(&mut s, vault_root);
        let notice = gitignore_change_notice(before, s.config.gitignore_mode());
        (outcome, s.config_warnings.clone(), notice)
    };
    match outcome {
        ReloadOutcome::Reloaded => {
            client
                .log_message(
                    MessageType::INFO,
                    "satz: reloaded configuration from .satz.toml",
                )
                .await;
            // Settings that were ignored must be seen: the rest applies, so nothing else would
            // tell the user that one of them did nothing.
            if !warnings.is_empty() {
                let message = crate::state::config_warnings_message(&warnings);
                client.log_message(MessageType::WARNING, &message).await;
                client.show_message(MessageType::WARNING, message).await;
            }
        }
        ReloadOutcome::RevertedToDefaults => {
            client
                .log_message(
                    MessageType::INFO,
                    "satz: .satz.toml was removed; using default settings",
                )
                .await;
        }
        ReloadOutcome::Failed(error) => {
            let message = crate::state::config_error_message(&error, "the previous");
            client.log_message(MessageType::WARNING, &message).await;
            client.show_message(MessageType::WARNING, message).await;
        }
    }
    // The vault was read once, at startup, with the old setting.
    if let Some(notice) = restart_notice {
        client.log_message(MessageType::INFO, &notice).await;
        client.show_message(MessageType::INFO, notice).await;
    }
}

/// From this many paths on, what the disk says about them is read on every core.
const PARALLEL_READ_FROM: usize = 8;

/// What the disk says about a path that is not (known to be) a folder, or that it is one.
enum Read {
    Note(PreparedChange),
    Folder,
}

/// Reads the notes among `paths` (a folder is only noted as one: whether it is worth reading is
/// asked of the index first).
fn read_paths(paths: &[PathBuf], vault_root: &Path) -> Vec<Read> {
    let read = |path: &PathBuf| {
        if path.is_dir() {
            Read::Folder
        } else {
            Read::Note(prepare_file(path, vault_root))
        }
    };
    if paths.len() >= PARALLEL_READ_FROM {
        paths.par_iter().map(read).collect()
    } else {
        paths.iter().map(read).collect()
    }
}

/// Reads the folders `paths` (each with the place it has in the window).
fn read_folders(
    folders: &[(usize, PathBuf)],
    vault_root: &Path,
    gitignore: satz_core::GitignoreMode,
) -> Vec<(usize, PreparedChange)> {
    let read = |(at, path): &(usize, PathBuf)| (*at, prepare_folder(path, vault_root, gitignore));
    if folders.len() >= 2 {
        folders.par_iter().map(read).collect()
    } else {
        folders.iter().map(read).collect()
    }
}

/// Reads what the disk says about `paths` and puts it into the index.
///
/// Reading and parsing happen before the lock is taken (a note, or a whole folder); everything
/// that is read goes into the index under one write lock.
async fn apply_disk_changes(
    paths: &[&PathBuf],
    vault_root: &Path,
    state: &Arc<RwLock<SatzState>>,
) -> FsChange {
    let gitignore = state.read().await.config.gitignore_mode();
    let owned: Vec<PathBuf> = paths.iter().map(|path| (*path).clone()).collect();
    let root = vault_root.to_path_buf();

    let (read_of, read_root) = (owned.clone(), root.clone());
    let mut reads = tokio::task::spawn_blocking(move || read_paths(&read_of, &read_root))
        .await
        .unwrap_or_else(|_| {
            (0..owned.len())
                .map(|_| Read::Note(PreparedChange::Skip))
                .collect()
        });

    // An existing folder the index already holds notes of says nothing new (saving a note makes
    // some systems report its folder as modified too): its notes have their own events.
    let folders: Vec<usize> = (0..reads.len())
        .filter(|&at| matches!(reads[at], Read::Folder))
        .collect();
    if !folders.is_empty() {
        let news = {
            let s = state.read().await;
            let folder_paths: Vec<&Path> = folders.iter().map(|&at| owned[at].as_path()).collect();
            folders_with_news(&s, &folder_paths, vault_root)
        };
        let mut to_read = Vec::new();
        for (&at, news) in folders.iter().zip(news) {
            if news {
                to_read.push((at, owned[at].clone()));
            } else {
                reads[at] = Read::Note(PreparedChange::Skip);
            }
        }
        let read = tokio::task::spawn_blocking(move || read_folders(&to_read, &root, gitignore))
            .await
            .unwrap_or_default();
        for (at, prepared) in read {
            reads[at] = Read::Note(prepared);
        }
        // A folder whose reading was lost is skipped, as it would have been on its own.
        for read in reads.iter_mut() {
            if matches!(read, Read::Folder) {
                *read = Read::Note(PreparedChange::Skip);
            }
        }
    }

    let changes: Vec<(PathBuf, PreparedChange)> = owned
        .into_iter()
        .zip(reads)
        .map(|(path, read)| match read {
            Read::Note(prepared) => (path, prepared),
            Read::Folder => (path, PreparedChange::Skip),
        })
        .collect();
    let mut s = state.write().await;
    apply_prepared_batch(&mut s, changes)
}

/// Whether the open documents' diagnostics can differ after this change: nothing happened to the
/// index for a skipped or deferred one, so nothing is recomputed or republished for it.
pub(crate) fn fs_change_needs_refresh(change: &FsChange) -> bool {
    matches!(change, FsChange::Reindexed | FsChange::Removed)
}

/// What an on-disk change did to the index.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum FsChange {
    Removed,
    Reindexed,
    Skipped,
    /// The initial indexing is not finished: the change is not applied and must be tried again.
    Deferred,
}

/// What the disk says about a changed path, read WITHOUT holding the state lock: reading and
/// parsing a note (or a whole folder) is slow compared to updating the index.
pub(crate) enum PreparedChange {
    /// A note that exists and was read.
    Doc(Box<satz_core::Document>),
    /// A note that no longer exists.
    RemoveDoc(satz_core::DocId),
    /// A folder that exists: every note found in it. Notes the index holds for that folder but that
    /// were not found any more are dropped.
    Subtree {
        prefix: String,
        docs: Vec<satz_core::Document>,
    },
    /// A path that no longer exists and may have been a folder: its notes go.
    RemovePrefix(String),
    /// Nothing to do (not a note, unreadable, not a folder that matters).
    Skip,
}

/// Reads what is on disk for `path` (a note, or a folder that appeared, moved or vanished).
#[cfg(test)]
pub(crate) fn prepare_fs_change(
    path: &Path,
    vault_root: &Path,
    gitignore: satz_core::GitignoreMode,
) -> PreparedChange {
    if path.is_dir() {
        prepare_folder(path, vault_root, gitignore)
    } else {
        prepare_file(path, vault_root)
    }
}

/// The notes of a folder that exists (also a folder that happens to be named like a note,
/// `x.md/`).
fn prepare_folder(
    path: &Path,
    vault_root: &Path,
    gitignore: satz_core::GitignoreMode,
) -> PreparedChange {
    let rel_path = SatzState::get_rel_path(path, Some(vault_root));
    let rel = satz_core::DocId::from_path(&rel_path).as_str().to_string();
    match satz_core::walk::walk_subtree_with(vault_root, path, gitignore) {
        Ok(docs) => PreparedChange::Subtree { prefix: rel, docs },
        Err(_) => PreparedChange::Skip,
    }
}

/// What is on disk for a path that is not a folder: a note, or nothing.
fn prepare_file(path: &Path, vault_root: &Path) -> PreparedChange {
    let rel_path = SatzState::get_rel_path(path, Some(vault_root));

    if satz_core::walk::is_markdown_path(path) {
        if !path.exists() {
            return PreparedChange::RemoveDoc(satz_core::DocId::from_path(&rel_path));
        }
        return match std::fs::read_to_string(path) {
            Ok(content) => PreparedChange::Doc(Box::new(satz_core::parse_document_owned(
                content, &rel_path,
            ))),
            Err(_) => PreparedChange::Skip,
        };
    }
    if !path.exists() {
        // It cannot be told any more whether this was a folder: drop whatever the index holds under it.
        return PreparedChange::RemovePrefix(
            satz_core::DocId::from_path(&rel_path).as_str().to_string(),
        );
    }
    PreparedChange::Skip
}

/// Whether an event for an existing folder can tell the index anything: only when it holds notes the
/// index does not know yet (a folder created, moved in or copied in). Editors saving a note also
/// make the OS report the folder as modified; re-reading every note in it for that is wasted work.
#[cfg(test)]
pub(crate) fn folder_event_is_news(state: &SatzState, folder: &Path, vault_root: &Path) -> bool {
    folders_with_news(state, &[folder], vault_root)[0]
}

/// `folder_event_is_news` for several folders at once.
///
/// The notes' ids are folded as they are needed, and once: a folder the index holds notes of (the
/// usual case, saving a note reports its folder) is found among the first few notes looked at, and
/// folding an id is what costs.
fn folders_with_news(state: &SatzState, folders: &[&Path], vault_root: &Path) -> Vec<bool> {
    // One folder (nearly always): nothing to keep for a next one.
    if let [folder] = folders {
        let prefix = folded_prefix(&SatzState::get_rel_path(folder, Some(vault_root)));
        let held = state
            .index
            .documents()
            .any(|doc| is_inside_folded(&satz_core::fold_key(doc.id.as_str()), &prefix));
        return vec![!held];
    }
    let ids: Vec<&satz_core::DocId> = state.index.documents().map(|doc| &doc.id).collect();
    let mut folded: Vec<Option<String>> = vec![None; ids.len()];
    folders
        .iter()
        .map(|folder| {
            let prefix = folded_prefix(&SatzState::get_rel_path(folder, Some(vault_root)));
            !(0..ids.len()).any(|at| {
                let id = folded[at].get_or_insert_with(|| satz_core::fold_key(ids[at].as_str()));
                is_inside_folded(id, &prefix)
            })
        })
        .collect()
}

/// The folded spelling of a folder's path relative to the vault, as `is_inside_folded` wants it.
fn folded_prefix(rel: &Path) -> String {
    satz_core::fold_key(
        satz_core::DocId::from_path(rel)
            .as_str()
            .trim_end_matches('/'),
    )
}

/// Whether the note `id` is `prefix` itself or lies inside the folder `prefix`, both already folded
/// (`fold_key`): the event path and the indexed path may be spelled differently in case and in the
/// separators, but they are compared as whole path components.
fn is_inside_folded(id: &str, prefix: &str) -> bool {
    id == prefix
        || id
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// Puts what was read for a window's paths into the index, as one change. Open documents are owned
/// by the editor's buffer, not by the file: they stay indexed when their file or folder disappears
/// and are never overwritten by what is on disk.
///
/// The paths come in the order they were reported in; a note that is named by more than one of them
/// is what the last says. What a folder held is what the index held before the window.
pub(crate) fn apply_prepared_batch(
    state: &mut SatzState,
    changes: Vec<(PathBuf, PreparedChange)>,
) -> FsChange {
    if !state.is_indexing_complete() {
        return FsChange::Deferred;
    }
    let open = state.open_path_keys();
    // `Some`: the note as it is on disk; `None`: gone.
    let mut ops: std::collections::BTreeMap<satz_core::DocId, Option<satz_core::Document>> =
        std::collections::BTreeMap::new();
    // The notes of the index and their folded ids, worked out when a folder needs them.
    let mut held: Option<Vec<(satz_core::DocId, String)>> = None;

    for (path, prepared) in changes {
        match prepared {
            PreparedChange::Skip => {}
            PreparedChange::Doc(doc) => {
                if !open.contains(&state.path_key(&path)) {
                    ops.insert(doc.id.clone(), Some(*doc));
                }
            }
            PreparedChange::RemoveDoc(id) => {
                if !open.contains(&state.path_key(&path)) {
                    ops.insert(id, None);
                }
            }
            PreparedChange::RemovePrefix(prefix) => {
                let prefix = satz_core::fold_key(prefix.trim_end_matches('/'));
                for (id, folded) in held_notes(&mut held, state) {
                    if is_inside_folded(folded, &prefix) && !open.contains(folded) {
                        ops.insert(id.clone(), None);
                    }
                }
            }
            PreparedChange::Subtree { prefix, docs } => {
                let prefix = satz_core::fold_key(prefix.trim_end_matches('/'));
                let folded: Vec<String> = docs
                    .iter()
                    .map(|doc| satz_core::fold_key(doc.id.as_str()))
                    .collect();
                // Every note the index holds in the folder goes, except the open ones; those the
                // folder still has are put back right after (the later change of a note counts).
                for (id, key) in held_notes(&mut held, state) {
                    if is_inside_folded(key, &prefix) && !open.contains(key) {
                        ops.insert(id.clone(), None);
                    }
                }
                for (doc, key) in docs.into_iter().zip(&folded) {
                    if !open.contains(key) {
                        ops.insert(doc.id.clone(), Some(doc));
                    }
                }
            }
        }
    }

    let (mut removed, mut upserted) = (Vec::new(), Vec::new());
    for (id, op) in ops {
        match op {
            Some(doc) => upserted.push(doc),
            // A note that is not in the index is not removed from it.
            None if state.index.get_doc(&id).is_some() => removed.push(id),
            None => {}
        }
    }
    let (removes, puts) = (removed.len(), upserted.len());
    state.index.apply_changes(&removed, upserted);
    match (puts, removes) {
        (0, 0) => FsChange::Skipped,
        (0, _) => {
            tracing::info!("Watcher: removed {removes} document(s)");
            FsChange::Removed
        }
        _ => {
            tracing::info!("Watcher: re-indexed {puts} document(s), removed {removes}");
            FsChange::Reindexed
        }
    }
}

/// The notes of the index with their folded ids, made the first time they are asked for.
fn held_notes<'a>(
    held: &'a mut Option<Vec<(satz_core::DocId, String)>>,
    state: &SatzState,
) -> &'a [(satz_core::DocId, String)] {
    held.get_or_insert_with(|| {
        state
            .index
            .documents()
            .map(|doc| (doc.id.clone(), satz_core::fold_key(doc.id.as_str())))
            .collect()
    })
}

/// Brings the index in line with a created/modified/deleted note or folder (reads, then applies).
#[cfg(test)]
pub(crate) fn apply_fs_change(state: &mut SatzState, vault_root: &Path, path: &Path) -> FsChange {
    let prepared = prepare_fs_change(path, vault_root, state.config.gitignore_mode());
    apply_prepared_batch(state, vec![(path.to_path_buf(), prepared)])
}

/// Whether a file system event for `path` can change the index or the configuration: a note, the
/// configuration file, or anything that might be a folder (deleting or moving one reports only the
/// folder's own path), except paths inside ignored folders.
fn is_relevant_path(path: &Path, vault_root: &Path) -> bool {
    !is_ignored_path(path, vault_root)
}

/// What happened when the configuration was re-read after `.satz.toml` changed.
#[derive(Debug, PartialEq, Eq)]
pub enum ReloadOutcome {
    /// The file was read and its settings are now in effect.
    Reloaded,
    /// The file no longer exists; default settings are in effect.
    RevertedToDefaults,
    /// The file exists but is unusable; the PREVIOUS settings stay in effect and the reason is
    /// carried here (and stored in `SatzState::config_error`).
    Failed(String),
}

/// Re-reads `<vault_root>/.satz.toml` into `state`. Never leaves the state half-updated: on
/// failure the previous configuration is kept and `config_error` is set; on success (including
/// the file having been deleted) `config_error` is cleared.
pub fn reload_config(state: &mut SatzState, vault_root: &Path) -> ReloadOutcome {
    let existed = vault_root
        .join(satz_core::config::CONFIG_FILE_NAME)
        .exists();
    match satz_core::VaultConfig::load_with_warnings(vault_root) {
        Ok((config, warnings)) => {
            // Cached formatted texts were computed with the old settings (and capacity).
            state.format_cache = crate::state::FormatCache::new(config.lsp.format_cache_capacity);
            state.config = config;
            state.config_error = None;
            state.config_warnings = warnings;
            state.config_revision += 1;
            state.sync_daily(chrono::Local::now().date_naive());
            if existed {
                ReloadOutcome::Reloaded
            } else {
                ReloadOutcome::RevertedToDefaults
            }
        }
        Err(e) => {
            let message = e.to_string();
            state.config_error = Some(message.clone());
            state.config_revision += 1;
            ReloadOutcome::Failed(message)
        }
    }
}

/// What to tell the user when a reloaded `.satz.toml` changes `vault.gitignore`: the notes the
/// server holds were read with the old setting, and re-reading the vault is not done on a reload,
/// so it takes a restart. `None` when the setting is the same.
pub(crate) fn gitignore_change_notice(
    before: satz_core::GitignoreMode,
    after: satz_core::GitignoreMode,
) -> Option<String> {
    (before != after).then(|| {
        "satz: vault.gitignore changed in .satz.toml; restart the language server to read the vault \
         with it"
            .to_string()
    })
}

/// True only for the vault ROOT's `.satz.toml` -- the one file the server reads at startup. A
/// `.satz.toml` in a subfolder, or any other name such as `satz.toml`, is not configuration.
fn is_config_file(path: &Path, vault_root: &Path) -> bool {
    SatzState::get_rel_path(path, Some(vault_root))
        == Path::new(satz_core::config::CONFIG_FILE_NAME)
}

fn is_ignored_path(path: &Path, root: &Path) -> bool {
    let rel = path.strip_prefix(root).unwrap_or(path);
    for c in rel.components() {
        let s = c.as_os_str().to_string_lossy();
        if satz_core::walk::DEFAULT_IGNORED_DIRS
            .iter()
            .any(|d| s.eq_ignore_ascii_case(d))
        {
            return true;
        }
        if s.starts_with('.') && s != "." && s != ".." && s != ".satz.toml" {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests;
