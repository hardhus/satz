#![allow(clippy::collapsible_if)]

use std::collections::HashMap;
use tower_lsp_server::ls_types::{
    DocumentChangeOperation, DocumentChanges, OneOf, OptionalVersionedTextDocumentIdentifier,
    PrepareRenameResponse, RenameFile, RenameFileOptions, RenameParams, ResourceOp,
    TextDocumentEdit, TextDocumentPositionParams, TextEdit, Uri, WorkspaceEdit,
};

use crate::convert::{byte_range_to_lsp, lsp_pos_to_satz, path_to_uri};
use crate::state::SatzState;
use satz_core::ByteRange;
use satz_core::model::{Document, Heading, LinkKind};

/// The part of a heading line that is its text: without the `#` markers, the closing `#`s, a
/// trailing ` ^block-id` or (setext) the underline. Replacing exactly this range renames the
/// heading and leaves its level, line ending and block id alone.
struct HeadingText {
    range: ByteRange,
    /// The heading is `##` with nothing after the markers, so the new text needs a space first.
    needs_space: bool,
}

fn heading_text_range(source: &str, heading: &Heading) -> Option<HeadingText> {
    let base = heading.range.start;
    let block = source.get(base..heading.range.end.min(source.len()))?;
    let is_blank = |c: char| c == ' ' || c == '\t';

    let mut lines: Vec<(usize, &str)> = Vec::new();
    let mut offset = 0;
    for line in block.split_inclusive('\n') {
        lines.push((offset, line));
        offset += line.len();
    }
    let (first_offset, first) = *lines.first()?;
    let first = first.trim_end_matches(['\n', '\r']);
    let indent = first.len() - first.trim_start_matches(is_blank).len();

    let (start, mut end, needs_space) = if first[indent..].starts_with('#') {
        // ATX: `## text ##`
        let hashes = first[indent..].len() - first[indent..].trim_start_matches('#').len();
        let after_marker = indent + hashes;
        let gap =
            first[after_marker..].len() - first[after_marker..].trim_start_matches(is_blank).len();
        let start = after_marker + gap;
        let mut end = first.trim_end_matches(is_blank).len().max(start);
        let text = &first[start..end];
        let without_closing = text.trim_end_matches('#');
        if without_closing.len() != text.len()
            && (without_closing.is_empty() || without_closing.ends_with(is_blank))
        {
            end = start + without_closing.trim_end_matches(is_blank).len();
        }
        (start, end, start == end && gap == 0)
    } else {
        // Setext: every line but the underline is text.
        let text_lines = if lines.len() > 1 {
            &lines[..lines.len() - 1]
        } else {
            &lines[..]
        };
        let (last_offset, last) = *text_lines.last()?;
        let end = last_offset + last.trim_end_matches(['\n', '\r', ' ', '\t']).len();
        (first_offset + indent, end.max(first_offset + indent), false)
    };

    // A trailing ` ^block-id` belongs to the heading, not to its text.
    end = start + Heading::split_block_id(&block[start..end]).0.len();

    Some(HeadingText {
        range: ByteRange::new(base + start, base + end),
        needs_space,
    })
}

pub fn prepare_rename(
    params: TextDocumentPositionParams,
    state: &SatzState,
) -> Option<PrepareRenameResponse> {
    let uri = params.text_document.uri.as_str();
    let pos = params.position;
    tracing::debug!(uri, ?pos, "prepare_rename");

    let open_doc = state.open_docs.get(uri)?;
    let rel_path =
        crate::state::SatzState::get_rel_path(&open_doc.path, state.vault_root.as_deref());
    let rel_path_str = rel_path.to_string_lossy().replace('\\', "/");
    let doc_id = satz_core::DocId::new(&rel_path_str);
    let doc = state.index.get_doc(&doc_id)?;

    let satz_pos = lsp_pos_to_satz(pos);
    let byte_offset = doc.line_index.position_to_byte(satz_pos);

    // 1. Heading definition
    if let Some(h) = doc.headings.iter().find(|h| h.range.contains(byte_offset)) {
        let text = heading_text_range(doc.line_index.source(), h)?;
        return Some(PrepareRenameResponse::RangeWithPlaceholder {
            range: byte_range_to_lsp(text.range, &doc.line_index),
            placeholder: doc.line_index.source()[text.range.start..text.range.end].to_string(),
        });
    }

    // 2. Link
    if let Some(link) = doc.links.iter().find(|l| l.range.contains(byte_offset)) {
        if let Some(target_heading) = &link.target_heading {
            return Some(PrepareRenameResponse::RangeWithPlaceholder {
                range: byte_range_to_lsp(link.range, &doc.line_index),
                placeholder: target_heading.clone(),
            });
        }
        if !link.target_doc.is_empty() {
            return Some(PrepareRenameResponse::RangeWithPlaceholder {
                range: byte_range_to_lsp(link.range, &doc.line_index),
                placeholder: link.target_doc.clone(),
            });
        }
    }

    None
}

/// Renames the heading or note under the cursor.
///
/// `Ok(None)` means there is nothing to rename at that position; `Err` carries a message for the
/// user (invalid name, name already taken, link that points nowhere).
pub fn rename(params: RenameParams, state: &SatzState) -> Result<Option<WorkspaceEdit>, String> {
    let uri = params.text_document_position.text_document.uri.as_str();
    let pos = params.text_document_position.position;
    let new_name = params.new_name.trim();
    tracing::debug!(uri, ?pos, new_name, "rename");

    let Some(open_doc) = state.open_docs.get(uri) else {
        return Ok(None);
    };
    let rel_path =
        crate::state::SatzState::get_rel_path(&open_doc.path, state.vault_root.as_deref());
    let rel_path_str = rel_path.to_string_lossy().replace('\\', "/");
    let doc_id = satz_core::DocId::new(&rel_path_str);
    let Some(doc) = state.index.get_doc(&doc_id) else {
        return Ok(None);
    };

    let satz_pos = lsp_pos_to_satz(pos);
    let byte_offset = doc.line_index.position_to_byte(satz_pos);

    // 1. Cursor on a heading definition
    if let Some(h) = doc.headings.iter().find(|h| h.range.contains(byte_offset)) {
        validate_heading_name(new_name)?;
        return Ok(rename_heading(state, &doc_id, doc, h, new_name));
    }

    // 2. Cursor on a link
    let Some(link) = doc.links.iter().find(|l| l.range.contains(byte_offset)) else {
        return Ok(None);
    };

    // A) A link with a heading renames that heading, wherever it is defined.
    if let Some(target_heading) = &link.target_heading {
        let target_id = if link.target_doc.is_empty() {
            &doc_id
        } else {
            state.index.resolve_link(&link.target_doc).ok_or_else(|| {
                format!("cannot rename: note '{}' does not exist", link.target_doc)
            })?
        };
        let target_doc = state
            .index
            .get_doc(target_id)
            .ok_or_else(|| format!("cannot rename: note '{}' does not exist", link.target_doc))?;
        let h = target_doc
            .headings
            .iter()
            .find(|h| h.matches(target_heading))
            .ok_or_else(|| {
                format!(
                    "cannot rename: heading '{}' not found in '{}'",
                    target_heading, target_doc.title
                )
            })?;
        validate_heading_name(new_name)?;
        return Ok(rename_heading(state, target_id, target_doc, h, new_name));
    }

    // B) Otherwise it renames the note the link points to.
    if link.target_doc.is_empty() {
        return Ok(None);
    }
    let target_id = state
        .index
        .resolve_link(&link.target_doc)
        .ok_or_else(|| format!("cannot rename: note '{}' does not exist", link.target_doc))?;
    let target_doc = state
        .index
        .get_doc(target_id)
        .ok_or_else(|| format!("cannot rename: note '{}' does not exist", link.target_doc))?;
    let clean_new_doc_name = validate_note_name(new_name)?;

    let old_doc_path = absolute_path(state, target_doc);
    let new_doc_path = old_doc_path.with_file_name(format!("{clean_new_doc_name}.md"));
    let new_rel = match &state.vault_root {
        Some(root) => new_doc_path.strip_prefix(root).unwrap_or(&new_doc_path),
        None => &new_doc_path,
    };
    let new_id = satz_core::DocId::new(new_rel.to_string_lossy().replace('\\', "/"));
    if new_id != *target_id && state.index.get_doc(&new_id).is_some() {
        return Err(format!(
            "cannot rename: a note named '{clean_new_doc_name}' already exists there"
        ));
    }

    let mut changes: HashMap<Uri, Vec<TextEdit>> = HashMap::new();

    // Scoped to backlinks + target
    let mut candidate_ids: std::collections::HashSet<&satz_core::DocId> =
        state.index.backlinks_of(target_id).collect();
    candidate_ids.insert(target_id);

    for src_id in candidate_ids {
        let Some(src_doc) = state.index.get_doc(src_id) else {
            continue;
        };
        let Some(src_url) = path_to_uri(&absolute_path(state, src_doc)) else {
            continue;
        };

        for l in &src_doc.links {
            if !l.target_doc.is_empty()
                && state.index.resolve_link(&l.target_doc) == Some(target_id)
            {
                let new_link_text = format_wikilink_doc(
                    l.kind,
                    &clean_new_doc_name,
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
    if let (Some(old_uri), Some(new_uri)) = (path_to_uri(&old_doc_path), path_to_uri(&new_doc_path))
    {
        // Text edits first: they address the files by their current URIs, which stop existing
        // once the rename has been applied.
        let mut document_changes: Vec<DocumentChangeOperation> = changes
            .into_iter()
            .map(|(url, edits)| {
                DocumentChangeOperation::Edit(TextDocumentEdit {
                    text_document: OptionalVersionedTextDocumentIdentifier {
                        uri: url,
                        version: None,
                    },
                    edits: edits.into_iter().map(OneOf::Left).collect(),
                })
            })
            .collect();
        document_changes.push(DocumentChangeOperation::Op(ResourceOp::Rename(
            RenameFile {
                old_uri,
                new_uri,
                options: Some(RenameFileOptions {
                    overwrite: Some(false),
                    ignore_if_exists: None,
                }),
                annotation_id: None,
            },
        )));

        return Ok(Some(WorkspaceEdit {
            document_changes: Some(DocumentChanges::Operations(document_changes)),
            ..Default::default()
        }));
    }

    Ok(Some(WorkspaceEdit {
        changes: Some(changes),
        ..Default::default()
    }))
}

fn absolute_path(state: &SatzState, doc: &Document) -> std::path::PathBuf {
    match &state.vault_root {
        Some(root) if !doc.path.is_absolute() => root.join(&doc.path),
        _ => doc.path.clone(),
    }
}

/// Edits that rename `heading` (defined in `target_doc`) and every link pointing at it.
fn rename_heading(
    state: &SatzState,
    target_id: &satz_core::DocId,
    target_doc: &Document,
    heading: &Heading,
    new_name: &str,
) -> Option<WorkspaceEdit> {
    let mut changes: HashMap<Uri, Vec<TextEdit>> = HashMap::new();

    // The definition: only the heading text changes.
    let text = heading_text_range(target_doc.line_index.source(), heading)?;
    if let Some(url) = path_to_uri(&absolute_path(state, target_doc)) {
        changes.entry(url).or_default().push(TextEdit {
            range: byte_range_to_lsp(text.range, &target_doc.line_index),
            new_text: if text.needs_space {
                format!(" {new_name}")
            } else {
                new_name.to_string()
            },
        });
    }

    // The links (scoped to backlinks + the defining document itself).
    let mut candidate_ids: std::collections::HashSet<&satz_core::DocId> =
        state.index.backlinks_of(target_id).collect();
    candidate_ids.insert(target_id);

    for src_id in candidate_ids {
        let Some(src_doc) = state.index.get_doc(src_id) else {
            continue;
        };
        let Some(src_url) = path_to_uri(&absolute_path(state, src_doc)) else {
            continue;
        };

        for l in &src_doc.links {
            let matches_doc = if l.target_doc.is_empty() {
                src_doc.id == *target_id
            } else {
                state.index.resolve_link(&l.target_doc) == Some(target_id)
            };
            let matches_heading = l
                .target_heading
                .as_deref()
                .is_some_and(|th| heading.matches(th));

            if matches_doc && matches_heading {
                let new_link_text =
                    format_wikilink_heading(l.kind, &l.target_doc, new_name, l.display.as_deref());
                changes.entry(src_url.clone()).or_default().push(TextEdit {
                    range: byte_range_to_lsp(l.range, &src_doc.line_index),
                    new_text: new_link_text,
                });
            }
        }
    }

    Some(WorkspaceEdit {
        changes: Some(changes),
        ..Default::default()
    })
}

/// A heading name is written into `[[note#name]]` links, so it may not contain what would end
/// or split such a link.
fn validate_heading_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("heading name must not be empty".to_string());
    }
    if name.chars().any(char::is_control) {
        return Err("heading name must not contain line breaks or control characters".to_string());
    }
    for forbidden in ["|", "[[", "]]", "#"] {
        if name.contains(forbidden) {
            return Err(format!("heading name must not contain '{forbidden}'"));
        }
    }
    if name.starts_with('^') {
        return Err("heading name must not start with '^'".to_string());
    }
    Ok(())
}

/// Checks a note name and returns it without a trailing `.md` (stripped once).
fn validate_note_name(name: &str) -> Result<String, String> {
    let clean = name.strip_suffix(".md").unwrap_or(name);
    if clean.trim().is_empty() {
        return Err("note name must not be empty".to_string());
    }
    if let Some(c) = clean.chars().find(|c| c.is_control()) {
        return Err(format!(
            "note name must not contain control characters (U+{:04X})",
            c as u32
        ));
    }
    if let Some(c) = clean.chars().find(|c| "/\\:*?\"<>|#[]^".contains(*c)) {
        return Err(format!("note name must not contain '{c}'"));
    }
    if clean.ends_with(['.', ' ']) {
        return Err("note name must not end with '.' or a space".to_string());
    }
    // `<name>.md` has to fit the common 255-byte file name limit.
    if clean.len() + ".md".len() > 255 {
        return Err("note name is too long for a file name".to_string());
    }
    Ok(clean.to_string())
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
        Position, Range, TextDocumentIdentifier, TextDocumentPositionParams,
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

        let edit = rename(params, &state)
            .unwrap()
            .expect("WorkspaceEdit expected");
        let changes = edit.changes.expect("Changes map expected");
        let uri_a = path_to_uri(abs_a).unwrap();
        let uri_b = path_to_uri(abs_b).unwrap();
        let edits_a = &changes[&uri_a];
        assert_eq!(edits_a[0].new_text, "New Heading");

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

        let edit = rename(params, &state)
            .unwrap()
            .expect("WorkspaceEdit expected");
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

    #[test]
    fn rename_heading_matches_slug_and_case_variants() {
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

        let doc_a = parse_document("## Günün Özeti\n\nNotlar burada.", rel_a);
        let doc_b = parse_document("Link: [[doc-a#günün özeti]]", rel_b);

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
                "## Günün Özeti\n\nNotlar burada.",
                1,
            ),
        );

        let params = RenameParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: uri_a_str.parse().unwrap(),
                },
                position: Position::new(0, 4), // on "## Günün Özeti"
            },
            new_name: "Haftanın Özeti".to_string(),
            work_done_progress_params: Default::default(),
        };

        let edit = rename(params, &state)
            .unwrap()
            .expect("WorkspaceEdit expected");
        let changes = edit.changes.expect("Changes map expected");
        let uri_b = path_to_uri(abs_b).unwrap();
        let edits_b = &changes[&uri_b];
        assert_eq!(edits_b[0].new_text, "[[doc-a#Haftanın Özeti]]");
    }

    #[test]
    fn test_prepare_rename_valid_and_invalid_positions() {
        let abs_a = if cfg!(windows) {
            Path::new("C:\\doc-a.md")
        } else {
            Path::new("/doc-a.md")
        };
        let rel_a = Path::new("doc-a.md");
        let doc_a = parse_document("## Başlık\n\nBoş metin satırı.", rel_a);

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
                "## Başlık\n\nBoş metin satırı.",
                1,
            ),
        );

        // 1. Valid position on heading
        let params_valid = TextDocumentPositionParams {
            text_document: TextDocumentIdentifier {
                uri: uri_a_str.parse().unwrap(),
            },
            position: Position::new(0, 4),
        };
        let prep = prepare_rename(params_valid, &state).expect("PrepareRename expected");
        if let PrepareRenameResponse::RangeWithPlaceholder { placeholder, .. } = prep {
            assert_eq!(placeholder, "Başlık");
        } else {
            panic!("Expected RangeWithPlaceholder");
        }

        // 2. Invalid position on empty text
        let params_invalid = TextDocumentPositionParams {
            text_document: TextDocumentIdentifier {
                uri: uri_a_str.parse().unwrap(),
            },
            position: Position::new(2, 4),
        };
        assert!(prepare_rename(params_invalid, &state).is_none());
    }

    // ---- applied-result harness ----

    fn root() -> std::path::PathBuf {
        if cfg!(windows) {
            Path::new("C:\\vault").to_path_buf()
        } else {
            Path::new("/vault").to_path_buf()
        }
    }

    fn uri_of(rel: &str) -> String {
        path_to_uri(&root().join(rel)).unwrap().as_str().to_string()
    }

    struct Vault {
        files: Vec<(&'static str, String)>,
        state: SatzState,
    }

    fn vault(files: &[(&'static str, &str)]) -> Vault {
        let mut state = SatzState::default();
        state.index = Index::build(
            files
                .iter()
                .map(|(rel, text)| parse_document(text, Path::new(rel)))
                .collect(),
        );
        state.vault_root = Some(root());
        for (rel, text) in files {
            let uri = uri_of(rel);
            state.open_docs.insert(
                uri.clone(),
                crate::state::OpenDocument::new(&uri, root().join(rel), *text, 1),
            );
        }
        Vault {
            files: files.iter().map(|(r, t)| (*r, t.to_string())).collect(),
            state,
        }
    }

    fn rename_params(rel: &str, line: u32, col: u32, new_name: &str) -> RenameParams {
        RenameParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: uri_of(rel).parse().unwrap(),
                },
                position: Position::new(line, col),
            },
            new_name: new_name.to_string(),
            work_done_progress_params: Default::default(),
        }
    }

    /// The result of applying a rename: the new text of every file, plus the file renames.
    struct Applied {
        texts: std::collections::BTreeMap<String, String>,
        renames: Vec<(
            String,
            String,
            Option<tower_lsp_server::ls_types::RenameFileOptions>,
        )>,
    }

    fn rel_of(uri: &Uri) -> String {
        crate::convert::uri_to_path(uri.as_str())
            .unwrap()
            .strip_prefix(root())
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/")
    }

    impl Vault {
        fn rename(
            &self,
            rel: &str,
            line: u32,
            col: u32,
            new_name: &str,
        ) -> Result<Applied, String> {
            let edit = rename(rename_params(rel, line, col, new_name), &self.state)?
                .ok_or_else(|| "no edit".to_string())?;
            let mut per_file: std::collections::BTreeMap<String, Vec<TextEdit>> =
                Default::default();
            let mut renames = Vec::new();
            for (uri, edits) in edit.changes.unwrap_or_default() {
                per_file.entry(rel_of(&uri)).or_default().extend(edits);
            }
            if let Some(DocumentChanges::Operations(ops)) = edit.document_changes {
                for op in ops {
                    match op {
                        DocumentChangeOperation::Op(ResourceOp::Rename(r)) => {
                            renames.push((rel_of(&r.old_uri), rel_of(&r.new_uri), r.options))
                        }
                        DocumentChangeOperation::Edit(e) => {
                            per_file
                                .entry(rel_of(&e.text_document.uri))
                                .or_default()
                                .extend(e.edits.into_iter().filter_map(|o| match o {
                                    OneOf::Left(t) => Some(t),
                                    OneOf::Right(_) => None,
                                }));
                        }
                        _ => {}
                    }
                }
            }
            let mut texts = std::collections::BTreeMap::new();
            for (rel, text) in &self.files {
                let edits = per_file.remove(*rel).unwrap_or_default();
                texts.insert(
                    rel.to_string(),
                    crate::convert::apply_text_edits(text, &edits),
                );
            }
            assert!(per_file.is_empty(), "edits for unknown files: {per_file:?}");
            Ok(Applied { texts, renames })
        }

        fn text_after(&self, rel: &str, line: u32, col: u32, new_name: &str) -> String {
            let applied = self
                .rename(rel, line, col, new_name)
                .unwrap_or_else(|e| panic!("rename rejected: {e}"));
            applied.texts[rel].clone()
        }
    }

    // ---- F1: heading rename touches the heading text only ----

    #[test]
    fn heading_rename_replaces_only_the_heading_text() {
        for (before, after) in [
            ("## Old\nfoo", "## New\nfoo"),
            ("# Old\n\nText\n", "# New\n\nText\n"),
            ("## Old", "## New"),
            ("## Old ^blk\nfoo", "## New ^blk\nfoo"),
            ("## Old ##\nfoo", "## New ##\nfoo"),
            ("## Old ## \nfoo", "## New ## \nfoo"),
            ("###   Old   \nfoo", "###   New   \nfoo"),
            ("   ## Old\nfoo", "   ## New\nfoo"),
            ("## Old\r\nfoo\r\n", "## New\r\nfoo\r\n"),
            ("## `Old` and [[x]]\nfoo", "## New\nfoo"),
            ("## Günün Özeti\nfoo", "## New\nfoo"),
            ("Old\n===\nfoo", "New\n===\nfoo"),
            ("Old\n---\nfoo", "New\n---\nfoo"),
            ("Old\r\n===\r\nfoo", "New\r\n===\r\nfoo"),
        ] {
            let v = vault(&[("a.md", before)]);
            let applied = v
                .rename("a.md", 0, 3, "New")
                .unwrap_or_else(|e| panic!("input {before:?}: {e}"));
            assert_eq!(applied.texts["a.md"], after, "input {before:?}");
        }
    }

    #[test]
    fn heading_rename_works_from_any_column_of_the_heading_line() {
        let src = "## Old title\nfoo";
        let v = vault(&[("a.md", src)]);
        for col in [0, 1, 2, 3, 8, 12] {
            assert_eq!(
                v.text_after("a.md", 0, col, "New"),
                "## New\nfoo",
                "col {col}"
            );
        }
    }

    #[test]
    fn heading_rename_only_changes_the_clicked_heading() {
        let src = "# One\n\n## Two\ntext\n\n## Three\n";
        let v = vault(&[("a.md", src)]);
        assert_eq!(
            v.text_after("a.md", 2, 4, "Deux"),
            "# One\n\n## Deux\ntext\n\n## Three\n"
        );
        assert_eq!(
            v.text_after("a.md", 5, 4, "Trois"),
            "# One\n\n## Two\ntext\n\n## Trois\n"
        );
    }

    #[test]
    fn heading_rename_rewrites_links_in_other_notes_and_keeps_everything_else() {
        let b =
            "See [[a#Old]] and [[a#old|shown]] and ![[a#Old]] end\n\nOther [[a]] and [[c#Old]]\n";
        let v = vault(&[("a.md", "## Old\nfoo\n"), ("b.md", b), ("c.md", "## Old\n")]);
        let applied = v.rename("a.md", 0, 4, "New").unwrap();
        assert_eq!(applied.texts["a.md"], "## New\nfoo\n");
        assert_eq!(
            applied.texts["b.md"],
            "See [[a#New]] and [[a#New|shown]] and ![[a#New]] end\n\nOther [[a]] and [[c#Old]]\n"
        );
        assert_eq!(applied.texts["c.md"], "## Old\n");
        assert!(applied.renames.is_empty());
    }

    #[test]
    fn heading_rename_from_a_link_edits_the_target_heading() {
        let v = vault(&[("a.md", "## Old\nfoo\n"), ("b.md", "See [[a#Old]] here\n")]);
        let applied = v.rename("b.md", 0, 8, "New").unwrap();
        assert_eq!(applied.texts["a.md"], "## New\nfoo\n");
        assert_eq!(applied.texts["b.md"], "See [[a#New]] here\n");
    }

    #[test]
    fn a_link_to_a_heading_that_has_a_block_id_resolves_and_renames_it() {
        let v = vault(&[
            ("a.md", "## Old ^blk\nfoo\n"),
            ("b.md", "See [[a#Old]] and [[a#^blk]]\n"),
        ]);
        let applied = v.rename("b.md", 0, 8, "New").unwrap();
        assert_eq!(applied.texts["a.md"], "## New ^blk\nfoo\n");
        assert_eq!(applied.texts["b.md"], "See [[a#New]] and [[a#^blk]]\n");
    }

    #[test]
    fn heading_rename_in_the_same_note_updates_its_own_links() {
        let v = vault(&[("a.md", "## Old\nsee [[#Old]] and [[a#Old]]\n")]);
        assert_eq!(
            v.text_after("a.md", 0, 4, "New"),
            "## New\nsee [[#New]] and [[a#New]]\n"
        );
    }

    #[test]
    fn empty_heading_can_be_named() {
        let v = vault(&[("a.md", "##\nfoo\n")]);
        assert_eq!(v.text_after("a.md", 0, 1, "New"), "## New\nfoo\n");
    }

    #[test]
    fn prepare_rename_covers_exactly_the_heading_text() {
        let src = "## Old ^blk\nfoo\n";
        let v = vault(&[("a.md", src)]);
        let params = TextDocumentPositionParams {
            text_document: TextDocumentIdentifier {
                uri: uri_of("a.md").parse().unwrap(),
            },
            position: Position::new(0, 1),
        };
        let Some(PrepareRenameResponse::RangeWithPlaceholder { range, placeholder }) =
            prepare_rename(params, &v.state)
        else {
            panic!("expected a range");
        };
        assert_eq!(placeholder, "Old");
        assert_eq!(range, Range::new(Position::new(0, 3), Position::new(0, 6)));
    }

    #[test]
    fn renaming_a_link_to_a_missing_heading_is_an_error_not_a_document_rename() {
        let v = vault(&[("a.md", "## Real\n"), ("b.md", "See [[a#Missing]] here\n")]);
        let err = v
            .rename("b.md", 0, 8, "New")
            .err()
            .expect("must be rejected");
        assert!(err.contains("Missing"), "{err}");
    }

    #[test]
    fn renaming_a_link_to_a_missing_note_is_an_error() {
        let v = vault(&[("b.md", "See [[nowhere#H]] and [[gone]]\n")]);
        assert!(v.rename("b.md", 0, 8, "New").is_err());
        assert!(v.rename("b.md", 0, 25, "New").is_err());
    }

    // ---- F2: names are validated ----

    #[test]
    fn invalid_heading_names_are_rejected_with_a_reason() {
        let v = vault(&[("a.md", "## Old\n"), ("b.md", "[[a#Old]]\n")]);
        for bad in [
            "", "   ", "a\nb", "a\rb", "a|b", "a[[b", "a]]b", "a#b", "^block", "a\u{0}b",
        ] {
            for (rel, col) in [("a.md", 4), ("b.md", 4)] {
                let err = v
                    .rename(rel, 0, col, bad)
                    .err()
                    .unwrap_or_else(|| panic!("{bad:?} must be rejected for {rel}"));
                assert!(!err.is_empty());
            }
        }
    }

    #[test]
    fn heading_names_with_ordinary_punctuation_are_accepted() {
        let v = vault(&[("a.md", "## Old\n")]);
        for good in [
            "Yeni Başlık",
            "a.b",
            "Q: what?",
            "100% sure",
            "a^b",
            "日本語",
            "x (y) - z",
        ] {
            assert_eq!(v.text_after("a.md", 0, 4, good), format!("## {good}\n"));
        }
        // Surrounding whitespace is not part of the name.
        assert_eq!(v.text_after("a.md", 0, 4, "  Padded  "), "## Padded\n");
    }

    #[test]
    fn invalid_note_names_are_rejected_naming_the_character() {
        let v = vault(&[("a.md", "# A\n"), ("b.md", "Link [[a]]\n")]);
        for bad in [
            "x/y", "x\\y", "x:y", "x*y", "x?y", "x\"y", "x<y", "x>y", "x|y", "x#y", "x[y", "x]y",
            "x^y",
        ] {
            let err = v
                .rename("b.md", 0, 7, bad)
                .err()
                .unwrap_or_else(|| panic!("{bad:?}"));
            let offending = bad.chars().nth(1).unwrap();
            assert!(err.contains(offending), "{bad:?}: {err}");
        }
        for bad in ["", "  ", "a\nb", "a\u{7}b", "trailing.", ".md", "a\u{0}"] {
            assert!(v.rename("b.md", 0, 7, bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn note_rename_length_limit_is_the_file_name_limit() {
        let v = vault(&[("a.md", "# A\n"), ("b.md", "Link [[a]]\n")]);
        assert!(v.rename("b.md", 0, 7, &"a".repeat(252)).is_ok());
        assert!(v.rename("b.md", 0, 7, &"a".repeat(253)).is_err());
    }

    #[test]
    fn note_rename_updates_links_and_renames_the_file_without_overwriting() {
        let v = vault(&[
            ("a.md", "# A\n"),
            (
                "b.md",
                "Link [[a]] and [[a#H|shown]] and ![[a]] and [[a#^blk]]\n",
            ),
        ]);
        let applied = v.rename("b.md", 0, 7, "renamed").unwrap();
        assert_eq!(
            applied.texts["b.md"],
            "Link [[renamed]] and [[renamed#H|shown]] and ![[renamed]] and [[renamed#^blk]]\n"
        );
        assert_eq!(applied.renames.len(), 1);
        let (old, new, options) = &applied.renames[0];
        assert_eq!((old.as_str(), new.as_str()), ("a.md", "renamed.md"));
        let options = options.as_ref().expect("explicit RenameFile options");
        assert_eq!(options.overwrite, Some(false));
        assert_eq!(options.ignore_if_exists, None);
    }

    #[test]
    fn note_rename_strips_the_md_extension_exactly_once() {
        let v = vault(&[("a.md", "# A\n"), ("b.md", "Link [[a]]\n")]);
        let applied = v.rename("b.md", 0, 7, "new.md").unwrap();
        assert_eq!(applied.texts["b.md"], "Link [[new]]\n");
        assert_eq!(applied.renames[0].1, "new.md");
        let applied = v.rename("b.md", 0, 7, "new.md.md").unwrap();
        assert_eq!(applied.renames[0].1, "new.md.md");
    }

    #[test]
    fn note_rename_keeps_the_note_in_its_folder() {
        let v = vault(&[("sub/a.md", "# A\n"), ("b.md", "Link [[a]]\n")]);
        let applied = v.rename("b.md", 0, 7, "renamed").unwrap();
        assert_eq!(applied.renames[0].0, "sub/a.md");
        assert_eq!(applied.renames[0].1, "sub/renamed.md");
    }

    #[test]
    fn note_rename_refuses_a_name_that_is_already_taken() {
        let v = vault(&[
            ("a.md", "# A\n"),
            ("taken.md", "# T\n"),
            ("sub/other.md", "# O\n"),
            ("b.md", "Link [[a]] [[other]]\n"),
        ]);
        let err = v.rename("b.md", 0, 7, "taken").err().unwrap();
        assert!(err.contains("already exists"), "{err}");
        let err = v.rename("b.md", 0, 7, "taken.md").err().unwrap();
        assert!(err.contains("already exists"), "{err}");
        // Same folder collision only; the same name in another folder is fine.
        assert!(v.rename("b.md", 0, 7, "other").is_ok());
        // Renaming a note to itself (or only its case) is not a collision.
        assert!(v.rename("b.md", 0, 7, "a").is_ok());
        assert!(v.rename("b.md", 0, 7, "A").is_ok());
    }

    #[test]
    fn a_position_that_is_not_renameable_is_no_edit_and_no_error() {
        let v = vault(&[("a.md", "plain text\n\n## H\n")]);
        let out = rename(rename_params("a.md", 0, 3, "New"), &v.state);
        assert_eq!(out, Ok(None));
    }
}
