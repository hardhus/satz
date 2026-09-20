use crate::state::SatzState;
use tower_lsp_server::ls_types::{FoldingRange, FoldingRangeKind, FoldingRangeParams};

/// Computes folding ranges for headers and frontmatter in a document.
pub fn folding_range(params: FoldingRangeParams, state: &SatzState) -> Option<Vec<FoldingRange>> {
    let uri = params.text_document.uri.as_str();
    tracing::debug!(uri, "folding_range");

    let (_, doc) = state.doc_for_uri(uri)?;

    let mut ranges: Vec<FoldingRange> = Vec::new();
    let source = doc.line_index.source();
    let line_of = |byte: usize| doc.line_index.byte_to_position(byte).line;
    // The last line that holds anything but whitespace (no phantom line after the final newline).
    let last_content_line = line_of(source.trim_end().len().saturating_sub(1));

    let fold = |start_line: u32, end_line: u32| FoldingRange {
        start_line,
        start_character: None,
        end_line,
        end_character: None,
        kind: Some(FoldingRangeKind::Region),
        collapsed_text: None,
    };

    // 1. Frontmatter: exactly the block the parser found (not anything that merely looks like one).
    if let Some(block) = doc.frontmatter_range {
        let block_end = source[..block.end.min(source.len())].trim_end().len();
        let end_line = line_of(block_end.saturating_sub(1));
        let start_line = line_of(block.start);
        if end_line > start_line {
            ranges.push(fold(start_line, end_line));
        }
    }

    // 2. Heading sections, in one pass: a section ends on the line before the next heading of the
    // same or a higher level (or on the last content line of the document).
    let mut end_lines: Vec<u32> = vec![last_content_line; doc.headings.len()];
    let mut open: Vec<usize> = Vec::new(); // indices of headings whose section is still open
    for (i, heading) in doc.headings.iter().enumerate() {
        let start_line = line_of(heading.range.start);
        while let Some(&top) = open.last() {
            if doc.headings[top].level < heading.level {
                break;
            }
            end_lines[top] = start_line.saturating_sub(1);
            open.pop();
        }
        open.push(i);
    }
    for (heading, end_line) in doc.headings.iter().zip(end_lines) {
        let start_line = line_of(heading.range.start);
        if end_line > start_line {
            ranges.push(fold(start_line, end_line));
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

        // Section 2 (lines 7 to 8: the last content line; the newline that ends the file does not
        // start a line of its own)
        assert_eq!(result[2].start_line, 7);
        assert_eq!(result[2].end_line, 8);
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

        // Heading (line 4 to 5: the last content line, not the empty line after the final newline)
        assert_eq!(result[1].start_line, 4);
        assert_eq!(result[1].end_line, 5);
    }

    fn folds(text: &str) -> Vec<(u32, u32)> {
        let rel_path = Path::new("f.md");
        let mut state = SatzState {
            index: Index::build(vec![parse_document(text, rel_path)]),
            vault_root: Some(Path::new("").to_path_buf()),
            ..Default::default()
        };
        state.open_docs.insert(
            "file:///f.md".to_string(),
            crate::state::OpenDocument::new("file:///f.md", rel_path.to_path_buf(), text, 1),
        );
        let params = FoldingRangeParams {
            text_document: TextDocumentIdentifier {
                uri: "file:///f.md".parse().unwrap(),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };
        folding_range(params, &state)
            .unwrap_or_default()
            .into_iter()
            .map(|r| (r.start_line, r.end_line))
            .collect()
    }

    #[test]
    fn a_fence_that_is_not_the_frontmatter_closing_folds_nothing() {
        // `----` and `---text` are not the closing fence, so this is not frontmatter at all.
        assert!(folds("---\ntitle: x\n\n----\ntext\n").is_empty());
        assert!(folds("---\ntitle: x\n---text\nmore\n").is_empty());
        // Never closed.
        assert!(folds("---\ntitle: x\nno closing fence\n").is_empty());
    }

    #[test]
    fn real_frontmatter_folds_exactly_its_block() {
        assert_eq!(folds("---\ntitle: x\ntags: [a]\n---\ntext\n"), vec![(0, 3)]);
        assert_eq!(folds("---\r\ntitle: x\r\n---\r\ntext\r\n"), vec![(0, 2)]);
        // A one-line block has nothing to fold; body text after it is not part of it.
        assert_eq!(folds("---\n---\ntext\n"), vec![]);
    }

    #[test]
    fn the_last_section_stops_at_the_last_real_line() {
        // The final newline does not start another line worth folding.
        assert_eq!(folds("# A\n\ntext\n"), vec![(0, 2)]);
        assert_eq!(folds("# A\n\ntext"), vec![(0, 2)]);
        assert_eq!(folds("# A\r\n\r\ntext\r\n"), vec![(0, 2)]);
        assert_eq!(folds("# A\n\ntext\n\n\n"), vec![(0, 2)]);
    }

    #[test]
    fn nested_sections_and_lone_headings() {
        assert_eq!(
            folds("# A\n\n## B\n\nb\n\n## C\n\nc\n\n# D\n\nd\n"),
            vec![(0, 9), (2, 5), (6, 9), (10, 12)]
        );
        // Headings with nothing under them fold nothing.
        assert!(folds("# A\n# B\n# C\n").is_empty());
        assert!(folds("plain text only\n").is_empty());
    }

    #[test]
    fn a_document_with_thousands_of_headings_is_folded_quickly() {
        let mut text = String::from("# Top\n\n");
        for i in 0..4000 {
            text.push_str(&format!("## Section {i}\n\ntext\n\n"));
        }
        let start = std::time::Instant::now();
        let found = folds(&text);
        assert_eq!(found.len(), 4001);
        assert!(
            start.elapsed() < std::time::Duration::from_secs(5),
            "{:?}",
            start.elapsed()
        );
    }
}
