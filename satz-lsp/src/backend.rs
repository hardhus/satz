use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::RwLock;
use tower_lsp_server::jsonrpc;
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

impl Backend {
    pub fn new(client: Client) -> Self {
        Self {
            client,
            state: Arc::new(RwLock::new(SatzState::default())),
        }
    }

    /// Computes diagnostics for the specified open document URI and sends them to the client.
    async fn publish_diagnostics_for_uri(&self, uri: &str) {
        let (diagnostics, uri_obj) = {
            let state = self.state.read().await;

            let Some(open_doc) = state.open_docs.get(uri) else {
                return;
            };
            let rel_path = match &state.vault_root {
                Some(root) => open_doc.path.strip_prefix(root).unwrap_or(&open_doc.path),
                None => &open_doc.path,
            };
            let rel_path_str = rel_path.to_string_lossy().replace('\\', "/");
            let doc_id = satz_core::DocId::new(&rel_path_str);

            let Some(doc) = state.index.get_doc(&doc_id) else {
                return;
            };

            let diags = compute_diagnostics(doc, &state.index, &state.config);
            let uri_obj = match uri.parse::<Uri>() {
                Ok(u) => u,
                Err(_) => return,
            };
            (diags, uri_obj)
        };

        self.client
            .publish_diagnostics(uri_obj, diagnostics, None)
            .await;
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

        if let Some(root) = vault_root {
            let state_arc = self.state.clone();
            let client = self.client.clone();
            let root_clone = root.clone();

            tokio::task::spawn(async move {
                let result =
                    tokio::task::spawn_blocking(move || SatzState::initialize_index(root_clone))
                        .await;

                match result {
                    Ok(Ok(new_state)) => {
                        let doc_count = new_state.index.doc_count();
                        let broken = new_state.index.broken_link_count();
                        *state_arc.write().await = new_state;
                        client
                            .log_message(
                                MessageType::INFO,
                                format!(
                                    "satz: indexed {} documents ({} broken links)",
                                    doc_count, broken
                                ),
                            )
                            .await;
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
                    TextDocumentSyncKind::FULL,
                )),
                diagnostic_provider: Some(DiagnosticServerCapabilities::Options(
                    DiagnosticOptions {
                        identifier: Some("satz".to_string()),
                        inter_file_dependencies: true,
                        workspace_diagnostics: false,
                        work_done_progress_options: WorkDoneProgressOptions::default(),
                    },
                )),
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

        {
            let mut state = self.state.write().await;
            state.reparse_document(&uri, &content, &path, version);
        }

        self.publish_diagnostics_for_uri(&uri).await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri.to_string();
        let version = params.text_document.version;
        let Some(change) = params.content_changes.into_iter().last() else {
            return;
        };
        let content = change.text;
        let Some(path) = uri_to_path(&uri) else {
            return;
        };

        {
            let mut state = self.state.write().await;
            state.reparse_document(&uri, &content, &path, version);
        }

        self.publish_diagnostics_for_uri(&uri).await;
    }

    async fn did_save(&self, params: DidSaveTextDocumentParams) {
        let uri = params.text_document.uri.to_string();
        self.publish_diagnostics_for_uri(&uri).await;
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
}
