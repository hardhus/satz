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
mod tests;
