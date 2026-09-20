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
// Test states are built field by field so each test shows exactly what it sets up.
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use super::*;
    use satz_core::{Index, parse_document};
    use std::path::Path;
    use tower_lsp_server::ls_types::TextDocumentIdentifier;

    fn symbols(text: &str) -> Option<Vec<DocumentSymbol>> {
        let rel = Path::new("a.md");
        let mut state = SatzState::default();
        state.index = Index::build(vec![parse_document(text, rel)]);
        state.open_docs.insert(
            "file:///a.md".to_string(),
            crate::state::OpenDocument::new("file:///a.md", rel.to_path_buf(), text, 1),
        );
        let params = DocumentSymbolParams {
            text_document: TextDocumentIdentifier {
                uri: "file:///a.md".parse().unwrap(),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };
        match document_symbol(params, &state)? {
            DocumentSymbolResponse::Nested(s) => Some(s),
            DocumentSymbolResponse::Flat(_) => panic!("nested symbols expected"),
        }
    }

    fn lines(s: &DocumentSymbol) -> (u32, u32) {
        (s.range.start.line, s.range.end.line)
    }

    fn contains(outer: &DocumentSymbol, inner: &DocumentSymbol) -> bool {
        let a = outer.range;
        let b = inner.range;
        (a.start.line, a.start.character) <= (b.start.line, b.start.character)
            && (a.end.line, a.end.character) >= (b.end.line, b.end.character)
    }

    fn check_tree(symbol: &DocumentSymbol) {
        let sel = symbol.selection_range;
        assert!(
            contains(
                symbol,
                &DocumentSymbol {
                    range: sel,
                    ..symbol.clone()
                }
            ),
            "selection_range must lie inside range for {}",
            symbol.name
        );
        for child in symbol.children.as_deref().unwrap_or_default() {
            assert!(
                contains(symbol, child),
                "{} must contain {}",
                symbol.name,
                child.name
            );
            check_tree(child);
        }
    }

    #[test]
    fn a_heading_symbol_spans_its_whole_section_including_its_children() {
        let text = "# One\n\ntext\n\n## Sub\n\nmore\n\n# Two\n\nend\n";
        let tree = symbols(text).unwrap();
        assert_eq!(tree.len(), 2);
        assert_eq!(tree[0].name, "One");
        assert_eq!(
            lines(&tree[0]),
            (0, 6),
            "up to the last content line before `# Two`"
        );
        assert_eq!(tree[0].children.as_ref().unwrap().len(), 1);
        let sub = &tree[0].children.as_ref().unwrap()[0];
        assert_eq!((sub.name.as_str(), lines(sub)), ("Sub", (4, 6)));
        assert_eq!(lines(&tree[1]), (8, 10));
        // The selection is the heading line only.
        assert_eq!(tree[0].selection_range.start.line, 0);
        assert_eq!(tree[0].selection_range.end.line, 0);
        for symbol in &tree {
            check_tree(symbol);
        }
    }

    #[test]
    fn the_last_section_ends_at_the_last_content_line() {
        let tree = symbols("# A\n\ntext\n\n\n").unwrap();
        assert_eq!(lines(&tree[0]), (0, 2));
        let tree = symbols("# A").unwrap();
        assert_eq!(lines(&tree[0]), (0, 0));
    }

    #[test]
    fn skipped_levels_and_repeated_levels_nest_correctly() {
        let tree = symbols("# A\n\n### Deep\n\n## Mid\n\n# B\n").unwrap();
        assert_eq!(tree.len(), 2);
        let names: Vec<&str> = tree[0]
            .children
            .as_ref()
            .unwrap()
            .iter()
            .map(|c| c.name.as_str())
            .collect();
        assert_eq!(names, vec!["Deep", "Mid"]);
        for symbol in &tree {
            check_tree(symbol);
        }
    }

    #[test]
    fn a_heading_without_text_still_gets_a_name() {
        let tree = symbols("#\n\ntext\n\n## \n").unwrap();
        assert_eq!(tree[0].name, "(empty heading)");
        assert!(!tree[0].children.as_ref().unwrap()[0].name.is_empty());
    }

    #[test]
    fn setext_crlf_block_id_and_unicode_headings() {
        let tree = symbols("Title ^blk\r\n=====\r\n\r\ntext\r\n\r\n## Günün 🦀 Özeti\r\n").unwrap();
        assert_eq!(tree[0].name, "Title");
        assert_eq!(tree[0].selection_range.start.line, 0);
        let child = &tree[0].children.as_ref().unwrap()[0];
        assert_eq!(child.name, "Günün 🦀 Özeti");
        assert_eq!(child.range.start.line, 5);
        check_tree(&tree[0]);
    }

    #[test]
    fn a_document_without_headings_has_no_symbols() {
        assert!(symbols("just text\n\nmore\n").is_none());
        assert!(symbols("").is_none());
    }
}
