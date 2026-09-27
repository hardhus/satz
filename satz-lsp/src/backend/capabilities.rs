use tower_lsp_server::ls_types::*;

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
