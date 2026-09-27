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
    tracing::debug!(uri, "document_symbol");

    let (_, doc) = state.doc_for_uri(uri)?;

    // We will build a flat list for now, or maybe nested.
    // For nested, we can use a stack.
    let mut symbols: Vec<DocumentSymbol> = Vec::new();

    // A stack of (level, DocumentSymbol)
    let mut stack: Vec<(u8, DocumentSymbol)> = Vec::new();

    let source = doc.line_index.source();
    for (i, heading) in doc.headings.iter().enumerate() {
        // The symbol covers the heading's whole SECTION -- up to the next heading of the same or a
        // higher level, without trailing blank lines -- so its children lie inside it (the LSP
        // requires that). The selection is the heading line itself.
        let section_end = doc.headings[i + 1..]
            .iter()
            .find(|next| next.level <= heading.level)
            .map_or(source.len(), |next| next.range.start);
        let first_line_end = source[heading.range.start..]
            .find('\n')
            .map_or(source.len(), |n| heading.range.start + n);
        let heading_line_end =
            heading.range.start + source[heading.range.start..first_line_end].trim_end().len();
        let content_end = source[..section_end].trim_end().len().max(heading_line_end);
        let range = byte_range_to_lsp(
            satz_core::ByteRange::new(heading.range.start, content_end),
            &doc.line_index,
        );
        let selection_range = byte_range_to_lsp(
            satz_core::ByteRange::new(heading.range.start, heading_line_end),
            &doc.line_index,
        );
        let name = match heading.text.trim() {
            "" => "(empty heading)".to_string(),
            text => text.to_string(),
        };

        // The LSP type marks this field `#[deprecated]` but the protocol still requires it.
        #[allow(deprecated)]
        let symbol = DocumentSymbol {
            name,

            detail: None,
            kind: SymbolKind::STRING,
            tags: None,
            deprecated: None,
            range,
            selection_range,
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

#[cfg(test)]
mod tests;
