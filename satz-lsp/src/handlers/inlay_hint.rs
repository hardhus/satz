use crate::convert::byte_range_to_lsp;
use crate::state::SatzState;
use satz_core::LinkKind;
use tower_lsp_server::ls_types::{InlayHint, InlayHintKind, InlayHintLabel, InlayHintParams};

/// Computes InlayHint entries displaying target note metadata right next to links.
pub fn inlay_hint(params: InlayHintParams, state: &SatzState) -> Option<Vec<InlayHint>> {
    if !state.config.lsp.inlay_hints.enable {
        return None;
    }

    let uri = params.text_document.uri.as_str();
    let open_doc = state.open_docs.get(uri)?;
    let rel_path = match &state.vault_root {
        Some(root) => open_doc.path.strip_prefix(root).unwrap_or(&open_doc.path),
        None => &open_doc.path,
    };
    let rel_path_str = rel_path.to_string_lossy().replace('\\', "/");
    let doc_id = satz_core::DocId::new(&rel_path_str);
    let doc = state.index.get_doc(&doc_id)?;

    let mut hints: Vec<InlayHint> = Vec::new();

    for link in &doc.links {
        match link.kind {
            LinkKind::WikiLink | LinkKind::Embed | LinkKind::Markdown => {
                if link.target_doc.is_empty()
                    || link.target_doc.starts_with("http://")
                    || link.target_doc.starts_with("https://")
                {
                    continue;
                }

                let range = byte_range_to_lsp(link.range, &doc.line_index);
                let position = range.end;

                let label_text = match state.index.resolve_link(&link.target_doc) {
                    Some(target_id) => {
                        if let Some(target_doc) = state.index.get_doc(target_id) {
                            if !target_doc.tags.is_empty() {
                                let tag_str: Vec<String> = target_doc
                                    .tags
                                    .iter()
                                    .take(3)
                                    .map(|t| format!("#{}", t.name.trim_start_matches('#')))
                                    .collect();
                                format!(" {}", tag_str.join(" "))
                            } else {
                                format!(" ({})", target_doc.title)
                            }
                        } else {
                            " ⚠ not found".to_string()
                        }
                    }
                    None => " ⚠ not found".to_string(),
                };

                hints.push(InlayHint {
                    position,
                    label: InlayHintLabel::String(label_text),
                    kind: Some(InlayHintKind::TYPE),
                    text_edits: None,
                    tooltip: None,
                    padding_left: Some(true),
                    padding_right: None,
                    data: None,
                });
            }
            LinkKind::Footnote => {}
        }
    }

    if hints.is_empty() { None } else { Some(hints) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use satz_core::{Index, VaultConfig, parse_document};
    use std::path::Path;
    use tower_lsp_server::ls_types::{Position, Range, TextDocumentIdentifier};

    #[test]
    fn test_inlay_hints_disabled() {
        let mut config = VaultConfig::default();
        config.lsp.inlay_hints.enable = false;

        let doc_a = parse_document("# Doc A\n\n[[doc-b]]", Path::new("doc-a.md"));
        let doc_b = parse_document("# Doc B", Path::new("doc-b.md"));

        let mut state = SatzState {
            index: Index::build(vec![doc_a, doc_b]),
            vault_root: Some(Path::new("").to_path_buf()),
            config,
            ..Default::default()
        };
        state.open_docs.insert(
            "file:///doc-a.md".to_string(),
            crate::state::OpenDocument::new(
                "file:///doc-a.md",
                Path::new("doc-a.md").to_path_buf(),
                "# Doc A\n\n[[doc-b]]",
                1,
            ),
        );

        let params = InlayHintParams {
            work_done_progress_params: Default::default(),
            text_document: TextDocumentIdentifier {
                uri: "file:///doc-a.md".parse().unwrap(),
            },
            range: Range::new(Position::new(0, 0), Position::new(10, 0)),
        };

        assert!(inlay_hint(params, &state).is_none());
    }

    #[test]
    fn test_inlay_hints_resolved_and_unresolved() {
        let doc_a = parse_document(
            "# Doc A\n\n[[doc-b]] and [[missing-doc]]",
            Path::new("doc-a.md"),
        );
        let doc_b = parse_document(
            "---\ntags: [rust, lsp]\n---\n# Doc B",
            Path::new("doc-b.md"),
        );

        let mut state = SatzState {
            index: Index::build(vec![doc_a, doc_b]),
            vault_root: Some(Path::new("").to_path_buf()),
            ..Default::default()
        };
        state.open_docs.insert(
            "file:///doc-a.md".to_string(),
            crate::state::OpenDocument::new(
                "file:///doc-a.md",
                Path::new("doc-a.md").to_path_buf(),
                "# Doc A\n\n[[doc-b]] and [[missing-doc]]",
                1,
            ),
        );

        let params = InlayHintParams {
            work_done_progress_params: Default::default(),
            text_document: TextDocumentIdentifier {
                uri: "file:///doc-a.md".parse().unwrap(),
            },
            range: Range::new(Position::new(0, 0), Position::new(10, 0)),
        };

        let result = inlay_hint(params, &state).expect("Inlay hints expected");
        assert_eq!(result.len(), 2);

        // Resolved link to doc-b
        if let InlayHintLabel::String(s) = &result[0].label {
            assert!(s.contains("#rust"));
            assert!(s.contains("#lsp"));
        } else {
            panic!("Expected String label");
        }

        // Unresolved link to missing-doc
        if let InlayHintLabel::String(s) = &result[1].label {
            assert!(s.contains("⚠ not found"));
        } else {
            panic!("Expected String label");
        }
    }
}
