use crate::state::SatzState;
use tower_lsp_server::ls_types::{CodeLens, CodeLensParams, Command, Position, Range};

/// Computes CodeLens entries for a document, displaying incoming backlink count.
pub fn code_lens(params: CodeLensParams, state: &SatzState) -> Option<Vec<CodeLens>> {
    if !state.config.lsp.codelens.enable {
        return None;
    }

    let uri = params.text_document.uri.as_str();
    tracing::debug!(uri, "code_lens");
    let open_doc = state.open_docs.get(uri)?;
    let doc_id = state.doc_id_for_path(&open_doc.path);

    let count = state.index.incoming_from_others(&doc_id).count();
    let title = match count {
        0 => "0 backlinks".to_string(),
        1 => "1 backlink".to_string(),
        n => format!("{} backlinks", n),
    };

    Some(vec![CodeLens {
        range: Range::new(Position::new(0, 0), Position::new(0, 0)),
        command: Some(Command {
            title,
            command: crate::handlers::execute_command::SHOW_BACKLINKS_COMMAND.to_string(),
            arguments: Some(vec![serde_json::json!(uri)]),
        }),
        data: None,
    }])
}

#[cfg(test)]
mod tests;
