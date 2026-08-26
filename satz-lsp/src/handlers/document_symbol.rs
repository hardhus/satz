use crate::convert::byte_range_to_lsp;
use crate::state::SatzState;
use tower_lsp_server::ls_types::{
    DocumentSymbol, DocumentSymbolParams, DocumentSymbolResponse, SymbolKind,
};

pub fn document_symbol(
    params: DocumentSymbolParams,
    state: &SatzState,
) -> Option<DocumentSymbolResponse> {
    let uri = params.text_document.uri.as_str();

    let open_doc = state.open_docs.get(uri)?;
    let rel_path = match &state.vault_root {
        Some(root) => open_doc.path.strip_prefix(root).unwrap_or(&open_doc.path),
        None => &open_doc.path,
    };
    let rel_path_str = rel_path.to_string_lossy().replace('\\', "/");
    let doc_id = satz_core::DocId::new(&rel_path_str);
    let doc = state.index.get_doc(&doc_id)?;

    // We will build a flat list for now, or maybe nested.
    // For nested, we can use a stack.
    let mut symbols: Vec<DocumentSymbol> = Vec::new();

    // A stack of (level, DocumentSymbol)
    let mut stack: Vec<(u8, DocumentSymbol)> = Vec::new();

    for heading in &doc.headings {
        let range = byte_range_to_lsp(heading.range, &doc.line_index);

        #[allow(deprecated)]
        let symbol = DocumentSymbol {
            name: heading.text.trim().to_string(),

            detail: None,
            kind: SymbolKind::STRING,
            tags: None,
            deprecated: None,
            range,
            selection_range: range,
            children: Some(Vec::new()),
        };

        // Pop elements from stack that have level >= current heading's level
        while let Some((level, _)) = stack.last() {
            if *level >= heading.level {
                let (_, popped_symbol) = stack.pop().unwrap();
                // Add popped to its parent, or to root if stack is empty
                if let Some((_, parent)) = stack.last_mut() {
                    if let Some(children) = &mut parent.children {
                        children.push(popped_symbol);
                    }
                } else {
                    symbols.push(popped_symbol);
                }
            } else {
                break;
            }
        }

        stack.push((heading.level, symbol));
    }

    // Flush the rest of the stack
    while let Some((_, popped_symbol)) = stack.pop() {
        if let Some((_, parent)) = stack.last_mut() {
            if let Some(children) = &mut parent.children {
                children.push(popped_symbol);
            }
        } else {
            symbols.push(popped_symbol);
        }
    }

    if symbols.is_empty() {
        None
    } else {
        Some(DocumentSymbolResponse::Nested(symbols))
    }
}
