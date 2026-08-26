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
    let rel_path = match &state.vault_root {
        Some(root) => open_doc.path.strip_prefix(root).unwrap_or(&open_doc.path),
        None => &open_doc.path,
    };
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

    // Resolve target document
    let target_id = if link.target_doc.is_empty() {
        &doc_id
    } else {
        state.index.resolve_link(&link.target_doc)?
    };

    let target_doc = state.index.get_doc(target_id)?;
    let target_path = match &state.vault_root {
        Some(root) if !target_doc.path.is_absolute() => root.join(&target_doc.path),
        _ => target_doc.path.clone(),
    };
    let target_uri = path_to_uri(&target_path)?;

    let mut target_range = Range {
        start: tower_lsp_server::ls_types::Position::new(0, 0),
        end: tower_lsp_server::ls_types::Position::new(0, 0),
    };

    // If there's a heading target, find its range
    if let Some(heading_slug) = &link.target_heading {
        if let Some(h) = target_doc
            .headings
            .iter()
            .find(|h| h.slug == *heading_slug || h.text.eq_ignore_ascii_case(heading_slug))
        {
            target_range = byte_range_to_lsp(h.range, &target_doc.line_index);
        }
    } else if let Some(block_id) = &link.target_block {
        if let Some(b) = target_doc.blocks.iter().find(|b| b.id == *block_id) {
            target_range = byte_range_to_lsp(b.range, &target_doc.line_index);
        }
    }

    Some(GotoDefinitionResponse::Scalar(Location::new(
        target_uri,
        target_range,
    )))
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
            crate::state::OpenDocument {
                uri: uri_a_str.to_string(),
                path: abs_a.to_path_buf(),
                content: "# Doc A\n\n[[doc-b]]".to_string(),
                version: 1,
            },
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
