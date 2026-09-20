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
        return;
    };
    let version = job.version;
    let parsed =
        tokio::task::spawn_blocking(move || satz_core::parse_document(&job.content, &job.rel_path))
            .await;
    let Ok(new_doc) = parsed else {
        tracing::error!(%uri, "reparse: the parsing task failed");
        return;
    };

    let (applied, peers_dirty, supports_pull, other_uris) = {
        let mut state = state_arc.write().await;
        let applied = state.apply_reparse(&uri, version, new_doc);
        let dirty = state.peers_dirty;
        if applied {
            state.peers_dirty = false;
        }
        let other: Vec<String> = state
            .open_docs
            .keys()
            .filter(|u| **u != uri)
            .cloned()
            .collect();
        (
            applied,
            dirty && applied,
            state.client_supports_pull_diagnostics,
            other,
        )
    };
    if !applied {
        return; // the buffer changed while parsing; the task of that change takes over
    }

    publish_for(&client, &state_arc, &uri).await;

    let plan = refresh_after_reparse(peers_dirty, supports_pull);
    if plan.pull_diagnostics {
        let _ = client.send_request::<WorkspaceDiagnosticRefresh>(()).await;
    }
    if plan.push_peers {
        for other_uri in other_uris {
            publish_for(&client, &state_arc, &other_uri).await;
        }
    }
    if plan.semantic_tokens {
        let _ = client.send_request::<SemanticTokensRefresh>(()).await;
    }
}

pub struct Backend {
    pub client: Client,
    pub state: Arc<RwLock<SatzState>>,
    log_reload_handle: LogReloadHandle,
}

/// Computes diagnostics for the specified open document URI and sends them to the client.
pub(crate) async fn publish_for(client: &Client, state: &Arc<RwLock<SatzState>>, uri: &str) {
    let (diagnostics, uri_obj) = {
        let state_guard = state.read().await;

        if state_guard.client_supports_pull_diagnostics {
            return;
        }

        if !state_guard.indexing_complete {
            tracing::debug!(uri, "publish_for: initial indexing not complete yet, skipping");
            return;
        }

        let Some(open_doc) = state_guard.open_docs.get(uri) else {
            return;
        };
        let rel_path = SatzState::get_rel_path(&open_doc.path, state_guard.vault_root.as_deref());
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
    pub fn new(client: Client, log_reload_handle: LogReloadHandle) -> Self {
        Self {
            client,
            state: Arc::new(RwLock::new(SatzState::default())),
            log_reload_handle,
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

        let vault_root: Option<PathBuf> = params
            .workspace_folders
            .as_deref()
            .and_then(|folders| folders.first())
            .and_then(|f| uri_to_path(f.uri.as_str()))
            .or_else(|| {
                // The LSP type marks this field `#[deprecated]` but the protocol still requires it.
                #[allow(deprecated)]
                params
                    .root_uri
                    .as_ref()
                    .and_then(|u| uri_to_path(u.as_str()))
            });

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
            let state_arc = self.state.clone();
            let client = self.client.clone();
            let root_clone = root.clone();

            tokio::task::spawn(async move {
                tracing::debug!(vault_root = ?root_clone, "walk_vault: starting");
                // Watching starts BEFORE the walk: what changes while it runs is held back until
                // the index is complete and is then applied from what is on disk.
                crate::watcher::spawn_watcher(
                    root_clone.clone(),
                    state_arc.clone(),
                    client.clone(),
                );

                let root_for_blocking = root_clone.clone();
                let result = tokio::task::spawn_blocking(move || {
                    SatzState::initialize_index(root_for_blocking)
                })
                .await
                .unwrap_or_else(|e| Err(anyhow::anyhow!("the indexing task panicked: {e}")));

                let outcome = {
                    let mut current_state = state_arc.write().await;
                    current_state.finish_indexing(result, &root_clone)
                };

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

                // A `.satz.toml` that exists but can't be used must be visible: falling back to
                // defaults silently would look like the settings were ignored.
                if let Some(error) = outcome.config_error {
                    let message = crate::state::config_error_message(&error, "default");
                    client.log_message(MessageType::WARNING, &message).await;
                    client.show_message(MessageType::WARNING, message).await;
                }

                let (supports_pull, uris) = {
                    let s = state_arc.read().await;
                    (
                        s.client_supports_pull_diagnostics,
                        s.open_docs.keys().cloned().collect::<Vec<_>>(),
                    )
                };

                if supports_pull {
                    let _ = client.send_request::<WorkspaceDiagnosticRefresh>(()).await;
                } else {
                    for uri in uris {
                        publish_for(&client, &state_arc, &uri).await;
                    }
                }

                // Any document opened before indexing finished had its links colored against a
                // still-partial index (peers not yet indexed resolve as missing), so its semantic
                // tokens may be stale/wrong now that the full index is in place. Unconditional
                // (unlike the diagnostics push above, which branches on pull-vs-push support) --
                // this is a separate capability a client simply ignores if it never declared support.
                let _ = client.send_request::<SemanticTokensRefresh>(()).await;
            });
        } else {
            // No workspace root at all: there is no vault to walk, so there is nothing for
            // `indexing_complete` to wait on — diagnostics can run immediately.
            let mut state = self.state.write().await;
            state.indexing_complete = true;
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
    }

    async fn shutdown(&self) -> jsonrpc::Result<()> {
        tracing::debug!("shutdown requested");
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

        let (peers_dirty, supports_pull, other_uris) = {
            let mut state = self.state.write().await;
            state.open_document(&uri, &content, &path, version);
            let dirty = state.peers_dirty;
            state.peers_dirty = false;
            let other: Vec<String> = state
                .open_docs
                .keys()
                .filter(|u| *u != &uri)
                .cloned()
                .collect();
            (dirty, state.client_supports_pull_diagnostics, other)
        };

        self.publish_diagnostics_for_uri(&uri).await;

        if peers_dirty {
            if supports_pull {
                let _ = self
                    .client
                    .send_request::<WorkspaceDiagnosticRefresh>(())
                    .await;
            } else {
                for other_uri in other_uris {
                    publish_for(&self.client, &self.state, &other_uri).await;
                }
            }
        }
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
        let delay = debounce.min(max_wait.saturating_sub(elapsed));

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

        let (peers_dirty, supports_pull, other_uris, prev_task) = {
            let mut state = self.state.write().await;
            let prev = if let Some(open_doc) = state.open_docs.get_mut(&uri) {
                open_doc.pending_task.take()
            } else {
                None
            };
            state.reparse_open_document(&uri);
            let dirty = state.peers_dirty;
            state.peers_dirty = false;
            let other: Vec<String> = state
                .open_docs
                .keys()
                .filter(|u| *u != &uri)
                .cloned()
                .collect();
            (dirty, state.client_supports_pull_diagnostics, other, prev)
        };

        if let Some(task) = prev_task {
            task.abort();
        }

        self.publish_diagnostics_for_uri(&uri).await;

        if peers_dirty {
            if supports_pull {
                let _ = self
                    .client
                    .send_request::<WorkspaceDiagnosticRefresh>(())
                    .await;
            } else {
                for other_uri in other_uris {
                    publish_for(&self.client, &self.state, &other_uri).await;
                }
            }
        }
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        let uri = params.text_document.uri.to_string();
        let lsp_uri = params.text_document.uri;
        tracing::debug!(%uri, "did_close");
        let (peers_dirty, supports_pull, other_uris) = {
            let mut state = self.state.write().await;
            state.close_document(&uri);
            let dirty = state.peers_dirty;
            state.peers_dirty = false;
            (
                dirty,
                state.client_supports_pull_diagnostics,
                state.open_docs.keys().cloned().collect::<Vec<_>>(),
            )
        };
        self.client.publish_diagnostics(lsp_uri, vec![], None).await;

        // Discarded unsaved edits change what the remaining documents' diagnostics should say.
        if peers_dirty {
            if supports_pull {
                let _ = self
                    .client
                    .send_request::<WorkspaceDiagnosticRefresh>(())
                    .await;
            } else {
                for other_uri in other_uris {
                    publish_for(&self.client, &self.state, &other_uri).await;
                }
            }
        }
    }

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> jsonrpc::Result<Option<GotoDefinitionResponse>> {
        let state = self.state.read().await;
        Ok(crate::handlers::definition::goto_definition(params, &state))
    }

    async fn references(&self, params: ReferenceParams) -> jsonrpc::Result<Option<Vec<Location>>> {
        let state = self.state.read().await;
        Ok(crate::handlers::references::find_references(params, &state))
    }

    async fn hover(&self, params: HoverParams) -> jsonrpc::Result<Option<Hover>> {
        let state = self.state.read().await;
        Ok(crate::handlers::hover::hover(params, &state))
    }

    async fn document_symbol(
        &self,
        params: DocumentSymbolParams,
    ) -> jsonrpc::Result<Option<DocumentSymbolResponse>> {
        let state = self.state.read().await;
        Ok(crate::handlers::document_symbol::document_symbol(
            params, &state,
        ))
    }

    async fn completion(
        &self,
        params: CompletionParams,
    ) -> jsonrpc::Result<Option<CompletionResponse>> {
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
        let state = self.state.read().await;
        Ok(crate::handlers::workspace_symbol::workspace_symbol(
            params, &state,
        ))
    }

    async fn prepare_rename(
        &self,
        params: TextDocumentPositionParams,
    ) -> jsonrpc::Result<Option<PrepareRenameResponse>> {
        let state = self.state.read().await;
        Ok(crate::handlers::rename::prepare_rename(params, &state))
    }

    async fn rename(&self, params: RenameParams) -> jsonrpc::Result<Option<WorkspaceEdit>> {
        let state = self.state.read().await;
        crate::handlers::rename::rename(params, &state).map_err(jsonrpc::Error::invalid_params)
    }

    async fn document_highlight(
        &self,
        params: DocumentHighlightParams,
    ) -> jsonrpc::Result<Option<Vec<DocumentHighlight>>> {
        let state = self.state.read().await;
        Ok(crate::handlers::document_highlight::document_highlight(
            params, &state,
        ))
    }

    async fn code_action(
        &self,
        params: CodeActionParams,
    ) -> jsonrpc::Result<Option<CodeActionResponse>> {
        let state = self.state.read().await;
        Ok(crate::handlers::code_action::code_action(params, &state))
    }

    async fn document_link(
        &self,
        params: DocumentLinkParams,
    ) -> jsonrpc::Result<Option<Vec<DocumentLink>>> {
        let state = self.state.read().await;
        Ok(crate::handlers::document_link::document_link(
            params, &state,
        ))
    }

    async fn folding_range(
        &self,
        params: FoldingRangeParams,
    ) -> jsonrpc::Result<Option<Vec<FoldingRange>>> {
        let state = self.state.read().await;
        Ok(crate::handlers::folding_range::folding_range(
            params, &state,
        ))
    }

    async fn code_lens(&self, params: CodeLensParams) -> jsonrpc::Result<Option<Vec<CodeLens>>> {
        let state = self.state.read().await;
        Ok(crate::handlers::codelens::code_lens(params, &state))
    }

    async fn inlay_hint(&self, params: InlayHintParams) -> jsonrpc::Result<Option<Vec<InlayHint>>> {
        let state = self.state.read().await;
        Ok(crate::handlers::inlay_hint::inlay_hint(params, &state))
    }

    async fn semantic_tokens_full(
        &self,
        params: SemanticTokensParams,
    ) -> jsonrpc::Result<Option<SemanticTokensResult>> {
        let state = self.state.read().await;
        Ok(crate::handlers::semantic_tokens::semantic_tokens_full(
            params, &state,
        ))
    }

    async fn formatting(
        &self,
        params: DocumentFormattingParams,
    ) -> jsonrpc::Result<Option<Vec<TextEdit>>> {
        let state = self.state.read().await;
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
        let diagnostics = {
            let state = self.state.read().await;
            crate::handlers::diagnostics::pull_document_diagnostics(&uri, &state)
        };

        Ok(DocumentDiagnosticReportResult::Report(
            DocumentDiagnosticReport::Full(RelatedFullDocumentDiagnosticReport {
                related_documents: None,
                full_document_diagnostic_report: FullDocumentDiagnosticReport {
                    result_id: None,
                    items: diagnostics,
                },
            }),
        ))
    }

    async fn workspace_diagnostic(
        &self,
        _params: WorkspaceDiagnosticParams,
    ) -> jsonrpc::Result<WorkspaceDiagnosticReportResult> {
        let state = self.state.read().await;
        let items = crate::handlers::diagnostics::pull_workspace_diagnostics(&state);

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
    RefreshPlan {
        pull_diagnostics: supports_pull,
        push_peers: peers_dirty && !supports_pull,
        semantic_tokens: true,
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
            state.vault_root = Some(root());
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
            state.vault_root = Some(root());
            state.config.lsp.reparse_debounce_ms = 600_000;
            state.config.lsp.reparse_max_wait_ms = 600_000;
            state.indexing_complete = true;
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
}
