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
    let link = doc.links.iter().find(|l| l.range.contains(byte_offset))?;

    // Special case for footnotes: jump to definition in the SAME document
    if link.kind == LinkKind::Footnote
        && let Some(label) = &link.display
        && let Some(def) = doc.footnotes.definitions.iter().find(|d| d.label == *label)
    {
        let range = byte_range_to_lsp(def.range, &doc.line_index);
        let url = path_to_uri(&doc.path)?;
        return Some(GotoDefinitionResponse::Scalar(Location::new(url, range)));
    }

    match state.index.resolve_link_full(link, Some(doc)) {
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
}
