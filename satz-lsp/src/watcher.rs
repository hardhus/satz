use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::{RwLock, mpsc};

use crate::state::SatzState;

/// Spawns a background task that watches `vault_root` for `.md` file changes.
pub fn spawn_watcher(vault_root: PathBuf, state: Arc<RwLock<SatzState>>) {
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

            for res in event_rx {
                match res {
                    Ok(Event { paths, kind, .. }) => {
                        if matches!(
                            kind,
                            EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
                        ) {
                            for path in paths {
                                if is_markdown_file(&path) && !is_ignored_path(&path) {
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
                        process_file_event(&path, &vault_root, &state).await;
                    }
                }
            }
        }
    });
}

async fn process_file_event(path: &Path, vault_root: &Path, state: &Arc<RwLock<SatzState>>) {
    let rel_path = crate::state::SatzState::get_rel_path(path, Some(vault_root));
    let rel_path_str = rel_path.to_string_lossy().replace('\\', "/");
    let doc_id = satz_core::DocId::new(&rel_path_str);

    if !path.exists() {
        // File was deleted
        let mut s = state.write().await;
        s.index.remove_doc(&doc_id);
        tracing::info!("Watcher: removed deleted document {}", doc_id);
    } else {
        // File created or modified
        // If the file is currently open in editor, editor state takes precedence
        let is_open = {
            let s = state.read().await;
            s.open_docs.values().any(|d| d.path == path)
        };

        if !is_open && let Ok(content) = std::fs::read_to_string(path) {
            let new_doc = satz_core::parse_document(&content, &rel_path);
            let mut s = state.write().await;
            s.index.replace_doc(new_doc);
            tracing::info!("Watcher: re-indexed {}", doc_id);
        }
    }
}

fn is_markdown_file(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.eq_ignore_ascii_case("md") || ext.eq_ignore_ascii_case("markdown"))
        .unwrap_or(false)
}

fn is_ignored_path(path: &Path) -> bool {
    path.components().any(|comp| {
        let s = comp.as_os_str().to_string_lossy();
        s.starts_with('.') && s != "." && s != ".."
    })
}
