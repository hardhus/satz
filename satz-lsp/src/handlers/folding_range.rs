use crate::state::SatzState;
use tower_lsp_server::ls_types::{FoldingRange, FoldingRangeKind, FoldingRangeParams};

/// Computes folding ranges for headers and frontmatter in a document.
pub fn folding_range(params: FoldingRangeParams, state: &SatzState) -> Option<Vec<FoldingRange>> {
    let uri = params.text_document.uri.as_str();

    let open_doc = state.open_docs.get(uri)?;
    let rel_path = match &state.vault_root {
        Some(root) => open_doc.path.strip_prefix(root).unwrap_or(&open_doc.path),
        None => &open_doc.path,
    };
    let rel_path_str = rel_path.to_string_lossy().replace('\\', "/");
    let doc_id = satz_core::DocId::new(&rel_path_str);
    let doc = state.index.get_doc(&doc_id)?;

    let mut ranges: Vec<FoldingRange> = Vec::new();
    let total_lines = doc.line_index.line_count() as u32;

    // 1. Frontmatter folding range
    let source = doc.line_index.source();
    let rest = source
        .strip_prefix("---\n")
        .or_else(|| source.strip_prefix("---\r\n"));

    if let Some(rest) = rest
        && let Some(pos) = rest.find("\n---")
    {
        let closing_offset = 4 + pos + 4; // up to closing '---'
        let end_line = doc.line_index.byte_to_position(closing_offset).line;
        if end_line > 0 {
            ranges.push(FoldingRange {
                start_line: 0,
                start_character: None,
                end_line,
                end_character: None,
                kind: Some(FoldingRangeKind::Region),
                collapsed_text: None,
            });
        }
    }

    // 2. Heading folding ranges
    for (i, heading) in doc.headings.iter().enumerate() {
        let start_line = doc.line_index.byte_to_position(heading.range.start).line;

        // Find the next heading with level <= current heading level
        let end_line = doc.headings[i + 1..]
            .iter()
            .find(|next| next.level <= heading.level)
            .map(|next| {
                doc.line_index
                    .byte_to_position(next.range.start)
                    .line
                    .saturating_sub(1)
            })
            .unwrap_or(total_lines.saturating_sub(1));

        if end_line > start_line {
            ranges.push(FoldingRange {
                start_line,
                start_character: None,
                end_line,
                end_character: None,
                kind: Some(FoldingRangeKind::Region),
                collapsed_text: None,
            });
        }
    }

    if ranges.is_empty() {
        None
    } else {
        Some(ranges)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use satz_core::{Index, parse_document};
    use std::path::Path;
    use tower_lsp_server::ls_types::TextDocumentIdentifier;

    #[test]
    fn test_heading_folding_ranges() {
        let text = r#"# Section 1
Content under section 1
More content

## Subsection 1.1
Sub content

# Section 2
Section 2 content
"#;
        let rel_path = Path::new("test.md");
        let doc = parse_document(text, rel_path);

        let mut state = SatzState {
            index: Index::build(vec![doc]),
            vault_root: Some(Path::new("").to_path_buf()),
            ..Default::default()
        };

        let uri_str = "file:///test.md";
        state.open_docs.insert(
            uri_str.to_string(),
            crate::state::OpenDocument::new(uri_str, rel_path.to_path_buf(), text, 1),
        );

        let params = FoldingRangeParams {
            text_document: TextDocumentIdentifier {
                uri: uri_str.parse().unwrap(),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };

        let result = folding_range(params, &state).expect("Folding ranges expected");
        assert_eq!(result.len(), 3);

        // Section 1 (lines 0 to 6)
        assert_eq!(result[0].start_line, 0);
        assert_eq!(result[0].end_line, 6);

        // Subsection 1.1 (lines 4 to 6)
        assert_eq!(result[1].start_line, 4);
        assert_eq!(result[1].end_line, 6);

        // Section 2 (lines 7 to 9)
        assert_eq!(result[2].start_line, 7);
        assert_eq!(result[2].end_line, 9);
    }

    #[test]
    fn test_frontmatter_folding_range() {
        let text = r#"---
title: Note With Frontmatter
tags: [test]
---
# Main Heading
Content here
"#;
        let rel_path = Path::new("fm.md");
        let doc = parse_document(text, rel_path);

        let mut state = SatzState {
            index: Index::build(vec![doc]),
            vault_root: Some(Path::new("").to_path_buf()),
            ..Default::default()
        };

        let uri_str = "file:///fm.md";
        state.open_docs.insert(
            uri_str.to_string(),
            crate::state::OpenDocument::new(uri_str, rel_path.to_path_buf(), text, 1),
        );

        let params = FoldingRangeParams {
            text_document: TextDocumentIdentifier {
                uri: uri_str.parse().unwrap(),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };

        let result = folding_range(params, &state).expect("Folding ranges expected");
        assert_eq!(result.len(), 2);

        // Frontmatter (line 0 to 3)
        assert_eq!(result[0].start_line, 0);
        assert_eq!(result[0].end_line, 3);

        // Heading (line 4 to 6)
        assert_eq!(result[1].start_line, 4);
        assert_eq!(result[1].end_line, 6);
    }
}
