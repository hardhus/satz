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

/// Whether the client answers `workspace/diagnostic/refresh` (LSP 3.17 `workspace.diagnostics.refreshSupport`).
pub fn client_supports_diagnostic_refresh(capabilities: &ClientCapabilities) -> bool {
    capabilities
        .workspace
        .as_ref()
        .and_then(|w| w.diagnostics.as_ref())
        .and_then(|d| d.refresh_support)
        .unwrap_or(false)
}

/// Whether the client answers `workspace/semanticTokens/refresh`.
pub fn client_supports_semantic_tokens_refresh(capabilities: &ClientCapabilities) -> bool {
    capabilities
        .workspace
        .as_ref()
        .and_then(|w| w.semantic_tokens.as_ref())
        .and_then(|s| s.refresh_support)
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

/// Whether `workspace/diagnostic/refresh` is worth sending: to a client that pulls diagnostics.
/// `refreshSupport` is not required: many clients answer the request (or ignore it harmlessly)
/// without announcing it, and the refresh after the first indexing is what makes them fetch again.
pub(crate) fn diagnostic_refresh_wanted(state: &SatzState) -> bool {
    state.client_supports_pull_diagnostics
}

/// Whether `workspace/semanticTokens/refresh` is sent: always, to every client. The colours of a
/// note opened during the first indexing are computed from an incomplete index (links look
/// unresolved); this request is what makes the client ask again once it is complete. It must not
/// depend on what the client announced (`refreshSupport`, or the semantic token capability): a
/// client that announces neither but still answers it (Helix) would keep the wrong colours. A client
/// that does not know the request just answers with an error, which is logged at debug level.
pub(crate) fn semantic_tokens_refresh_wanted(_state: &SatzState) -> bool {
    true
}

/// Asks a pull-diagnostics client to fetch again.
pub(crate) async fn refresh_diagnostics(client: &Client, state: &Arc<RwLock<SatzState>>) {
    if diagnostic_refresh_wanted(&*state.read().await) {
        send_refresh(
            "workspace/diagnostic/refresh",
            client.send_request::<WorkspaceDiagnosticRefresh>(()),
            REFRESH_TIMEOUT,
        )
        .await;
    }
}

/// Asks the client to fetch semantic tokens again.
pub(crate) async fn refresh_semantic_tokens(client: &Client, state: &Arc<RwLock<SatzState>>) {
    if semantic_tokens_refresh_wanted(&*state.read().await) {
        send_refresh(
            "workspace/semanticTokens/refresh",
            client.send_request::<SemanticTokensRefresh>(()),
            REFRESH_TIMEOUT,
        )
        .await;
    }
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
    if supports_pull {
        refresh_diagnostics(client, state).await;
    } else {
        for uri in uris {
            publish_for(client, state, &uri).await;
        }
    }
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
        refresh_semantic_tokens(&client, &state)
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
        for other_uri in &peers.others {
            publish_for(client, state_arc, other_uri).await;
        }
    }
    // Neither refresh waits for the other: a client that never answers one must not keep the
    // other from being sent.
    tokio::join!(
        async {
            if plan.semantic_tokens {
                refresh_semantic_tokens(client, state_arc).await;
            }
        },
        async {
            if plan.pull_diagnostics {
                refresh_diagnostics(client, state_arc).await;
            }
        }
    );
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
        for uri in uris {
            publish_for(client, state_arc, &uri).await;
        }
    }
    tokio::join!(refresh_semantic_tokens(client, state_arc), async {
        if supports_pull {
            refresh_diagnostics(client, state_arc).await;
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
        let rel_path = SatzState::get_rel_path(&open_doc.path, state_guard.vault_root());
        let rel_path_str = rel_path.to_string_lossy().replace('\\', "/");
        let doc_id = satz_core::DocId::new(&rel_path_str);

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
        if peers.supports_pull {
            refresh_diagnostics(&self.client, &self.state).await;
        } else {
            for other_uri in &peers.others {
                publish_for(&self.client, &self.state, other_uri).await;
            }
        }
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
            state.client_supports_diagnostic_refresh =
                client_supports_diagnostic_refresh(&params.capabilities);
            state.client_supports_semantic_tokens_refresh =
                client_supports_semantic_tokens_refresh(&params.capabilities);
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

        let result = {
            let state = self.state.read().await;
            crate::handlers::execute_command::compute_format_changes(&state)
        };

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
            crate::handlers::execute_command::build_workspace_edit_versioned(&changes)
        } else {
            crate::handlers::execute_command::build_workspace_edit(&changes)
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

/// What to tell the client after an open document has been re-parsed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RefreshPlan {
    /// Send `workspace/diagnostic/refresh` so a pull client fetches the new results.
    pub pull_diagnostics: bool,
    /// Publish diagnostics for the other open documents (push clients).
    pub push_peers: bool,
    /// Send `workspace/semanticTokens/refresh`.
    pub semantic_tokens: bool,
}

/// Decides the notifications after a debounced re-parse.
///
/// The client fetches diagnostics and tokens right after each edit -- before the debounced
/// re-parse has updated the index -- so it always holds results one edit old. A pull client is
/// therefore asked to fetch again after EVERY re-parse (not only when other documents are
/// affected), and semantic tokens are refreshed too. A push client already gets its own
/// document's diagnostics published; other documents only when what they depend on changed.
pub(crate) fn refresh_after_reparse(peers_dirty: bool, supports_pull: bool) -> RefreshPlan {
    plan_refresh(peers_dirty, supports_pull, true, true)
}

/// Like `refresh_after_reparse`, for a client that may not understand the refresh requests: they
/// are only planned when it said it does (the client then fetches on its own schedule).
pub(crate) fn plan_refresh(
    peers_dirty: bool,
    supports_pull: bool,
    refresh_diagnostics: bool,
    refresh_semantic_tokens: bool,
) -> RefreshPlan {
    RefreshPlan {
        pull_diagnostics: supports_pull && refresh_diagnostics,
        push_peers: peers_dirty && !supports_pull,
        semantic_tokens: refresh_semantic_tokens,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pull_client_is_always_asked_to_refetch_after_a_reparse() {
        for peers_dirty in [false, true] {
            let plan = refresh_after_reparse(peers_dirty, true);
            assert!(plan.pull_diagnostics, "peers_dirty={peers_dirty}");
            assert!(!plan.push_peers, "a pull client is not pushed to");
        }
    }

    #[test]
    fn a_push_client_gets_peers_only_when_they_are_affected() {
        let clean = refresh_after_reparse(false, false);
        assert!(!clean.pull_diagnostics && !clean.push_peers);
        let dirty = refresh_after_reparse(true, false);
        assert!(!dirty.pull_diagnostics && dirty.push_peers);
    }

    #[test]
    fn semantic_tokens_are_refreshed_after_every_reparse() {
        for peers_dirty in [false, true] {
            for supports_pull in [false, true] {
                assert!(
                    refresh_after_reparse(peers_dirty, supports_pull).semantic_tokens,
                    "peers_dirty={peers_dirty} supports_pull={supports_pull}"
                );
            }
        }
    }

    // ---- the server's advertised commands and how execute_command answers them ----

    /// A real `Backend` (its client end is never connected: the paths tested here do not talk to
    /// the client).
    fn test_service() -> tower_lsp_server::LspService<Backend> {
        let (_layer, handle): (_, LogReloadHandle) = reload::Layer::new(EnvFilter::new("off"));
        let (service, _socket) =
            tower_lsp_server::LspService::new(|client| Backend::new(client, handle));
        service
    }

    fn command(name: &str, arguments: Vec<serde_json::Value>) -> ExecuteCommandParams {
        ExecuteCommandParams {
            command: name.to_string(),
            arguments,
            work_done_progress_params: Default::default(),
        }
    }

    fn root() -> PathBuf {
        if cfg!(windows) {
            PathBuf::from("C:\\vault")
        } else {
            PathBuf::from("/vault")
        }
    }

    #[test]
    fn the_server_advertises_exactly_the_supported_commands() {
        let caps = server_capabilities();
        let commands = caps
            .execute_command_provider
            .expect("execute commands are offered")
            .commands;
        let expected: Vec<String> = crate::handlers::execute_command::SUPPORTED_COMMANDS
            .iter()
            .map(|c| c.to_string())
            .collect();
        assert_eq!(commands, expected);
        assert!(commands.contains(&"satz.showBacklinks".to_string()));
        assert!(commands.contains(&"satz.formatWorkspace".to_string()));
    }

    #[tokio::test]
    async fn show_backlinks_answers_with_the_locations_and_rejects_bad_arguments() {
        let service = test_service();
        let backend = service.inner();
        {
            let mut state = backend.state.write().await;
            state.set_vault_root(Some(root()));
            state.index = satz_core::Index::build(vec![
                satz_core::parse_document("# A\n", std::path::Path::new("a.md")),
                satz_core::parse_document("see [[a]]\n", std::path::Path::new("b.md")),
            ]);
        }
        let uri = crate::convert::path_to_uri(&root().join("a.md"))
            .unwrap()
            .as_str()
            .to_string();

        let answer = backend
            .execute_command(command("satz.showBacklinks", vec![serde_json::json!(uri)]))
            .await
            .expect("valid arguments")
            .expect("a value");
        let list = answer.as_array().expect("an array of locations");
        assert_eq!(list.len(), 1);
        assert!(list[0]["uri"].as_str().unwrap().ends_with("b.md"));

        for bad in [
            vec![],
            vec![serde_json::json!(42)],
            vec![serde_json::json!(null)],
            vec![serde_json::json!("")],
            vec![serde_json::json!("not a uri")],
        ] {
            let err = backend
                .execute_command(command("satz.showBacklinks", bad.clone()))
                .await
                .expect_err(&format!("{bad:?} must be rejected"));
            assert_eq!(err.code, jsonrpc::ErrorCode::InvalidParams, "{bad:?}");
            assert!(!err.message.is_empty());
        }
    }

    #[tokio::test]
    async fn an_unknown_or_differently_cased_command_is_method_not_found() {
        let service = test_service();
        let backend = service.inner();
        for name in [
            "satz.nope",
            "",
            "SATZ.SHOWBACKLINKS",
            "satz.showbacklinks",
            "satz.showBacklinks ",
        ] {
            let err = backend
                .execute_command(command(name, vec![]))
                .await
                .expect_err(&format!("{name:?} is not a command"));
            assert_eq!(err.code, jsonrpc::ErrorCode::MethodNotFound, "{name:?}");
        }
    }

    // ---- did_change: one reparse task per document, whoever wins the lock ----

    fn change_params(uri: &str, version: i32, text: &str) -> DidChangeTextDocumentParams {
        DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier {
                uri: uri.parse().unwrap(),
                version,
            },
            content_changes: vec![TextDocumentContentChangeEvent {
                range: None,
                range_length: None,
                text: text.to_string(),
            }],
        }
    }

    /// A backend that can be shared between tasks, with `a.md` open and a debounce so long that no
    /// reparse ever fires during a test.
    async fn shared_backend() -> (Arc<Backend>, tower_lsp_server::LspService<Backend>) {
        let client_slot = std::sync::Mutex::new(None);
        let (_layer, handle): (_, LogReloadHandle) = reload::Layer::new(EnvFilter::new("off"));
        let (service, _socket) = tower_lsp_server::LspService::new(|client| {
            *client_slot.lock().unwrap() = Some(client.clone());
            Backend::new(client, handle)
        });
        let client = client_slot.lock().unwrap().take().unwrap();
        let (_layer2, handle2): (_, LogReloadHandle) = reload::Layer::new(EnvFilter::new("off"));
        let backend = Arc::new(Backend::new(client, handle2));
        {
            let mut state = backend.state.write().await;
            state.set_vault_root(Some(root()));
            state.config.lsp.reparse_debounce_ms = 600_000;
            state.config.lsp.reparse_max_wait_ms = 600_000;
            state.set_indexing_complete(true);
            state.open_document("file:///a.md", "# A\n", &root().join("a.md"), 1);
        }
        (backend, service)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn many_simultaneous_changes_leave_exactly_one_pending_reparse() {
        let (backend, _service) = shared_backend().await;
        let alive_before = tokio::runtime::Handle::current()
            .metrics()
            .num_alive_tasks();

        let mut changes = Vec::new();
        for i in 0..64 {
            let backend = backend.clone();
            changes.push(tokio::spawn(async move {
                backend
                    .did_change(change_params(
                        "file:///a.md",
                        2,
                        &format!("# A\n\n[[n{i}]]\n"),
                    ))
                    .await;
            }));
        }
        for change in changes {
            change.await.unwrap();
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let alive_after = tokio::runtime::Handle::current()
            .metrics()
            .num_alive_tasks();
        assert_eq!(
            alive_after - alive_before,
            1,
            "every earlier reparse must have been cancelled: only the stored one may live"
        );
        let state = backend.state.read().await;
        let pending = state.open_docs["file:///a.md"].pending_task.as_ref();
        assert!(pending.is_some_and(|task| !task.is_finished()));
    }

    #[tokio::test]
    async fn a_change_stores_its_reparse_task_before_it_returns() {
        let (backend, _service) = shared_backend().await;
        backend
            .did_change(change_params("file:///a.md", 2, "# A\n\nfirst\n"))
            .await;
        let first = {
            let state = backend.state.read().await;
            let task = state.open_docs["file:///a.md"]
                .pending_task
                .as_ref()
                .unwrap();
            task.abort_handle()
        };
        backend
            .did_change(change_params("file:///a.md", 3, "# A\n\nsecond\n"))
            .await;
        // The first task was cancelled by the second change and a new one is stored.
        for _ in 0..50 {
            if first.is_finished() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(first.is_finished());
        let state = backend.state.read().await;
        let second = state.open_docs["file:///a.md"]
            .pending_task
            .as_ref()
            .unwrap();
        assert!(!second.is_finished());
    }

    #[tokio::test]
    async fn changes_that_cannot_be_applied_start_no_task() {
        let (backend, _service) = shared_backend().await;
        // Unknown document.
        backend
            .did_change(change_params("file:///unknown.md", 2, "x"))
            .await;
        // A version older than the buffer's is stale.
        backend
            .did_change(change_params("file:///a.md", 5, "# A\n\nfive\n"))
            .await;
        let first = {
            let state = backend.state.read().await;
            state.open_docs["file:///a.md"]
                .pending_task
                .as_ref()
                .unwrap()
                .abort_handle()
        };
        backend
            .did_change(change_params("file:///a.md", 4, "# A\n\nstale\n"))
            .await;
        let state = backend.state.read().await;
        assert!(
            !first.is_finished(),
            "a stale change must not cancel the pending task"
        );
        assert_eq!(
            state.open_docs["file:///a.md"].rope.to_string(),
            "# A\n\nfive\n"
        );
    }

    #[test]
    fn versioned_edits_are_used_only_when_the_client_says_it_supports_them() {
        let mut caps = ClientCapabilities::default();
        assert!(!client_supports_document_changes(&caps));
        caps.workspace = Some(WorkspaceClientCapabilities::default());
        assert!(!client_supports_document_changes(&caps));
        caps.workspace.as_mut().unwrap().workspace_edit = Some(WorkspaceEditClientCapabilities {
            document_changes: Some(false),
            ..Default::default()
        });
        assert!(!client_supports_document_changes(&caps));
        caps.workspace.as_mut().unwrap().workspace_edit = Some(WorkspaceEditClientCapabilities {
            document_changes: Some(true),
            ..Default::default()
        });
        assert!(client_supports_document_changes(&caps));
    }

    // ---- refresh requests only go to clients that said they understand them ----

    fn caps_with(
        diagnostics: Option<Option<bool>>,
        semantic_tokens: Option<Option<bool>>,
    ) -> ClientCapabilities {
        ClientCapabilities {
            workspace: Some(WorkspaceClientCapabilities {
                diagnostics: diagnostics.map(|refresh_support| {
                    DiagnosticWorkspaceClientCapabilities { refresh_support }
                }),
                semantic_tokens: semantic_tokens.map(|refresh_support| {
                    SemanticTokensWorkspaceClientCapabilities { refresh_support }
                }),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn refresh_support_is_read_from_the_client_capabilities() {
        let nothing = ClientCapabilities::default();
        assert!(!client_supports_diagnostic_refresh(&nothing));
        assert!(!client_supports_semantic_tokens_refresh(&nothing));
        // A workspace section without the entries.
        let empty = caps_with(None, None);
        assert!(!client_supports_diagnostic_refresh(&empty));
        assert!(!client_supports_semantic_tokens_refresh(&empty));
        // The entries without the flag, and with it false.
        for flag in [None, Some(false)] {
            let caps = caps_with(Some(flag), Some(flag));
            assert!(!client_supports_diagnostic_refresh(&caps), "{flag:?}");
            assert!(!client_supports_semantic_tokens_refresh(&caps), "{flag:?}");
        }
        // Each one independently.
        let only_diag = caps_with(Some(Some(true)), Some(Some(false)));
        assert!(client_supports_diagnostic_refresh(&only_diag));
        assert!(!client_supports_semantic_tokens_refresh(&only_diag));
        let only_tokens = caps_with(None, Some(Some(true)));
        assert!(!client_supports_diagnostic_refresh(&only_tokens));
        assert!(client_supports_semantic_tokens_refresh(&only_tokens));
    }

    #[test]
    fn a_refresh_is_planned_only_when_the_client_supports_it() {
        for peers_dirty in [false, true] {
            for supports_pull in [false, true] {
                for refresh_diag in [false, true] {
                    for refresh_tokens in [false, true] {
                        let plan =
                            plan_refresh(peers_dirty, supports_pull, refresh_diag, refresh_tokens);
                        let label = format!(
                            "dirty={peers_dirty} pull={supports_pull} diag={refresh_diag} tokens={refresh_tokens}"
                        );
                        assert_eq!(
                            plan.pull_diagnostics,
                            supports_pull && refresh_diag,
                            "{label}"
                        );
                        assert_eq!(plan.push_peers, peers_dirty && !supports_pull, "{label}");
                        assert_eq!(plan.semantic_tokens, refresh_tokens, "{label}");
                    }
                }
            }
        }
    }

    #[test]
    fn the_old_planning_function_assumes_a_client_that_supports_everything() {
        for peers_dirty in [false, true] {
            for supports_pull in [false, true] {
                assert_eq!(
                    refresh_after_reparse(peers_dirty, supports_pull),
                    plan_refresh(peers_dirty, supports_pull, true, true)
                );
            }
        }
    }

    #[test]
    fn a_failed_refresh_request_is_reported_not_swallowed() {
        let ok: Result<(), String> = Ok(());
        assert!(refresh_succeeded("workspace/diagnostic/refresh", &ok));
        let failed: Result<(), String> = Err("method not found".to_string());
        assert!(!refresh_succeeded("workspace/diagnostic/refresh", &failed));
    }

    // ---- requests see the buffer, not the last debounced parse ----

    fn type_full(state: &mut SatzState, uri: &str, version: i32, text: &str) {
        let open = state.open_docs.get_mut(uri).unwrap();
        assert!(open.apply_change_events(
            version,
            vec![TextDocumentContentChangeEvent {
                range: None,
                range_length: None,
                text: text.to_string(),
            }],
        ));
    }

    fn position_params(uri: &str, line: u32, character: u32) -> TextDocumentPositionParams {
        TextDocumentPositionParams {
            text_document: TextDocumentIdentifier {
                uri: uri.parse().unwrap(),
            },
            position: Position::new(line, character),
        }
    }

    /// `a.md` is open; its buffer already has a link to `b.md` that the index does not know yet.
    async fn backend_with_an_unparsed_link() -> (Arc<Backend>, tower_lsp_server::LspService<Backend>)
    {
        let (backend, service) = shared_backend().await;
        {
            let mut state = backend.state.write().await;
            state.index.replace_doc(satz_core::parse_document(
                "# B\n\nbody of b\n",
                std::path::Path::new("b.md"),
            ));
            type_full(&mut state, "file:///a.md", 2, "# A\n\nsee [[b]] here\n");
            assert!(state.has_stale_open_documents());
        }
        (backend, service)
    }

    #[tokio::test]
    async fn go_to_definition_follows_a_link_typed_a_moment_ago() {
        let (backend, _service) = backend_with_an_unparsed_link().await;
        let found = backend
            .goto_definition(GotoDefinitionParams {
                text_document_position_params: position_params("file:///a.md", 2, 7),
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
            })
            .await
            .unwrap();
        let Some(GotoDefinitionResponse::Scalar(location)) = found else {
            panic!("the fresh link must resolve: {found:?}");
        };
        assert!(location.uri.as_str().ends_with("b.md"));
    }

    #[tokio::test]
    async fn hover_shows_the_note_a_freshly_typed_link_points_at() {
        let (backend, _service) = backend_with_an_unparsed_link().await;
        let hover = backend
            .hover(HoverParams {
                text_document_position_params: position_params("file:///a.md", 2, 7),
                work_done_progress_params: Default::default(),
            })
            .await
            .unwrap()
            .expect("a hover for the fresh link");
        let HoverContents::Markup(markup) = hover.contents else {
            panic!()
        };
        assert!(markup.value.contains("body of b"), "{}", markup.value);
    }

    #[tokio::test]
    async fn references_and_highlights_use_the_buffers_positions() {
        let (backend, _service) = backend_with_an_unparsed_link().await;
        let highlights = backend
            .document_highlight(DocumentHighlightParams {
                text_document_position_params: position_params("file:///a.md", 2, 7),
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
            })
            .await
            .unwrap()
            .expect("the link is highlighted");
        assert_eq!(highlights.len(), 1);
        assert_eq!(highlights[0].range.start, Position::new(2, 4));
        assert_eq!(highlights[0].range.end, Position::new(2, 9));
    }

    #[tokio::test]
    async fn folding_uses_the_lines_of_the_buffer() {
        let (backend, _service) = shared_backend().await;
        {
            let mut state = backend.state.write().await;
            type_full(
                &mut state,
                "file:///a.md",
                2,
                "# A\n\ntext\n\n## Sub\n\nmore\n",
            );
        }
        let folds = backend
            .folding_range(FoldingRangeParams {
                text_document: TextDocumentIdentifier {
                    uri: "file:///a.md".parse().unwrap(),
                },
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
            })
            .await
            .unwrap()
            .unwrap_or_default();
        assert_eq!(
            folds
                .iter()
                .map(|f| (f.start_line, f.end_line))
                .collect::<Vec<_>>(),
            vec![(0, 6), (4, 6)]
        );
    }

    #[tokio::test]
    async fn a_rename_lands_on_the_right_lines_after_the_buffer_moved() {
        let (backend, _service) = shared_backend().await;
        let old = "# Old\n\n[[a#Old]]\n";
        let typed = "\n\n# Old\n\n[[a#Old]]\n"; // two lines added above; not reparsed yet
        {
            let mut state = backend.state.write().await;
            state.open_document("file:///a.md", old, &root().join("a.md"), 1);
            type_full(&mut state, "file:///a.md", 2, typed);
        }
        let edit = backend
            .rename(RenameParams {
                text_document_position: position_params("file:///a.md", 2, 3),
                new_name: "New".to_string(),
                work_done_progress_params: Default::default(),
            })
            .await
            .unwrap()
            .expect("an edit");
        let Some(DocumentChanges::Operations(ops)) = edit.document_changes else {
            panic!("versioned edits expected: {edit:?}");
        };
        let mut result = typed.to_string();
        let mut seen_version = None;
        for op in ops {
            let DocumentChangeOperation::Edit(doc_edit) = op else {
                continue;
            };
            seen_version = doc_edit.text_document.version;
            let edits: Vec<TextEdit> = doc_edit
                .edits
                .into_iter()
                .map(|e| match e {
                    OneOf::Left(t) => t,
                    OneOf::Right(a) => a.text_edit,
                })
                .collect();
            result = crate::convert::apply_text_edits(&result, &edits);
        }
        assert_eq!(result, "\n\n# New\n\n[[a#New]]\n");
        assert_eq!(
            seen_version,
            Some(2),
            "the edit names the buffer version it was computed for"
        );
    }

    #[tokio::test]
    async fn requests_without_a_stale_buffer_do_not_take_the_write_lock() {
        let (backend, _service) = shared_backend().await;
        let read = backend.read_fresh().await;
        // Fast path: with a read guard held, nobody could have refreshed under a write lock.
        assert!(backend.state.try_write().is_err());
        assert!(!read.has_stale_open_documents());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn simultaneous_requests_and_edits_stay_consistent() {
        let (backend, _service) = shared_backend().await;
        let mut jobs = Vec::new();
        for i in 0..24 {
            let backend = backend.clone();
            jobs.push(tokio::spawn(async move {
                if i % 3 == 0 {
                    backend
                        .did_change(change_params(
                            "file:///a.md",
                            2 + i,
                            &format!("# A\n\n[[n{i}]]\n"),
                        ))
                        .await;
                } else {
                    let _ = backend
                        .hover(HoverParams {
                            text_document_position_params: position_params("file:///a.md", 2, 3),
                            work_done_progress_params: Default::default(),
                        })
                        .await;
                }
            }));
        }
        for job in jobs {
            tokio::time::timeout(std::time::Duration::from_secs(10), job)
                .await
                .expect("no deadlock")
                .unwrap();
        }
        // A last request leaves the index in step with the buffer.
        let _ = backend
            .hover(HoverParams {
                text_document_position_params: position_params("file:///a.md", 2, 3),
                work_done_progress_params: Default::default(),
            })
            .await;
        assert!(!backend.state.read().await.has_stale_open_documents());
    }

    /// `a.md` and `p.md` (which links to `a`) are open, with a debounce short enough to fire
    /// inside a test.
    async fn backend_with_a_peer_and_a_short_debounce()
    -> (Arc<Backend>, tower_lsp_server::LspService<Backend>) {
        let (backend, service) = shared_backend().await;
        backend
            .did_open(open_params("file:///p.md", 1, "# P\n\n[[a]]\n"))
            .await;
        {
            let mut state = backend.state.write().await;
            state.config.lsp.reparse_debounce_ms = 40;
            state.config.lsp.reparse_max_wait_ms = 40;
        }
        (backend, service)
    }

    #[tokio::test]
    async fn a_request_before_the_debounce_leaves_the_peer_flag_to_the_debounced_task() {
        // The request reparses the stale buffer itself (`read_fresh`); the debounced task then
        // finds nothing to parse. It must still tell the other open documents that what they
        // depend on (here: the title `a` is linked by) changed -- and consume the flag.
        let (backend, _service) = backend_with_a_peer_and_a_short_debounce().await;
        backend
            .did_change(change_params("file:///a.md", 2, "# Renamed\n"))
            .await;
        let _ = backend
            .hover(HoverParams {
                text_document_position_params: position_params("file:///a.md", 0, 3),
                work_done_progress_params: Default::default(),
            })
            .await;
        assert!(
            backend.state.read().await.peers_dirty(),
            "the request's own reparse changed what peers depend on"
        );

        tokio::time::sleep(std::time::Duration::from_millis(400)).await;

        assert!(
            !backend.state.read().await.peers_dirty(),
            "the debounced task must have told the peers and consumed the flag"
        );
    }

    // ---- what the client actually receives (a real JSON-RPC connection over an in-memory pipe) ----

    async fn write_frame(
        out: &mut (impl tokio::io::AsyncWrite + Unpin),
        message: serde_json::Value,
    ) {
        use tokio::io::AsyncWriteExt;
        let body = message.to_string();
        let frame = format!("Content-Length: {}\r\n\r\n{body}", body.len());
        out.write_all(frame.as_bytes()).await.unwrap();
    }

    async fn read_frame(
        input: &mut (impl tokio::io::AsyncBufRead + Unpin),
    ) -> Option<serde_json::Value> {
        use tokio::io::{AsyncBufReadExt, AsyncReadExt};
        let mut length = None;
        loop {
            let mut line = String::new();
            if input.read_line(&mut line).await.ok()? == 0 {
                return None;
            }
            let line = line.trim();
            if line.is_empty() {
                break;
            }
            if let Some(value) = line.strip_prefix("Content-Length:") {
                length = value.trim().parse::<usize>().ok();
            }
        }
        let mut body = vec![0; length?];
        input.read_exact(&mut body).await.ok()?;
        serde_json::from_slice(&body).ok()
    }

    /// Everything the server sends for `quiet` without a pause: `(method, uri of the document)`.
    async fn sent_meanwhile(
        input: &mut (impl tokio::io::AsyncBufRead + Unpin),
        quiet: std::time::Duration,
    ) -> Vec<(String, Option<String>)> {
        let mut seen = Vec::new();
        while let Ok(Some(frame)) = tokio::time::timeout(quiet, read_frame(input)).await {
            if let Some(method) = frame["method"].as_str() {
                let uri = frame["params"]["uri"].as_str().map(str::to_string);
                seen.push((method.to_string(), uri));
            }
        }
        seen
    }

    /// `shared_backend`'s setup (a.md open, indexing done) but with a connected push client
    /// (it announced no pull-diagnostics support) whose incoming messages can be read.
    async fn connected_backend() -> (
        Arc<Backend>,
        tokio::io::BufReader<tokio::io::ReadHalf<tokio::io::DuplexStream>>,
    ) {
        let client_slot = std::sync::Mutex::new(None);
        let (_layer, handle): (_, LogReloadHandle) = reload::Layer::new(EnvFilter::new("off"));
        let (service, socket) = tower_lsp_server::LspService::new(|client| {
            *client_slot.lock().unwrap() = Some(client.clone());
            Backend::new(client, handle)
        });
        let client = client_slot.lock().unwrap().take().unwrap();
        let (client_io, server_io) = tokio::io::duplex(1 << 20);
        let (server_read, server_write) = tokio::io::split(server_io);
        tokio::spawn(async move {
            tower_lsp_server::Server::new(server_read, server_write, socket)
                .serve(service)
                .await;
        });
        let (client_read, mut client_write) = tokio::io::split(client_io);
        let mut client_read = tokio::io::BufReader::new(client_read);

        // The handshake: a client is only sent anything once it has initialized the server.
        write_frame(
            &mut client_write,
            serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
                "params": {"processId": null, "rootUri": null, "capabilities": {}}}),
        )
        .await;
        loop {
            let frame = read_frame(&mut client_read)
                .await
                .expect("initialize answer");
            if frame["id"] == 1 {
                break;
            }
        }
        write_frame(
            &mut client_write,
            serde_json::json!({"jsonrpc": "2.0", "method": "initialized", "params": {}}),
        )
        .await;
        // `client_write` stays open for as long as the test (leaked: a closed pipe would end the
        // server).
        std::mem::forget(client_write);

        let (_layer2, handle2): (_, LogReloadHandle) = reload::Layer::new(EnvFilter::new("off"));
        let backend = Arc::new(Backend::new(client, handle2));
        {
            let mut state = backend.state.write().await;
            state.set_vault_root(Some(root()));
            state.set_indexing_complete(true);
            state.open_document("file:///a.md", "# A\n", &root().join("a.md"), 1);
        }
        (backend, client_read)
    }

    #[tokio::test]
    async fn a_request_before_the_debounce_still_gets_its_diagnostics_and_refreshes_sent() {
        let (backend, mut from_server) = connected_backend().await;
        backend
            .did_open(open_params("file:///p.md", 1, "# P\n\n[[a]]\n"))
            .await;
        {
            let mut state = backend.state.write().await;
            state.config.lsp.reparse_debounce_ms = 40;
            state.config.lsp.reparse_max_wait_ms = 40;
        }
        // What the handshake and the opening sent is not what is asserted below.
        let _ = sent_meanwhile(&mut from_server, std::time::Duration::from_millis(300)).await;

        backend
            .did_change(change_params("file:///a.md", 2, "# Renamed\n"))
            .await;
        let _ = backend
            .hover(HoverParams {
                text_document_position_params: position_params("file:///a.md", 0, 3),
                work_done_progress_params: Default::default(),
            })
            .await;
        let sent = sent_meanwhile(&mut from_server, std::time::Duration::from_millis(400)).await;

        let published = |uri: &str| {
            sent.iter().any(|(method, target)| {
                method == "textDocument/publishDiagnostics" && target.as_deref() == Some(uri)
            })
        };
        assert!(
            published("file:///a.md"),
            "the edited note's own diagnostics: {sent:?}"
        );
        assert!(
            published("file:///p.md"),
            "the open note that links to the renamed one: {sent:?}"
        );
        assert!(
            sent.iter()
                .any(|(method, _)| method == "workspace/semanticTokens/refresh"),
            "the client is asked for fresh colours: {sent:?}"
        );
    }

    #[tokio::test]
    async fn without_a_request_the_debounced_task_announces_once() {
        // The ordinary path, which must not change: typing, the debounce fires, one round of
        // notifications (no duplicate from the request-refresh debt).
        let (backend, mut from_server) = connected_backend().await;
        backend
            .did_open(open_params("file:///p.md", 1, "# P\n\n[[a]]\n"))
            .await;
        {
            let mut state = backend.state.write().await;
            state.config.lsp.reparse_debounce_ms = 40;
            state.config.lsp.reparse_max_wait_ms = 40;
        }
        let _ = sent_meanwhile(&mut from_server, std::time::Duration::from_millis(300)).await;

        backend
            .did_change(change_params("file:///a.md", 2, "# Renamed\n"))
            .await;
        let sent = sent_meanwhile(&mut from_server, std::time::Duration::from_millis(400)).await;

        let count = |method: &str, uri: Option<&str>| {
            sent.iter()
                .filter(|(m, u)| m == method && (uri.is_none() || u.as_deref() == uri))
                .count()
        };
        assert_eq!(
            count("textDocument/publishDiagnostics", Some("file:///a.md")),
            1,
            "{sent:?}"
        );
        assert_eq!(
            count("textDocument/publishDiagnostics", Some("file:///p.md")),
            1,
            "{sent:?}"
        );
        assert_eq!(
            count("workspace/semanticTokens/refresh", None),
            1,
            "{sent:?}"
        );
    }

    // ---- debounce and max-wait, through the real `did_change` ----

    /// The title the index holds for the open `a.md`.
    async fn indexed_title_of_a(backend: &Backend) -> String {
        let state = backend.state.read().await;
        state
            .index
            .get_doc(&satz_core::DocId::new("a.md"))
            .expect("a.md is indexed")
            .title
            .clone()
    }

    #[tokio::test]
    async fn quick_successive_changes_are_parsed_once_with_the_last_text() {
        let (backend, _service) = shared_backend().await;
        {
            let mut state = backend.state.write().await;
            state.config.lsp.reparse_debounce_ms = 50;
            state.config.lsp.reparse_max_wait_ms = 5_000;
        }
        for version in 2..=4 {
            backend
                .did_change(change_params(
                    "file:///a.md",
                    version,
                    &format!("# Version {version}\n"),
                ))
                .await;
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert_eq!(
            indexed_title_of_a(&backend).await,
            "A",
            "the debounce has not run out yet: the index still holds the old text"
        );

        tokio::time::sleep(std::time::Duration::from_millis(400)).await;

        assert_eq!(indexed_title_of_a(&backend).await, "Version 4");
        let state = backend.state.read().await;
        assert!(!state.has_stale_open_documents());
        assert!(
            state.open_docs["file:///a.md"]
                .pending_task
                .as_ref()
                .is_some_and(|task| task.is_finished()),
            "only the reparse of the last change was left, and it is done"
        );
    }

    #[tokio::test]
    async fn typing_without_a_pause_is_still_parsed_once_the_max_wait_is_up() {
        // Every change comes sooner (30 ms) than the debounce (100 ms) would fire, so without the
        // max-wait (250 ms) the index would not move until the typing stops.
        let (backend, _service) = shared_backend().await;
        {
            let mut state = backend.state.write().await;
            state.config.lsp.reparse_debounce_ms = 100;
            state.config.lsp.reparse_max_wait_ms = 250;
        }
        let mut title_while_typing = String::new();
        for i in 0..14 {
            backend
                .did_change(change_params(
                    "file:///a.md",
                    2 + i,
                    &format!("# Typing {i}\n"),
                ))
                .await;
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
            if i == 10 {
                // ~330 ms in and still typing: the max-wait fired at ~250 ms.
                title_while_typing = indexed_title_of_a(&backend).await;
            }
        }
        assert!(
            title_while_typing.starts_with("Typing"),
            "the index never moved while typing went on: {title_while_typing:?}"
        );

        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        assert_eq!(indexed_title_of_a(&backend).await, "Typing 13");
        assert!(!backend.state.read().await.has_stale_open_documents());
    }

    #[tokio::test]
    async fn a_request_after_midnight_moves_the_daily_alias_to_the_new_day() {
        let (backend, _service) = shared_backend().await;
        let today = chrono::Local::now().date_naive();
        {
            let mut state = backend.state.write().await;
            let config = state.config.daily_note.clone();
            state
                .index
                .set_daily(Some((config, today.pred_opt().unwrap())));
            assert!(state.daily_is_stale(today));
        }
        let read = backend.read_fresh().await;
        assert_eq!(read.index.daily().map(|(_, d)| *d), Some(today));
        assert!(!read.daily_is_stale(today));
    }

    // ---- the document lifecycle through the real handlers ----

    fn open_params(uri: &str, version: i32, text: &str) -> DidOpenTextDocumentParams {
        DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: uri.parse().unwrap(),
                language_id: "markdown".to_string(),
                version,
                text: text.to_string(),
            },
        }
    }

    fn close_params(uri: &str) -> DidCloseTextDocumentParams {
        DidCloseTextDocumentParams {
            text_document: TextDocumentIdentifier {
                uri: uri.parse().unwrap(),
            },
        }
    }

    fn range_change(
        uri: &str,
        version: i32,
        range: Range,
        text: &str,
    ) -> DidChangeTextDocumentParams {
        DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier {
                uri: uri.parse().unwrap(),
                version,
            },
            content_changes: vec![TextDocumentContentChangeEvent {
                range: Some(range),
                range_length: None,
                text: text.to_string(),
            }],
        }
    }

    #[tokio::test]
    async fn opening_a_note_indexes_its_buffer_and_closing_it_forgets_a_note_without_a_file() {
        let (backend, _service) = shared_backend().await;
        // Inside the vault root, so the path is the same on every operating system.
        let uri = crate::convert::path_to_uri(&root().join("new-note.md"))
            .unwrap()
            .to_string();
        backend
            .did_open(open_params(&uri, 1, "# New\n\n[[a]]\n"))
            .await;
        {
            let state = backend.state.read().await;
            assert!(state.open_docs.contains_key(&uri));
            assert!(
                state
                    .index
                    .get_doc(&satz_core::DocId::new("new-note.md"))
                    .is_some()
            );
            assert_eq!(
                state
                    .index
                    .backlinks_of(&satz_core::DocId::new("a.md"))
                    .count(),
                1
            );
        }
        backend.did_close(close_params(&uri)).await;
        let state = backend.state.read().await;
        assert!(!state.open_docs.contains_key(&uri));
        assert!(
            state
                .index
                .get_doc(&satz_core::DocId::new("new-note.md"))
                .is_none(),
            "no file on disk: the unsaved note is gone with its buffer"
        );
        assert_eq!(
            state
                .index
                .backlinks_of(&satz_core::DocId::new("a.md"))
                .count(),
            0
        );
    }

    #[tokio::test]
    async fn a_buffer_that_is_not_a_local_file_is_ignored() {
        let (backend, _service) = shared_backend().await;
        let before = backend.state.read().await.open_docs.len();
        backend
            .did_open(open_params("untitled:Untitled-1", 1, "# X\n"))
            .await;
        backend
            .did_open(open_params("https://example.com/x.md", 1, "# X\n"))
            .await;
        assert_eq!(backend.state.read().await.open_docs.len(), before);
    }

    #[tokio::test]
    async fn crlf_text_is_kept_and_ranged_edits_address_its_lines() {
        let (backend, _service) = shared_backend().await;
        backend
            .did_open(open_params("file:///c.md", 1, "one\r\ntwo\r\nthree\r\n"))
            .await;
        // Replace "two" (line 1, columns 0-3) and then insert at the start of line 2.
        backend
            .did_change(range_change(
                "file:///c.md",
                2,
                Range::new(Position::new(1, 0), Position::new(1, 3)),
                "2",
            ))
            .await;
        backend
            .did_change(range_change(
                "file:///c.md",
                3,
                Range::new(Position::new(2, 0), Position::new(2, 0)),
                ">",
            ))
            .await;
        // A range that spans the line break itself.
        backend
            .did_change(range_change(
                "file:///c.md",
                4,
                Range::new(Position::new(0, 3), Position::new(1, 0)),
                " ",
            ))
            .await;
        let state = backend.state.read().await;
        let open = &state.open_docs["file:///c.md"];
        assert_eq!(open.rope.to_string(), "one 2\r\n>three\r\n");
        assert_eq!(open.version, 4);
    }

    #[tokio::test]
    async fn a_change_for_a_note_that_was_never_opened_is_ignored() {
        let (backend, _service) = shared_backend().await;
        backend
            .did_change(change_params("file:///never-opened.md", 2, "# X\n"))
            .await;
        let state = backend.state.read().await;
        assert!(!state.open_docs.contains_key("file:///never-opened.md"));
        assert!(
            state
                .index
                .get_doc(&satz_core::DocId::new("never-opened.md"))
                .is_none()
        );
    }

    #[tokio::test]
    async fn opening_changing_and_closing_consume_the_peer_flag_once() {
        let (backend, _service) = shared_backend().await;
        backend
            .did_open(open_params("file:///p.md", 1, "# P\n\n[[a]]\n"))
            .await;
        assert!(
            !backend.state.read().await.peers_dirty(),
            "consumed by did_open"
        );

        backend.state.write().await.mark_peers_dirty();
        backend
            .did_change(change_params("file:///p.md", 2, "# P\n\n[[nothing]]\n"))
            .await;
        backend.state.write().await.mark_peers_dirty();
        backend.did_close(close_params("file:///p.md")).await;
        assert!(
            !backend.state.read().await.peers_dirty(),
            "consumed by did_close"
        );
    }

    #[tokio::test]
    async fn shutdown_stops_the_running_watcher_and_is_harmless_without_one() {
        let (backend, _service) = shared_backend().await;
        backend.shutdown().await.unwrap();
        let handle = crate::watcher::WatcherHandle::default();
        *backend.watcher.lock().unwrap() = Some(handle.clone());
        backend.shutdown().await.unwrap();
        assert!(handle.is_stopped());
        assert!(backend.watcher.lock().unwrap().is_none());
        backend.shutdown().await.unwrap();
    }

    // ---- which workspace folder is the vault ----

    fn uri(path: &str) -> String {
        crate::convert::path_to_uri(&root().join(path))
            .unwrap()
            .as_str()
            .to_string()
    }

    #[test]
    fn one_folder_is_the_vault_and_nothing_is_ignored() {
        let choice = pick_workspace_root(&[uri("v")], None);
        assert_eq!(choice.root, Some(root().join("v")));
        assert!(choice.ignored.is_empty());
    }

    #[test]
    fn with_several_folders_the_first_is_the_vault_and_the_rest_are_ignored() {
        let choice = pick_workspace_root(&[uri("a"), uri("b"), uri("c")], None);
        assert_eq!(choice.root, Some(root().join("a")));
        assert_eq!(choice.ignored, vec![root().join("b"), root().join("c")]);
    }

    #[test]
    fn the_same_folder_twice_is_not_another_folder() {
        let choice = pick_workspace_root(&[uri("a"), uri("a"), uri("b")], None);
        assert_eq!(choice.root, Some(root().join("a")));
        assert_eq!(choice.ignored, vec![root().join("b")]);
    }

    #[test]
    fn folders_that_are_not_local_files_are_skipped() {
        let choice = pick_workspace_root(
            &[
                "untitled:x".to_string(),
                "https://example.com/w".to_string(),
                uri("real"),
            ],
            None,
        );
        assert_eq!(choice.root, Some(root().join("real")));
        assert!(choice.ignored.is_empty());
    }

    #[test]
    fn without_folders_the_root_uri_is_used_and_without_either_there_is_no_vault() {
        let choice = pick_workspace_root(&[], Some(&uri("legacy")));
        assert_eq!(choice.root, Some(root().join("legacy")));
        assert_eq!(pick_workspace_root(&[], None), WorkspaceChoice::default());
        assert_eq!(
            pick_workspace_root(&["untitled:x".to_string()], None).root,
            None
        );
        // Folders win over the legacy root.
        let both = pick_workspace_root(&[uri("f")], Some(&uri("legacy")));
        assert_eq!(both.root, Some(root().join("f")));
    }

    #[tokio::test]
    async fn completion_answers_from_the_live_buffer_without_waiting_for_a_reparse() {
        // The note is stale (typed since the last parse) and another reader holds the state: a
        // request that had to re-parse first would wait for that reader; completion reads the
        // live text and must not.
        let (backend, _service) = backend_with_an_unparsed_link().await;
        let _other_reader = backend.state.read().await;
        let params = CompletionParams {
            text_document_position: position_params("file:///a.md", 2, 6),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: None,
        };
        let answer = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            backend.completion(params),
        )
        .await;
        assert!(answer.is_ok(), "completion waited for a re-parse");
    }

    // ---- the colour refresh does not depend on what the client announces ----

    fn state_with(pull: bool, diag_refresh: bool, tokens_refresh: bool) -> SatzState {
        let mut state = SatzState::default();
        state.client_supports_pull_diagnostics = pull;
        state.client_supports_diagnostic_refresh = diag_refresh;
        state.client_supports_semantic_tokens_refresh = tokens_refresh;
        state
    }

    #[test]
    fn every_client_is_asked_to_fetch_semantic_tokens_again_whatever_it_announced() {
        // A note opened during the first indexing is coloured from an incomplete index (its links
        // look unresolved); this request after indexing is what fixes the colours. Before the
        // clean-up it was sent unconditionally, and a client that does not announce
        // `refreshSupport` (Helix) then never got it: the links stayed uncoloured on first open.
        for pull in [false, true] {
            for diag_refresh in [false, true] {
                for tokens_refresh in [false, true] {
                    assert!(
                        semantic_tokens_refresh_wanted(&state_with(
                            pull,
                            diag_refresh,
                            tokens_refresh
                        )),
                        "pull={pull} diag_refresh={diag_refresh} tokens_refresh={tokens_refresh}"
                    );
                }
            }
        }
        assert!(
            semantic_tokens_refresh_wanted(&SatzState::default()),
            "a client that announced nothing"
        );
    }

    #[test]
    fn the_refresh_after_indexing_and_the_one_after_a_reparse_both_include_semantic_tokens() {
        for peers_dirty in [false, true] {
            for supports_pull in [false, true] {
                assert!(refresh_after_reparse(peers_dirty, supports_pull).semantic_tokens);
                assert!(plan_refresh(peers_dirty, supports_pull, true, true).semantic_tokens);
            }
        }
    }

    #[test]
    fn a_pull_client_is_refreshed_even_without_announcing_it_and_a_push_client_never() {
        assert!(diagnostic_refresh_wanted(&state_with(true, false, false)));
        assert!(diagnostic_refresh_wanted(&state_with(true, true, true)));
        assert!(!diagnostic_refresh_wanted(&state_with(false, true, true)));
        assert!(!diagnostic_refresh_wanted(&SatzState::default()));
    }

    // ---- the first colours are right, and the refreshes do not wait for each other ----

    #[tokio::test]
    async fn a_refresh_that_is_never_answered_times_out_and_does_not_hold_back_another() {
        let started = std::time::Instant::now();
        let never = send_refresh(
            "a",
            std::future::pending::<Result<(), String>>(),
            std::time::Duration::from_millis(200),
        );
        let quick_one = async {
            let outcome = send_refresh(
                "b",
                async { Ok::<(), String>(()) },
                std::time::Duration::from_secs(5),
            )
            .await;
            (outcome, started.elapsed())
        };
        let (never_outcome, (quick_outcome, quick_took)) = tokio::join!(never, quick_one);
        assert_eq!(never_outcome, RefreshOutcome::TimedOut);
        assert_eq!(quick_outcome, RefreshOutcome::Answered);
        assert!(
            quick_took < std::time::Duration::from_millis(100),
            "{quick_took:?}"
        );
        assert!(started.elapsed() >= std::time::Duration::from_millis(190));
    }

    #[tokio::test]
    async fn an_error_answer_is_reported_as_failed_not_as_answered() {
        let outcome = send_refresh(
            "x",
            async { Err::<(), _>("method not found") },
            std::time::Duration::from_secs(1),
        )
        .await;
        assert_eq!(outcome, RefreshOutcome::Failed);
    }

    // ---- initialize waits for the first indexing, so the first request already sees every note ----

    async fn fresh_backend(
        job: fn(PathBuf) -> anyhow::Result<SatzState>,
        wait: std::time::Duration,
    ) -> (Backend, tower_lsp_server::LspService<Backend>) {
        let client_slot = std::sync::Mutex::new(None);
        let (_layer, handle): (_, LogReloadHandle) = reload::Layer::new(EnvFilter::new("off"));
        let (service, _socket) = tower_lsp_server::LspService::new(|client| {
            *client_slot.lock().unwrap() = Some(client.clone());
            Backend::new(client, handle)
        });
        let client = client_slot.lock().unwrap().take().unwrap();
        let (_layer2, handle2): (_, LogReloadHandle) = reload::Layer::new(EnvFilter::new("off"));
        let mut backend = Backend::new(client, handle2);
        backend.index_job = job;
        backend.initialize_wait = wait;
        (backend, service)
    }

    struct VaultDir(PathBuf);
    impl VaultDir {
        fn new(tag: &str, files: &[(&str, &str)]) -> Self {
            let dir = std::env::temp_dir().join(format!("satz_init_{tag}_{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            for (rel, text) in files {
                let path = dir.join(rel);
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(path, text).unwrap();
            }
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }
    impl Drop for VaultDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn init_params(root: Option<&std::path::Path>) -> InitializeParams {
        InitializeParams {
            workspace_folders: root.map(|r| {
                vec![WorkspaceFolder {
                    uri: crate::convert::path_to_uri(r).unwrap(),
                    name: "vault".to_string(),
                }]
            }),
            ..Default::default()
        }
    }

    fn slow_job(root: PathBuf) -> anyhow::Result<SatzState> {
        std::thread::sleep(std::time::Duration::from_millis(300));
        SatzState::initialize_index(root)
    }

    fn failing_job(_root: PathBuf) -> anyhow::Result<SatzState> {
        anyhow::bail!("the vault folder cannot be read")
    }

    #[tokio::test]
    async fn the_first_document_link_after_initialize_already_has_every_link() {
        // What Helix does: `initialize`, then `didOpen`, then `documentLink` at once. With the
        // index still empty at that moment the answer used to be `[]`, and Helix only asks again
        // after the next edit: the links of the first note stayed uncoloured.
        let vault = VaultDir::new(
            "links",
            &[
                ("tlp/a.md", "# A\n\nsee [[tlp/b]] and [[c#Head]]\n"),
                ("tlp/b.md", "# B\n"),
                ("c.md", "# C\n\n## Head\n"),
            ],
        );
        let (backend, _service) = fresh_backend(slow_job, std::time::Duration::from_secs(10)).await;
        backend
            .initialize(init_params(Some(&vault.0)))
            .await
            .unwrap();
        assert!(backend.state.read().await.is_indexing_complete());
        assert_eq!(backend.state.read().await.index.doc_count(), 3);

        let uri = crate::convert::path_to_uri(&vault.0.join("tlp/a.md")).unwrap();
        backend
            .did_open(open_params(
                uri.as_str(),
                0,
                "# A\n\nsee [[tlp/b]] and [[c#Head]]\n",
            ))
            .await;
        let links = backend
            .document_link(DocumentLinkParams {
                text_document: TextDocumentIdentifier { uri: uri.clone() },
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
            })
            .await
            .unwrap()
            .expect("links");
        let targets: Vec<String> = links
            .iter()
            .map(|l| l.target.as_ref().unwrap().as_str().to_string())
            .collect();
        assert_eq!(targets.len(), 2, "{targets:?}");
        assert!(
            targets[0].ends_with("tlp/b.md") && targets[1].ends_with("c.md"),
            "{targets:?}"
        );
    }

    #[tokio::test]
    async fn a_first_indexing_slower_than_the_limit_does_not_hold_initialize_up() {
        let vault = VaultDir::new("slow", &[("a.md", "# A\n"), ("b.md", "# B\n")]);
        let (backend, _service) =
            fresh_backend(slow_job, std::time::Duration::from_millis(50)).await;
        let started = std::time::Instant::now();
        backend
            .initialize(init_params(Some(&vault.0)))
            .await
            .unwrap();
        assert!(
            started.elapsed() < std::time::Duration::from_millis(250),
            "{:?}",
            started.elapsed()
        );
        assert!(
            !backend.state.read().await.is_indexing_complete(),
            "still indexing"
        );
        // The indexing finishes in the background and the state becomes complete.
        for _ in 0..100 {
            if backend.state.read().await.is_indexing_complete() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert!(backend.state.read().await.is_indexing_complete());
        assert_eq!(backend.state.read().await.index.doc_count(), 2);
    }

    #[tokio::test]
    async fn the_result_of_the_first_indexing_is_announced_once_by_initialized() {
        let vault = VaultDir::new("announce", &[("a.md", "# A\n")]);
        std::fs::write(vault.0.join(".satz.toml"), "[nonsense]\nx = 1\n").unwrap();
        let (backend, _service) = fresh_backend(slow_job, std::time::Duration::from_secs(10)).await;
        backend
            .initialize(init_params(Some(&vault.0)))
            .await
            .unwrap();
        {
            let pending = backend.pending_announcement.lock().unwrap();
            let outcome = pending.as_ref().expect("kept for `initialized`");
            assert_eq!(outcome.doc_count, 1);
            assert_eq!(outcome.config_warnings.len(), 1);
        }
        backend.initialized(InitializedParams {}).await;
        assert!(
            backend.pending_announcement.lock().unwrap().is_none(),
            "announced and cleared"
        );
        backend.initialized(InitializedParams {}).await; // a second one has nothing left to say
        assert!(backend.pending_announcement.lock().unwrap().is_none());
    }

    #[tokio::test]
    async fn an_indexing_that_fails_still_answers_initialize_and_leaves_a_working_server() {
        let vault = VaultDir::new("fail", &[("a.md", "# A\n")]);
        let (backend, _service) =
            fresh_backend(failing_job, std::time::Duration::from_secs(10)).await;
        backend
            .initialize(init_params(Some(&vault.0)))
            .await
            .unwrap();
        assert!(
            backend.state.read().await.is_indexing_complete(),
            "nothing left to wait for"
        );
        let pending = backend.pending_announcement.lock().unwrap();
        assert!(pending.as_ref().unwrap().failure.is_some());
    }

    #[tokio::test]
    async fn without_a_vault_folder_initialize_does_not_wait_at_all() {
        let (backend, _service) = fresh_backend(slow_job, std::time::Duration::from_secs(10)).await;
        let started = std::time::Instant::now();
        backend.initialize(init_params(None)).await.unwrap();
        assert!(started.elapsed() < std::time::Duration::from_millis(200));
        assert!(backend.state.read().await.is_indexing_complete());
        assert!(backend.pending_announcement.lock().unwrap().is_none());
    }

    // ---- the day changes under open notes (T3): what the client is told ----

    /// The index still holds yesterday's date, as it does after midnight until a request moves it.
    async fn make_the_day_stale(backend: &Backend) {
        let mut state = backend.state.write().await;
        let today = chrono::Local::now().date_naive();
        let config = state.config.daily_note.clone();
        state
            .index
            .set_daily(Some((config, today.pred_opt().unwrap())));
    }

    /// The index is up to date with today.
    async fn make_the_day_current(backend: &Backend) {
        let mut state = backend.state.write().await;
        state.sync_daily(chrono::Local::now().date_naive());
        state.clear_peers_dirty();
    }

    async fn hover_a(backend: &Backend) {
        let _ = backend
            .hover(HoverParams {
                text_document_position_params: position_params("file:///a.md", 0, 3),
                work_done_progress_params: Default::default(),
            })
            .await;
    }

    fn count_of(sent: &[(String, Option<String>)], method: &str, uri: Option<&str>) -> usize {
        sent.iter()
            .filter(|(m, u)| m == method && (uri.is_none() || u.as_deref() == uri))
            .count()
    }

    /// Two open notes, the second one linking to the daily note by its alias.
    async fn day_backend() -> (
        Arc<Backend>,
        tokio::io::BufReader<tokio::io::ReadHalf<tokio::io::DuplexStream>>,
    ) {
        let (backend, mut from_server) = connected_backend().await;
        backend
            .did_open(open_params("file:///p.md", 1, "# P\n\n[[today]]\n"))
            .await;
        make_the_day_stale(&backend).await;
        // What the handshake and the opening sent is not what is asserted below.
        let _ = sent_meanwhile(&mut from_server, std::time::Duration::from_millis(300)).await;
        (backend, from_server)
    }

    #[tokio::test]
    async fn a_request_that_moves_the_day_tells_a_push_client_to_refresh_every_open_note() {
        let (backend, mut from_server) = day_backend().await;

        hover_a(&backend).await;
        let sent = sent_meanwhile(&mut from_server, std::time::Duration::from_millis(500)).await;

        for uri in ["file:///a.md", "file:///p.md"] {
            assert_eq!(
                count_of(&sent, "textDocument/publishDiagnostics", Some(uri)),
                1,
                "{uri}: {sent:?}"
            );
        }
        assert_eq!(
            count_of(&sent, "workspace/semanticTokens/refresh", None),
            1,
            "the client is asked for fresh colours: {sent:?}"
        );
    }

    #[tokio::test]
    async fn a_request_that_moves_the_day_tells_a_pull_client_to_fetch_again() {
        let (backend, mut from_server) = day_backend().await;
        backend.state.write().await.client_supports_pull_diagnostics = true;

        hover_a(&backend).await;
        let sent = sent_meanwhile(&mut from_server, std::time::Duration::from_millis(500)).await;

        assert_eq!(
            count_of(&sent, "workspace/diagnostic/refresh", None),
            1,
            "{sent:?}"
        );
        assert_eq!(
            count_of(&sent, "workspace/semanticTokens/refresh", None),
            1,
            "{sent:?}"
        );
        assert_eq!(
            count_of(&sent, "textDocument/publishDiagnostics", None),
            0,
            "a pull client is not sent diagnostics: {sent:?}"
        );
    }

    #[tokio::test]
    async fn a_request_on_the_same_day_tells_nobody_anything() {
        let (backend, mut from_server) = day_backend().await;
        make_the_day_current(&backend).await;

        hover_a(&backend).await;
        hover_a(&backend).await;
        let sent = sent_meanwhile(&mut from_server, std::time::Duration::from_millis(500)).await;

        assert!(
            sent.is_empty(),
            "nothing changed, nothing is sent: {sent:?}"
        );
    }

    #[tokio::test]
    async fn two_requests_that_meet_midnight_together_tell_the_client_once() {
        let (backend, mut from_server) = day_backend().await;

        tokio::join!(hover_a(&backend), hover_a(&backend));
        let sent = sent_meanwhile(&mut from_server, std::time::Duration::from_millis(500)).await;

        for uri in ["file:///a.md", "file:///p.md"] {
            assert_eq!(
                count_of(&sent, "textDocument/publishDiagnostics", Some(uri)),
                1,
                "{uri}: {sent:?}"
            );
        }
        assert_eq!(
            count_of(&sent, "workspace/semanticTokens/refresh", None),
            1,
            "{sent:?}"
        );
    }

    #[tokio::test]
    async fn what_a_refresh_at_midnight_announced_is_not_announced_again_by_the_next_reparse() {
        let (backend, mut from_server) = day_backend().await;
        {
            let mut state = backend.state.write().await;
            state.config.lsp.reparse_debounce_ms = 40;
            state.config.lsp.reparse_max_wait_ms = 40;
        }
        hover_a(&backend).await;
        let _ = sent_meanwhile(&mut from_server, std::time::Duration::from_millis(500)).await;
        assert!(
            !backend.state.read().await.peers_dirty(),
            "the refresh took the flag"
        );

        // An ordinary edit that changes nothing the neighbours depend on: only its own
        // diagnostics go out.
        backend
            .did_change(change_params("file:///a.md", 2, "# A\n\ntext\n"))
            .await;
        let sent = sent_meanwhile(&mut from_server, std::time::Duration::from_millis(500)).await;
        assert_eq!(
            count_of(
                &sent,
                "textDocument/publishDiagnostics",
                Some("file:///p.md")
            ),
            0,
            "{sent:?}"
        );
    }

    /// What a note is told about its links on the day the index holds.
    fn messages_on(
        index: &mut satz_core::Index,
        config: &satz_core::VaultConfig,
        day: chrono::NaiveDate,
        note: &str,
    ) -> Vec<String> {
        index.set_daily(Some((config.daily_note.clone(), day)));
        let doc = index.get_doc(&satz_core::DocId::new(note)).unwrap();
        compute_diagnostics(doc, index, config)
            .iter()
            .map(|d| d.message.clone())
            .collect()
    }

    #[test]
    fn the_day_the_index_holds_decides_which_daily_note_is_an_orphan() {
        // `[[today]]` reaches the daily note of the day the index holds: on yesterday's date
        // today's daily note has no backlink, on today's it has one.
        let today = chrono::Local::now().date_naive();
        let yesterday = today.pred_opt().unwrap();
        let daily = format!("daily/{}.md", today.format("%Y-%m-%d"));
        let mut index = satz_core::Index::build(vec![
            satz_core::parse_document(
                "# P

[[today]]
",
                std::path::Path::new("p.md"),
            ),
            satz_core::parse_document(
                "# Entry
",
                std::path::Path::new(&daily),
            ),
        ]);
        let config = satz_core::VaultConfig::default();

        let on_yesterday = messages_on(&mut index, &config, yesterday, &daily);
        let on_today = messages_on(&mut index, &config, today, &daily);
        assert!(
            on_yesterday.iter().any(|m| m.starts_with("Orphan note")),
            "{on_yesterday:?}"
        );
        assert!(on_today.is_empty(), "{on_today:?}");
    }

    /// The messages of every `publishDiagnostics` sent for `quiet` without a pause, by document.
    async fn diagnostics_meanwhile(
        input: &mut (impl tokio::io::AsyncBufRead + Unpin),
        quiet: std::time::Duration,
    ) -> Vec<(String, Vec<String>)> {
        let mut seen = Vec::new();
        while let Ok(Some(frame)) = tokio::time::timeout(quiet, read_frame(input)).await {
            if frame["method"] == "textDocument/publishDiagnostics" {
                let uri = frame["params"]["uri"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                let messages = frame["params"]["diagnostics"]
                    .as_array()
                    .map(|list| {
                        list.iter()
                            .filter_map(|d| d["message"].as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                seen.push((uri, messages));
            }
        }
        seen
    }

    #[tokio::test]
    async fn a_note_opened_after_midnight_is_read_on_the_new_day() {
        let (backend, mut from_server) = connected_backend().await;
        let today = chrono::Local::now().date_naive();
        let daily = format!("{}.md", today.format("%Y-%m-%d"));
        {
            let mut state = backend.state.write().await;
            state.index.replace_doc(satz_core::parse_document(
                "# P

[[today]]
",
                std::path::Path::new("p.md"),
            ));
        }
        make_the_day_stale(&backend).await;
        let _ = sent_meanwhile(&mut from_server, std::time::Duration::from_millis(300)).await;

        // No request has come since midnight: today's daily note is opened first.
        let uri = crate::convert::path_to_uri(&root().join("daily").join(&daily))
            .unwrap()
            .as_str()
            .to_string();
        backend
            .did_open(open_params(
                &uri, 1, "# Entry
",
            ))
            .await;
        let shown =
            diagnostics_meanwhile(&mut from_server, std::time::Duration::from_millis(500)).await;
        let of_note: Vec<&Vec<String>> = shown
            .iter()
            .filter(|(u, _)| *u == uri)
            .map(|(_, messages)| messages)
            .collect();
        assert!(!of_note.is_empty(), "{shown:?}");
        assert!(
            shown.iter().any(|(u, _)| u == "file:///a.md"),
            "the note that was already open is told as well: {shown:?}"
        );
        assert!(
            of_note.iter().all(|messages| messages.is_empty()),
            "[[today]] links to it, so it is no orphan: {shown:?}"
        );
    }

    #[tokio::test]
    async fn a_request_that_only_refreshes_a_stale_buffer_does_not_announce_a_day_change() {
        let (backend, mut from_server) = day_backend().await;
        make_the_day_current(&backend).await;
        {
            let mut state = backend.state.write().await;
            state.config.lsp.reparse_debounce_ms = 40;
            state.config.lsp.reparse_max_wait_ms = 40;
        }

        backend
            .did_change(change_params(
                "file:///a.md",
                2,
                "# Renamed
",
            ))
            .await;
        hover_a(&backend).await;
        let sent = sent_meanwhile(&mut from_server, std::time::Duration::from_millis(500)).await;

        // The reparse that follows the edit announces once; the same day adds nothing.
        assert_eq!(
            count_of(&sent, "workspace/semanticTokens/refresh", None),
            1,
            "{sent:?}"
        );
        assert_eq!(
            count_of(
                &sent,
                "textDocument/publishDiagnostics",
                Some("file:///a.md")
            ),
            1,
            "{sent:?}"
        );
    }

    #[tokio::test]
    async fn a_note_that_appears_on_disk_refreshes_the_open_notes() {
        let (backend, mut from_server) = connected_backend().await;
        let vault = std::env::temp_dir().join(format!("satz_t3_watch_{}", std::process::id()));
        std::fs::create_dir_all(&vault).unwrap();
        std::fs::write(
            vault.join("new.md"),
            "# New
",
        )
        .unwrap();
        {
            // The vault is this folder, and the note that is open lies in it.
            let mut state = backend.state.write().await;
            state.set_vault_root(Some(vault.clone()));
            state.open_document(
                "file:///a.md",
                "# A
",
                &vault.join("a.md"),
                1,
            );
        }
        let _ = sent_meanwhile(&mut from_server, std::time::Duration::from_millis(300)).await;

        crate::watcher::process_file_event(
            &vault.join("new.md"),
            &vault,
            &backend.state,
            &backend.client,
        )
        .await;
        let sent = sent_meanwhile(&mut from_server, std::time::Duration::from_millis(500)).await;
        let _ = std::fs::remove_dir_all(&vault);

        assert_eq!(
            count_of(
                &sent,
                "textDocument/publishDiagnostics",
                Some("file:///a.md")
            ),
            1,
            "the open note is told, a new note may resolve its links: {sent:?}"
        );
    }
}
