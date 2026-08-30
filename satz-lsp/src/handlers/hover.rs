#![allow(clippy::collapsible_if)]

use crate::convert::lsp_pos_to_satz;
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

    let open_doc = state.open_docs.get(uri)?;
    let rel_path =
        crate::state::SatzState::get_rel_path(&open_doc.path, state.vault_root.as_deref());
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
        value.push_str(&format!("⚠ '{}' bulunamadı\n\n", missing));
    }

    let source = target_doc.line_index.source();

    let slice = if missing_anchor.is_none() && link.target_heading.is_some() {
        // Section preview: from matching heading to next heading of same or higher level
        let heading_name = link.target_heading.as_deref().unwrap();
        if let Some(h) = target_doc.headings.iter().find(|h| h.matches(heading_name)) {
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
        if let Some(b) = target_doc.blocks.iter().find(|b| b.id == block_id) {
            let p_start = source[..b.range.start]
                .rfind("\n\n")
                .map(|i| i + 2)
                .unwrap_or(0);
            let p_end = source[b.range.end..]
                .find("\n\n")
                .map(|i| b.range.end + i)
                .unwrap_or(source.len());
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
        if all_lines.len() <= preview_lines_limit {
            value.push_str("```markdown\n");
            value.push_str(&all_lines.join("\n"));
            value.push_str("\n```");
        } else {
            let preview = all_lines[..preview_lines_limit].join("\n");
            let remaining = all_lines.len() - preview_lines_limit;
            value.push_str("```markdown\n");
            value.push_str(&preview);
            value.push_str("\n```\n");
            value.push_str(&format!("… ({} satır daha)", remaining));
        }
    }

    value
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
    {
        if source[start..h1.range.start].trim().is_empty() {
            start = h1.range.end;
        }
    }

    if start < source.len() {
        &source[start..]
    } else {
        ""
    }
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
    fn test_hover_skips_frontmatter_and_first_h1() {
        let rel_a = Path::new("doc-a.md");
        let rel_b = Path::new("doc-b.md");
        let doc_a = parse_document("# Doc A\n\n[[doc-b]]", rel_a);
        let doc_b = parse_document(
            "---\ntitle: Target Doc\ntags: [rust]\n---\n\n# Target Doc\n\nContent line 1\nContent line 2",
            rel_b,
        );

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
            assert!(m.value.starts_with("# Target Doc\n\n"));
            assert!(!m.value.contains("tags: [rust]"));
            assert!(!m.value.contains("---"));
            assert!(m.value.contains("Content line 1\nContent line 2"));
        } else {
            panic!("Expected markup content");
        }
    }

    #[test]
    fn test_hover_heading_section() {
        let rel_a = Path::new("doc-a.md");
        let rel_b = Path::new("doc-b.md");
        let doc_a = parse_document("# Doc A\n\n[[doc-b#Bölüm 1]]", rel_a);
        let doc_b = parse_document(
            "# Doc B\n\n## Bölüm 1\nBölüm 1 detayı burada.\n\n## Bölüm 2\nBölüm 2 detayı burada.",
            rel_b,
        );

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
                "# Doc A\n\n[[doc-b#Bölüm 1]]",
                1,
            ),
        );

        let params = HoverParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: uri_a_str.parse().unwrap(),
                },
                position: tower_lsp_server::ls_types::Position::new(2, 5),
            },
            work_done_progress_params: Default::default(),
        };

        let hover = hover(params, &state).expect("Hover should return Some");
        if let HoverContents::Markup(m) = hover.contents {
            assert!(m.value.contains("## Bölüm 1\nBölüm 1 detayı burada."));
            assert!(!m.value.contains("## Bölüm 2"));
        } else {
            panic!("Expected markup content");
        }
    }

    #[test]
    fn test_hover_block_paragraph() {
        let rel_a = Path::new("doc-a.md");
        let rel_b = Path::new("doc-b.md");
        let doc_a = parse_document("# Doc A\n\n[[doc-b#^tanim]]", rel_a);
        let doc_b = parse_document(
            "# Doc B\n\nİlk paragraf.\n\nİkinci paragraf tanım içerir. ^tanim\n\nÜçüncü paragraf.",
            rel_b,
        );

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
                "# Doc A\n\n[[doc-b#^tanim]]",
                1,
            ),
        );

        let params = HoverParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: uri_a_str.parse().unwrap(),
                },
                position: tower_lsp_server::ls_types::Position::new(2, 5),
            },
            work_done_progress_params: Default::default(),
        };

        let hover = hover(params, &state).expect("Hover should return Some");
        if let HoverContents::Markup(m) = hover.contents {
            assert!(m.value.contains("İkinci paragraf tanım içerir. ^tanim"));
            assert!(!m.value.contains("İlk paragraf."));
            assert!(!m.value.contains("Üçüncü paragraf."));
        } else {
            panic!("Expected markup content");
        }
    }

    #[test]
    fn test_hover_anchor_missing_warning() {
        let rel_a = Path::new("doc-a.md");
        let rel_b = Path::new("doc-b.md");
        let doc_a = parse_document("# Doc A\n\n[[doc-b#Olmayan Başlık]]", rel_a);
        let doc_b = parse_document("# Doc B\n\nGenel içerik.", rel_b);

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
                "# Doc A\n\n[[doc-b#Olmayan Başlık]]",
                1,
            ),
        );

        let params = HoverParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: uri_a_str.parse().unwrap(),
                },
                position: tower_lsp_server::ls_types::Position::new(2, 5),
            },
            work_done_progress_params: Default::default(),
        };

        let hover = hover(params, &state).expect("Hover should return Some");
        if let HoverContents::Markup(m) = hover.contents {
            assert!(m.value.contains("⚠ 'Olmayan Başlık' bulunamadı"));
            assert!(m.value.contains("Genel içerik."));
        } else {
            panic!("Expected markup content");
        }
    }

    #[test]
    fn test_hover_preview_lines_truncated() {
        let rel_a = Path::new("doc-a.md");
        let rel_b = Path::new("doc-b.md");
        let doc_a = parse_document("# Doc A\n\n[[doc-b]]", rel_a);
        let long_content = (1..=12)
            .map(|i| format!("Satır {}", i))
            .collect::<Vec<_>>()
            .join("\n");
        let doc_b = parse_document(&format!("# Doc B\n\n{}", long_content), rel_b);

        let mut state = SatzState::default();
        state.config.hover.preview_lines = 4;
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
                "# Doc A\n\n[[doc-b]]",
                1,
            ),
        );

        let params = HoverParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: uri_a_str.parse().unwrap(),
                },
                position: tower_lsp_server::ls_types::Position::new(2, 3),
            },
            work_done_progress_params: Default::default(),
        };

        let hover = hover(params, &state).expect("Hover should return Some");
        if let HoverContents::Markup(m) = hover.contents {
            assert!(m.value.contains("Satır 1\nSatır 2\nSatır 3\nSatır 4"));
            assert!(m.value.contains("… (8 satır daha)"));
        } else {
            panic!("Expected markup content");
        }
    }
}
