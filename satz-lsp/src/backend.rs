use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::RwLock;
use tower_lsp_server::jsonrpc;
use tower_lsp_server::ls_types::request::{SemanticTokensRefresh, WorkspaceDiagnosticRefresh};
use tower_lsp_server::ls_types::*;
use tower_lsp_server::{Client, LanguageServer};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::reload;

use crate::convert::uri_to_path;
use crate::handlers::diagnostics::compute_diagnostics;
use crate::state::SatzState;

/// Handle to the process-wide log filter, set up in `main`. Lets a single
/// client-supplied `initializationOptions.logLevel` change verbosity at
/// runtime without an env var or a rebuild.
pub type LogReloadHandle = reload::Handle<EnvFilter, tracing_subscriber::Registry>;

/// Whether the client can take versioned document edits (`WorkspaceEdit.documentChanges`).
pub fn client_supports_document_changes(capabilities: &ClientCapabilities) -> bool {
    capabilities
        .workspace
        .as_ref()
        .and_then(|w| w.workspace_edit.as_ref())
        .and_then(|e| e.document_changes)
        .unwrap_or(false)
}

/// Logs a failed refresh request and says whether it succeeded. Nothing is retried: the client refetches on its own schedule anyway.
pub(crate) fn refresh_succeeded<T, E: std::fmt::Display>(
    what: &str,
    result: &Result<T, E>,
) -> bool {
    match result {
        Ok(_) => true,
        Err(error) => {
            // Not a warning: a client that never announced the request may simply not know it.
            tracing::debug!(request = what, %error, "the client did not accept a refresh request");
            false
        }
    }
}

/// How long a refresh request may wait for the client's answer.
pub(crate) const REFRESH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// How a refresh request ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RefreshOutcome {
    Answered,
    Failed,
    TimedOut,
}

/// Sends a refresh request and waits for the answer, at most `limit`: a client that never answers
/// must not hold back what comes after (the other refresh, for instance).
pub(crate) async fn send_refresh<F, T, E>(
    what: &str,
    request: F,
    limit: std::time::Duration,
) -> RefreshOutcome
where
    F: std::future::Future<Output = Result<T, E>>,
    E: std::fmt::Display,
{
    let started = std::time::Instant::now();
    tracing::debug!(request = what, "sending a refresh request");
    match tokio::time::timeout(limit, request).await {
        Ok(result) => {
            let ok = refresh_succeeded(what, &result);
            tracing::debug!(request = what, ok, took = ?started.elapsed(), "refresh request answered");
            if ok {
                RefreshOutcome::Answered
            } else {
                RefreshOutcome::Failed
            }
        }
        Err(_) => {
            tracing::debug!(request = what, after = ?limit, "refresh request not answered in time");
            RefreshOutcome::TimedOut
        }
    }
}

/// Asks a pull-diagnostics client to fetch again. The caller has decided that the client pulls; what
/// the client announced about refreshing does not matter: many clients answer the request (or ignore
/// it harmlessly) without announcing `refreshSupport`, and the refresh after the first indexing is what
/// makes them fetch again.
pub(crate) async fn refresh_diagnostics(client: &Client) {
    send_refresh(
        "workspace/diagnostic/refresh",
        client.send_request::<WorkspaceDiagnosticRefresh>(()),
        REFRESH_TIMEOUT,
    )
    .await;
}

/// Publishes the diagnostics of each of the notes `uris`: what a push client is sent when what
/// they show may have changed.
pub(crate) async fn publish_for_all(
    client: &Client,
    state: &Arc<RwLock<SatzState>>,
    uris: &[String],
) {
    for uri in uris {
        publish_for(client, state, uri).await;
    }
}

/// Tells the client that the diagnostics of the notes `uris` may have changed: a pull client is
/// asked to fetch them again, a push client is sent them.
pub(crate) async fn send_diagnostics_refresh(
    client: &Client,
    state: &Arc<RwLock<SatzState>>,
    supports_pull: bool,
    uris: &[String],
) {
    if supports_pull {
        refresh_diagnostics(client).await;
    } else {
        publish_for_all(client, state, uris).await;
    }
}

/// Asks the client to fetch semantic tokens again: every client is sent this, whatever it announced.
/// The colours of a note opened during the first indexing are computed from an incomplete index
/// (links look unresolved); this request is what makes the client ask again once it is complete. It
/// must not depend on what the client announced (`refreshSupport`, or the semantic token
/// capability): a client that announces neither but still answers it (Helix) would keep the wrong
/// colours. A client that does not know the request just answers with an error, which is logged at
/// debug level.
pub(crate) async fn refresh_semantic_tokens(client: &Client) {
    send_refresh(
        "workspace/semanticTokens/refresh",
        client.send_request::<SemanticTokensRefresh>(()),
        REFRESH_TIMEOUT,
    )
    .await;
}

/// Tells the client that what the open notes show may have changed under them: a pull client is
/// asked to fetch again, a push client is sent the diagnostics of every open note.
pub(crate) async fn refresh_open_documents(client: &Client, state: &Arc<RwLock<SatzState>>) {
    let (supports_pull, uris) = {
        let s = state.read().await;
        (
            s.client_supports_pull_diagnostics,
            s.open_docs.keys().cloned().collect::<Vec<_>>(),
        )
    };
    send_diagnostics_refresh(client, state, supports_pull, &uris).await;
}

/// The day changed under the open notes (midnight passed, or the daily-note settings changed): a
/// `[[bugün]]` now reaches another note, so which daily note is an orphan, and how the links are
/// coloured, may differ. The client is told once, after the request that noticed it is done with
/// the state.
async fn announce_daily_change(client: Client, state: Arc<RwLock<SatzState>>) {
    // Everything the peers depend on is announced here; the next reparse has nothing to add.
    state.write().await.clear_peers_dirty();
    tokio::join!(
        refresh_open_documents(&client, &state),
        refresh_semantic_tokens(&client)
    );
}

/// What the server offers the client (the `capabilities` of the `initialize` response).
pub fn server_capabilities() -> ServerCapabilities {
    ServerCapabilities {
        text_document_sync: Some(TextDocumentSyncCapability::Kind(
            TextDocumentSyncKind::INCREMENTAL,
        )),
        diagnostic_provider: Some(DiagnosticServerCapabilities::Options(DiagnosticOptions {
            identifier: Some("satz".to_string()),
            inter_file_dependencies: true,
            workspace_diagnostics: true,
            work_done_progress_options: WorkDoneProgressOptions::default(),
        })),
        definition_provider: Some(OneOf::Left(true)),
        references_provider: Some(OneOf::Left(true)),
        hover_provider: Some(HoverProviderCapability::Simple(true)),
        document_symbol_provider: Some(OneOf::Left(true)),
        completion_provider: Some(CompletionOptions {
            resolve_provider: Some(true),
            trigger_characters: Some(vec!["[".into(), "#".into(), "^".into()]),
            ..Default::default()
        }),
        workspace_symbol_provider: Some(OneOf::Left(true)),
        rename_provider: Some(OneOf::Right(RenameOptions {
            prepare_provider: Some(true),
            work_done_progress_options: Default::default(),
        })),
        document_highlight_provider: Some(OneOf::Left(true)),
        code_action_provider: Some(CodeActionProviderCapability::Simple(true)),
        document_link_provider: Some(DocumentLinkOptions {
            resolve_provider: Some(false),
            work_done_progress_options: Default::default(),
        }),
        folding_range_provider: Some(FoldingRangeProviderCapability::Simple(true)),
        code_lens_provider: Some(CodeLensOptions {
            resolve_provider: Some(false),
        }),
        inlay_hint_provider: Some(OneOf::Left(true)),
        semantic_tokens_provider: Some(SemanticTokensServerCapabilities::SemanticTokensOptions(
            SemanticTokensOptions {
                work_done_progress_options: Default::default(),
                legend: crate::handlers::semantic_tokens::semantic_tokens_legend(),
                range: None,
                full: Some(SemanticTokensFullOptions::Bool(true)),
            },
        )),
        document_formatting_provider: Some(OneOf::Left(true)),
        execute_command_provider: Some(ExecuteCommandOptions {
            commands: crate::handlers::execute_command::SUPPORTED_COMMANDS
                .iter()
                .map(|c| c.to_string())
                .collect(),
            work_done_progress_options: Default::default(),
        }),
        ..Default::default()
    }
}

/// The debounced reparse of one open document after a change: wait, snapshot the buffer, parse it
/// WITHOUT holding the state lock (readers keep working while a big note is parsed), then apply
/// the result if the buffer has not moved on -- a newer change has its own task -- and refresh
/// diagnostics.
async fn run_reparse(
    state_arc: Arc<RwLock<SatzState>>,
    client: Client,
    uri: String,
    delay: std::time::Duration,
) {
    tokio::time::sleep(delay).await;

    let Some(job) = ({ state_arc.read().await.prepare_reparse(&uri) }) else {
        // Nothing to parse. If a request brought the index up to date first (`read_fresh`), the
        // notifications that follow a reparse are still owed: they are sent from here.
        let peers = {
            let mut state = state_arc.write().await;
            if !state.take_announce_pending(&uri) {
                return;
            }
            state.take_peer_refresh(&uri, true)
        };
        announce_reparse(&client, &state_arc, &uri, peers).await;
        return;
    };
    let version = job.version;
    let parsed = tokio::task::spawn_blocking(move || {
        satz_core::parse_document_owned(job.content, &job.rel_path)
    })
    .await;
    let Ok(new_doc) = parsed else {
        tracing::error!(%uri, "reparse: the parsing task failed");
        return;
    };

    let (applied, peers) = {
        let mut state = state_arc.write().await;
        let applied = state.apply_reparse(&uri, version, new_doc);
        (applied, state.take_peer_refresh(&uri, applied))
    };
    if !applied {
        return; // the buffer changed while parsing; the task of that change takes over
    }

    announce_reparse(&client, &state_arc, &uri, peers).await;
}

/// What follows a reparse that reached the index: the document's own diagnostics, the other open
/// documents' when they depend on it, and the refresh requests to pull clients.
async fn announce_reparse(
    client: &Client,
    state_arc: &Arc<RwLock<SatzState>>,
    uri: &str,
    peers: crate::state::PeerRefresh,
) {
    publish_for(client, state_arc, uri).await;

    let plan = refresh_after_reparse(peers.dirty, peers.supports_pull);
    if plan.push_peers {
        publish_for_all(client, state_arc, &peers.others).await;
    }
    // Neither refresh waits for the other: a client that never answers one must not keep the
    // other from being sent.
    tokio::join!(refresh_semantic_tokens(client), async {
        if plan.pull_diagnostics {
            refresh_diagnostics(client).await;
        }
    });
}

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
async fn refresh_after_first_index(client: &Client, state_arc: &Arc<RwLock<SatzState>>) {
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

pub struct Backend {
    pub client: Client,
    pub state: Arc<RwLock<SatzState>>,
    log_reload_handle: LogReloadHandle,
    /// The running file watcher, if any: a new one replaces (stops) the old, `shutdown` stops it.
    watcher: Arc<std::sync::Mutex<Option<crate::watcher::WatcherHandle>>>,
    /// How long `initialize` waits for the first indexing before it answers anyway.
    initialize_wait: std::time::Duration,
    /// The first indexing itself (replaceable in tests).
    index_job: fn(PathBuf) -> anyhow::Result<SatzState>,
    /// What the first indexing came to, kept until `initialized` may announce it.
    pending_announcement: Arc<std::sync::Mutex<Option<crate::state::IndexingOutcome>>>,
}

/// How long `initialize` waits for the first indexing (a vault that is slower to read than this is
/// indexed in the background, as before).
pub(crate) const INITIALIZE_INDEX_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

/// Computes diagnostics for the specified open document URI and sends them to the client.
pub(crate) async fn publish_for(client: &Client, state: &Arc<RwLock<SatzState>>, uri: &str) {
    let (diagnostics, uri_obj) = {
        let state_guard = state.read().await;

        if state_guard.client_supports_pull_diagnostics {
            return;
        }

        if !state_guard.is_indexing_complete() {
            tracing::debug!(
                uri,
                "publish_for: initial indexing not complete yet, skipping"
            );
            return;
        }

        let Some(open_doc) = state_guard.open_docs.get(uri) else {
            return;
        };
        let doc_id = state_guard.doc_id_for_path(&open_doc.path);

        let Some(doc) = state_guard.index.get_doc(&doc_id) else {
            return;
        };

        let diags = compute_diagnostics(doc, &state_guard.index, &state_guard.config);
        let uri_obj = match uri.parse::<Uri>() {
            Ok(u) => u,
            Err(_) => return,
        };
        (diags, uri_obj)
    };

    client.publish_diagnostics(uri_obj, diagnostics, None).await;
}

impl Backend {
    /// Tells the other open documents' diagnostics to catch up, when what they depend on changed:
    /// a pull client is asked to fetch again, a push client is sent them.
    async fn refresh_peers(&self, peers: &crate::state::PeerRefresh) {
        if !peers.dirty {
            return;
        }
        send_diagnostics_refresh(
            &self.client,
            &self.state,
            peers.supports_pull,
            &peers.others,
        )
        .await;
    }

    /// Read access to a state whose index reflects every open buffer.
    ///
    /// The debounced reparse trails typing by a few hundred milliseconds; a request that maps the
    /// client's (live) positions through the index would then land on the wrong text. So when an
    /// open document is stale it is reparsed here first, under a short write lock. With nothing
    /// stale (the usual case) this is just a read lock.
    pub(crate) async fn read_fresh(&self) -> tokio::sync::RwLockReadGuard<'_, SatzState> {
        let today = chrono::Local::now().date_naive();
        {
            let state = self.state.read().await;
            if !state.has_stale_open_documents() && !state.daily_is_stale(today) {
                return state;
            }
        }
        let mut state = self.state.write().await;
        state.refresh_stale_open_documents();
        // Midnight passed (or the daily settings changed): `[[bugün]]` means another note now.
        let day_moved = state.sync_daily(today);
        // Downgraded, not released and re-acquired: a `did_change` slipping in between would make
        // the state the caller reads stale again.
        let read = state.downgrade();
        if day_moved {
            // Only the request that moves the day gets here (afterwards the index is current),
            // so the client is told once. It cannot be told while this request holds the state.
            tokio::spawn(announce_daily_change(
                self.client.clone(),
                self.state.clone(),
            ));
        }
        read
    }

    pub fn new(client: Client, log_reload_handle: LogReloadHandle) -> Self {
        Self {
            client,
            state: Arc::new(RwLock::new(SatzState::default())),
            log_reload_handle,
            watcher: Default::default(),
            initialize_wait: INITIALIZE_INDEX_WAIT,
            index_job: SatzState::initialize_index,
            pending_announcement: Default::default(),
        }
    }

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

    async fn publish_diagnostics_for_uri(&self, uri: &str) {
        publish_for(&self.client, &self.state, uri).await;
    }
}

impl LanguageServer for Backend {
    async fn initialize(&self, params: InitializeParams) -> jsonrpc::Result<InitializeResult> {
        if let Some(level) = params
            .initialization_options
            .as_ref()
            .and_then(|opts| opts.get("logLevel"))
            .and_then(|v| v.as_str())
        {
            self.apply_log_level(level);
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
            self.client
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
            let mut state = self.state.write().await;
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
                self.state.clone(),
                self.client.clone(),
            );
            if let Some(old) = self.watcher.lock().unwrap().replace(handle) {
                old.stop();
            }

            // The first indexing is waited for (up to `initialize_wait`) BEFORE the answer to
            // `initialize` goes out. The client sends `didOpen` and its first requests only after
            // that answer, so they all meet a complete index: an editor that asks for the links of
            // the note it opens (Helix's `documentLink`) gets them, where an index that was still
            // empty answered `[]` and the client only asked again after the next edit.
            let job = self.index_job;
            let root_for_job = root.clone();
            let mut indexing = tokio::task::spawn_blocking(move || job(root_for_job));
            match tokio::time::timeout(self.initialize_wait, &mut indexing).await {
                Ok(joined) => {
                    let result = joined.unwrap_or_else(|e| {
                        Err(anyhow::anyhow!("the indexing task panicked: {e}"))
                    });
                    let outcome = self.state.write().await.finish_indexing(result, &root);
                    // Nothing has been opened yet, so there is nothing to refresh; the server may
                    // not send requests before it has answered, so what happened is announced
                    // once `initialized` arrives.
                    *self.pending_announcement.lock().unwrap() = Some(outcome);
                }
                Err(_) => {
                    // A vault that takes longer than that (or a stalled disk): answer now and
                    // finish in the background; documents opened meanwhile are refreshed after.
                    tracing::debug!(
                        waited = ?self.initialize_wait,
                        "initialize: the first indexing is still running; answering anyway"
                    );
                    let state_arc = self.state.clone();
                    let client = self.client.clone();
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
            let mut state = self.state.write().await;
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

    async fn initialized(&self, _: InitializedParams) {
        self.client
            .log_message(MessageType::INFO, "satz-lsp initialized")
            .await;
        let outcome = self.pending_announcement.lock().unwrap().take();
        if let Some(outcome) = outcome {
            announce_indexing(&self.client, &outcome).await;
        }
    }

    async fn shutdown(&self) -> jsonrpc::Result<()> {
        tracing::debug!("shutdown requested");
        if let Some(watcher) = self.watcher.lock().unwrap().take() {
            watcher.stop();
        }
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let uri = params.text_document.uri.to_string();
        let content = params.text_document.text;
        let version = params.text_document.version;
        tracing::debug!(%uri, version, "did_open");
        let Some(path) = uri_to_path(&uri) else {
            tracing::warn!(%uri, "did_open: not a local file (untitled buffer, remote or malformed URI); ignoring");
            return;
        };

        let peers = {
            let mut state = self.state.write().await;
            // A note opened first thing after midnight is read on the new day: whether a daily
            // note is an orphan depends on it. Moving the day marks the peers as changed, which
            // `take_peer_refresh` below turns into their refresh.
            state.sync_daily(chrono::Local::now().date_naive());
            state.open_document(&uri, &content, &path, version);
            state.take_peer_refresh(&uri, true)
        };

        self.publish_diagnostics_for_uri(&uri).await;
        self.refresh_peers(&peers).await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri.to_string();
        let version = params.text_document.version;
        tracing::trace!(%uri, version, "did_change");

        // Everything happens under ONE lock: the buffer is updated, the previous reparse task is
        // cancelled and the new one is spawned and stored. (Storing the handle under a second lock
        // let two simultaneous changes overwrite each other's handle and leave a task that nothing
        // could cancel any more.)
        let mut state = self.state.write().await;
        let debounce = std::time::Duration::from_millis(state.config.lsp.reparse_debounce_ms);
        let max_wait = std::time::Duration::from_millis(state.config.lsp.reparse_max_wait_ms);

        let Some(open_doc) = state.open_docs.get_mut(&uri) else {
            return;
        };
        if !open_doc.apply_change_events(version, params.content_changes) {
            return;
        }

        let now = std::time::Instant::now();
        let first = open_doc.first_change_at.get_or_insert(now);
        let elapsed = now.duration_since(*first);
        let delay = crate::state::debounce_delay(debounce, max_wait, elapsed);

        if let Some(previous) = open_doc.pending_task.take() {
            previous.abort();
        }
        open_doc.pending_task = Some(tokio::task::spawn(run_reparse(
            self.state.clone(),
            self.client.clone(),
            uri,
            delay,
        )));
    }

    async fn did_save(&self, params: DidSaveTextDocumentParams) {
        let uri = params.text_document.uri.to_string();
        tracing::debug!(%uri, "did_save");

        let (peers, prev_task) = {
            let mut state = self.state.write().await;
            let prev = if let Some(open_doc) = state.open_docs.get_mut(&uri) {
                open_doc.pending_task.take()
            } else {
                None
            };
            state.reparse_open_document(&uri);
            (state.take_peer_refresh(&uri, true), prev)
        };

        if let Some(task) = prev_task {
            task.abort();
        }

        self.publish_diagnostics_for_uri(&uri).await;
        self.refresh_peers(&peers).await;
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        let uri = params.text_document.uri.to_string();
        let lsp_uri = params.text_document.uri;
        tracing::debug!(%uri, "did_close");
        let peers = {
            let mut state = self.state.write().await;
            state.close_document(&uri);
            state.take_peer_refresh(&uri, true)
        };
        self.client.publish_diagnostics(lsp_uri, vec![], None).await;

        // Discarded unsaved edits change what the remaining documents' diagnostics should say.
        self.refresh_peers(&peers).await;
    }

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> jsonrpc::Result<Option<GotoDefinitionResponse>> {
        let state = self.read_fresh().await;
        Ok(crate::handlers::definition::goto_definition(params, &state))
    }

    async fn references(&self, params: ReferenceParams) -> jsonrpc::Result<Option<Vec<Location>>> {
        let state = self.read_fresh().await;
        Ok(crate::handlers::references::find_references(params, &state))
    }

    async fn hover(&self, params: HoverParams) -> jsonrpc::Result<Option<Hover>> {
        let state = self.read_fresh().await;
        Ok(crate::handlers::hover::hover(params, &state))
    }

    async fn document_symbol(
        &self,
        params: DocumentSymbolParams,
    ) -> jsonrpc::Result<Option<DocumentSymbolResponse>> {
        let state = self.read_fresh().await;
        Ok(crate::handlers::document_symbol::document_symbol(
            params, &state,
        ))
    }

    async fn completion(
        &self,
        params: CompletionParams,
    ) -> jsonrpc::Result<Option<CompletionResponse>> {
        // Completion reads the live text of the buffer itself, so it does not wait for the index to
        // be re-parsed (that would parse the whole note on every `[` typed).
        let state = self.state.read().await;
        Ok(crate::handlers::completion::completion(params, &state))
    }

    async fn completion_resolve(&self, params: CompletionItem) -> jsonrpc::Result<CompletionItem> {
        let state = self.state.read().await;
        Ok(crate::handlers::completion::completion_resolve(
            params, &state,
        ))
    }

    async fn symbol(
        &self,
        params: WorkspaceSymbolParams,
    ) -> jsonrpc::Result<Option<WorkspaceSymbolResponse>> {
        let state = self.read_fresh().await;
        Ok(crate::handlers::workspace_symbol::workspace_symbol(
            params, &state,
        ))
    }

    async fn prepare_rename(
        &self,
        params: TextDocumentPositionParams,
    ) -> jsonrpc::Result<Option<PrepareRenameResponse>> {
        let state = self.read_fresh().await;
        Ok(crate::handlers::rename::prepare_rename(params, &state))
    }

    async fn rename(&self, params: RenameParams) -> jsonrpc::Result<Option<WorkspaceEdit>> {
        let state = self.read_fresh().await;
        crate::handlers::rename::rename(params, &state).map_err(jsonrpc::Error::invalid_params)
    }

    async fn document_highlight(
        &self,
        params: DocumentHighlightParams,
    ) -> jsonrpc::Result<Option<Vec<DocumentHighlight>>> {
        let state = self.read_fresh().await;
        Ok(crate::handlers::document_highlight::document_highlight(
            params, &state,
        ))
    }

    async fn code_action(
        &self,
        params: CodeActionParams,
    ) -> jsonrpc::Result<Option<CodeActionResponse>> {
        let state = self.read_fresh().await;
        Ok(crate::handlers::code_action::code_action(params, &state))
    }

    async fn document_link(
        &self,
        params: DocumentLinkParams,
    ) -> jsonrpc::Result<Option<Vec<DocumentLink>>> {
        let state = self.read_fresh().await;
        Ok(crate::handlers::document_link::document_link(
            params, &state,
        ))
    }

    async fn folding_range(
        &self,
        params: FoldingRangeParams,
    ) -> jsonrpc::Result<Option<Vec<FoldingRange>>> {
        let state = self.read_fresh().await;
        Ok(crate::handlers::folding_range::folding_range(
            params, &state,
        ))
    }

    async fn code_lens(&self, params: CodeLensParams) -> jsonrpc::Result<Option<Vec<CodeLens>>> {
        let state = self.read_fresh().await;
        Ok(crate::handlers::codelens::code_lens(params, &state))
    }

    async fn inlay_hint(&self, params: InlayHintParams) -> jsonrpc::Result<Option<Vec<InlayHint>>> {
        let state = self.read_fresh().await;
        Ok(crate::handlers::inlay_hint::inlay_hint(params, &state))
    }

    async fn semantic_tokens_full(
        &self,
        params: SemanticTokensParams,
    ) -> jsonrpc::Result<Option<SemanticTokensResult>> {
        let state = self.read_fresh().await;
        let answer = crate::handlers::semantic_tokens::semantic_tokens_full(params, &state);
        if let Some(SemanticTokensResult::Tokens(tokens)) = &answer {
            let unresolved = tokens.data.iter().filter(|t| t.token_type == 1).count();
            tracing::debug!(
                indexing_complete = state.is_indexing_complete(),
                tokens = tokens.data.len(),
                unresolved_links = unresolved,
                "semantic_tokens_full"
            );
        }
        Ok(answer)
    }

    async fn formatting(
        &self,
        params: DocumentFormattingParams,
    ) -> jsonrpc::Result<Option<Vec<TextEdit>>> {
        let state = self.read_fresh().await;
        Ok(crate::handlers::formatting::formatting(params, &state))
    }

    async fn execute_command(
        &self,
        params: ExecuteCommandParams,
    ) -> jsonrpc::Result<Option<serde_json::Value>> {
        tracing::debug!(command = %params.command, "execute_command");
        {
            let state = self.state.read().await;
            if let Some(answer) = crate::handlers::execute_command::run_read_only_command(
                &state,
                &params.command,
                &params.arguments,
            ) {
                return match answer {
                    Ok(value) => Ok(Some(value)),
                    Err(reason) => Err(jsonrpc::Error::invalid_params(reason)),
                };
            }
        }
        if params.command != crate::handlers::execute_command::FORMAT_WORKSPACE_COMMAND {
            return Err(jsonrpc::Error::method_not_found());
        }

        // The state is not held while the notes are formatted: an edit typed meanwhile is not kept
        // waiting for the whole vault.
        let result = crate::handlers::execute_command::format_workspace(&self.state).await;

        if !result.cache_updates.is_empty() {
            let mut state = self.state.write().await;
            state.apply_format_cache_updates(result.cache_updates);
        }

        let changes = result.changes;
        if changes.is_empty() {
            self.client
                .log_message(MessageType::INFO, "satz: vault is already fully formatted")
                .await;
            return Ok(Some(serde_json::json!({ "formatted": 0 })));
        }

        let count = changes.len();
        // Open documents carry the version the edits were computed against, when the client can
        // take versioned edits: it then refuses them if the user has typed since.
        let versioned = self.state.read().await.client_supports_document_changes;
        let edit = if versioned {
            crate::handlers::execute_command::build_workspace_edit_versioned(changes)
        } else {
            crate::handlers::execute_command::build_workspace_edit(changes)
        };

        let applied = match self.client.apply_edit(edit).await {
            Ok(response) => response.applied,
            Err(e) => {
                self.client
                    .log_message(
                        MessageType::ERROR,
                        format!("satz: workspace/applyEdit request failed: {}", e),
                    )
                    .await;
                false
            }
        };

        if !applied {
            self.client
                .log_message(
                    MessageType::WARNING,
                    "satz: client did not apply the format-workspace edit",
                )
                .await;
            return Ok(Some(serde_json::json!({ "formatted": 0 })));
        }

        // The server does NOT touch the open documents' buffers here. The client applied the edit
        // to its own buffer and now reports it with `didChange`; changing the rope first would apply
        // the same edit twice. Files that are not open are picked up by the file watcher.
        self.client
            .log_message(
                MessageType::INFO,
                format!("satz: formatted {count} file(s)"),
            )
            .await;

        Ok(Some(serde_json::json!({ "formatted": count })))
    }

    async fn diagnostic(
        &self,
        params: DocumentDiagnosticParams,
    ) -> jsonrpc::Result<DocumentDiagnosticReportResult> {
        let uri = params.text_document.uri.to_string();
        let report = {
            let state = self.read_fresh().await;
            crate::handlers::diagnostics::pull_document_report(
                &uri,
                params.previous_result_id.as_deref(),
                &state,
            )
        };

        use crate::handlers::diagnostics::DocumentPull;
        Ok(DocumentDiagnosticReportResult::Report(match report {
            DocumentPull::Full { items, result_id } => {
                DocumentDiagnosticReport::Full(RelatedFullDocumentDiagnosticReport {
                    related_documents: None,
                    full_document_diagnostic_report: FullDocumentDiagnosticReport {
                        result_id,
                        items,
                    },
                })
            }
            DocumentPull::Unchanged { result_id } => {
                DocumentDiagnosticReport::Unchanged(RelatedUnchangedDocumentDiagnosticReport {
                    related_documents: None,
                    unchanged_document_diagnostic_report: UnchangedDocumentDiagnosticReport {
                        result_id,
                    },
                })
            }
        }))
    }

    async fn workspace_diagnostic(
        &self,
        params: WorkspaceDiagnosticParams,
    ) -> jsonrpc::Result<WorkspaceDiagnosticReportResult> {
        let previous: std::collections::HashMap<String, String> = params
            .previous_result_ids
            .into_iter()
            .map(|p| (p.uri.as_str().to_string(), p.value))
            .collect();
        let state = self.read_fresh().await;
        let items = crate::handlers::diagnostics::pull_workspace_report(&previous, &state);

        Ok(WorkspaceDiagnosticReportResult::Report(
            WorkspaceDiagnosticReport { items },
        ))
    }
}

/// What to tell the client after an open document has been re-parsed. (`workspace/semanticTokens/
/// refresh` is not part of the plan: every client is sent it.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RefreshPlan {
    /// Send `workspace/diagnostic/refresh` so a pull client fetches the new results.
    pub pull_diagnostics: bool,
    /// Publish diagnostics for the other open documents (push clients).
    pub push_peers: bool,
}

/// Decides the notifications after a debounced re-parse.
///
/// The client fetches diagnostics and tokens right after each edit -- before the debounced
/// re-parse has updated the index -- so it always holds results one edit old. A pull client is
/// therefore asked to fetch again after EVERY re-parse (not only when other documents are
/// affected), and so is the colouring of every client. A push client already gets its own
/// document's diagnostics published; other documents only when what they depend on changed.
pub(crate) fn refresh_after_reparse(peers_dirty: bool, supports_pull: bool) -> RefreshPlan {
    RefreshPlan {
        pull_diagnostics: supports_pull,
        push_peers: peers_dirty && !supports_pull,
    }
}

#[cfg(test)]
pub(crate) mod tests;
