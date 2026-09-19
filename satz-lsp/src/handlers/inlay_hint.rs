use crate::convert::byte_range_to_lsp;
use crate::state::SatzState;
use satz_core::LinkKind;
use tower_lsp_server::ls_types::{InlayHint, InlayHintKind, InlayHintLabel, InlayHintParams};

/// Whether `position` lies in `range` (both ends inclusive).
fn in_range(
    position: tower_lsp_server::ls_types::Position,
    range: tower_lsp_server::ls_types::Range,
) -> bool {
    (range.start.line, range.start.character) <= (position.line, position.character)
        && (position.line, position.character) <= (range.end.line, range.end.character)
}

/// Computes InlayHint entries displaying target note metadata right next to links.
pub fn inlay_hint(params: InlayHintParams, state: &SatzState) -> Option<Vec<InlayHint>> {
    if !state.config.lsp.inlay_hints.enable {
        return None;
    }

    let uri = params.text_document.uri.as_str();
    tracing::debug!(uri, "inlay_hint");
    let open_doc = state.open_docs.get(uri)?;
    let rel_path =
        crate::state::SatzState::get_rel_path(&open_doc.path, state.vault_root.as_deref());
    let rel_path_str = rel_path.to_string_lossy().replace('\\', "/");
    let doc_id = satz_core::DocId::new(&rel_path_str);
    let doc = state.index.get_doc(&doc_id)?;

    let mut hints: Vec<InlayHint> = Vec::new();

    for link in &doc.links {
        match link.kind {
            LinkKind::WikiLink | LinkKind::Embed | LinkKind::Markdown => {
                if satz_core::model::link::is_external_target(&link.target_doc) {
                    continue;
                }
                // A link inside this same note (`[[#Heading]]`) only needs a hint when its anchor
                // is missing; there is nothing to say about the note it already is.
                let same_note = link.target_doc.is_empty();
                if same_note && link.target_heading.is_none() && link.target_block.is_none() {
                    continue;
                }

                let range = byte_range_to_lsp(link.range, &doc.line_index);
                let position = range.end;
                if !in_range(position, params.range) {
                    continue;
                }

                let resolution = state.resolve(link, doc);
                if same_note
                    && !matches!(resolution, satz_core::LinkResolution::AnchorMissing { .. })
                {
                    continue;
                }

                let label_text = match resolution {
                    satz_core::LinkResolution::AnchorMissing { .. } => {
                        if link.target_block.is_some() {
                            " ⚠ block not found".to_string()
                        } else {
                            " ⚠ heading not found".to_string()
                        }
                    }
                    satz_core::LinkResolution::Resolved {
                        doc: target_doc, ..
                    } => {
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
                    }
                    satz_core::LinkResolution::DocMissing => " ⚠ not found".to_string(),
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

    // ---- range, anchors and same-note links ----

    const A_TEXT: &str =
        "[[b]] [[b#Nope]] [[b#^nope]] [[b#Real]]\n\n[[#Missing]] [[#Here]]\n\n## Here\n";

    /// `((line, col), label)` of every hint `inlay_hint` returns for `A_TEXT` in `range`.
    fn hints_in(range: Range) -> Vec<((u32, u32), String)> {
        let rel_a = Path::new("a.md");
        let mut config = VaultConfig::default();
        config.lsp.inlay_hints.enable = true;
        let mut state = SatzState {
            index: Index::build(vec![
                parse_document(A_TEXT, rel_a),
                parse_document("# B\n\n## Real\n\ntext ^blk\n", Path::new("b.md")),
            ]),
            vault_root: Some(Path::new("").to_path_buf()),
            config,
            ..Default::default()
        };
        state.open_docs.insert(
            "file:///a.md".to_string(),
            crate::state::OpenDocument::new("file:///a.md", rel_a.to_path_buf(), A_TEXT, 1),
        );
        let params = InlayHintParams {
            work_done_progress_params: Default::default(),
            text_document: TextDocumentIdentifier {
                uri: "file:///a.md".parse().unwrap(),
            },
            range,
        };
        inlay_hint(params, &state)
            .unwrap_or_default()
            .into_iter()
            .map(|h| {
                let InlayHintLabel::String(label) = h.label else {
                    panic!("string label expected")
                };
                ((h.position.line, h.position.character), label)
            })
            .collect()
    }

    fn whole_document() -> Range {
        Range::new(Position::new(0, 0), Position::new(9, 0))
    }

    #[test]
    fn a_missing_anchor_is_flagged_and_a_present_one_is_not() {
        let hints = hints_in(whole_document());
        assert_eq!(
            hints,
            vec![
                ((0, 5), " (B)".to_string()),
                ((0, 16), " ⚠ heading not found".to_string()),
                ((0, 28), " ⚠ block not found".to_string()),
                ((0, 39), " (B)".to_string()),
                // A link inside the same note: only a missing anchor gets a hint.
                ((2, 12), " ⚠ heading not found".to_string()),
            ]
        );
    }

    #[test]
    fn only_links_inside_the_requested_range_get_hints() {
        // Second line of the note only: nothing on line 0, just the `[[#Missing]]` warning.
        let second = Range::new(Position::new(2, 0), Position::new(2, 30));
        assert_eq!(
            hints_in(second),
            vec![((2, 12), " ⚠ heading not found".to_string())]
        );
        // The first two links.
        let first_two = Range::new(Position::new(0, 0), Position::new(0, 17));
        let found = hints_in(first_two);
        assert_eq!(found.len(), 2, "{found:?}");
        assert_eq!(found[1].0, (0, 16));
        // An empty range far from any link.
        let far = Range::new(Position::new(4, 0), Position::new(4, 3));
        assert!(hints_in(far).is_empty());
    }

    #[test]
    fn a_range_boundary_is_inclusive_of_the_hint_position() {
        let exact = Range::new(Position::new(0, 5), Position::new(0, 5));
        assert_eq!(hints_in(exact), vec![((0, 5), " (B)".to_string())]);
        let just_before = Range::new(Position::new(0, 0), Position::new(0, 4));
        assert!(hints_in(just_before).is_empty());
    }
}
