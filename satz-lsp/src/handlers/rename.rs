#![allow(clippy::collapsible_if)]

use std::collections::HashMap;
use tower_lsp_server::ls_types::{
    DocumentChangeOperation, DocumentChanges, OneOf, OptionalVersionedTextDocumentIdentifier,
    RenameFile, RenameParams, ResourceOp, TextDocumentEdit, TextEdit, Uri, WorkspaceEdit,
};

use crate::convert::{byte_range_to_lsp, lsp_pos_to_satz, path_to_uri};
use crate::state::SatzState;
use satz_core::model::LinkKind;

pub fn rename(params: RenameParams, state: &SatzState) -> Option<WorkspaceEdit> {
    let uri = params.text_document_position.text_document.uri.as_str();
    let pos = params.text_document_position.position;
    let new_name = params.new_name.trim();

    let open_doc = state.open_docs.get(uri)?;
    let rel_path = match &state.vault_root {
        Some(root) => open_doc.path.strip_prefix(root).unwrap_or(&open_doc.path),
        None => &open_doc.path,
    };
    let rel_path_str = rel_path.to_string_lossy().replace('\\', "/");
    let doc_id = satz_core::DocId::new(&rel_path_str);
    let doc = state.index.get_doc(&doc_id)?;

    let satz_pos = lsp_pos_to_satz(pos);
    let byte_offset = doc.line_index.position_to_byte(satz_pos);

    // 1. Check if cursor is on a Heading definition
    if let Some(h) = doc.headings.iter().find(|h| h.range.contains(byte_offset)) {
        let mut changes: HashMap<Uri, Vec<TextEdit>> = HashMap::new();

        // Edit 1: In current document, replace heading definition
        let heading_level_hashes = "#".repeat(h.level as usize);
        let new_heading_def = format!("{} {}", heading_level_hashes, new_name);
        let doc_path = match &state.vault_root {
            Some(root) if !doc.path.is_absolute() => root.join(&doc.path),
            _ => doc.path.clone(),
        };
        if let Some(url) = path_to_uri(&doc_path) {
            changes.entry(url).or_default().push(TextEdit {
                range: byte_range_to_lsp(h.range, &doc.line_index),
                new_text: new_heading_def,
            });
        }

        // Edit 2: Update all incoming backlinks to this heading across all documents
        for src_doc in state.index.documents() {
            let src_path = match &state.vault_root {
                Some(root) if !src_doc.path.is_absolute() => root.join(&src_doc.path),
                _ => src_doc.path.clone(),
            };
            let src_url = match path_to_uri(&src_path) {
                Some(u) => u,
                None => continue,
            };

            for link in &src_doc.links {
                let matches_doc = if link.target_doc.is_empty() {
                    src_doc.id == doc_id
                } else {
                    state.index.resolve_link(&link.target_doc) == Some(&doc_id)
                };

                let matches_heading = link.target_heading.as_deref() == Some(&h.slug)
                    || link.target_heading.as_deref() == Some(&h.text);

                if matches_doc && matches_heading {
                    let new_link_text = format_wikilink_heading(
                        link.kind,
                        &link.target_doc,
                        new_name,
                        link.display.as_deref(),
                    );

                    changes.entry(src_url.clone()).or_default().push(TextEdit {
                        range: byte_range_to_lsp(link.range, &src_doc.line_index),
                        new_text: new_link_text,
                    });
                }
            }
        }

        return Some(WorkspaceEdit {
            changes: Some(changes),
            ..Default::default()
        });
    }

    // 2. Check if cursor is on a Link
    if let Some(link) = doc.links.iter().find(|l| l.range.contains(byte_offset)) {
        // A) If link has a target_heading and cursor is pointing to heading, rename the heading
        if let Some(target_heading_str) = &link.target_heading {
            let target_id = if link.target_doc.is_empty() {
                &doc_id
            } else {
                state.index.resolve_link(&link.target_doc)?
            };
            let target_doc = state.index.get_doc(target_id)?;

            if let Some(h) = target_doc
                .headings
                .iter()
                .find(|h| h.matches(target_heading_str))
            {
                let mut changes: HashMap<Uri, Vec<TextEdit>> = HashMap::new();

                // Update heading definition in target document
                let heading_level_hashes = "#".repeat(h.level as usize);
                let new_heading_def = format!("{} {}", heading_level_hashes, new_name);
                let target_path = match &state.vault_root {
                    Some(root) if !target_doc.path.is_absolute() => root.join(&target_doc.path),
                    _ => target_doc.path.clone(),
                };
                if let Some(url) = path_to_uri(&target_path) {
                    changes.entry(url).or_default().push(TextEdit {
                        range: byte_range_to_lsp(h.range, &target_doc.line_index),
                        new_text: new_heading_def,
                    });
                }

                // Update all references
                for src_doc in state.index.documents() {
                    let src_path = match &state.vault_root {
                        Some(root) if !src_doc.path.is_absolute() => root.join(&src_doc.path),
                        _ => src_doc.path.clone(),
                    };
                    let src_url = match path_to_uri(&src_path) {
                        Some(u) => u,
                        None => continue,
                    };

                    for l in &src_doc.links {
                        let matches_doc = if l.target_doc.is_empty() {
                            src_doc.id == *target_id
                        } else {
                            state.index.resolve_link(&l.target_doc) == Some(target_id)
                        };

                        let matches_heading = l.target_heading.as_deref() == Some(&h.slug)
                            || l.target_heading.as_deref() == Some(&h.text);

                        if matches_doc && matches_heading {
                            let new_link_text = format_wikilink_heading(
                                l.kind,
                                &l.target_doc,
                                new_name,
                                l.display.as_deref(),
                            );

                            changes.entry(src_url.clone()).or_default().push(TextEdit {
                                range: byte_range_to_lsp(l.range, &src_doc.line_index),
                                new_text: new_link_text,
                            });
                        }
                    }
                }

                return Some(WorkspaceEdit {
                    changes: Some(changes),
                    ..Default::default()
                });
            }
        }

        // B) Renaming target document
        if !link.target_doc.is_empty() {
            let target_id = state.index.resolve_link(&link.target_doc)?;
            let target_doc = state.index.get_doc(target_id)?;
            let clean_new_doc_name = new_name.trim_end_matches(".md");

            let mut changes: HashMap<Uri, Vec<TextEdit>> = HashMap::new();

            for src_doc in state.index.documents() {
                let src_path = match &state.vault_root {
                    Some(root) if !src_doc.path.is_absolute() => root.join(&src_doc.path),
                    _ => src_doc.path.clone(),
                };
                let src_url = match path_to_uri(&src_path) {
                    Some(u) => u,
                    None => continue,
                };

                for l in &src_doc.links {
                    if !l.target_doc.is_empty()
                        && state.index.resolve_link(&l.target_doc) == Some(target_id)
                    {
                        let new_link_text = format_wikilink_doc(
                            l.kind,
                            clean_new_doc_name,
                            l.target_heading.as_deref(),
                            l.target_block.as_deref(),
                            l.display.as_deref(),
                        );

                        changes.entry(src_url.clone()).or_default().push(TextEdit {
                            range: byte_range_to_lsp(l.range, &src_doc.line_index),
                            new_text: new_link_text,
                        });
                    }
                }
            }

            // Also produce file rename operation if possible
            let old_doc_path = match &state.vault_root {
                Some(root) if !target_doc.path.is_absolute() => root.join(&target_doc.path),
                _ => target_doc.path.clone(),
            };
            let new_doc_path = old_doc_path.with_file_name(format!("{}.md", clean_new_doc_name));

            if let (Some(old_uri), Some(new_uri)) =
                (path_to_uri(&old_doc_path), path_to_uri(&new_doc_path))
            {
                let mut document_changes = Vec::new();

                // 1. Rename operation
                document_changes.push(DocumentChangeOperation::Op(ResourceOp::Rename(
                    RenameFile {
                        old_uri,
                        new_uri,
                        options: None,
                        annotation_id: None,
                    },
                )));

                // 2. Text edits
                for (url, edits) in changes {
                    document_changes.push(DocumentChangeOperation::Edit(TextDocumentEdit {
                        text_document: OptionalVersionedTextDocumentIdentifier {
                            uri: url,
                            version: None,
                        },
                        edits: edits.into_iter().map(OneOf::Left).collect(),
                    }));
                }

                return Some(WorkspaceEdit {
                    document_changes: Some(DocumentChanges::Operations(document_changes)),
                    ..Default::default()
                });
            }

            return Some(WorkspaceEdit {
                changes: Some(changes),
                ..Default::default()
            });
        }
    }

    None
}

fn format_wikilink_heading(
    kind: LinkKind,
    target_doc: &str,
    new_heading: &str,
    display: Option<&str>,
) -> String {
    let prefix = if kind == LinkKind::Embed { "![[" } else { "[[" };
    let disp = display.map(|d| format!("|{}", d)).unwrap_or_default();
    format!("{}{}#{}{}", prefix, target_doc, new_heading, disp) + "]]"
}

fn format_wikilink_doc(
    kind: LinkKind,
    new_doc: &str,
    target_heading: Option<&str>,
    target_block: Option<&str>,
    display: Option<&str>,
) -> String {
    let prefix = if kind == LinkKind::Embed { "![[" } else { "[[" };
    let heading = target_heading
        .map(|h| format!("#{}", h))
        .unwrap_or_default();
    let block = target_block.map(|b| format!("#^{}", b)).unwrap_or_default();
    let disp = display.map(|d| format!("|{}", d)).unwrap_or_default();
    format!("{}{}{}{}{}", prefix, new_doc, heading, block, disp) + "]]"
}

#[cfg(test)]
#[allow(unused_variables)]
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use super::*;
    use satz_core::{Index, parse_document};
    use std::path::Path;
    use tower_lsp_server::ls_types::{
        Position, TextDocumentIdentifier, TextDocumentPositionParams,
    };

    #[test]
    fn test_rename_heading_and_backlinks() {
        let abs_a = if cfg!(windows) {
            Path::new("C:\\doc-a.md")
        } else {
            Path::new("/doc-a.md")
        };
        let abs_b = if cfg!(windows) {
            Path::new("C:\\doc-b.md")
        } else {
            Path::new("/doc-b.md")
        };

        let rel_a = Path::new("doc-a.md");
        let rel_b = Path::new("doc-b.md");

        let doc_a = parse_document("# Old Heading\n\nSome text", rel_a);
        let doc_b = parse_document("# Doc B\n\nSee [[doc-a#Old Heading|display text]]", rel_b);

        let mut state = SatzState::default();
        state.index = Index::build(vec![doc_a.clone(), doc_b.clone()]);
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
                "# Old Heading\n\nSome text",
                1,
            ),
        );

        let params = RenameParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: uri_a_str.parse().unwrap(),
                },
                position: Position::new(0, 3), // on "# Old Heading"
            },
            new_name: "New Heading".to_string(),
            work_done_progress_params: Default::default(),
        };

        let edit = rename(params, &state).expect("WorkspaceEdit expected");
        let changes = edit.changes.expect("Changes map expected");
        let uri_a = path_to_uri(abs_a).unwrap();
        let uri_b = path_to_uri(abs_b).unwrap();
        let edits_a = &changes[&uri_a];
        assert_eq!(edits_a[0].new_text, "# New Heading");

        let edits_b = &changes[&uri_b];
        assert_eq!(edits_b[0].new_text, "[[doc-a#New Heading|display text]]");
    }

    #[test]
    fn test_rename_document_updates_backlinks() {
        let abs_a = if cfg!(windows) {
            Path::new("C:\\doc-a.md")
        } else {
            Path::new("/doc-a.md")
        };
        let abs_b = if cfg!(windows) {
            Path::new("C:\\doc-b.md")
        } else {
            Path::new("/doc-b.md")
        };

        let rel_a = Path::new("doc-a.md");
        let rel_b = Path::new("doc-b.md");

        let doc_a = parse_document("# Doc A", rel_a);
        let doc_b = parse_document("# Doc B\n\nLink to [[doc-a]] here", rel_b);

        let mut state = SatzState::default();
        state.index = Index::build(vec![doc_a, doc_b]);
        state.vault_root = Some(if cfg!(windows) {
            Path::new("C:\\").to_path_buf()
        } else {
            Path::new("/").to_path_buf()
        });

        let uri_b_str = if cfg!(windows) {
            "file:///C:/doc-b.md"
        } else {
            "file:///doc-b.md"
        };

        state.open_docs.insert(
            uri_b_str.to_string(),
            crate::state::OpenDocument::new(
                uri_b_str,
                abs_b.to_path_buf(),
                "# Doc B\n\nLink to [[doc-a]] here",
                1,
            ),
        );

        let params = RenameParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: uri_b_str.parse().unwrap(),
                },
                position: Position::new(2, 10), // on "[[doc-a]]"
            },
            new_name: "renamed-a".to_string(),
            work_done_progress_params: Default::default(),
        };

        let edit = rename(params, &state).expect("WorkspaceEdit expected");
        if let Some(DocumentChanges::Operations(ops)) = edit.document_changes {
            assert!(
                ops.iter()
                    .any(|op| matches!(op, DocumentChangeOperation::Op(ResourceOp::Rename(_))))
            );
            assert!(
                ops.iter()
                    .any(|op| matches!(op, DocumentChangeOperation::Edit(_)))
            );
        } else {
            panic!("Expected DocumentChanges::Operations");
        }
    }
}
