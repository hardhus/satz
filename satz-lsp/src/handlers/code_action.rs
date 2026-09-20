#![allow(clippy::collapsible_if)]

use tower_lsp_server::ls_types::{
    CodeAction, CodeActionKind, CodeActionOrCommand, CodeActionParams, CodeActionResponse, Command,
    CreateFile, CreateFileOptions, DocumentChangeOperation, DocumentChanges, OneOf,
    OptionalVersionedTextDocumentIdentifier, Range, ResourceOp, TextDocumentEdit, TextEdit,
    WorkspaceEdit,
};

use crate::convert::{lsp_pos_to_satz, path_to_uri};
use crate::state::SatzState;
use satz_core::model::LinkKind;

/// The vault-relative path components (`.md` appended to the last) a broken link target may be
/// turned into, or `None` when the target is not a plain note name: it would leave the vault
/// (`..`, drive letters, URL schemes), name a non-note file (`image.png`), or contain characters
/// no file name can hold.
fn note_components(target: &str) -> Option<Vec<String>> {
    let mut parts: Vec<String> = target
        .split(['/', '\\'])
        .filter(|part| !part.is_empty())
        .map(str::to_string)
        .collect();
    let last = parts.last_mut()?;
    if let Some(stripped) = last.strip_suffix(".md") {
        *last = stripped.to_string();
    }
    for part in &parts {
        let bad_char = |c: char| c.is_control() || ":*?\"<>|".contains(c);
        if part == "." || part == ".." || part.chars().any(bad_char) {
            return None;
        }
        if part.len() > 255 || part.ends_with(['.', ' ']) {
            return None;
        }
    }
    // A dotted final component is a file extension unless it reads as a version or a sentence
    // (`2.0121`, `Dr. Smith`); a real extension means the target is not a note.
    let last = parts.last()?;
    if last.is_empty() {
        return None;
    }
    if let Some((_, ext)) = last.rsplit_once('.')
        && ext.chars().any(char::is_alphabetic)
        && !ext.contains(char::is_whitespace)
    {
        return None;
    }
    let last = parts.last_mut()?;
    last.push_str(".md");
    Some(parts)
}

pub fn code_action(params: CodeActionParams, state: &SatzState) -> Option<CodeActionResponse> {
    let uri = params.text_document.uri.as_str();
    tracing::debug!(uri, "code_action");

    let (_, doc) = state.doc_for_uri(uri)?;

    let satz_start = lsp_pos_to_satz(params.range.start);
    let satz_end = lsp_pos_to_satz(params.range.end);
    let start_off = doc.line_index.position_to_byte(satz_start);
    let end_off = doc.line_index.position_to_byte(satz_end);
    let sel = satz_core::ByteRange::new(start_off.min(end_off), start_off.max(end_off));

    let mut actions: Vec<CodeActionOrCommand> = Vec::new();

    // 1. Check for Link under cursor / selection -> quickfixes
    let link_opt = doc.links.iter().find(|l| {
        if sel.is_empty() {
            l.range.contains(sel.start)
        } else {
            l.range.overlaps(&sel) || l.range.contains(sel.start)
        }
    });

    if let Some(link) = link_opt {
        if matches!(
            link.kind,
            LinkKind::WikiLink | LinkKind::Embed | LinkKind::Markdown
        ) && !satz_core::model::link::is_external_target(&link.target_doc)
        {
            match state.resolve(link, doc) {
                satz_core::LinkResolution::DocMissing if !link.target_doc.is_empty() => {
                    let components = note_components(&link.target_doc);
                    let target_path = components.as_ref().map(|parts| {
                        let mut path = match &state.vault_root {
                            Some(root) => root.clone(),
                            None => std::path::PathBuf::new(),
                        };
                        path.extend(parts);
                        path
                    });

                    if let (Some(parts), Some(target_path)) = (components, target_path)
                        && let Some(target_uri) = path_to_uri(&target_path)
                    {
                        let clean_name = parts.join("/");
                        let clean_name = clean_name.trim_end_matches(".md");
                        let title = parts
                            .last()
                            .map_or(clean_name, |last| last.strip_suffix(".md").unwrap_or(last));
                        let initial_content = satz_core::generate_document_template(title, None);

                        let ops = vec![
                            DocumentChangeOperation::Op(ResourceOp::Create(CreateFile {
                                uri: target_uri.clone(),
                                // A file that exists but is not indexed yet must not fail the whole
                                // edit, and must never be overwritten.
                                options: Some(CreateFileOptions {
                                    overwrite: Some(false),
                                    ignore_if_exists: Some(true),
                                }),
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

    // 2. Check for missing frontmatter -> "Insert frontmatter template" quickfix
    let source = doc.line_index.source();
    if !source.trim_start().starts_with("---")
        || (params.range.start.line == 0 && !source.starts_with("---"))
    {
        let title = doc
            .path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(&doc.title);
        let template_text = satz_core::generate_frontmatter_block(title, None);

        if let Ok(current_uri) = uri.parse() {
            let edit = TextEdit {
                range: Range::new(
                    tower_lsp_server::ls_types::Position::new(0, 0),
                    tower_lsp_server::ls_types::Position::new(0, 0),
                ),
                new_text: template_text,
            };

            let mut changes = std::collections::HashMap::new();
            changes.insert(current_uri, vec![edit]);

            let action = CodeAction {
                title: "Insert frontmatter template".to_string(),
                kind: Some(CodeActionKind::QUICKFIX),
                diagnostics: None,
                edit: Some(WorkspaceEdit {
                    changes: Some(changes),
                    ..Default::default()
                }),
                is_preferred: Some(false),
                disabled: None,
                command: None,
                data: None,
            };

            actions.push(CodeActionOrCommand::CodeAction(action));
        }
    }

    // Source action: some clients surface `workspace/executeCommand`s more discoverably through
    // the code action menu than through a dedicated command palette entry.
    if state.formatting_allowed() {
        let action = CodeAction {
            title: "Format entire vault".to_string(),
            kind: Some(CodeActionKind::SOURCE),
            diagnostics: None,
            edit: None,
            is_preferred: Some(false),
            disabled: None,
            command: Some(Command {
                title: "Format entire vault".to_string(),
                command: crate::handlers::execute_command::FORMAT_WORKSPACE_COMMAND.to_string(),
                arguments: None,
            }),
            data: None,
        };
        actions.push(CodeActionOrCommand::CodeAction(action));
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
    use tower_lsp_server::ls_types::{
        CodeActionContext, CreateFileOptions, Position, TextDocumentIdentifier,
    };

    #[test]
    fn test_code_action_create_missing_note() {
        let abs_a = if cfg!(windows) {
            Path::new("C:\\doc-a.md")
        } else {
            Path::new("/doc-a.md")
        };
        let rel_a = Path::new("doc-a.md");

        let doc_a = parse_document(
            "---\ntitle: Doc A\n---\n\nLink to [[missing-note]] here",
            rel_a,
        );

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
                "---\ntitle: Doc A\n---\n\nLink to [[missing-note]] here",
                1,
            ),
        );

        // Selection range covering the link partially
        let params = CodeActionParams {
            text_document: TextDocumentIdentifier {
                uri: uri_a_str.parse().unwrap(),
            },
            range: Range::new(Position::new(4, 5), Position::new(4, 20)),
            context: CodeActionContext::default(),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };

        let response = code_action(params, &state).expect("CodeAction response expected");
        // One quickfix for the broken link, plus the always-offered "Format entire vault" source
        // action.
        assert_eq!(response.len(), 2);

        let quickfix = response
            .iter()
            .find_map(|a| match a {
                CodeActionOrCommand::CodeAction(ca)
                    if ca.kind == Some(CodeActionKind::QUICKFIX) =>
                {
                    Some(ca)
                }
                _ => None,
            })
            .expect("quickfix action expected");
        assert!(quickfix.title.contains("Create note: \"missing-note\""));
        assert!(quickfix.edit.is_some());
        assert_source_action_present(&response);
    }

    // ---- create-note: which files may be created, and where ----

    fn vault_root() -> std::path::PathBuf {
        if cfg!(windows) {
            Path::new("C:\\vault").to_path_buf()
        } else {
            Path::new("/vault").to_path_buf()
        }
    }

    /// The "Create note" quickfix offered for a broken `[[target]]`, if any.
    fn create_note_action(target: &str) -> Option<CodeAction> {
        let text = format!("Link to [[{target}]] here");
        let rel_a = Path::new("doc-a.md");
        let mut state = SatzState::default();
        state.index = Index::build(vec![parse_document(&text, rel_a)]);
        state.vault_root = Some(vault_root());
        let (uri_str, abs) = if cfg!(windows) {
            ("file:///C:/vault/doc-a.md", "C:\\vault\\doc-a.md")
        } else {
            ("file:///vault/doc-a.md", "/vault/doc-a.md")
        };
        state.open_docs.insert(
            uri_str.to_string(),
            crate::state::OpenDocument::new(uri_str, Path::new(abs).to_path_buf(), &text, 1),
        );
        let params = CodeActionParams {
            text_document: TextDocumentIdentifier {
                uri: uri_str.parse().unwrap(),
            },
            range: Range::new(Position::new(0, 10), Position::new(0, 10)),
            context: CodeActionContext::default(),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };
        code_action(params, &state)?
            .into_iter()
            .find_map(|a| match a {
                CodeActionOrCommand::CodeAction(ca)
                    if ca.kind == Some(CodeActionKind::QUICKFIX)
                        && ca.title.starts_with("Create note") =>
                {
                    Some(ca)
                }
                _ => None,
            })
    }

    /// `(created file path relative to the vault, CreateFile options, new file content)`.
    fn created_file(action: &CodeAction) -> (String, Option<CreateFileOptions>, String) {
        let Some(DocumentChanges::Operations(ops)) =
            &action.edit.as_ref().unwrap().document_changes
        else {
            panic!("expected document change operations");
        };
        let mut create = None;
        let mut content = String::new();
        for op in ops {
            match op {
                DocumentChangeOperation::Op(ResourceOp::Create(c)) => create = Some(c),
                DocumentChangeOperation::Edit(e) => {
                    if let Some(OneOf::Left(edit)) = e.edits.first() {
                        content = edit.new_text.clone();
                    }
                }
                _ => {}
            }
        }
        let create = create.expect("a CreateFile operation");
        let path = crate::convert::uri_to_path(create.uri.as_str()).expect("file URI");
        let rel = path
            .strip_prefix(vault_root())
            .unwrap_or_else(|_| panic!("{path:?} is outside the vault {:?}", vault_root()))
            .to_string_lossy()
            .replace('\\', "/");
        (rel, create.options.clone(), content)
    }

    #[test]
    fn create_note_puts_the_file_where_the_link_says_inside_the_vault() {
        for (target, expected) in [
            ("new", "new.md"),
            ("a/b/c", "a/b/c.md"),
            ("tlp/2.0121", "tlp/2.0121.md"),
            ("v1.2.3", "v1.2.3.md"),
            ("Türkçe Not", "Türkçe Not.md"),
            ("with.md", "with.md"),
            ("sub\\name", "sub/name.md"),
            ("a//b", "a/b.md"),
            ("/abs", "abs.md"),
            ("note with   spaces", "note with   spaces.md"),
        ] {
            let action = create_note_action(target)
                .unwrap_or_else(|| panic!("{target:?} should offer a create-note fix"));
            let (rel, _, _) = created_file(&action);
            assert_eq!(rel, expected, "target {target:?}");
        }
    }

    #[test]
    fn create_note_is_not_offered_for_targets_that_are_not_safe_note_names() {
        for target in [
            // escaping the vault
            "../../x",
            "a/../../x",
            "..",
            ".",
            "./x",
            "..\\..\\x",
            // drive letters / URL schemes / stream names
            "C:x",
            "C:\\x",
            "a:b",
            "mailto:a@b.c",
            // not a note: has a file extension
            "image.png",
            "doc.pdf",
            "archive.tar.gz",
            "photo.JPG",
            // characters Windows forbids in file names
            "con*",
            "a?b",
            "a<b",
            "a>b",
            "quote\"",
            // trailing dot
            "trailing.",
            "dir./name",
        ] {
            assert!(
                create_note_action(target).is_none(),
                "{target:?} must not offer to create a file"
            );
        }
        // Over-long names.
        assert!(create_note_action(&"a".repeat(256)).is_none());
        assert!(create_note_action(&"a".repeat(255)).is_some());
    }

    #[test]
    fn create_note_never_overwrites_and_tolerates_an_existing_unindexed_file() {
        let action = create_note_action("new").unwrap();
        let (_, options, _) = created_file(&action);
        let options = options.expect("explicit CreateFile options");
        assert_eq!(options.overwrite, Some(false));
        assert_eq!(options.ignore_if_exists, Some(true));
    }

    #[test]
    fn the_created_note_has_valid_frontmatter_even_for_awkward_names() {
        for (target, title) in [
            ("new", "new"),
            ("Q- what", "Q- what"),
            ("2.01231", "2.01231"),
            ("yes", "yes"),
            ("Türkçe Not", "Türkçe Not"),
            ("a/b/c", "c"),
        ] {
            let action = create_note_action(target).unwrap();
            let (_, _, content) = created_file(&action);
            let parsed = parse_document(&content, Path::new("n.md"));
            assert!(parsed.frontmatter_range.is_some(), "{target:?}:\n{content}");
            assert_eq!(
                parsed.frontmatter.title.as_deref(),
                Some(title),
                "{target:?}:\n{content}"
            );
            assert!(parsed.frontmatter.aliases.is_empty(), "{target:?}");
        }
    }

    #[test]
    fn test_code_action_insert_frontmatter_template() {
        let rel_a = Path::new("doc-a.md");
        let doc_a = parse_document("# Doc A Without Frontmatter", rel_a);

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
                Path::new(if cfg!(windows) {
                    "C:\\doc-a.md"
                } else {
                    "/doc-a.md"
                })
                .to_path_buf(),
                "# Doc A Without Frontmatter",
                1,
            ),
        );

        let params = CodeActionParams {
            text_document: TextDocumentIdentifier {
                uri: uri_a_str.parse().unwrap(),
            },
            range: Range::new(Position::new(0, 0), Position::new(0, 0)),
            context: CodeActionContext::default(),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };

        let response = code_action(params, &state).expect("CodeAction response expected");
        assert!(
            response
                .iter()
                .any(|a| matches!(a, CodeActionOrCommand::CodeAction(ca) if ca.title == "Insert frontmatter template"))
        );
    }

    #[test]
    fn test_code_action_add_missing_heading() {
        let rel_a = Path::new("doc-a.md");
        let rel_b = Path::new("doc-b.md");

        let doc_a = parse_document(
            "---\ntitle: Doc A\n---\n\nLink to [[doc-b#İstemciler]] here",
            rel_a,
        );
        let doc_b = parse_document("---\ntitle: Doc B\n---\n\nExisting text.", rel_b);

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
                "---\ntitle: Doc A\n---\n\nLink to [[doc-b#İstemciler]] here",
                1,
            ),
        );

        let params = CodeActionParams {
            text_document: TextDocumentIdentifier {
                uri: uri_a_str.parse().unwrap(),
            },
            range: Range::new(Position::new(4, 12), Position::new(4, 12)),
            context: CodeActionContext::default(),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };

        let response = code_action(params, &state).expect("CodeAction response expected");
        // One quickfix for the missing heading, plus the always-offered "Format entire vault"
        // source action.
        assert_eq!(response.len(), 2);

        let quickfix = response
            .iter()
            .find_map(|a| match a {
                CodeActionOrCommand::CodeAction(ca)
                    if ca.kind == Some(CodeActionKind::QUICKFIX) =>
                {
                    Some(ca)
                }
                _ => None,
            })
            .expect("quickfix action expected");
        assert!(
            quickfix
                .title
                .contains("Add heading '## İstemciler' to \"Doc B\"")
        );
        assert!(quickfix.edit.is_some());
        assert_source_action_present(&response);
    }

    fn assert_source_action_present(response: &[CodeActionOrCommand]) {
        let source_action = response
            .iter()
            .find_map(|a| match a {
                CodeActionOrCommand::CodeAction(ca) if ca.kind == Some(CodeActionKind::SOURCE) => {
                    Some(ca)
                }
                _ => None,
            })
            .expect("'Format entire vault' source action expected");
        assert_eq!(source_action.title, "Format entire vault");
        let command = source_action
            .command
            .as_ref()
            .expect("source action should carry a Command");
        assert_eq!(
            command.command,
            crate::handlers::execute_command::FORMAT_WORKSPACE_COMMAND
        );
    }

    #[test]
    fn format_entire_vault_is_offered_only_while_the_config_is_valid() {
        let rel_a = Path::new("doc-a.md");
        let text = "---\ntitle: Doc A\n---\n\nPlain content, no links.";
        let doc_a = parse_document(text, rel_a);
        let uri_a_str = if cfg!(windows) {
            "file:///C:/doc-a.md"
        } else {
            "file:///doc-a.md"
        };
        let abs_a = Path::new(if cfg!(windows) {
            "C:\\doc-a.md"
        } else {
            "/doc-a.md"
        });

        let mut state = SatzState::default();
        state.index = Index::build(vec![doc_a]);
        state.vault_root = Some(if cfg!(windows) {
            Path::new("C:\\").to_path_buf()
        } else {
            Path::new("/").to_path_buf()
        });
        state.open_docs.insert(
            uri_a_str.to_string(),
            crate::state::OpenDocument::new(uri_a_str, abs_a.to_path_buf(), text, 1),
        );
        let params = || CodeActionParams {
            text_document: TextDocumentIdentifier {
                uri: uri_a_str.parse().unwrap(),
            },
            range: Range::new(Position::new(0, 0), Position::new(0, 0)),
            context: CodeActionContext::default(),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };

        // Control: valid config -> the source action is offered.
        let response = code_action(params(), &state).expect("source action expected");
        assert_source_action_present(&response);

        // Invalid config -> nothing is offered (no quickfix applies to this document either).
        state.config_error = Some("invalid .satz.toml: line 1".to_string());
        assert!(code_action(params(), &state).is_none());

        // Fixed -> offered again.
        state.config_error = None;
        let response = code_action(params(), &state).expect("source action expected");
        assert_source_action_present(&response);
    }

    #[test]
    fn test_source_action_absent_when_formatter_disabled() {
        let rel_a = Path::new("doc-a.md");
        // Has frontmatter already and no links, so no quickfix action applies — isolates whether
        // the source action alone appears.
        let doc_a = parse_document("---\ntitle: Doc A\n---\n\nPlain content, no links.", rel_a);

        let mut state = SatzState::default();
        state.config.formatter.enabled = false;
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
                Path::new(if cfg!(windows) {
                    "C:\\doc-a.md"
                } else {
                    "/doc-a.md"
                })
                .to_path_buf(),
                "---\ntitle: Doc A\n---\n\nPlain content, no links.",
                1,
            ),
        );

        let params = CodeActionParams {
            text_document: TextDocumentIdentifier {
                uri: uri_a_str.parse().unwrap(),
            },
            range: Range::new(Position::new(0, 0), Position::new(0, 0)),
            context: CodeActionContext::default(),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };

        // No quickfix applies here, so with the formatter disabled there should be nothing
        // offered at all.
        assert!(code_action(params, &state).is_none());
    }
}
