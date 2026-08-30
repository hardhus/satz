use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::RwLock;
use tower_lsp_server::jsonrpc;
use tower_lsp_server::ls_types::request::WorkspaceDiagnosticRefresh;
use tower_lsp_server::ls_types::*;
use tower_lsp_server::{Client, LanguageServer};

use crate::convert::uri_to_path;
use crate::handlers::diagnostics::compute_diagnostics;
use crate::state::SatzState;

#[derive(Debug)]
pub struct Backend {
    pub client: Client,
    pub state: Arc<RwLock<SatzState>>,
}

/// Computes diagnostics for the specified open document URI and sends them to the client.
pub(crate) async fn publish_for(client: &Client, state: &Arc<RwLock<SatzState>>, uri: &str) {
    let (diagnostics, uri_obj) = {
        let state_guard = state.read().await;

        if state_guard.client_supports_pull_diagnostics {
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
    pub fn new(client: Client) -> Self {
        Self {
            client,
            state: Arc::new(RwLock::new(SatzState::default())),
        }
    }

    async fn publish_diagnostics_for_uri(&self, uri: &str) {
        publish_for(&self.client, &self.state, uri).await;
    }
}

impl LanguageServer for Backend {
    async fn initialize(&self, params: InitializeParams) -> jsonrpc::Result<InitializeResult> {
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

        if let Some(root) = vault_root {
            let state_arc = self.state.clone();
            let client = self.client.clone();
            let root_clone = root.clone();

            tokio::task::spawn(async move {
                let root_for_blocking = root_clone.clone();
                let result = tokio::task::spawn_blocking(move || {
                    SatzState::initialize_index(root_for_blocking)
                })
                .await;

                match result {
                    Ok(Ok(mut new_state)) => {
                        let doc_count = new_state.index.doc_count();

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
                    }
                    Ok(Err(e)) => {
                        client
                            .log_message(
                                MessageType::ERROR,
                                format!("satz: indexing failed: {}", e),
                            )
                            .await;
                    }
                    Err(e) => {
                        client
                            .log_message(
                                MessageType::ERROR,
                                format!("satz: spawn_blocking panicked: {}", e),
                            )
                            .await;
                    }
                }
            });
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
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let uri = params.text_document.uri.to_string();
        let content = params.text_document.text;
        let version = params.text_document.version;
        let Some(path) = uri_to_path(&uri) else {
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

        let (delay, prev_task) = {
            let mut state = self.state.write().await;
            let debounce = std::time::Duration::from_millis(state.config.lsp.reparse_debounce_ms);
            let max_wait = std::time::Duration::from_millis(state.config.lsp.reparse_max_wait_ms);

            let Some(open_doc) = state.open_docs.get_mut(&uri) else {
                return;
            };

            crate::sync::apply_changes_to_rope(&mut open_doc.rope, params.content_changes);
            open_doc.version = version;

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
        {
            let mut state = self.state.write().await;
            state.close_document(&uri);
        }
        self.client.publish_diagnostics(lsp_uri, vec![], None).await;
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
        Ok(crate::handlers::rename::rename(params, &state))
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

    async fn diagnostic(
        &self,
        params: DocumentDiagnosticParams,
    ) -> jsonrpc::Result<DocumentDiagnosticReportResult> {
        let uri = params.text_document.uri.to_string();
        let diagnostics = {
            let state = self.state.read().await;

            if let Some(open_doc) = state.open_docs.get(&uri) {
                let rel_path = SatzState::get_rel_path(&open_doc.path, state.vault_root.as_deref());
                let rel_path_str = rel_path.to_string_lossy().replace('\\', "/");
                let doc_id = satz_core::DocId::new(&rel_path_str);

                if let Some(doc) = state.index.get_doc(&doc_id) {
                    compute_diagnostics(doc, &state.index, &state.config)
                } else {
                    vec![]
                }
            } else {
                vec![]
            }
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
        let mut items = Vec::new();

        for doc in state.index.documents() {
            let doc_path = match &state.vault_root {
                Some(root) if !doc.path.is_absolute() => root.join(&doc.path),
                _ => doc.path.clone(),
            };
            if let Some(uri) = crate::convert::path_to_uri(&doc_path) {
                let diagnostics = compute_diagnostics(doc, &state.index, &state.config);
                let version = state
                    .open_docs
                    .get(uri.as_str())
                    .map(|od| od.version as i64);
                items.push(WorkspaceDocumentDiagnosticReport::Full(
                    WorkspaceFullDocumentDiagnosticReport {
                        uri,
                        version,
                        full_document_diagnostic_report: FullDocumentDiagnosticReport {
                            result_id: None,
                            items: diagnostics,
                        },
                    },
                ));
            }
        }

        Ok(WorkspaceDiagnosticReportResult::Report(
            WorkspaceDiagnosticReport { items },
        ))
    }
}
