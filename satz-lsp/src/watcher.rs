use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::{RwLock, mpsc};
use tower_lsp_server::Client;
use tower_lsp_server::ls_types::request::WorkspaceDiagnosticRefresh;

use crate::state::SatzState;

/// Spawns a background task that watches `vault_root` for `.md` file changes.
pub fn spawn_watcher(vault_root: PathBuf, state: Arc<RwLock<SatzState>>, client: Client) {
    tracing::debug!(vault_root = %vault_root.display(), "watcher: spawning");
    let (tx, mut rx) = mpsc::unbounded_channel::<PathBuf>();

    // 1. Setup notify watcher
    std::thread::spawn({
        let vault_root = vault_root.clone();
        move || {
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

            for res in event_rx {
                match res {
                    Ok(Event { paths, kind, .. }) => {
                        tracing::trace!(?kind, ?paths, "watcher: raw fs event");
                        if matches!(
                            kind,
                            EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
                        ) {
                            for path in paths {
                                if (is_markdown_file(&path) || is_config_file(&path, &vault_root))
                                    && !is_ignored_path(&path, &vault_root)
                                {
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
        }
    });

    // 2. Debounce and process events in tokio runtime
    tokio::spawn(async move {
        let debounce_duration = Duration::from_millis(200);
        let mut pending: HashMap<PathBuf, Instant> = HashMap::new();

        loop {
            tokio::select! {
                Some(path) = rx.recv() => {
                    pending.insert(path, Instant::now());
                }
                _ = tokio::time::sleep(Duration::from_millis(50)), if !pending.is_empty() => {
                    let now = Instant::now();
                    let ready_paths: Vec<PathBuf> = pending
                        .iter()
                        .filter(|(_, time)| now.duration_since(**time) >= debounce_duration)
                        .map(|(path, _)| path.clone())
                        .collect();

                    for path in ready_paths {
                        pending.remove(&path);
                        process_file_event(&path, &vault_root, &state, &client).await;
                    }
                }
            }
        }
    });
}

async fn process_file_event(
    path: &Path,
    vault_root: &Path,
    state: &Arc<RwLock<SatzState>>,
    client: &Client,
) {
    tracing::debug!(?path, "watcher: processing debounced event");
    if is_config_file(path, vault_root) {
        use tower_lsp_server::ls_types::MessageType;

        let outcome = {
            let mut s = state.write().await;
            reload_config(&mut s, vault_root)
        };
        match outcome {
            ReloadOutcome::Reloaded => {
                client
                    .log_message(
                        MessageType::INFO,
                        "satz: reloaded configuration from .satz.toml",
                    )
                    .await;
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
    } else {
        let mut s = state.write().await;
        apply_fs_change(&mut s, vault_root, path);
    }

    let (supports_pull, uris) = {
        let s = state.read().await;
        (
            s.client_supports_pull_diagnostics,
            s.open_docs.keys().cloned().collect::<Vec<_>>(),
        )
    };

    if supports_pull {
        let _ = client.send_request::<WorkspaceDiagnosticRefresh>(()).await;
    } else {
        for uri in uris {
            crate::backend::publish_for(client, state, &uri).await;
        }
    }
}

/// What an on-disk change did to the index.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum FsChange {
    Removed,
    Reindexed,
    Skipped,
}

/// Brings the index in line with a created/modified/deleted markdown file.
pub(crate) fn apply_fs_change(state: &mut SatzState, vault_root: &Path, path: &Path) -> FsChange {
    let rel_path = crate::state::SatzState::get_rel_path(path, Some(vault_root));
    let rel_path_str = rel_path.to_string_lossy().replace('\\', "/");
    let doc_id = satz_core::DocId::new(&rel_path_str);

    // An open document is owned by the editor's buffer, not by the file: it stays indexed when
    // its file is briefly missing (save-by-rename) and is not overwritten by what is on disk.
    if state.is_open_path(path) {
        return FsChange::Skipped;
    }
    if !path.exists() {
        state.index.remove_doc(&doc_id);
        tracing::info!("Watcher: removed deleted document {}", doc_id);
        return FsChange::Removed;
    }
    if let Ok(content) = std::fs::read_to_string(path) {
        let new_doc = satz_core::parse_document(&content, &rel_path);
        state.index.replace_doc(new_doc);
        tracing::info!("Watcher: re-indexed {}", doc_id);
        return FsChange::Reindexed;
    }
    FsChange::Skipped
}

fn is_markdown_file(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.eq_ignore_ascii_case("md") || ext.eq_ignore_ascii_case("markdown"))
        .unwrap_or(false)
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
    match satz_core::VaultConfig::load(vault_root) {
        Ok(config) => {
            // Cached formatted texts were computed with the old settings (and capacity).
            state.format_cache = crate::state::FormatCache::new(config.lsp.format_cache_capacity);
            state.config = config;
            state.config_error = None;
            if existed {
                ReloadOutcome::Reloaded
            } else {
                ReloadOutcome::RevertedToDefaults
            }
        }
        Err(e) => {
            let message = e.to_string();
            state.config_error = Some(message.clone());
            ReloadOutcome::Failed(message)
        }
    }
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
// Test states are built field by field so each test shows exactly what it sets up.
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use super::*;

    fn root() -> PathBuf {
        if cfg!(windows) {
            PathBuf::from("C:\\vault")
        } else {
            PathBuf::from("/vault")
        }
    }

    #[test]
    fn only_the_vault_roots_dot_satz_toml_is_the_config_file() {
        let r = root();
        for (rel, expected) in [
            (".satz.toml", true),
            ("sub/.satz.toml", false),
            ("a/b/.satz.toml", false),
            ("satz.toml", false),
            ("sub/satz.toml", false),
            (".satz.toml.bak", false),
            (".satz.tom", false),
            ("x.satz.toml", false),
            (".SATZ.TOML", false),
            ("notes.md", false),
            (".satz.toml/inner.md", false),
        ] {
            assert_eq!(
                is_config_file(&r.join(rel), &r),
                expected,
                "relative path {rel:?}"
            );
        }
    }

    #[test]
    fn config_file_is_recognised_despite_a_case_different_vault_prefix() {
        // Windows drive letters / folder names differ in case between the client and the OS.
        if cfg!(windows) {
            let path = PathBuf::from("c:\\VAULT\\.satz.toml");
            assert!(is_config_file(&path, &root()));
        }
    }

    #[test]
    fn a_path_outside_the_vault_is_never_the_config_file() {
        let other = if cfg!(windows) {
            PathBuf::from("D:\\elsewhere\\.satz.toml")
        } else {
            PathBuf::from("/elsewhere/.satz.toml")
        };
        assert!(!is_config_file(&other, &root()));
    }

    struct TempVault(PathBuf);
    impl TempVault {
        fn new(tag: &str) -> Self {
            use std::sync::atomic::{AtomicUsize, Ordering};
            static N: AtomicUsize = AtomicUsize::new(0);
            let dir = std::env::temp_dir().join(format!(
                "satz_watch_{tag}_{}_{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
        fn write(&self, content: &str) {
            std::fs::write(self.0.join(".satz.toml"), content).unwrap();
        }
        fn delete(&self) {
            std::fs::remove_file(self.0.join(".satz.toml")).unwrap();
        }
    }
    impl Drop for TempVault {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn reload_config_keeps_the_previous_settings_on_error_and_recovers_when_fixed() {
        let v = TempVault::new("reload");
        let mut state = SatzState::default();

        v.write("[hover]\npreview_lines = 3\n");
        assert_eq!(reload_config(&mut state, &v.0), ReloadOutcome::Reloaded);
        assert_eq!(state.config.hover.preview_lines, 3);
        assert_eq!(state.config_error, None);

        // Broken TOML: previous settings stay, the error is recorded and reported.
        v.write("[hover\npreview_lines = 9\n");
        let outcome = reload_config(&mut state, &v.0);
        let ReloadOutcome::Failed(message) = outcome else {
            panic!("expected Failed, got {outcome:?}");
        };
        assert!(message.contains(".satz.toml"), "{message}");
        assert_eq!(state.config.hover.preview_lines, 3, "previous config kept");
        assert_eq!(state.config_error.as_deref(), Some(message.as_str()));
        assert!(!state.formatting_allowed());

        // Fixed: new settings apply and the error clears.
        v.write("[hover]\npreview_lines = 5\n");
        assert_eq!(reload_config(&mut state, &v.0), ReloadOutcome::Reloaded);
        assert_eq!(state.config.hover.preview_lines, 5);
        assert_eq!(state.config_error, None);
        assert!(state.formatting_allowed());

        // A typo'd key is an error too, and does not half-apply the valid keys next to it.
        v.write("[hover]\npreview_lines = 7\nbogus = 1\n");
        assert!(matches!(
            reload_config(&mut state, &v.0),
            ReloadOutcome::Failed(m) if m.contains("bogus")
        ));
        assert_eq!(state.config.hover.preview_lines, 5, "nothing half-applied");
    }

    #[test]
    fn reload_config_when_the_file_is_deleted_reverts_to_defaults_and_clears_the_error() {
        let v = TempVault::new("delete");
        let mut state = SatzState::default();

        v.write("[hover]\npreview_lines = 3\n");
        reload_config(&mut state, &v.0);
        assert_eq!(state.config.hover.preview_lines, 3);
        v.delete();
        assert_eq!(
            reload_config(&mut state, &v.0),
            ReloadOutcome::RevertedToDefaults
        );
        assert_eq!(state.config, satz_core::VaultConfig::default());
        assert_eq!(state.config_error, None);

        // Deleting a file that was in an error state clears the error as well.
        v.write("not = [valid\n");
        assert!(matches!(
            reload_config(&mut state, &v.0),
            ReloadOutcome::Failed(_)
        ));
        assert!(state.config_error.is_some());
        v.delete();
        assert_eq!(
            reload_config(&mut state, &v.0),
            ReloadOutcome::RevertedToDefaults
        );
        assert_eq!(state.config_error, None);
        assert!(state.formatting_allowed());
    }

    #[test]
    fn reload_config_reports_an_unreadable_config_path() {
        // A DIRECTORY named .satz.toml can't be read as a file.
        let v = TempVault::new("isdir");
        std::fs::create_dir_all(v.0.join(".satz.toml")).unwrap();
        let mut state = SatzState::default();

        let outcome = reload_config(&mut state, &v.0);

        assert!(matches!(outcome, ReloadOutcome::Failed(_)), "{outcome:?}");
        assert!(state.config_error.is_some());
    }

    // ---- on-disk changes vs. open documents ----

    fn temp_dir(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "satz-watch-{}-{}-{}",
            std::process::id(),
            tag,
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn state_in(dir: &Path) -> SatzState {
        let mut state = SatzState::default();
        state.vault_root = Some(dir.to_path_buf());
        state.indexing_complete = true;
        state
    }

    fn targets(state: &SatzState, id: &str) -> Option<Vec<String>> {
        state
            .index
            .get_doc(&satz_core::DocId::new(id))
            .map(|d| d.links.iter().map(|l| l.target_doc.clone()).collect())
    }

    #[test]
    fn a_closed_note_is_reindexed_on_change_and_dropped_on_delete() {
        let dir = temp_dir("closed");
        let path = dir.join("a.md");
        std::fs::write(&path, "# A\n\n[[one]]\n").unwrap();
        let mut state = state_in(&dir);

        assert_eq!(
            apply_fs_change(&mut state, &dir, &path),
            FsChange::Reindexed
        );
        assert_eq!(targets(&state, "a.md"), Some(vec!["one".into()]));

        std::fs::write(&path, "# A\n\n[[two]]\n").unwrap();
        assert_eq!(
            apply_fs_change(&mut state, &dir, &path),
            FsChange::Reindexed
        );
        assert_eq!(targets(&state, "a.md"), Some(vec!["two".into()]));

        std::fs::remove_file(&path).unwrap();
        assert_eq!(apply_fs_change(&mut state, &dir, &path), FsChange::Removed);
        assert_eq!(targets(&state, "a.md"), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_open_note_is_never_dropped_when_its_file_briefly_disappears() {
        // Editors that save by writing a temp file and renaming leave a window with no file.
        let dir = temp_dir("open-missing");
        let path = dir.join("a.md");
        let mut state = state_in(&dir);
        state.open_document("file:///a.md", "# A\n\n[[buffer]]\n", &path, 1);
        assert!(!path.exists());

        assert_eq!(apply_fs_change(&mut state, &dir, &path), FsChange::Skipped);

        assert_eq!(targets(&state, "a.md"), Some(vec!["buffer".into()]));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_open_note_is_not_overwritten_by_its_file_on_disk() {
        let dir = temp_dir("open-modify");
        let path = dir.join("a.md");
        std::fs::write(&path, "# A\n\n[[disk]]\n").unwrap();
        let mut state = state_in(&dir);
        state.open_document("file:///a.md", "# A\n\n[[buffer]]\n", &path, 1);

        assert_eq!(apply_fs_change(&mut state, &dir, &path), FsChange::Skipped);

        assert_eq!(targets(&state, "a.md"), Some(vec!["buffer".into()]));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_differently_spelled_event_path_still_finds_the_open_note() {
        // notify may report the path with other casing (Windows) than the client's URI gave.
        let dir = temp_dir("open-case");
        let path = dir.join("Notes.md");
        std::fs::write(&path, "# N\n\n[[disk]]\n").unwrap();
        let mut state = state_in(&dir);
        state.open_document("file:///n.md", "# N\n\n[[buffer]]\n", &path, 1);

        let event_path = dir.join("NOTES.md");
        let expected = if event_path.exists() {
            // Case-insensitive file system: the same file, spelled differently.
            FsChange::Skipped
        } else {
            // Case-sensitive: a different path; only the open-note check is being probed, so
            // stop here rather than assert on a file that does not exist.
            let _ = std::fs::remove_dir_all(&dir);
            return;
        };
        assert_eq!(apply_fs_change(&mut state, &dir, &event_path), expected);
        assert_eq!(targets(&state, "Notes.md"), Some(vec!["buffer".into()]));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn open_path_matching_ignores_case_and_separator_spelling() {
        let mut state = SatzState::default();
        state.vault_root = Some(PathBuf::from("/Vault"));
        state.open_document("file:///x", "# X\n", Path::new("/Vault/Sub/X.md"), 1);
        assert!(state.is_open_path(Path::new("/Vault/Sub/X.md")));
        assert!(state.is_open_path(Path::new("/vault/sub/x.md")));
        assert!(!state.is_open_path(Path::new("/Vault/Sub/Y.md")));
        assert!(!state.is_open_path(Path::new("/Vault/Other/X.md")));
    }

    #[test]
    fn an_unreadable_path_is_skipped_without_touching_the_index() {
        let dir = temp_dir("unreadable");
        let path = dir.join("a.md");
        std::fs::create_dir_all(&path).unwrap(); // exists, but is a directory
        let mut state = state_in(&dir);
        assert_eq!(apply_fs_change(&mut state, &dir, &path), FsChange::Skipped);
        assert_eq!(targets(&state, "a.md"), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- the workspace-format cache must not outlive the settings it was computed with ----

    fn state_with_list_doc() -> SatzState {
        let mut state = SatzState::default();
        state.index = satz_core::Index::build(vec![satz_core::parse_document(
            "- a\n- b\n",
            Path::new("a.md"),
        )]);
        state
    }

    fn format_and_cache(state: &mut SatzState) -> Vec<String> {
        let result = crate::handlers::execute_command::compute_format_changes(state);
        for (hash, formatted) in result.cache_updates {
            state.format_cache.insert(hash, formatted);
        }
        result.changes.into_iter().map(|c| c.formatted).collect()
    }

    #[test]
    fn changing_formatter_settings_changes_what_workspace_format_produces() {
        let v = TempVault::new("fmt-cache");
        let mut state = state_with_list_doc();
        state.vault_root = Some(v.0.clone());

        // Default settings: already clean. The (unchanged) result is cached.
        assert_eq!(
            reload_config(&mut state, &v.0),
            ReloadOutcome::RevertedToDefaults
        );
        assert!(format_and_cache(&mut state).is_empty());
        assert!(!state.format_cache.is_empty());

        // The user switches the list marker: the next run must use the new setting.
        v.write("[formatter.lists]\nmarker = \"*\"\n");
        assert_eq!(reload_config(&mut state, &v.0), ReloadOutcome::Reloaded);
        assert_eq!(format_and_cache(&mut state), vec!["* a\n* b\n".to_string()]);

        // And back again.
        v.delete();
        assert_eq!(
            reload_config(&mut state, &v.0),
            ReloadOutcome::RevertedToDefaults
        );
        assert!(format_and_cache(&mut state).is_empty());
    }

    #[test]
    fn a_reload_replaces_the_cache_and_applies_the_new_capacity() {
        let v = TempVault::new("fmt-cap");
        let mut state = SatzState::default();
        state.format_cache.insert(1, "x".to_string());

        v.write("[lsp]\nformat_cache_capacity = 7\n");
        assert_eq!(reload_config(&mut state, &v.0), ReloadOutcome::Reloaded);

        assert!(state.format_cache.is_empty());
        for hash in 0..20u64 {
            state.format_cache.insert(hash, String::new());
        }
        assert_eq!(state.format_cache.len(), 7);
    }

    #[test]
    fn a_failed_reload_keeps_the_settings_and_so_the_cache() {
        let v = TempVault::new("fmt-failed");
        let mut state = SatzState::default();
        state.format_cache.insert(1, "kept".to_string());

        v.write("[formatter\nbroken = \n");
        assert!(matches!(
            reload_config(&mut state, &v.0),
            ReloadOutcome::Failed(_)
        ));

        assert_eq!(state.format_cache.get(1), Some("kept"));
    }
}
