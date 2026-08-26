#![allow(clippy::collapsible_if)]

use serde_json::Value;
use tower_lsp_server::ls_types::{
    CompletionItem, CompletionItemKind, CompletionParams, CompletionResponse, Documentation,
    MarkupContent, MarkupKind,
};

use crate::convert::lsp_pos_to_satz;
use crate::state::SatzState;

pub fn completion(params: CompletionParams, state: &SatzState) -> Option<CompletionResponse> {
    let uri = params.text_document_position.text_document.uri.as_str();
    let pos = params.text_document_position.position;

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
    let source = doc.line_index.source();

    // Get prefix of the current line up to byte_offset
    let line_start_offset = source[..byte_offset]
        .rfind('\n')
        .map(|idx| idx + 1)
        .unwrap_or(0);
    let line_prefix = &source[line_start_offset..byte_offset];

    // 1. Check for wikilink completion: `[[...`
    if let Some(open_bracket_idx) = line_prefix.rfind("[[") {
        let inside_wikilink = &line_prefix[open_bracket_idx + 2..];

        // Check if inside heading reference `[[doc#...` or `[[#...`
        if let Some((target_doc_str, _heading_query)) = inside_wikilink.split_once('#') {
            let target_id = if target_doc_str.is_empty() {
                &doc_id
            } else if let Some(resolved) = state.index.resolve_link(target_doc_str) {
                resolved
            } else {
                return Some(CompletionResponse::Array(vec![]));
            };

            if let Some(target_doc) = state.index.get_doc(target_id) {
                let items = target_doc
                    .headings
                    .iter()
                    .map(|h| CompletionItem {
                        label: h.text.trim().to_string(),
                        kind: Some(CompletionItemKind::FIELD),
                        detail: Some(format!("Level {} Heading", h.level)),
                        insert_text: Some(h.text.trim().to_string()),
                        ..Default::default()
                    })
                    .collect();
                return Some(CompletionResponse::Array(items));
            }
        } else {
            // Document / Note completion
            let mut items = Vec::new();

            for d in state.index.documents() {
                // Title completion
                let title_label = if d.title != "Untitled" && !d.title.is_empty() {
                    d.title.clone()
                } else {
                    d.id.as_str().to_string()
                };

                // Insert document by title or stem
                let insert_text = if !d.title.is_empty() && d.title != "Untitled" {
                    d.title.clone()
                } else {
                    d.path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or(d.id.as_str())
                        .to_string()
                };

                items.push(CompletionItem {
                    label: title_label,
                    kind: Some(CompletionItemKind::FILE),
                    detail: Some(d.id.as_str().to_string()),
                    insert_text: Some(insert_text),
                    data: Some(serde_json::json!({ "doc_id": d.id.as_str() })),
                    ..Default::default()
                });

                // Alias completions
                for alias in &d.frontmatter.aliases {
                    items.push(CompletionItem {
                        label: format!("{} (alias)", alias),
                        kind: Some(CompletionItemKind::REFERENCE),
                        detail: Some(format!("Alias for: {}", d.title)),
                        insert_text: Some(alias.clone()),
                        data: Some(serde_json::json!({ "doc_id": d.id.as_str() })),
                        ..Default::default()
                    });
                }
            }

            return Some(CompletionResponse::Array(items));
        }
    }

    // 2. Check for Footnote completion: `[^...`
    if let Some(open_fn_idx) = line_prefix.rfind("[^") {
        let inside_fn = &line_prefix[open_fn_idx + 2..];
        if !inside_fn.contains(']') {
            let items = doc
                .footnotes
                .definitions
                .iter()
                .map(|f| CompletionItem {
                    label: f.label.clone(),
                    kind: Some(CompletionItemKind::REFERENCE),
                    detail: Some("Footnote Definition".to_string()),
                    insert_text: Some(f.label.clone()),
                    ..Default::default()
                })
                .collect();
            return Some(CompletionResponse::Array(items));
        }
    }

    // 3. Check for Tag completion: `#...`
    if let Some(hash_idx) = line_prefix.rfind('#') {
        // Ensure # is at start of line or preceded by whitespace
        let is_valid_tag_start = if hash_idx == 0 {
            true
        } else {
            line_prefix.as_bytes()[hash_idx - 1].is_ascii_whitespace()
        };

        if is_valid_tag_start {
            let items = state
                .index
                .all_tags()
                .into_iter()
                .map(|tag_name| CompletionItem {
                    label: format!("#{}", tag_name),
                    kind: Some(CompletionItemKind::KEYWORD),
                    detail: Some("Tag".to_string()),
                    insert_text: Some(tag_name.to_string()),
                    ..Default::default()
                })
                .collect();
            return Some(CompletionResponse::Array(items));
        }
    }

    None
}

pub fn completion_resolve(mut item: CompletionItem, state: &SatzState) -> CompletionItem {
    if let Some(Value::Object(map)) = &item.data {
        if let Some(Value::String(doc_id_str)) = map.get("doc_id") {
            let doc_id = satz_core::DocId::new(doc_id_str);
            if let Some(target_doc) = state.index.get_doc(&doc_id) {
                let mut value = format!("# {}\n\n", target_doc.title);

                if !target_doc.tags.is_empty() {
                    let tags_str: Vec<String> =
                        target_doc.tags.iter().map(|t| t.name.clone()).collect();
                    value.push_str(&format!("**Tags:** {}\n\n", tags_str.join(", ")));
                }

                let source = target_doc.line_index.source();
                let preview_lines: Vec<&str> = source
                    .lines()
                    .filter(|l| !l.trim().is_empty())
                    .take(5)
                    .collect();

                value.push_str("```markdown\n");
                value.push_str(&preview_lines.join("\n"));
                if source.lines().count() > 5 {
                    value.push_str("\n...");
                }
                value.push_str("\n```");

                item.documentation = Some(Documentation::MarkupContent(MarkupContent {
                    kind: MarkupKind::Markdown,
                    value,
                }));
            }
        }
    }

    item
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
    fn test_wikilink_completion() {
        let rel_a = Path::new("doc-a.md");
        let rel_b = Path::new("doc-b.md");
        let doc_a = parse_document("# Doc A\n\n[[", rel_a);
        let doc_b = parse_document(
            "---\ntitle: Target Note\naliases: [TargetAlias]\n---\n# Note B",
            rel_b,
        );

        let mut state = SatzState::default();
        state.index = Index::build(vec![doc_a.clone(), doc_b]);
        state.vault_root = Some(Path::new("").to_path_buf());

        let uri_str = "file:///doc-a.md";
        state.open_docs.insert(
            uri_str.to_string(),
            crate::state::OpenDocument::new(uri_str, rel_a.to_path_buf(), "# Doc A\n\n[[", 1),
        );

        let params = CompletionParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: uri_str.parse().unwrap(),
                },
                position: Position::new(2, 2), // right after `[[`
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: None,
        };

        let response = completion(params, &state).expect("Completion response expected");
        if let CompletionResponse::Array(items) = response {
            assert!(items.iter().any(|i| i.label == "Target Note"));
            assert!(items.iter().any(|i| i.label.contains("TargetAlias")));
        } else {
            panic!("Expected CompletionResponse::Array");
        }
    }

    #[test]
    fn test_heading_completion() {
        let rel_a = Path::new("doc-a.md");
        let rel_b = Path::new("doc-b.md");
        let doc_a = parse_document("# Doc A\n\n[[doc-b#", rel_a);
        let doc_b = parse_document("# Heading In B\n## Subheading", rel_b);

        let mut state = SatzState::default();
        state.index = Index::build(vec![doc_a.clone(), doc_b]);
        state.vault_root = Some(Path::new("").to_path_buf());

        let uri_str = "file:///doc-a.md";
        state.open_docs.insert(
            uri_str.to_string(),
            crate::state::OpenDocument::new(uri_str, rel_a.to_path_buf(), "# Doc A\n\n[[doc-b#", 1),
        );

        let params = CompletionParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: uri_str.parse().unwrap(),
                },
                position: Position::new(2, 8), // right after `[[doc-b#`
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: None,
        };

        let response = completion(params, &state).expect("Completion response expected");
        if let CompletionResponse::Array(items) = response {
            assert!(items.iter().any(|i| i.label == "Heading In B"));
            assert!(items.iter().any(|i| i.label == "Subheading"));
        } else {
            panic!("Expected CompletionResponse::Array");
        }
    }

    #[test]
    fn test_completion_resolve() {
        let rel_a = Path::new("doc-a.md");
        let doc_a = parse_document(
            "---\ntags: [rust]\n---\n# Doc Title\nLine 1 of content\nLine 2 of content",
            rel_a,
        );

        let mut state = SatzState::default();
        state.index = Index::build(vec![doc_a]);

        let item = CompletionItem {
            label: "Doc Title".to_string(),
            data: Some(serde_json::json!({ "doc_id": "doc-a.md" })),
            ..Default::default()
        };

        let resolved = completion_resolve(item, &state);
        assert!(resolved.documentation.is_some());
        if let Some(Documentation::MarkupContent(m)) = resolved.documentation {
            assert!(m.value.contains("Doc Title"));
            assert!(m.value.contains("Line 1 of content"));
            assert!(m.value.contains("rust"));
        } else {
            panic!("Expected MarkupContent in documentation");
        }
    }
}
