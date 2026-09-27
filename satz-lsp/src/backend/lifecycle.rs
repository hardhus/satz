use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::RwLock;
use tower_lsp_server::ls_types::*;
use tower_lsp_server::{Client, jsonrpc};
use tracing_subscriber::EnvFilter;

use crate::convert::uri_to_path;
use crate::state::SatzState;

use super::Backend;
use super::capabilities::{client_supports_document_changes, server_capabilities};
use super::refresh::{publish_for_all, refresh_diagnostics, refresh_semantic_tokens};

/// Which folder the server indexes, and which of the client's workspace folders it leaves out.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct WorkspaceChoice {
    pub root: Option<PathBuf>,
    pub ignored: Vec<PathBuf>,
}

/// One vault per server: the first local workspace folder is the vault, the others are reported as
/// ignored (the same folder listed twice is not "another"). Without folders the deprecated
/// `rootUri` is used.
pub(crate) fn pick_workspace_root(folders: &[String], root_uri: Option<&str>) -> WorkspaceChoice {
    let mut local: Vec<PathBuf> = Vec::new();
    for path in folders.iter().filter_map(|uri| uri_to_path(uri)) {
        if !local.contains(&path) {
            local.push(path);
        }
    }
    if local.is_empty() {
        return WorkspaceChoice {
            root: root_uri.and_then(uri_to_path),
            ignored: Vec::new(),
        };
    }
    let root = local.remove(0);
    WorkspaceChoice {
        root: Some(root),
        ignored: local,
    }
}

/// Tells the user what the first indexing came to: how many notes, or why it failed, and every
/// problem `.satz.toml` had (an unusable file, settings that were ignored).
async fn announce_indexing(client: &Client, outcome: &crate::state::IndexingOutcome) {
    if let Some(failure) = &outcome.failure {
        // The server keeps working for the documents that are open; the reason is shown.
        tracing::error!(error = %failure, "walk_vault: failed");
        let message = format!("satz: indexing failed: {failure}");
        client.log_message(MessageType::ERROR, &message).await;
        client.show_message(MessageType::ERROR, message).await;
    } else {
        tracing::info!(doc_count = outcome.doc_count, "walk_vault: succeeded");
        client
            .log_message(
                MessageType::INFO,
                format!("satz: indexed {} documents", outcome.doc_count),
            )
            .await;
    }

    // A `.satz.toml` that exists but can't be used must be visible: falling back to defaults
    // silently would look like the settings were ignored.
    if let Some(error) = &outcome.config_error {
        let message = crate::state::config_error_message(error, "default");
        client.log_message(MessageType::WARNING, &message).await;
        client.show_message(MessageType::WARNING, message).await;
    }
    // Settings that were ignored (an unknown key, a value outside its choices): the rest of the
    // file applies, so this is the only place the user learns of them.
    if !outcome.config_warnings.is_empty() {
        let message = crate::state::config_warnings_message(&outcome.config_warnings);
        client.log_message(MessageType::WARNING, &message).await;
        client.show_message(MessageType::WARNING, message).await;
    }
}

/// After an indexing that finished while documents were already open: their diagnostics (and, for a
/// client that asks for them, their colours) were computed from a partial index, so they are
/// published or refreshed again. Both refreshes go out at once and neither waits for the other's
/// answer.
///
/// `pub(crate)`: `backend::tests` (a sibling of this module) calls it directly.
pub(crate) async fn refresh_after_first_index(client: &Client, state_arc: &Arc<RwLock<SatzState>>) {
    let (supports_pull, uris) = {
        let s = state_arc.read().await;
        (
            s.client_supports_pull_diagnostics,
            s.open_docs.keys().cloned().collect::<Vec<_>>(),
        )
    };
    if !supports_pull {
        publish_for_all(client, state_arc, &uris).await;
    }
    tokio::join!(refresh_semantic_tokens(client), async {
        if supports_pull {
            refresh_diagnostics(client).await;
        }
    });
}

impl Backend {
    /// Applies a client-requested log level/filter, if valid. Accepts either
    /// a bare level (`"debug"`) or a full `EnvFilter` directive string
    /// (`"satz_lsp=trace,satz_core=debug"`) — `EnvFilter` parses both the
    /// same way, so no extra parsing is needed here.
    fn apply_log_level(&self, level: &str) {
        match EnvFilter::try_new(level) {
            Ok(filter) => {
                if self.log_reload_handle.reload(filter).is_ok() {
                    tracing::info!("satz-lsp log level set to '{level}'");
                }
            }
            Err(e) => {
                tracing::error!("satz-lsp: invalid logLevel '{level}': {e}");
            }
        }
    }
}

pub(crate) async fn initialize(
    backend: &Backend,
    params: InitializeParams,
) -> jsonrpc::Result<InitializeResult> {
    if let Some(level) = params
        .initialization_options
        .as_ref()
        .and_then(|opts| opts.get("logLevel"))
        .and_then(|v| v.as_str())
    {
        backend.apply_log_level(level);
    }

    let folder_uris: Vec<String> = params
        .workspace_folders
        .as_deref()
        .unwrap_or_default()
        .iter()
        .map(|f| f.uri.as_str().to_string())
        .collect();
    // The LSP type marks `root_uri` `#[deprecated]` but the protocol still requires it.
    #[allow(deprecated)]
    let legacy_root = params.root_uri.as_ref().map(|u| u.as_str().to_string());
    let WorkspaceChoice {
        root: vault_root,
        ignored: ignored_folders,
    } = pick_workspace_root(&folder_uris, legacy_root.as_deref());
    if !ignored_folders.is_empty() {
        // One server indexes one vault; say so instead of silently serving only the first.
        let names: Vec<String> = ignored_folders
            .iter()
            .map(|p| p.display().to_string())
            .collect();
        let message = format!(
            "satz indexes one vault per server: using {} and ignoring {}. Start another server for each other folder.",
            vault_root
                .as_deref()
                .map_or_else(String::new, |p| p.display().to_string()),
            names.join(", ")
        );
        tracing::warn!("{message}");
        backend
            .client
            .show_message(MessageType::WARNING, message)
            .await;
    }

    let supports_pull = params
        .capabilities
        .text_document
        .as_ref()
        .and_then(|td| td.diagnostic.as_ref())
        .is_some();
    let supports_document_changes = client_supports_document_changes(&params.capabilities);

    {
        let mut state = backend.state.write().await;
        state.client_supports_pull_diagnostics = supports_pull;
        state.client_supports_document_changes = supports_document_changes;
    }

    tracing::debug!(?vault_root, "initialize: resolved vault root");

    if let Some(root) = vault_root {
        tracing::debug!(vault_root = ?root, "walk_vault: starting");
        // Watching starts BEFORE the walk: what changes while it runs is held back until the
        // index is complete and is then applied from what is on disk.
        let handle = crate::watcher::spawn_watcher(
            root.clone(),
            backend.state.clone(),
            backend.client.clone(),
        );
        if let Some(old) = backend.watcher.lock().unwrap().replace(handle) {
            old.stop();
        }

        // The first indexing is waited for (up to `initialize_wait`) BEFORE the answer to
        // `initialize` goes out. The client sends `didOpen` and its first requests only after
        // that answer, so they all meet a complete index: an editor that asks for the links of
        // the note it opens (Helix's `documentLink`) gets them, where an index that was still
        // empty answered `[]` and the client only asked again after the next edit.
        let job = backend.index_job;
        let root_for_job = root.clone();
        let mut indexing = tokio::task::spawn_blocking(move || job(root_for_job));
        match tokio::time::timeout(backend.initialize_wait, &mut indexing).await {
            Ok(joined) => {
                let result = joined
                    .unwrap_or_else(|e| Err(anyhow::anyhow!("the indexing task panicked: {e}")));
                let outcome = backend.state.write().await.finish_indexing(result, &root);
                // Nothing has been opened yet, so there is nothing to refresh; the server may
                // not send requests before it has answered, so what happened is announced
                // once `initialized` arrives.
                *backend.pending_announcement.lock().unwrap() = Some(outcome);
            }
            Err(_) => {
                // A vault that takes longer than that (or a stalled disk): answer now and
                // finish in the background; documents opened meanwhile are refreshed after.
                tracing::debug!(
                    waited = ?backend.initialize_wait,
                    "initialize: the first indexing is still running; answering anyway"
                );
                let state_arc = backend.state.clone();
                let client = backend.client.clone();
                tokio::task::spawn(async move {
                    let result = indexing.await.unwrap_or_else(|e| {
                        Err(anyhow::anyhow!("the indexing task panicked: {e}"))
                    });
                    let outcome = state_arc.write().await.finish_indexing(result, &root);
                    announce_indexing(&client, &outcome).await;
                    refresh_after_first_index(&client, &state_arc).await;
                });
            }
        }
    } else {
        // No workspace root at all: there is no vault to walk, so there is nothing for
        // `indexing_complete` to wait on — diagnostics can run immediately.
        let mut state = backend.state.write().await;
        state.set_indexing_complete(true);
    }

    Ok(InitializeResult {
        capabilities: server_capabilities(),

        server_info: Some(ServerInfo {
            name: "satz-lsp".to_string(),
            version: Some(env!("CARGO_PKG_VERSION").to_string()),
        }),
        offset_encoding: None,
    })
}

pub(crate) async fn initialized(backend: &Backend) {
    backend
        .client
        .log_message(MessageType::INFO, "satz-lsp initialized")
        .await;
    let outcome = backend.pending_announcement.lock().unwrap().take();
    if let Some(outcome) = outcome {
        announce_indexing(&backend.client, &outcome).await;
    }
}

pub(crate) async fn shutdown(backend: &Backend) -> jsonrpc::Result<()> {
    tracing::debug!("shutdown requested");
    if let Some(watcher) = backend.watcher.lock().unwrap().take() {
        watcher.stop();
    }
    Ok(())
}
