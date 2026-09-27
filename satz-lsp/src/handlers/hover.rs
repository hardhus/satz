use crate::state::SatzState;
use satz_core::model::document::Document;
use satz_core::model::link::{Link, LinkKind};
use tower_lsp_server::ls_types::{Hover, HoverContents, HoverParams, MarkupContent, MarkupKind};

pub fn hover(params: HoverParams, state: &SatzState) -> Option<Hover> {
    let uri = params
        .text_document_position_params
        .text_document
        .uri
        .as_str();
    let pos = params.text_document_position_params.position;
    tracing::debug!(uri, ?pos, "hover");

    let (_, doc) = state.doc_for_uri(uri)?;

    let byte_offset = crate::convert::lsp_pos_to_byte(&doc.line_index, pos);

    let link = doc.link_at(byte_offset)?;

    if link.kind == LinkKind::Footnote {
        if let Some(label) = &link.display
            && let Some(def) = doc.footnotes.find_def(label)
        {
            let source = doc.line_index.source();
            let def_text = &source[def.range.start..def.range.end];
            let value = format!("```markdown\n{}\n```", def_text.trim());
            return Some(Hover {
                contents: HoverContents::Markup(MarkupContent {
                    kind: MarkupKind::Markdown,
                    value,
                }),
                range: None,
            });
        }
        return None;
    }

    match state
        .index
        .resolve_link_full_with_config(link, Some(doc), Some(&state.config))
    {
        satz_core::LinkResolution::Resolved {
            doc: target_doc, ..
        } => {
            let value =
                format_hover_content(target_doc, link, None, state.config.hover.preview_lines);
            Some(Hover {
                contents: HoverContents::Markup(MarkupContent {
                    kind: MarkupKind::Markdown,
                    value,
                }),
                range: None,
            })
        }
        satz_core::LinkResolution::AnchorMissing { doc: target_doc } => {
            let missing_anchor = link
                .target_heading
                .as_deref()
                .or(link.target_block.as_deref())
                .unwrap_or("");
            let value = format_hover_content(
                target_doc,
                link,
                Some(missing_anchor),
                state.config.hover.preview_lines,
            );
            Some(Hover {
                contents: HoverContents::Markup(MarkupContent {
                    kind: MarkupKind::Markdown,
                    value,
                }),
                range: None,
            })
        }
        satz_core::LinkResolution::DocMissing => None,
    }
}

fn format_hover_content(
    target_doc: &Document,
    link: &Link,
    missing_anchor: Option<&str>,
    preview_lines_limit: usize,
) -> String {
    let mut value = format!("# {}\n\n", target_doc.title);

    if let Some(missing) = missing_anchor {
        value.push_str(&format!("⚠ '{}' not found\n\n", missing));
    }

    let source = target_doc.line_index.source();

    let slice = if missing_anchor.is_none() && link.target_heading.is_some() {
        // Section preview: from matching heading to next heading of same or higher level
        let heading_name = link.target_heading.as_deref().unwrap();
        if let Some(h) = target_doc
            .resolve_heading(heading_name)
            .map(|i| &target_doc.headings[i])
        {
            let next_heading = target_doc
                .headings
                .iter()
                .find(|other| other.range.start > h.range.start && other.level <= h.level);
            let end_byte = next_heading
                .map(|other| other.range.start)
                .unwrap_or(source.len());
            &source[h.range.start..end_byte]
        } else {
            get_default_preview(target_doc, source)
        }
    } else if missing_anchor.is_none() && link.target_block.is_some() {
        // Block preview: show the paragraph containing the block
        let block_id = link.target_block.as_deref().unwrap();
        if let Some(b) = target_doc
            .resolve_block(block_id)
            .map(|i| &target_doc.blocks[i])
        {
            let (p_start, p_end) = paragraph_bounds(source, b.range.start, b.range.end);
            &source[p_start..p_end]
        } else {
            get_default_preview(target_doc, source)
        }
    } else {
        get_default_preview(target_doc, source)
    };

    let trimmed = slice.trim();
    if !trimmed.is_empty() {
        let all_lines: Vec<&str> = trimmed.lines().collect();
        let shown = &all_lines[..all_lines.len().min(preview_lines_limit)];
        let preview = shown.join("\n");
        // The preview is Markdown that may itself contain code fences: fence it with a longer one
        // than any backtick run inside, or the first ``` in the text would close it early.
        let fence = "`".repeat((longest_backtick_run(&preview) + 1).max(3));
        value.push_str(&format!("{fence}markdown\n{preview}\n{fence}"));
        if all_lines.len() > preview_lines_limit {
            value.push_str(&format!(
                "\n… ({} more lines)",
                all_lines.len() - preview_lines_limit
            ));
        }
    }

    value
}

/// Length of the longest run of consecutive backticks in `text`.
fn longest_backtick_run(text: &str) -> usize {
    let mut longest = 0;
    let mut current = 0;
    for c in text.chars() {
        if c == '`' {
            current += 1;
            longest = longest.max(current);
        } else {
            current = 0;
        }
    }
    longest
}

/// The paragraph around `start..end`: the run of non-blank lines containing it. A blank line is
/// one that is empty or only whitespace, whichever way lines end (LF or CRLF).
fn paragraph_bounds(source: &str, start: usize, end: usize) -> (usize, usize) {
    let is_blank = |line: &str| line.trim().is_empty();
    let line_start = |pos: usize| source[..pos].rfind('\n').map_or(0, |i| i + 1);

    let mut first = line_start(start);
    while first > 0 {
        let prev = line_start(first - 1);
        if is_blank(&source[prev..first]) {
            break;
        }
        first = prev;
    }

    let mut last = source[end..].find('\n').map_or(source.len(), |i| end + i);
    while last < source.len() {
        let next_start = last + 1;
        let next_end = source[next_start..]
            .find('\n')
            .map_or(source.len(), |i| next_start + i);
        if next_start >= source.len() || is_blank(&source[next_start..next_end]) {
            break;
        }
        last = next_end;
    }
    (first, last)
}

fn get_default_preview<'a>(target_doc: &Document, source: &'a str) -> &'a str {
    let mut start = target_doc.frontmatter_range.map(|r| r.end).unwrap_or(0);

    // Skip leading whitespace / newlines after frontmatter
    while start < source.len()
        && (source.as_bytes()[start] == b'\n'
            || source.as_bytes()[start] == b'\r'
            || source.as_bytes()[start] == b' ')
    {
        start += 1;
    }

    // Skip first H1 if present right after frontmatter
    if let Some(h1) = target_doc
        .headings
        .iter()
        .find(|h| h.level == 1 && h.range.start >= start)
        && source[start..h1.range.start].trim().is_empty()
    {
        start = h1.range.end;
    }

    if start < source.len() {
        &source[start..]
    } else {
        ""
    }
}

#[cfg(test)]
mod tests;
