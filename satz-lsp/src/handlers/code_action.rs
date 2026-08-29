#![allow(clippy::collapsible_if)]

use tower_lsp_server::ls_types::{
    CodeAction, CodeActionKind, CodeActionOrCommand, CodeActionParams, CodeActionResponse,
    CreateFile, DocumentChangeOperation, DocumentChanges, OneOf,
    OptionalVersionedTextDocumentIdentifier, Range, ResourceOp, TextDocumentEdit, TextEdit,
    WorkspaceEdit,
};

use crate::convert::{lsp_pos_to_satz, path_to_uri};
use crate::state::SatzState;
use satz_core::model::LinkKind;

pub fn code_action(params: CodeActionParams, state: &SatzState) -> Option<CodeActionResponse> {
    let uri = params.text_document.uri.as_str();
    let pos = params.range.start;

    let open_doc = state.open_docs.get(uri)?;
    let rel_path =
        crate::state::SatzState::get_rel_path(&open_doc.path, state.vault_root.as_deref());
    let rel_path_str = rel_path.to_string_lossy().replace('\\', "/");
    let doc_id = satz_core::DocId::new(&rel_path_str);
    let doc = state.index.get_doc(&doc_id)?;

    let satz_pos = lsp_pos_to_satz(pos);
    let byte_offset = doc.line_index.position_to_byte(satz_pos);

    let mut actions: Vec<CodeActionOrCommand> = Vec::new();

    // Check for Link under cursor -> quickfixes
    if let Some(link) = doc.links.iter().find(|l| l.range.contains(byte_offset)) {
        if matches!(
            link.kind,
            LinkKind::WikiLink | LinkKind::Embed | LinkKind::Markdown
        ) && !link.target_doc.starts_with("http://")
            && !link.target_doc.starts_with("https://")
        {
            match state.index.resolve_link_full(link, Some(doc)) {
                satz_core::LinkResolution::DocMissing if !link.target_doc.is_empty() => {
                    let clean_name = link.target_doc.trim_end_matches(".md");
                    let target_filename = format!("{}.md", clean_name);
                    let target_path = match &state.vault_root {
                        Some(root) => root.join(&target_filename),
                        None => std::path::PathBuf::from(&target_filename),
                    };

                    if let Some(target_uri) = path_to_uri(&target_path) {
                        let initial_content =
                            format!("---\ntitle: {}\n---\n\n# {}\n", clean_name, clean_name);

                        let ops = vec![
                            DocumentChangeOperation::Op(ResourceOp::Create(CreateFile {
                                uri: target_uri.clone(),
                                options: None,
                                annotation_id: None,
                            })),
                            DocumentChangeOperation::Edit(TextDocumentEdit {
                                text_document: OptionalVersionedTextDocumentIdentifier {
                                    uri: target_uri,
                                    version: None,
                                },
                                edits: vec![OneOf::Left(TextEdit {
                                    range: Range::default(),
                                    new_text: initial_content,
                                })],
                            }),
                        ];

                        let action = CodeAction {
                            title: format!("Create note: \"{}\"", clean_name),
                            kind: Some(CodeActionKind::QUICKFIX),
                            diagnostics: None,
                            edit: Some(WorkspaceEdit {
                                document_changes: Some(DocumentChanges::Operations(ops)),
                                ..Default::default()
                            }),
                            is_preferred: Some(true),
                            disabled: None,
                            command: None,
                            data: None,
                        };

                        actions.push(CodeActionOrCommand::CodeAction(action));
                    }
                }
                satz_core::LinkResolution::AnchorMissing { doc: target_doc } => {
                    if let Some(heading_name) = &link.target_heading {
                        let target_path = match &state.vault_root {
                            Some(root) if !target_doc.path.is_absolute() => {
                                root.join(&target_doc.path)
                            }
                            _ => target_doc.path.clone(),
                        };
                        if let Some(target_uri) = path_to_uri(&target_path) {
                            let source = target_doc.line_index.source();
                            let end_pos = target_doc.line_index.byte_to_position(source.len());
                            let insert_text = if source.ends_with('\n') {
                                format!("\n## {}\n", heading_name)
                            } else {
                                format!("\n\n## {}\n", heading_name)
                            };

                            let edit = TextEdit {
                                range: Range::new(
                                    tower_lsp_server::ls_types::Position::new(
                                        end_pos.line,
                                        end_pos.character,
                                    ),
                                    tower_lsp_server::ls_types::Position::new(
                                        end_pos.line,
                                        end_pos.character,
                                    ),
                                ),
                                new_text: insert_text,
                            };

                            let mut changes = std::collections::HashMap::new();
                            changes.insert(target_uri, vec![edit]);

                            let action = CodeAction {
                                title: format!(
                                    "Add heading '## {}' to \"{}\"",
                                    heading_name, target_doc.title
                                ),
                                kind: Some(CodeActionKind::QUICKFIX),
                                diagnostics: None,
                                edit: Some(WorkspaceEdit {
                                    changes: Some(changes),
                                    ..Default::default()
                                }),
                                is_preferred: Some(true),
                                disabled: None,
                                command: None,
                                data: None,
                            };

                            actions.push(CodeActionOrCommand::CodeAction(action));
                        }
                    }
                }
                _ => {}
            }
        }
    }

    if actions.is_empty() {
        None
    } else {
        Some(actions)
    }
}

#[cfg(test)]
#[allow(unused_variables)]
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use super::*;
    use satz_core::{Index, parse_document};
    use std::path::Path;
    use tower_lsp_server::ls_types::{CodeActionContext, Position, TextDocumentIdentifier};

    #[test]
    fn test_code_action_create_missing_note() {
        let abs_a = if cfg!(windows) {
            Path::new("C:\\doc-a.md")
        } else {
            Path::new("/doc-a.md")
        };
        let rel_a = Path::new("doc-a.md");

        let doc_a = parse_document("# Doc A\n\nLink to [[missing-note]] here", rel_a);

        let mut state = SatzState::default();
        state.index = Index::build(vec![doc_a]);
        state.vault_root = Some(if cfg!(windows) {
            Path::new("C:\\").to_path_buf()
        } else {
            Path::new("/").to_path_buf()
        });

        let uri_a_str = if cfg!(windows) {
            "file:///C:/doc-a.md"
        } else {
            "file:///doc-a.md"
        };

        state.open_docs.insert(
            uri_a_str.to_string(),
            crate::state::OpenDocument::new(
                uri_a_str,
                abs_a.to_path_buf(),
                "# Doc A\n\nLink to [[missing-note]] here",
                1,
            ),
        );

        let params = CodeActionParams {
            text_document: TextDocumentIdentifier {
                uri: uri_a_str.parse().unwrap(),
            },
            range: Range::new(Position::new(2, 12), Position::new(2, 12)),
            context: CodeActionContext::default(),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };

        let response = code_action(params, &state).expect("CodeAction response expected");
        assert_eq!(response.len(), 1);

        if let CodeActionOrCommand::CodeAction(action) = &response[0] {
            assert!(action.title.contains("Create note: \"missing-note\""));
            assert_eq!(action.kind, Some(CodeActionKind::QUICKFIX));
            assert!(action.edit.is_some());
        } else {
            panic!("Expected CodeAction");
        }
    }

    #[test]
    fn test_code_action_add_missing_heading() {
        let rel_a = Path::new("doc-a.md");
        let rel_b = Path::new("doc-b.md");

        let doc_a = parse_document("# Doc A\n\nLink to [[doc-b#İstemciler]] here", rel_a);
        let doc_b = parse_document("# Doc B\n\nExisting text.", rel_b);

        let mut state = SatzState::default();
        state.index = Index::build(vec![doc_a, doc_b]);
        state.vault_root = Some(if cfg!(windows) {
            Path::new("C:\\").to_path_buf()
        } else {
            Path::new("/").to_path_buf()
        });

        let uri_a_str = if cfg!(windows) {
            "file:///C:/doc-a.md"
        } else {
            "file:///doc-a.md"
        };

        state.open_docs.insert(
            uri_a_str.to_string(),
            crate::state::OpenDocument::new(
                uri_a_str,
                Path::new(if cfg!(windows) {
                    "C:\\doc-a.md"
                } else {
                    "/doc-a.md"
                })
                .to_path_buf(),
                "# Doc A\n\nLink to [[doc-b#İstemciler]] here",
                1,
            ),
        );

        let params = CodeActionParams {
            text_document: TextDocumentIdentifier {
                uri: uri_a_str.parse().unwrap(),
            },
            range: Range::new(Position::new(2, 12), Position::new(2, 12)),
            context: CodeActionContext::default(),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };

        let response = code_action(params, &state).expect("CodeAction response expected");
        assert_eq!(response.len(), 1);

        if let CodeActionOrCommand::CodeAction(action) = &response[0] {
            assert!(
                action
                    .title
                    .contains("Add heading '## İstemciler' to \"Doc B\"")
            );
            assert_eq!(action.kind, Some(CodeActionKind::QUICKFIX));
            assert!(action.edit.is_some());
        } else {
            panic!("Expected CodeAction");
        }
    }
}
