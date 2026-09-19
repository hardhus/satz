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

        {
            let mut state = self.state.write().await;
            state.client_supports_pull_diagnostics = supports_pull;
        }

        tracing::debug!(?vault_root, "initialize: resolved vault root");

        if let Some(root) = vault_root {
            let state_arc = self.state.clone();
            let client = self.client.clone();
            let root_clone = root.clone();

            tokio::task::spawn(async move {
                tracing::debug!(vault_root = ?root_clone, "walk_vault: starting");
                let root_for_blocking = root_clone.clone();
                let result = tokio::task::spawn_blocking(move || {
                    SatzState::initialize_index(root_for_blocking)
                })
                .await;

                match result {
                    Ok(Ok(mut new_state)) => {
                        let doc_count = new_state.index.doc_count();
                        let startup_config_error = new_state.config_error.clone();
                        tracing::info!(doc_count, vault_root = ?new_state.vault_root, "walk_vault: succeeded");

                        {
                            let mut current_state = state_arc.write().await;
                            new_state.client_supports_pull_diagnostics =
                                current_state.client_supports_pull_diagnostics;
                            new_state.open_docs = std::mem::take(&mut current_state.open_docs);

                            for doc in new_state.open_docs.values() {
                                let rel_path = SatzState::get_rel_path(
                                    &doc.path,
                                    new_state.vault_root.as_deref(),
                                );
                                let content = doc.rope.to_string();
                                let parsed = satz_core::parse_document(&content, &rel_path);
                                new_state.index.replace_doc(parsed);
                            }

                            *current_state = new_state;
                        }

                        crate::watcher::spawn_watcher(
                            root_clone,
                            state_arc.clone(),
                            client.clone(),
                        );
                        client
                            .log_message(
                                MessageType::INFO,
                                format!("satz: indexed {} documents", doc_count),
                            )
                            .await;

                        // A `.satz.toml` that exists but can't be used must be visible: falling
                        // back to defaults silently would look like the settings were ignored.
                        if let Some(error) = startup_config_error {
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

                        // Any document opened before indexing finished had its links colored
                        // against a still-partial index (peers not yet indexed resolve as
                        // missing), so its semantic tokens may be stale/wrong now that the full
                        // index is in place. Unconditional (unlike the diagnostics push above,
                        // which branches on pull-vs-push support) -- this is a separate
                        // capability a client simply ignores if it never declared support.
                        let _ = client.send_request::<SemanticTokensRefresh>(()).await;
                    }
                    Ok(Err(e)) => {
                        tracing::error!(error = %e, "walk_vault: failed");
                        client
                            .log_message(
                                MessageType::ERROR,
                                format!("satz: indexing failed: {}", e),
                            )
                            .await;
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "walk_vault: spawn_blocking panicked");
                        client
                            .log_message(
                                MessageType::ERROR,
                                format!("satz: spawn_blocking panicked: {}", e),
                            )
                            .await;
                    }
                }
            });
        } else {
            // No workspace root at all: there is no vault to walk, so there is nothing for
            // `indexing_complete` to wait on — diagnostics can run immediately.
            let mut state = self.state.write().await;
            state.indexing_complete = true;
        }

        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::INCREMENTAL,
                )),
                diagnostic_provider: Some(DiagnosticServerCapabilities::Options(
                    DiagnosticOptions {
                        identifier: Some("satz".to_string()),
                        inter_file_dependencies: true,
                        workspace_diagnostics: true,
                        work_done_progress_options: WorkDoneProgressOptions::default(),
                    },
                )),
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
                semantic_tokens_provider: Some(
                    SemanticTokensServerCapabilities::SemanticTokensOptions(
                        SemanticTokensOptions {
                            work_done_progress_options: Default::default(),
                            legend: crate::handlers::semantic_tokens::semantic_tokens_legend(),
                            range: None,
                            full: Some(SemanticTokensFullOptions::Bool(true)),
                        },
                    ),
                ),
                document_formatting_provider: Some(OneOf::Left(true)),
                execute_command_provider: Some(ExecuteCommandOptions {
                    commands: vec![
                        crate::handlers::execute_command::FORMAT_WORKSPACE_COMMAND.to_string(),
                    ],
                    work_done_progress_options: Default::default(),
                }),
                ..Default::default()
            },

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
            tracing::warn!(%uri, "did_open: uri_to_path failed, ignoring");
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

        let (delay, prev_task) = {
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
            let remaining = max_wait.saturating_sub(elapsed);
            let delay = debounce.min(remaining);

            let prev = open_doc.pending_task.take();
            (delay, prev)
        };

        if let Some(task) = prev_task {
            task.abort();
        }

        let state_arc = self.state.clone();
        let client_clone = self.client.clone();
        let uri_clone = uri.clone();

        let handle = tokio::task::spawn(async move {
            tokio::time::sleep(delay).await;

            let (peers_dirty, supports_pull, other_uris) = {
                let mut state = state_arc.write().await;
                state.reparse_open_document(&uri_clone);
                let dirty = state.peers_dirty;
                state.peers_dirty = false;
                let other: Vec<String> = state
                    .open_docs
                    .keys()
                    .filter(|u| *u != &uri_clone)
                    .cloned()
                    .collect();
                (dirty, state.client_supports_pull_diagnostics, other)
            };

            publish_for(&client_clone, &state_arc, &uri_clone).await;

            if peers_dirty {
                if supports_pull {
                    let _ = client_clone
                        .send_request::<WorkspaceDiagnosticRefresh>(())
                        .await;
                } else {
                    for other_uri in other_uris {
                        publish_for(&client_clone, &state_arc, &other_uri).await;
                    }
                }
            }
        });

        let mut state = self.state.write().await;
        if let Some(open_doc) = state.open_docs.get_mut(&uri) {
            open_doc.pending_task = Some(handle);
        }
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
        if params.command != crate::handlers::execute_command::FORMAT_WORKSPACE_COMMAND {
            return Err(jsonrpc::Error::method_not_found());
        }

        let result = {
            let state = self.state.read().await;
            crate::handlers::execute_command::compute_format_changes(&state)
        };

        if !result.cache_updates.is_empty() {
            let mut state = self.state.write().await;
            for (hash, formatted) in result.cache_updates {
                state.format_cache.insert(hash, formatted);
            }
        }

        let changes = result.changes;
        if changes.is_empty() {
            self.client
                .log_message(MessageType::INFO, "satz: vault is already fully formatted")
                .await;
            return Ok(Some(serde_json::json!({ "formatted": 0 })));
        }

        let count = changes.len();
        let edit = crate::handlers::execute_command::build_workspace_edit(&changes);

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

        // Keep any open document's in-memory rope (and the index built from it) in sync right
        // away, rather than waiting for the client's own follow-up `didChange` notification.
        let formatted_by_uri: std::collections::HashMap<String, &str> = changes
            .iter()
            .map(|c| (c.uri.as_str().to_string(), c.formatted.as_str()))
            .collect();

        let (supports_pull, all_open_uris) = {
            let mut state = self.state.write().await;
            let open_uris: Vec<String> = formatted_by_uri
                .keys()
                .filter(|uri| state.open_docs.contains_key(*uri))
                .cloned()
                .collect();

            for uri in &open_uris {
                if let Some(open_doc) = state.open_docs.get_mut(uri) {
                    open_doc.rope = ropey::Rope::from_str(formatted_by_uri[uri]);
                }
            }
            for uri in &open_uris {
                state.reparse_open_document(uri);
            }

            (
                state.client_supports_pull_diagnostics,
                state.open_docs.keys().cloned().collect::<Vec<_>>(),
            )
        };

        self.client
            .log_message(
                MessageType::INFO,
                format!("satz: formatted {count} file(s)"),
            )
            .await;

        if supports_pull {
            let _ = self
                .client
                .send_request::<WorkspaceDiagnosticRefresh>(())
                .await;
        } else {
            for uri in all_open_uris {
                publish_for(&self.client, &self.state, &uri).await;
            }
        }

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
