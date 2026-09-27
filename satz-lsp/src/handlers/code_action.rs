use tower_lsp_server::ls_types::{
    CodeAction, CodeActionContext, CodeActionKind, CodeActionOrCommand, CodeActionParams,
    CodeActionResponse, Command, CreateFile, CreateFileOptions, Diagnostic,
    DocumentChangeOperation, DocumentChanges, NumberOrString, OneOf,
    OptionalVersionedTextDocumentIdentifier, Position, Range, ResourceOp, TextDocumentEdit,
    TextEdit, WorkspaceEdit,
};

use crate::convert::{byte_range_to_lsp, lsp_pos_to_satz, path_to_uri};
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

/// The diagnostics from the request's context that a fix for `link` answers: one of `codes`, on
/// the link's own range. `None` when there is none, so a client never ties a fix to the wrong squiggle.
fn diagnostics_fixed_by(
    context: &CodeActionContext,
    doc: &satz_core::Document,
    link: &satz_core::Link,
    codes: &[&str],
) -> Option<Vec<Diagnostic>> {
    let range = byte_range_to_lsp(link.range, &doc.line_index);
    let ends_before = |a: Position, b: Position| (a.line, a.character) < (b.line, b.character);
    let found: Vec<Diagnostic> = context
        .diagnostics
        .iter()
        .filter(|d| {
            matches!(&d.code, Some(NumberOrString::String(c)) if codes.contains(&c.as_str()))
                && !ends_before(d.range.end, range.start)
                && !ends_before(range.end, d.range.start)
        })
        .cloned()
        .collect();
    (!found.is_empty()).then_some(found)
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

    if let Some(link) = link_opt
        && matches!(
            link.kind,
            LinkKind::WikiLink | LinkKind::Embed | LinkKind::Markdown
        )
        && !satz_core::model::link::is_external_target(&link.target_doc)
    {
        match state.resolve(link, doc) {
            satz_core::LinkResolution::DocMissing if !link.target_doc.is_empty() => {
                let components = note_components(&link.target_doc);
                let target_path = components.as_ref().map(|parts| {
                    let mut path = match state.vault_root() {
                        Some(root) => root.to_path_buf(),
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
                        diagnostics: diagnostics_fixed_by(
                            &params.context,
                            doc,
                            link,
                            &["broken-link", "broken-embed"],
                        ),
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
                    let target_path = state.doc_path(target_doc);
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

                        // The index was refreshed against every open buffer before this request,
                        // so `source` is the target's live text; an open target names its version.
                        let version = state
                            .open_doc_for_path(&target_path)
                            .map(|(_, open)| open.version);
                        let document_edit = TextDocumentEdit {
                            text_document: OptionalVersionedTextDocumentIdentifier {
                                uri: target_uri,
                                version,
                            },
                            edits: vec![OneOf::Left(edit)],
                        };

                        let action = CodeAction {
                            title: format!(
                                "Add heading '## {}' to \"{}\"",
                                heading_name, target_doc.title
                            ),
                            kind: Some(CodeActionKind::QUICKFIX),
                            diagnostics: diagnostics_fixed_by(
                                &params.context,
                                doc,
                                link,
                                &["broken-heading"],
                            ),
                            edit: Some(WorkspaceEdit {
                                document_changes: Some(DocumentChanges::Edits(vec![document_edit])),
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

    // 2. Check for missing frontmatter -> "Insert frontmatter template" quickfix
    let source = doc.line_index.source();
    if doc.frontmatter_range.is_none() && !source.starts_with("---") {
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
mod tests;
