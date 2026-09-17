#![allow(clippy::collapsible_if)]

use serde_json::Value;
use tower_lsp_server::ls_types::{
    CompletionItem, CompletionItemKind, CompletionParams, CompletionResponse, CompletionTextEdit,
    Documentation, MarkupContent, MarkupKind, Range, TextEdit,
};

use crate::convert::{byte_range_to_lsp, lsp_pos_to_satz};
use crate::state::SatzState;

/// Builds an explicit replace-range `text_edit` covering `[query_start, cursor)` instead of a
/// bare `insert_text`. Without this, it's up to the client to guess how much of the
/// already-typed query to replace -- ambiguous and, per one field report, inconsistent between
/// a document's first and second wikilink completion in the same session (a stray extra `]]`,
/// or a garbled single-bracket result). An explicit range removes that guesswork entirely.
fn completion_text_edit(range: Range, new_text: String) -> CompletionTextEdit {
    CompletionTextEdit::Edit(TextEdit { range, new_text })
}

pub fn completion(params: CompletionParams, state: &SatzState) -> Option<CompletionResponse> {
    let uri = params.text_document_position.text_document.uri.as_str();
    let pos = params.text_document_position.position;
    tracing::debug!(uri, ?pos, "completion");

    let open_doc = state.open_docs.get(uri)?;
    let rel_path =
        crate::state::SatzState::get_rel_path(&open_doc.path, state.vault_root.as_deref());
    let rel_path_str = rel_path.to_string_lossy().replace('\\', "/");
    let doc_id = satz_core::DocId::new(&rel_path_str);
    let doc = state.index.get_doc(&doc_id)?;

    // Byte-offset/text-scan against the LIVE rope, not `doc.line_index`: `doc` is the
    // debounced (200-500ms) reparse snapshot, but completion re-fires immediately on every
    // `[`/`#`/`^` keystroke, faster than that debounce can settle. Scanning stale text here
    // corrupts the line-prefix/closing-bracket checks below -- e.g. producing a duplicated
    // `]]` when a second wikilink is typed quickly on the same line right after a first one.
    let live_line_index = satz_core::LineIndex::new(&open_doc.rope.to_string());
    let satz_pos = lsp_pos_to_satz(pos);
    let byte_offset = live_line_index.position_to_byte(satz_pos);
    let source = live_line_index.source();

    // Get prefix of the current line up to byte_offset
    let line_start_offset = source[..byte_offset]
        .rfind('\n')
        .map(|idx| idx + 1)
        .unwrap_or(0);
    let line_prefix = &source[line_start_offset..byte_offset];

    // Check if cursor is already followed by closing `]]` or `]`
    let line_rest = &source[byte_offset..];
    let has_closing_brackets = line_rest.starts_with("]]") || line_rest.starts_with(']');
    let close_suffix = if !has_closing_brackets { "]]" } else { "" };

    // 1. Check for wikilink completion: `[[...`
    if let Some(open_bracket_idx) = line_prefix.rfind("[[") {
        let inside_wikilink = &line_prefix[open_bracket_idx + 2..];
        let inside_wikilink_start = line_start_offset + open_bracket_idx + 2;

        // Check if inside heading or block reference `[[doc#...` or `[[#...`
        if let Some((target_doc_str, heading_or_block)) = inside_wikilink.split_once('#') {
            let heading_or_block_start = inside_wikilink_start + target_doc_str.len() + 1;
            let target_id = if target_doc_str.is_empty() {
                &doc_id
            } else if let Some(resolved) = state.index.resolve_link(target_doc_str) {
                resolved
            } else {
                tracing::debug!(
                    target_doc_str,
                    "completion: returning candidates count=0 (target doc did not resolve)"
                );
                return Some(CompletionResponse::Array(vec![]));
            };

            if let Some(target_doc) = state.index.get_doc(target_id) {
                if let Some(_block_prefix) = heading_or_block.strip_prefix('^') {
                    // Block anchor completion: `[[doc#^...`
                    let range = byte_range_to_lsp(
                        satz_core::ByteRange::new(heading_or_block_start + 1, byte_offset),
                        &live_line_index,
                    );
                    let items: Vec<CompletionItem> = target_doc
                        .blocks
                        .iter()
                        .map(|b| {
                            let new_text = format!("^{}{}", b.id, close_suffix);
                            CompletionItem {
                                label: format!("^{}", b.id),
                                kind: Some(CompletionItemKind::VARIABLE),
                                detail: Some("Block Anchor".to_string()),
                                text_edit: Some(completion_text_edit(range, new_text)),
                                filter_text: Some(format!("^{}", b.id)),
                                ..Default::default()
                            }
                        })
                        .collect();
                    tracing::debug!(
                        count = items.len(),
                        "completion: returning candidates (block anchors)"
                    );
                    return Some(CompletionResponse::Array(items));
                } else {
                    // Heading completion: `[[doc#...`
                    let range = byte_range_to_lsp(
                        satz_core::ByteRange::new(heading_or_block_start, byte_offset),
                        &live_line_index,
                    );
                    let mut items: Vec<CompletionItem> = target_doc
                        .headings
                        .iter()
                        .map(|h| {
                            let new_text = format!("{}{}", h.text.trim(), close_suffix);
                            CompletionItem {
                                label: h.text.trim().to_string(),
                                kind: Some(CompletionItemKind::FIELD),
                                detail: Some(format!("Level {} Heading", h.level)),
                                text_edit: Some(completion_text_edit(range, new_text)),
                                ..Default::default()
                            }
                        })
                        .collect();

                    // If query is empty or starts with '^', also suggest blocks
                    if heading_or_block.is_empty() {
                        for b in &target_doc.blocks {
                            let new_text = format!("^{}{}", b.id, close_suffix);
                            items.push(CompletionItem {
                                label: format!("^{}", b.id),
                                kind: Some(CompletionItemKind::VARIABLE),
                                detail: Some("Block Anchor".to_string()),
                                text_edit: Some(completion_text_edit(range, new_text)),
                                filter_text: Some(format!("^{}", b.id)),
                                ..Default::default()
                            });
                        }
                    }

                    tracing::debug!(
                        count = items.len(),
                        "completion: returning candidates (headings/blocks for doc)"
                    );
                    return Some(CompletionResponse::Array(items));
                }
            }
        } else {
            // Document / Note completion
            let mut items = Vec::new();
            let range = byte_range_to_lsp(
                satz_core::ByteRange::new(inside_wikilink_start, byte_offset),
                &live_line_index,
            );

            for d in state.index.documents() {
                // Title completion. `insert_text` is always the document's own vault-relative
                // path (extension stripped), never its title: a title is free-form prose the
                // user should be able to reword at any time (this is a book, chapters get
                // retitled) without silently breaking every wikilink that was inserted by
                // completion — paths only change via `rename`, which already rewrites every
                // link (of any style) pointing at the renamed document.
                let title_label = if d.title != "Untitled" && !d.title.is_empty() {
                    d.title.clone()
                } else {
                    d.id.as_str().to_string()
                };
                let path_str = d.path.to_string_lossy().replace('\\', "/");
                let insert_base = path_str.strip_suffix(".md").unwrap_or(&path_str).to_string();

                items.push(CompletionItem {
                    label: title_label.clone(),
                    kind: Some(CompletionItemKind::FILE),
                    detail: Some(d.id.as_str().to_string()),
                    text_edit: Some(completion_text_edit(
                        range,
                        format!("{}{}", insert_base, close_suffix),
                    )),
                    filter_text: Some(title_label.clone()),
                    data: Some(serde_json::json!({ "doc_id": d.id.as_str() })),
                    ..Default::default()
                });

                // Alias completions
                for alias in &d.frontmatter.aliases {
                    items.push(CompletionItem {
                        label: format!("{} (alias)", alias),
                        kind: Some(CompletionItemKind::REFERENCE),
                        detail: Some(format!("Alias for: {}", d.title)),
                        text_edit: Some(completion_text_edit(
                            range,
                            format!("{}{}", alias, close_suffix),
                        )),
                        filter_text: Some(alias.clone()),
                        data: Some(serde_json::json!({ "doc_id": d.id.as_str() })),
                        ..Default::default()
                    });
                }

                // Heading completions, so e.g. typing "olgu" can directly surface a `## Olgu`
                // heading buried in some other document as `path#Olgu`, without first having to
                // complete to that document and then separately complete `#`. No manual
                // `sort_text` bias here: a short, close-to-exact heading label like "Olgu"
                // already ranks above an unrelated, much longer title in any reasonable
                // client-side fuzzy matcher, so hand-tuning order here would just as likely
                // fight the client's own scoring as help it.
                for h in &d.headings {
                    let heading_text = h.text.trim();
                    if heading_text.is_empty() {
                        continue;
                    }
                    items.push(CompletionItem {
                        label: heading_text.to_string(),
                        kind: Some(CompletionItemKind::FIELD),
                        detail: Some(format!("Heading in {}", title_label)),
                        text_edit: Some(completion_text_edit(
                            range,
                            format!("{}#{}{}", insert_base, heading_text, close_suffix),
                        )),
                        filter_text: Some(heading_text.to_string()),
                        data: Some(serde_json::json!({ "doc_id": d.id.as_str() })),
                        ..Default::default()
                    });
                }
            }

            tracing::debug!(
                count = items.len(),
                "completion: returning candidates (documents/headings/aliases)"
            );
            return Some(CompletionResponse::Array(items));
        }
    }

    // 2. Check for Footnote completion: `[^...`
    if let Some(open_fn_idx) = line_prefix.rfind("[^") {
        let inside_fn = &line_prefix[open_fn_idx + 2..];
        if !inside_fn.contains(']') {
            let range = byte_range_to_lsp(
                satz_core::ByteRange::new(line_start_offset + open_fn_idx + 2, byte_offset),
                &live_line_index,
            );
            let items: Vec<CompletionItem> = doc
                .footnotes
                .definitions
                .iter()
                .map(|f| CompletionItem {
                    label: f.label.clone(),
                    kind: Some(CompletionItemKind::REFERENCE),
                    detail: Some("Footnote Definition".to_string()),
                    text_edit: Some(completion_text_edit(range, f.label.clone())),
                    ..Default::default()
                })
                .collect();
            tracing::debug!(
                count = items.len(),
                "completion: returning candidates (footnotes)"
            );
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
            let range = byte_range_to_lsp(
                satz_core::ByteRange::new(line_start_offset + hash_idx + 1, byte_offset),
                &live_line_index,
            );
            let items: Vec<CompletionItem> = state
                .index
                .all_tags()
                .into_iter()
                .map(|tag_name| CompletionItem {
                    label: format!("#{}", tag_name),
                    kind: Some(CompletionItemKind::KEYWORD),
                    detail: Some("Tag".to_string()),
                    text_edit: Some(completion_text_edit(range, tag_name.to_string())),
                    ..Default::default()
                })
                .collect();
            tracing::debug!(count = items.len(), "completion: returning candidates (tags)");
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

    /// Test helper: pulls the replacement text out of a completion item's `text_edit`
    /// (completion no longer sets bare `insert_text` -- see `completion_text_edit`).
    fn item_new_text(item: &CompletionItem) -> Option<&str> {
        match &item.text_edit {
            Some(CompletionTextEdit::Edit(edit)) => Some(edit.new_text.as_str()),
            _ => None,
        }
    }

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
            // Document completions insert the path, not the title, so the link survives a
            // future title edit; the title stays as the (searchable) label only.
            let target_note = items
                .iter()
                .find(|i| i.label == "Target Note")
                .expect("Target Note item");
            assert_eq!(item_new_text(target_note), Some("doc-b]]"));
        } else {
            panic!("Expected CompletionResponse::Array");
        }
    }

    #[test]
    fn test_completion_uses_live_rope_not_stale_index() {
        let rel_a = Path::new("doc-a.md");
        // The indexed snapshot is stale: reparse hasn't caught up to the "]]" the editor
        // already auto-paired in the live buffer, simulating typing faster than the
        // reparse debounce window (200-500ms).
        let stale_doc_a = parse_document("# Doc A\n\n[[Olgu", rel_a);
        let doc_b = parse_document("# Olgu\nContent", Path::new("doc-b.md"));

        let mut state = SatzState::default();
        state.index = Index::build(vec![stale_doc_a, doc_b]);
        state.vault_root = Some(Path::new("").to_path_buf());

        let uri_str = "file:///doc-a.md";
        // Live buffer already has the closing brackets the editor auto-paired.
        let live_content = "# Doc A\n\n[[Olgu]]";
        state.open_docs.insert(
            uri_str.to_string(),
            crate::state::OpenDocument::new(uri_str, rel_a.to_path_buf(), live_content, 1),
        );

        let params = CompletionParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: uri_str.parse().unwrap(),
                },
                position: Position::new(2, 6), // right after "Olgu", before the live "]]"
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: None,
        };

        let response = completion(params, &state).expect("Completion response expected");
        let CompletionResponse::Array(items) = response else {
            panic!("Expected CompletionResponse::Array");
        };
        assert!(!items.is_empty());

        // The live buffer already has closing brackets right after the cursor; completion must
        // not append a second "]]" on top of them. Before the fix, this scanned the stale
        // indexed text (which had no trailing "]]" yet) instead of the live rope, and always
        // appended "]]" -- producing a duplicated "]]]]" once the editor's own auto-pair merged
        // in.
        for item in &items {
            if let Some(text) = item_new_text(item) {
                assert!(
                    !text.ends_with("]]"),
                    "new_text must not append ]] when the live buffer already has \
                     closing brackets after the cursor: {text:?}"
                );
            }
        }

        // The replace range must cover exactly the already-typed query ("Olgu", from right
        // after "[[" to the cursor) -- not the client's own guess. Accepting any item should
        // replace "Olgu" in place, not insert alongside it.
        let any_item = items.first().expect("at least one candidate");
        match &any_item.text_edit {
            Some(CompletionTextEdit::Edit(edit)) => {
                assert_eq!(edit.range.start, Position::new(2, 2));
                assert_eq!(edit.range.end, Position::new(2, 6));
            }
            _ => panic!("expected an explicit text_edit, not a bare insert_text"),
        }
    }

    #[test]
    fn test_flat_wikilink_completion_includes_headings() {
        let rel_a = Path::new("doc-a.md");
        let rel_b = Path::new("tlp/sozluk.md");
        let doc_a = parse_document("# Doc A\n\n[[", rel_a);
        let doc_b = parse_document(
            "---\ntitle: Tractatus Sözlüğü\n---\n# Tractatus Sözlüğü\n\n## Olgu\n\ntext",
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
                position: Position::new(2, 2),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: None,
        };

        let response = completion(params, &state).expect("Completion response expected");
        let CompletionResponse::Array(items) = response else {
            panic!("Expected CompletionResponse::Array");
        };

        // Typing "olgu" should be able to jump straight to the `## Olgu` heading inside
        // tlp/sozluk.md without first completing to the document and then to `#Olgu`.
        let olgu_item = items
            .iter()
            .find(|i| i.label == "Olgu")
            .expect("heading completion item for 'Olgu'");
        assert_eq!(item_new_text(olgu_item), Some("tlp/sozluk#Olgu]]"));

        // The document-level completion for the same file is still path-based.
        assert!(
            items
                .iter()
                .any(|i| i.label == "Tractatus Sözlüğü"
                    && item_new_text(i) == Some("tlp/sozluk]]"))
        );
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

    #[test]
    fn test_block_anchor_completion() {
        let rel_a = Path::new("doc-a.md");
        let rel_b = Path::new("doc-b.md");
        let doc_a = parse_document("# Doc A\n\n[[doc-b#^", rel_a);
        let doc_b = parse_document(
            "Some block text ^my-block-id\nOther text ^other-block",
            rel_b,
        );

        let mut state = SatzState::default();
        state.index = Index::build(vec![doc_a.clone(), doc_b]);
        state.vault_root = Some(Path::new("").to_path_buf());

        let uri_str = "file:///doc-a.md";
        state.open_docs.insert(
            uri_str.to_string(),
            crate::state::OpenDocument::new(
                uri_str,
                rel_a.to_path_buf(),
                "# Doc A\n\n[[doc-b#^",
                1,
            ),
        );

        let params = CompletionParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: uri_str.parse().unwrap(),
                },
                position: Position::new(2, 9), // right after `[[doc-b#^`
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: None,
        };

        let response = completion(params, &state).expect("Completion response expected");
        if let CompletionResponse::Array(items) = response {
            assert!(items.iter().any(|i| i.label == "^my-block-id"));
            assert!(items.iter().any(|i| i.label == "^other-block"));
        } else {
            panic!("Expected CompletionResponse::Array");
        }
    }
}
