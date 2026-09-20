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
// Test states are built field by field so each test shows exactly what it sets up.
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use super::*;
    use satz_core::{Index, parse_document};
    use std::path::Path;
    use tower_lsp_server::ls_types::{TextDocumentIdentifier, TextDocumentPositionParams};

    #[test]
    fn footnote_hover_found_when_label_case_differs() {
        let rel_a = Path::new("doc-a.md");
        let content = "Here is a note[^A].\n\n[^a]: The footnote body.";
        let doc_a = parse_document(content, rel_a);

        let mut state = SatzState::default();
        state.index = Index::build(vec![doc_a]);
        let uri = "file:///doc-a.md";
        state.open_docs.insert(
            uri.to_string(),
            crate::state::OpenDocument::new(uri, rel_a.to_path_buf(), content, 1),
        );

        let params = HoverParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: uri.parse().unwrap(),
                },
                position: tower_lsp_server::ls_types::Position::new(0, 15), // on [^A]
            },
            work_done_progress_params: Default::default(),
        };

        let hover = hover(params, &state).expect("hover should find the footnote body");
        let HoverContents::Markup(m) = hover.contents else {
            panic!("Expected markup content");
        };
        assert!(m.value.contains("The footnote body."), "{}", m.value);
    }

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
            assert!(m.value.contains("⚠ 'Olmayan Başlık' not found"));
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
            assert!(m.value.contains("… (8 more lines)"));
        } else {
            panic!("Expected markup content");
        }
    }

    // ---- block previews show exactly the paragraph, whatever the line endings ----

    /// The hover text for `[[t#^blk]]` into a note with the given source.
    fn block_preview(source: &str) -> String {
        let target = parse_document(source, Path::new("t.md"));
        assert!(
            target.blocks.iter().any(|b| b.id == "blk"),
            "block not found in {source:?}"
        );
        let link = satz_core::Link::new(
            satz_core::LinkKind::WikiLink,
            "t".to_string(),
            None,
            Some("blk".to_string()),
            None,
            satz_core::ByteRange::new(0, 0),
        );
        format_hover_content(&target, &link, None, 50)
    }

    fn mentions(text: &str, needles: &[&str], absent: &[&str]) {
        for n in needles {
            assert!(text.contains(n), "missing {n:?} in {text:?}");
        }
        for n in absent {
            assert!(!text.contains(n), "unexpected {n:?} in {text:?}");
        }
    }

    #[test]
    fn a_block_preview_is_the_paragraph_with_lf_and_crlf() {
        let lf = "First para\n\nSecond para ^blk\n\nThird para\n";
        mentions(
            &block_preview(lf),
            &["Second para"],
            &["First para", "Third para"],
        );
        let crlf = lf.replace('\n', "\r\n");
        mentions(
            &block_preview(&crlf),
            &["Second para"],
            &["First para", "Third para"],
        );
    }

    #[test]
    fn block_previews_work_at_the_edges_and_across_blank_line_styles() {
        // First and last paragraph, no trailing newline.
        mentions(&block_preview("Only ^blk"), &["Only"], &[]);
        mentions(&block_preview("Top ^blk\n\nNext\n"), &["Top"], &["Next"]);
        mentions(&block_preview("Prev\n\nLast ^blk"), &["Last"], &["Prev"]);
        // Several blank lines, and a "blank" line holding only spaces.
        mentions(
            &block_preview("Prev\n\n\n\nMid ^blk\n\n\nNext\n"),
            &["Mid"],
            &["Prev", "Next"],
        );
        mentions(
            &block_preview("Prev\n   \nMid ^blk\n \t \nNext\n"),
            &["Mid"],
            &["Prev", "Next"],
        );
        mentions(
            &block_preview("Prev\r\n  \r\nMid ^blk\r\n\r\n\r\nNext\r\n"),
            &["Mid"],
            &["Prev", "Next"],
        );
        // A multi-line paragraph is kept whole.
        mentions(
            &block_preview("Prev\n\nline one\nline two ^blk\n\nNext\n"),
            &["line one", "line two"],
            &["Prev", "Next"],
        );
    }

    // ---- the hover text is English and its code fence cannot be closed from inside ----

    fn preview_of(source: &str, limit: usize) -> String {
        let target = parse_document(source, Path::new("t.md"));
        let link = satz_core::Link::new(
            satz_core::LinkKind::WikiLink,
            "t".to_string(),
            None,
            None,
            None,
            satz_core::ByteRange::new(0, 0),
        );
        format_hover_content(&target, &link, None, limit)
    }

    #[test]
    fn a_preview_containing_a_fence_is_wrapped_in_a_longer_one() {
        let with_three = "# T\n\n```rust\ncode\n```\n\nafter\n";
        let out = preview_of(with_three, 50);
        assert!(out.contains("````markdown\n"), "{out}");
        assert!(out.ends_with("\n````"), "{out}");
        assert!(out.contains("```rust"), "{out}");

        let with_four = "# T\n\n````\ninner ```\n````\n";
        let out = preview_of(with_four, 50);
        assert!(out.contains("`````markdown\n"), "{out}");
        assert!(out.ends_with("\n`````"), "{out}");
    }

    #[test]
    fn a_preview_without_backticks_keeps_the_plain_three_backtick_fence() {
        let out = preview_of("# T\n\nplain text\nmore `inline` code\n", 50);
        assert!(out.contains("```markdown\n"), "{out}");
        assert!(out.ends_with("\n```"), "{out}");
        assert!(!out.contains("````"), "{out}");
    }

    #[test]
    fn a_truncated_preview_still_counts_the_hidden_lines_and_closes_its_fence() {
        let body: String = (1..=12).map(|i| format!("line {i}\n")).collect();
        let out = preview_of(&format!("# T\n\n{body}"), 5);
        assert!(out.contains("… (7 more lines)"), "{out}");
        assert!(out.contains("line 5\n```\n"), "{out}");
        assert!(!out.contains("line 6"), "{out}");
        assert!(!out.contains("satır"), "{out}");
    }

    #[test]
    fn a_missing_anchor_warning_is_english() {
        let target = parse_document("# T\n\ntext\n", Path::new("t.md"));
        let link = satz_core::Link::new(
            satz_core::LinkKind::WikiLink,
            "t".to_string(),
            Some("Nope".to_string()),
            None,
            None,
            satz_core::ByteRange::new(0, 0),
        );
        let out = format_hover_content(&target, &link, Some("Nope"), 8);
        assert!(out.contains("⚠ 'Nope' not found"), "{out}");
        assert!(!out.contains("bulunamadı"), "{out}");
    }

    /// The hover text at `at` in `open`, with `files` indexed under a vault root.
    fn hover_text(files: &[(&str, &str)], open: &str, at: (u32, u32)) -> Option<String> {
        let root = if cfg!(windows) {
            Path::new("C:\\vault").to_path_buf()
        } else {
            Path::new("/vault").to_path_buf()
        };
        let mut state = SatzState::default();
        state.index = Index::build(
            files
                .iter()
                .map(|(p, t)| parse_document(t, Path::new(p)))
                .collect(),
        );
        state.vault_root = Some(root.clone());
        let uri = crate::convert::path_to_uri(&root.join(open))
            .unwrap()
            .as_str()
            .to_string();
        let text = files.iter().find(|(p, _)| *p == open).unwrap().1;
        state.open_docs.insert(
            uri.clone(),
            crate::state::OpenDocument::new(&uri, root.join(open), text, 1),
        );
        let params = HoverParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: uri.parse().unwrap(),
                },
                position: tower_lsp_server::ls_types::Position::new(at.0, at.1),
            },
            work_done_progress_params: Default::default(),
        };
        match hover(params, &state)?.contents {
            HoverContents::Markup(m) => Some(m.value),
            _ => panic!("markup expected"),
        }
    }

    #[test]
    fn hovering_a_link_inside_a_link_label_shows_the_inner_note() {
        let files = [
            ("a.md", "[see [[inner]]](outer.md)\n"),
            ("inner.md", "# Inner Title\n\ninner body\n"),
            ("outer.md", "# Outer Title\n\nouter body\n"),
        ];
        let inner = hover_text(&files, "a.md", (0, 8)).unwrap();
        assert!(
            inner.contains("inner body") && !inner.contains("outer body"),
            "{inner}"
        );
        for col in [2, 16, 22] {
            let outer = hover_text(&files, "a.md", (0, col)).unwrap();
            assert!(
                outer.contains("outer body") && !outer.contains("inner body"),
                "{col}: {outer}"
            );
        }
        assert!(hover_text(&files, "a.md", (0, 30)).is_none());
    }

    #[test]
    fn a_block_reference_finds_its_block_whatever_the_case() {
        let files = [
            ("a.md", "first\n\nthe block text ^abc\n\nlast\n"),
            ("b.md", "[[a#^ABC]] and [[a#^abd]]\n"),
        ];
        let found = hover_text(&files, "b.md", (0, 3)).unwrap();
        assert!(found.contains("the block text"), "{found}");
        assert!(!found.contains("not found"), "{found}");
        let missing = hover_text(&files, "b.md", (0, 16)).unwrap();
        assert!(missing.contains("not found"), "{missing}");
    }
}
