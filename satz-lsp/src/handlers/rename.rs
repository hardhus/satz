use std::collections::HashMap;
use tower_lsp_server::ls_types::{
    DocumentChangeOperation, DocumentChanges, OneOf, OptionalVersionedTextDocumentIdentifier,
    PrepareRenameResponse, RenameFile, RenameFileOptions, RenameParams, ResourceOp,
    TextDocumentEdit, TextDocumentPositionParams, TextEdit, Uri, WorkspaceEdit,
};

use crate::convert::{byte_range_to_lsp, path_to_uri};
use crate::state::{SatzState, SelfLinks};
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

    let (_, doc) = state.doc_for_uri(uri)?;

    let byte_offset = crate::convert::lsp_pos_to_byte(&doc.line_index, pos);

    // 1. Heading definition
    if let Some(h) = doc.headings.iter().find(|h| h.range.contains(byte_offset)) {
        let text = heading_text_range(doc.line_index.source(), h)?;
        return Some(PrepareRenameResponse::RangeWithPlaceholder {
            range: byte_range_to_lsp(text.range, &doc.line_index),
            placeholder: doc.line_index.source()[text.range.start..text.range.end].to_string(),
        });
    }

    // 2. Link
    if let Some(link) = doc.link_at(byte_offset) {
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

    let Some((_, doc)) = state.doc_for_uri(uri) else {
        return Ok(None);
    };

    let byte_offset = crate::convert::lsp_pos_to_byte(&doc.line_index, pos);

    // 1. Cursor on a heading definition
    if let Some(h) = doc.headings.iter().find(|h| h.range.contains(byte_offset)) {
        validate_heading_name(new_name)?;
        return Ok(rename_heading(state, &doc.id, doc, h, new_name));
    }

    // 2. Cursor on a link
    let Some(link) = doc.link_at(byte_offset) else {
        return Ok(None);
    };

    // A) A link with a heading renames that heading, wherever it is defined.
    if let Some(target_heading) = &link.target_heading {
        let target_id = state
            .link_target_doc(doc, link)
            .ok_or_else(|| format!("cannot rename: note '{}' does not exist", link.target_doc))?;
        let target_doc = state
            .index
            .get_doc(target_id)
            .ok_or_else(|| format!("cannot rename: note '{}' does not exist", link.target_doc))?;
        let h = target_doc
            .resolve_heading(target_heading)
            .map(|i| &target_doc.headings[i])
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
        .link_target_doc(doc, link)
        .ok_or_else(|| format!("cannot rename: note '{}' does not exist", link.target_doc))?;
    let target_doc = state
        .index
        .get_doc(target_id)
        .ok_or_else(|| format!("cannot rename: note '{}' does not exist", link.target_doc))?;
    let clean_new_doc_name = validate_note_name(new_name)?;

    let old_doc_path = state.doc_path(target_doc);
    let new_doc_path = old_doc_path.with_file_name(format!("{clean_new_doc_name}.md"));
    let new_rel = match state.vault_root() {
        Some(root) => new_doc_path.strip_prefix(root).unwrap_or(&new_doc_path),
        None => &new_doc_path,
    };
    let new_id = satz_core::DocId::from_path(new_rel);
    if new_id != *target_id && state.index.get_doc(&new_id).is_some() {
        return Err(format!(
            "cannot rename: a note named '{clean_new_doc_name}' already exists there"
        ));
    }

    let mut changes: HashMap<Uri, Vec<TextEdit>> = HashMap::new();

    // Scoped to backlinks + target
    for src_doc in state.documents_linking_to(target_id, SelfLinks::Include) {
        let Some(src_url) = state.doc_uri(src_doc) else {
            continue;
        };

        for l in state.links_to(src_doc, target_id) {
            // Only links that name the FILE stop working; one that reaches the note through its
            // title or an alias keeps resolving and is not touched.
            if !l.target_doc.is_empty()
                && names_file(&l.target_doc, &target_doc.path)
                && let Some(new_link_text) = rewritten_link(
                    src_doc.line_index.source(),
                    l,
                    LinkChange::Note(&clean_new_doc_name),
                )
            {
                changes.entry(src_url.clone()).or_default().push(TextEdit {
                    range: byte_range_to_lsp(l.range, &src_doc.line_index),
                    new_text: new_link_text,
                });
            }
        }
    }

    // The file rename. Without file locations the links could be rewritten but the file could not
    // be renamed, leaving every rewritten link broken: refuse instead.
    let (Some(old_uri), Some(new_uri)) = (path_to_uri(&old_doc_path), path_to_uri(&new_doc_path))
    else {
        return Err(format!(
            "cannot rename: no file location for '{}'",
            target_doc.path.display()
        ));
    };

    // Text edits first (stable order): they address the files by their current URIs, which stop
    // existing once the rename has been applied.
    let mut document_changes = versioned_edits(state, ordered_edits(changes));
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

    Ok(Some(WorkspaceEdit {
        document_changes: Some(DocumentChanges::Operations(document_changes)),
        ..Default::default()
    }))
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
    if let Some(url) = state.doc_uri(target_doc) {
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
    for src_doc in state.documents_linking_to(target_id, SelfLinks::Include) {
        let Some(src_url) = state.doc_uri(src_doc) else {
            continue;
        };

        for l in state.links_to(src_doc, target_id) {
            // A reference belongs to the FIRST heading that matches it; a later duplicate owns none.
            let matches_heading = l.target_heading.as_deref().is_some_and(|th| {
                target_doc
                    .resolve_heading(th)
                    .is_some_and(|i| target_doc.headings[i].range == heading.range)
            });

            if matches_heading
                && let Some(new_link_text) = rewritten_link(
                    src_doc.line_index.source(),
                    l,
                    LinkChange::Heading(new_name),
                )
            {
                changes.entry(src_url.clone()).or_default().push(TextEdit {
                    range: byte_range_to_lsp(l.range, &src_doc.line_index),
                    new_text: new_link_text,
                });
            }
        }
    }

    Some(WorkspaceEdit {
        document_changes: Some(DocumentChanges::Operations(versioned_edits(
            state,
            ordered_edits(changes),
        ))),
        ..Default::default()
    })
}

/// The edits as document edits. A file that is open names the buffer version the edits were
/// computed for (the index was refreshed against that buffer before the request ran), so the client
/// refuses them if the user has typed since; a file on disk carries no version.
fn versioned_edits(
    state: &SatzState,
    files: Vec<(Uri, Vec<TextEdit>)>,
) -> Vec<DocumentChangeOperation> {
    files
        .into_iter()
        .map(|(url, edits)| {
            let version = crate::convert::uri_to_path(url.as_str())
                .and_then(|path| state.open_doc_for_path(&path).map(|(_, open)| open.version));
            DocumentChangeOperation::Edit(TextDocumentEdit {
                text_document: OptionalVersionedTextDocumentIdentifier { uri: url, version },
                edits: edits.into_iter().map(OneOf::Left).collect(),
            })
        })
        .collect()
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

/// What a link rewrite is for: a renamed heading (its new text) or a renamed note (its new stem).
#[derive(Clone, Copy)]
enum LinkChange<'a> {
    Heading(&'a str),
    Note(&'a str),
}

/// The new source text of `link`, keeping its syntax (wikilink, embed or Markdown link). `None`
/// when a Markdown link's destination cannot be parsed; such a link is left alone.
fn rewritten_link(source: &str, link: &satz_core::Link, change: LinkChange) -> Option<String> {
    match (link.kind, change) {
        (LinkKind::Markdown, _) => {
            rewrite_markdown_link(source.get(link.range.start..link.range.end)?, change)
        }
        (_, LinkChange::Heading(new_heading)) => Some(format_wikilink_heading(
            link.kind,
            &link.target_doc,
            new_heading,
            link.display.as_deref(),
        )),
        (_, LinkChange::Note(new_stem)) => Some(format_wikilink_doc(
            link.kind,
            &replace_last_segment(&link.target_doc, new_stem),
            link.target_heading.as_deref(),
            link.target_block.as_deref(),
            link.display.as_deref(),
        )),
    }
}

/// `sub/a.md` + `c` -> `sub/c.md`: only the last path component changes; folders and a `.md`
/// extension are kept as the user wrote them.
fn replace_last_segment(target: &str, new_stem: &str) -> String {
    let (dir, file) = match target.rfind(['/', '\\']) {
        Some(i) => (&target[..=i], &target[i + 1..]),
        None => ("", target),
    };
    // The last three BYTES are `.md` only if they start a letter: a name such as `Çalışma` or `İş`
    // has a letter of two bytes there, and cutting it in the middle would panic.
    let ext = match file.len().checked_sub(3).and_then(|cut| file.get(cut..)) {
        Some(tail) if tail.eq_ignore_ascii_case(".md") => tail,
        _ => "",
    };
    format!("{dir}{new_stem}{ext}")
}

/// Rewrites the destination of `[text](dest "title")`, keeping text, title and bracket style.
fn rewrite_markdown_link(link_source: &str, change: LinkChange) -> Option<String> {
    let open = link_source.rfind("](")?;
    if !link_source.ends_with(')') {
        return None;
    }
    let head = &link_source[..open + 2];
    let inner = &link_source[open + 2..link_source.len() - 1];

    let (dest, rest, angled) = if let Some(after) = inner.strip_prefix('<') {
        let end = after.find('>')?;
        (&after[..end], &after[end + 1..], true)
    } else {
        let end = inner.find(char::is_whitespace).unwrap_or(inner.len());
        (&inner[..end], &inner[end..], false)
    };
    let (path, fragment) = match dest.split_once('#') {
        Some((path, fragment)) => (path, Some(fragment)),
        None => (dest, None),
    };
    // A bare destination cannot hold spaces; `<...>` can.
    let encode = |text: &str| {
        if angled {
            text.to_string()
        } else {
            text.replace(' ', "%20")
        }
    };
    let (new_path, new_fragment) = match change {
        LinkChange::Note(stem) => (
            replace_last_segment(path, &encode(stem)),
            fragment.map(str::to_string),
        ),
        LinkChange::Heading(heading) => (path.to_string(), Some(encode(heading))),
    };
    let mut new_dest = new_path;
    if let Some(fragment) = new_fragment {
        new_dest.push('#');
        new_dest.push_str(&fragment);
    }
    let new_dest = if angled {
        format!("<{new_dest}>")
    } else {
        new_dest
    };
    Some(format!("{head}{new_dest}{rest})"))
}

/// Whether a link target names the FILE (its path or its name), as opposed to reaching the note
/// through its title or an alias. Only the former stops working when the file is renamed.
fn names_file(target: &str, old_rel_path: &std::path::Path) -> bool {
    let normalize = |text: &str| {
        let mut t = text.trim().replace('\\', "/").replace("%20", " ");
        while let Some(rest) = t.strip_prefix("./").or_else(|| t.strip_prefix("../")) {
            t = rest.to_string();
        }
        let t = match t.len().checked_sub(3) {
            Some(cut) if t.is_char_boundary(cut) && t[cut..].eq_ignore_ascii_case(".md") => {
                t[..cut].to_string()
            }
            _ => t,
        };
        satz_core::fold_key(&t)
    };
    let wanted = normalize(target);
    let full = normalize(&old_rel_path.to_string_lossy());
    let stem = old_rel_path
        .file_stem()
        .map(|s| satz_core::fold_key(&s.to_string_lossy()))
        .unwrap_or_default();
    // A wrong or shorter folder part still reaches the note by its file name (`[[wrong/a]]` finds
    // `sub/a.md` the way `[[a]]` does), and such a link stops working when the file is renamed.
    let last_component = wanted.rsplit('/').next().unwrap_or(&wanted);
    wanted == full
        || wanted == stem
        || full.ends_with(&format!("/{wanted}"))
        || (!stem.is_empty() && last_component == stem)
}

/// Per-file edits in a stable order: files by URI, edits by position.
fn ordered_edits(changes: HashMap<Uri, Vec<TextEdit>>) -> Vec<(Uri, Vec<TextEdit>)> {
    let mut files: Vec<(Uri, Vec<TextEdit>)> = changes.into_iter().collect();
    files.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()));
    for (_, edits) in &mut files {
        edits.sort_by_key(|e| (e.range.start.line, e.range.start.character));
    }
    files
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
mod tests;
