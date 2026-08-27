#![allow(clippy::collapsible_if)]

use crate::convert::lsp_pos_to_satz;
use crate::state::SatzState;
use satz_core::LinkKind;
use tower_lsp_server::ls_types::{Hover, HoverContents, HoverParams, MarkupContent, MarkupKind};

pub fn hover(params: HoverParams, state: &SatzState) -> Option<Hover> {
    let uri = params
        .text_document_position_params
        .text_document
        .uri
        .as_str();
    let pos = params.text_document_position_params.position;

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

    let link = doc.links.iter().find(|l| l.range.contains(byte_offset))?;

    if link.kind == LinkKind::Footnote {
        if let Some(label) = &link.display {
            if let Some(def) = doc.footnotes.definitions.iter().find(|d| d.label == *label) {
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
        }
        return None;
    }

    let target_id = if link.target_doc.is_empty() {
        &doc_id
    } else {
        state.index.resolve_link(&link.target_doc)?
    };

    let target_doc = state.index.get_doc(target_id)?;
    let mut value = format!("# {}\n\n", target_doc.title);

    // If there's a heading target, start preview from that heading
    let mut preview_start = 0;
    if let Some(heading_slug) = &link.target_heading {
        if let Some(h) = target_doc.headings.iter().find(|h| h.matches(heading_slug)) {
            preview_start = h.range.start;
            value.push_str(&format!("*Jump to: {}*\n\n", h.text.trim()));
        }
    }

    let source = target_doc.line_index.source();
    let text_after = if preview_start < source.len() {
        &source[preview_start..]
    } else {
        ""
    };

    // Take first 5 non-empty lines
    let preview_lines: Vec<&str> = text_after
        .lines()
        .filter(|l| !l.trim().is_empty())
        .take(5)
        .collect();

    value.push_str("```markdown\n");
    value.push_str(&preview_lines.join("\n"));
    if text_after.lines().count() > 5 {
        value.push_str("\n...");
    }
    value.push_str("\n```");

    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value,
        }),
        range: None,
    })
}

#[cfg(test)]
#[allow(unused_variables)]
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use super::*;
    use satz_core::{Index, parse_document};
    use std::path::Path;
    use tower_lsp_server::ls_types::{TextDocumentIdentifier, TextDocumentPositionParams};

    #[test]
    fn test_hover_on_wikilink() {
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
        let doc_a = parse_document("# Doc A\n\n[[doc-b]]", rel_a);
        let doc_b = parse_document("---\ntitle: Target Doc\n---\n# H1\nContent line 1", rel_b);

        let mut state = SatzState::default();
        state.index = Index::build(vec![doc_a.clone(), doc_b]);
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
                "# Doc A\n\n[[doc-b]]",
                1,
            ),
        );

        let params = HoverParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: uri_a_str.parse().unwrap(),
                },
                position: tower_lsp_server::ls_types::Position::new(2, 3), // inside [[doc-b]]
            },
            work_done_progress_params: Default::default(),
        };

        let hover = hover(params, &state).expect("Hover should return Some");
        if let HoverContents::Markup(m) = hover.contents {
            assert!(m.value.contains("Target Doc"));
            assert!(m.value.contains("Content line 1"));
        } else {
            panic!("Expected markup content");
        }
    }
}
