#![allow(clippy::collapsible_if)]

use crate::convert::{byte_range_to_lsp, lsp_pos_to_satz, path_to_uri};
use crate::state::SatzState;
use satz_core::LinkKind;
use tower_lsp_server::ls_types::{GotoDefinitionParams, GotoDefinitionResponse, Location, Range};

pub fn goto_definition(
    params: GotoDefinitionParams,
    state: &SatzState,
) -> Option<GotoDefinitionResponse> {
    let uri = params
        .text_document_position_params
        .text_document
        .uri
        .as_str();
    let pos = params.text_document_position_params.position;
    tracing::debug!(uri, ?pos, "goto_definition");

    // Get the current document
    let open_doc = state.open_docs.get(uri)?;
    let rel_path = SatzState::get_rel_path(&open_doc.path, state.vault_root.as_deref());
    let rel_path_str = rel_path.to_string_lossy().replace('\\', "/");
    let doc_id = satz_core::DocId::new(&rel_path_str);
    let doc = state.index.get_doc(&doc_id)?;

    // Convert position to byte offset
    let satz_pos = lsp_pos_to_satz(pos);
    let byte_offset = doc.line_index.position_to_byte(satz_pos);

    // Find the link under the cursor
    let link = doc.link_at(byte_offset)?;

    // Special case for footnotes: jump to definition in the SAME document
    if link.kind == LinkKind::Footnote
        && let Some(label) = &link.display
        && let Some(def) = doc.footnotes.find_def(label)
    {
        let range = byte_range_to_lsp(def.range, &doc.line_index);
        let doc_path = match &state.vault_root {
            Some(root) if !doc.path.is_absolute() => root.join(&doc.path),
            _ => doc.path.clone(),
        };
        let url = path_to_uri(&doc_path)?;
        return Some(GotoDefinitionResponse::Scalar(Location::new(url, range)));
    }

    let resolution =
        state
            .index
            .resolve_link_full_with_config(link, Some(doc), Some(&state.config));
    // Deliberately not logging `resolution` itself: it borrows the full target
    // `Document` and its derived `Debug` would dump the whole parsed document
    // (content, headings, links, ...) on every `gd` call at debug level.
    let outcome = match &resolution {
        satz_core::LinkResolution::Resolved { doc, .. } => format!("Resolved({:?})", doc.id),
        satz_core::LinkResolution::AnchorMissing { doc } => format!("AnchorMissing({:?})", doc.id),
        satz_core::LinkResolution::DocMissing => "DocMissing".to_string(),
    };
    tracing::debug!(
        target_doc = %link.target_doc,
        target_heading = ?link.target_heading,
        outcome,
        "goto_definition: link resolution"
    );

    match resolution {
        satz_core::LinkResolution::Resolved {
            doc: target_doc,
            anchor,
        } => {
            let target_path = match &state.vault_root {
                Some(root) if !target_doc.path.is_absolute() => root.join(&target_doc.path),
                _ => target_doc.path.clone(),
            };
            let target_uri = path_to_uri(&target_path)?;
            let target_range = if let Some(r) = anchor {
                byte_range_to_lsp(r, &target_doc.line_index)
            } else {
                Range {
                    start: tower_lsp_server::ls_types::Position::new(0, 0),
                    end: tower_lsp_server::ls_types::Position::new(0, 0),
                }
            };
            Some(GotoDefinitionResponse::Scalar(Location::new(
                target_uri,
                target_range,
            )))
        }
        satz_core::LinkResolution::AnchorMissing { doc: target_doc } => {
            let target_path = match &state.vault_root {
                Some(root) if !target_doc.path.is_absolute() => root.join(&target_doc.path),
                _ => target_doc.path.clone(),
            };
            let target_uri = path_to_uri(&target_path)?;
            Some(GotoDefinitionResponse::Scalar(Location::new(
                target_uri,
                Range {
                    start: tower_lsp_server::ls_types::Position::new(0, 0),
                    end: tower_lsp_server::ls_types::Position::new(0, 0),
                },
            )))
        }
        satz_core::LinkResolution::DocMissing => None,
    }
}

#[cfg(test)]
#[allow(unused_variables)]
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use super::*;
    use satz_core::{Index, parse_document};
    use std::path::Path;
    use tower_lsp_server::ls_types::{
        GotoDefinitionParams, TextDocumentIdentifier, TextDocumentPositionParams,
    };

    #[test]
    fn test_goto_definition_wikilink() {
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

        // Use relative paths for parse_document, just like `walk.rs` does
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

        let params = GotoDefinitionParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: uri_a_str.parse().unwrap(),
                },
                position: tower_lsp_server::ls_types::Position::new(2, 3), // inside [[doc-b]]
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };

        let def = goto_definition(params, &state).expect("Should return definition");
        if let GotoDefinitionResponse::Scalar(loc) = def {
            assert!(loc.uri.as_str().ends_with("doc-b.md"));
        } else {
            panic!("Expected scalar location");
        }
    }

    #[test]
    fn footnote_definition_found_when_label_case_differs() {
        // pulldown-cmark matches footnote labels case-insensitively: `[^A]` refers to `[^a]:`.
        let abs_a = if cfg!(windows) {
            Path::new("C:\\doc-a.md")
        } else {
            Path::new("/doc-a.md")
        };
        let rel_a = Path::new("doc-a.md");
        let content = "Here is a note[^A].\n\n[^a]: This is the footnote definition.";
        let doc_a = parse_document(content, rel_a);

        let mut state = SatzState::default();
        state.index = Index::build(vec![doc_a]);
        state.vault_root = Some(if cfg!(windows) {
            Path::new("C:\\").to_path_buf()
        } else {
            Path::new("/").to_path_buf()
        });
        let uri = if cfg!(windows) {
            "file:///C:/doc-a.md"
        } else {
            "file:///doc-a.md"
        };
        state.open_docs.insert(
            uri.to_string(),
            crate::state::OpenDocument::new(uri, abs_a.to_path_buf(), content, 1),
        );

        let params = GotoDefinitionParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: uri.parse().unwrap(),
                },
                position: tower_lsp_server::ls_types::Position::new(0, 15), // on [^A]
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };

        let def = goto_definition(params, &state)
            .expect("footnote definition should be found despite the label's case");
        let GotoDefinitionResponse::Scalar(loc) = def else {
            panic!("Expected scalar location");
        };
        assert_eq!(loc.range.start.line, 2);
    }

    #[test]
    fn footnote_definition_uses_absolute_uri() {
        let abs_a = if cfg!(windows) {
            Path::new("C:\\doc-a.md")
        } else {
            Path::new("/doc-a.md")
        };
        let rel_a = Path::new("doc-a.md");
        let content = "Here is a note[^1].\n\n[^1]: This is the footnote definition.";
        let doc_a = parse_document(content, rel_a);

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
            crate::state::OpenDocument::new(uri_a_str, abs_a.to_path_buf(), content, 1),
        );

        let params = GotoDefinitionParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: uri_a_str.parse().unwrap(),
                },
                position: tower_lsp_server::ls_types::Position::new(0, 15), // on [^1]
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };

        let def = goto_definition(params, &state).expect("Footnote definition expected");
        if let GotoDefinitionResponse::Scalar(loc) = def {
            assert!(loc.uri.as_str().ends_with("doc-a.md"));
            assert_eq!(loc.range.start.line, 2);
        } else {
            panic!("Expected scalar location");
        }
    }

    fn definition_file(files: &[(&str, &str)], open: &str, at: (u32, u32)) -> Option<String> {
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
        let uri_of = |rel: &str| {
            crate::convert::path_to_uri(&root.join(rel))
                .unwrap()
                .as_str()
                .to_string()
        };
        let text = files.iter().find(|(p, _)| *p == open).unwrap().1;
        let uri = uri_of(open);
        state.open_docs.insert(
            uri.clone(),
            crate::state::OpenDocument::new(&uri, root.join(open), text, 1),
        );
        let params = GotoDefinitionParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: uri.parse().unwrap(),
                },
                position: tower_lsp_server::ls_types::Position::new(at.0, at.1),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };
        let response = goto_definition(params, &state)?;
        let location = match response {
            GotoDefinitionResponse::Scalar(l) => l,
            GotoDefinitionResponse::Array(mut v) => v.remove(0),
            GotoDefinitionResponse::Link(_) => return None,
        };
        let target = crate::convert::uri_to_path(location.uri.as_str())?;
        Some(
            target
                .strip_prefix(&root)
                .ok()?
                .to_string_lossy()
                .replace('\\', "/"),
        )
    }

    #[test]
    fn a_relative_markdown_link_goes_to_the_note_in_its_own_folder() {
        let files = [
            ("sub/a.md", "See [t](b.md) and [u](../c.md)\n"),
            ("sub/b.md", "# sub b\n"),
            ("b.md", "# root b\n"),
            ("c.md", "# c\n"),
            ("sub/c.md", "# sub c\n"),
        ];
        assert_eq!(
            definition_file(&files, "sub/a.md", (0, 8)),
            Some("sub/b.md".into())
        );
        assert_eq!(
            definition_file(&files, "sub/a.md", (0, 24)),
            Some("c.md".into())
        );
    }

    #[test]
    fn a_wikilink_still_uses_the_vault_wide_rule() {
        let files = [
            ("sub/a.md", "See [[b]]\n"),
            ("sub/b.md", "# sub b\n"),
            ("b.md", "# root b\n"),
        ];
        assert_eq!(
            definition_file(&files, "sub/a.md", (0, 7)),
            Some("b.md".into())
        );
    }

    #[test]
    fn the_innermost_of_nested_links_is_the_one_followed() {
        let files = [
            ("a.md", "[see [[inner]]](outer.md)\n"),
            ("inner.md", "# inner\n"),
            ("outer.md", "# outer\n"),
        ];
        // On the wikilink inside the label.
        assert_eq!(
            definition_file(&files, "a.md", (0, 8)),
            Some("inner.md".into())
        );
        // On the label text and on the destination: the outer link.
        assert_eq!(
            definition_file(&files, "a.md", (0, 2)),
            Some("outer.md".into())
        );
        assert_eq!(
            definition_file(&files, "a.md", (0, 20)),
            Some("outer.md".into())
        );
    }
}
