use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::RwLock;
use tower_lsp_server::jsonrpc;
use tower_lsp_server::ls_types::*;
use tower_lsp_server::{Client, LanguageServer};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::reload;

use crate::state::SatzState;

/// Handle to the process-wide log filter, set up in `main`. Lets a single
/// client-supplied `initializationOptions.logLevel` change verbosity at
/// runtime without an env var or a rebuild.
pub type LogReloadHandle = reload::Handle<EnvFilter, tracing_subscriber::Registry>;

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

impl Backend {
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
}

impl LanguageServer for Backend {
    async fn initialize(&self, params: InitializeParams) -> jsonrpc::Result<InitializeResult> {
        lifecycle::initialize(self, params).await
    }

    async fn initialized(&self, _: InitializedParams) {
        lifecycle::initialized(self).await
    }

    async fn shutdown(&self) -> jsonrpc::Result<()> {
        lifecycle::shutdown(self).await
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        documents::did_open(self, params).await
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        documents::did_change(self, params).await
    }

    async fn did_save(&self, params: DidSaveTextDocumentParams) {
        documents::did_save(self, params).await
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        documents::did_close(self, params).await
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

mod capabilities;
mod documents;
mod lifecycle;
mod refresh;

// `watcher.rs` still names this the old way (`crate::backend::refresh_open_documents`); the
// production code below always names its own module instead (e.g. `lifecycle::initialize(..)`).
pub(crate) use refresh::refresh_open_documents;

// Test-only: `backend::tests`'s `use super::*` keeps seeing every item by its old bare name,
// whichever of the submodules above it actually lives in now. A glob (`use module::*`) is not used
// here because rustc's unused-import check does not credit a glob for a re-export used only from a
// sibling module (`backend::tests`) through this one -- a named list is what stays warning-free.
#[cfg(test)]
pub(crate) use capabilities::{client_supports_document_changes, server_capabilities};
#[cfg(test)]
pub(crate) use lifecycle::{WorkspaceChoice, pick_workspace_root, refresh_after_first_index};
#[cfg(test)]
pub(crate) use refresh::{
    RefreshOutcome, refresh_after_reparse, refresh_diagnostics, refresh_semantic_tokens,
    refresh_succeeded, send_refresh,
};

#[cfg(test)]
pub(crate) mod tests;
