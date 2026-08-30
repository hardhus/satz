use std::fmt::Write as _;

use unicode_width::UnicodeWidthStr;

use crate::config::TablesConfig;
use crate::model::ByteRange;

/// Column alignment as declared by a GFM table's delimiter row (`:---`, `---:`, `:---:`, `---`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnAlignment {
    None,
    Left,
    Center,
    Right,
}

/// A GFM pipe table, re-derived from raw source text rather than pulldown-cmark's cell AST so
/// that inline markdown/wikilinks inside cells survive verbatim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableBlock {
    pub range: ByteRange,
    pub alignments: Vec<ColumnAlignment>,
    pub header: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

/// Parses the raw source text of a single table block (a byte range already validated as a GFM
/// table by pulldown-cmark's `ENABLE_TABLES` parser) into a `TableBlock`.
///
/// Cell text is extracted by splitting each row on unescaped `|` characters, then trimming
/// surrounding whitespace — the cell content itself (including any inline `**bold**`,
/// `` `code` ``, or `[[wikilink]]` syntax) is never re-parsed or altered. A `|` is only ever
/// treated as literal (not a column separator) when backslash-escaped (`\|`) — this matches
/// pulldown-cmark's own table-cell splitting exactly, which is verified (see `table_cell_probe`
/// investigation in the F2 phase) to split on a raw `|` even inside a backtick code span; a
/// pipe meant to display literally inside inline code in a cell must be escaped, same as GitHub.
pub fn parse_table_block(source: &str, range: ByteRange) -> Option<TableBlock> {
    let text = &source[range.start..range.end];

    let mut lines = Vec::new();
    for raw_line in text.split('\n') {
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        lines.push(line);
    }
    while matches!(lines.last(), Some(l) if l.trim().is_empty()) {
        lines.pop();
    }

    let header_line = *lines.first()?;
    let delim_line = *lines.get(1)?;

    let delim_cells = split_row(delim_line);
    if delim_cells.is_empty() {
        return None;
    }
    let alignments: Vec<ColumnAlignment> = delim_cells.iter().map(|c| parse_alignment(c)).collect();
    let col_count = alignments.len();

    let header = normalize_row(split_row(header_line), col_count);

    let mut rows = Vec::with_capacity(lines.len().saturating_sub(2));
    for &row_line in &lines[2..] {
        if row_line.trim().is_empty() {
            continue;
        }
        rows.push(normalize_row(split_row(row_line), col_count));
    }

    Some(TableBlock {
        range,
        alignments,
        header,
        rows,
    })
}

/// Pads or truncates a row's cells to exactly `col_count`, per the GFM rule that extra cells are
/// dropped and missing cells are treated as empty.
fn normalize_row(mut cells: Vec<String>, col_count: usize) -> Vec<String> {
    cells.resize(col_count, String::new());
    cells
}

fn parse_alignment(delim_cell: &str) -> ColumnAlignment {
    let trimmed = delim_cell.trim();
    let left = trimmed.starts_with(':');
    let right = trimmed.ends_with(':');
    match (left, right) {
        (true, true) => ColumnAlignment::Center,
        (true, false) => ColumnAlignment::Left,
        (false, true) => ColumnAlignment::Right,
        (false, false) => ColumnAlignment::None,
    }
}

/// Splits one raw table row line into trimmed cell strings, on unescaped `|` characters. A
/// single non-escaped leading and/or trailing `|` (GFM's optional row-bracketing pipes) is
/// dropped rather than producing an empty leading/trailing cell.
fn split_row(line: &str) -> Vec<String> {
    let bytes = line.as_bytes();

    let mut boundaries = Vec::new();
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'|' {
            let escaped = i > 0 && bytes[i - 1] == b'\\';
            if !escaped {
                boundaries.push(i);
            }
        }
    }

    let mut cells = Vec::with_capacity(boundaries.len() + 1);
    let mut start = 0;
    for &b in &boundaries {
        cells.push(line[start..b].trim().to_string());
        start = b + 1;
    }
    cells.push(line[start..].trim().to_string());

    if cells.first().is_some_and(|c| c.is_empty()) {
        cells.remove(0);
    }
    if cells.last().is_some_and(|c| c.is_empty()) {
        cells.pop();
    }

    cells
}

/// Renders a `TableBlock` back to GFM pipe-table text, with every column padded to the widest
/// cell (measured with Unicode display width, so emoji/CJK/Turkish content aligns visually) and
/// the delimiter row's alignment markers preserved. Cell text is reproduced verbatim.
pub fn render(table: &TableBlock, config: &TablesConfig) -> String {
    let col_count = table.alignments.len();
    if col_count == 0 {
        return String::new();
    }

    let mut widths = vec![config.min_column_width; col_count];
    for (i, w) in widths.iter_mut().enumerate() {
        if let Some(cell) = table.header.get(i) {
            *w = (*w).max(UnicodeWidthStr::width(cell.as_str()));
        }
        for row in &table.rows {
            if let Some(cell) = row.get(i) {
                *w = (*w).max(UnicodeWidthStr::width(cell.as_str()));
            }
        }
    }

    // Every write_row/write_separator call appends exactly one trailing '\n', including after
    // the last row — this must be kept: the detected table's own byte range always ends right
    // after the last row's newline (verified against pulldown-cmark's ENABLE_TABLES range), so
    // stripping it here would eat the newline that separates the table from whatever follows
    // (e.g. collapsing the required blank line before a following paragraph to nothing).
    let mut out = String::new();
    write_row(&mut out, &table.header, &widths, config.cell_padding);
    write_separator(&mut out, &table.alignments, &widths, config.cell_padding);
    for row in &table.rows {
        write_row(&mut out, row, &widths, config.cell_padding);
    }

    out
}

fn write_row(out: &mut String, cells: &[String], widths: &[usize], padding: usize) {
    out.push('|');
    let empty = String::new();
    for (i, width) in widths.iter().enumerate() {
        let cell = cells.get(i).unwrap_or(&empty);
        let visual = UnicodeWidthStr::width(cell.as_str());
        let fill = width.saturating_sub(visual);
        let _ = write!(
            out,
            "{pad}{cell}{fill}{pad}",
            pad = " ".repeat(padding),
            cell = cell,
            fill = " ".repeat(fill),
        );
        out.push('|');
    }
    out.push('\n');
}

fn write_separator(
    out: &mut String,
    alignments: &[ColumnAlignment],
    widths: &[usize],
    padding: usize,
) {
    out.push('|');
    for (align, width) in alignments.iter().zip(widths.iter()) {
        let (left, right) = match align {
            ColumnAlignment::Left => (":", ""),
            ColumnAlignment::Right => ("", ":"),
            ColumnAlignment::Center => (":", ":"),
            ColumnAlignment::None => ("", ""),
        };
        let total = width + padding * 2;
        let dashes = total.saturating_sub(left.len() + right.len()).max(1);
        let _ = write!(out, "{left}{}{right}", "-".repeat(dashes));
        out.push('|');
    }
    out.push('\n');
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render_source(source: &str, config: &TablesConfig) -> String {
        let structure = crate::parser::structure::parse_structure(source);
        let span = structure.table_spans[0];
        let block = parse_table_block(source, span).unwrap();
        render(&block, config)
    }

    #[test]
    fn test_basic_two_column_table_aligned() {
        let source = "| A | B |\n|---|---|\n| 1 | 22 |\n";
        let config = TablesConfig {
            enable: true,
            cell_padding: 1,
            min_column_width: 1,
        };
        let out = render_source(source, &config);
        assert_eq!(out, "| A | B  |\n|---|----|\n| 1 | 22 |\n");
    }

    #[test]
    fn test_basic_two_column_table_respects_min_column_width_default() {
        // Default min_column_width (3) pads every column out to at least 3 dashes even when
        // the content itself is a single character wide.
        let source = "| A | B |\n|---|---|\n| 1 | 22 |\n";
        let config = TablesConfig::default();
        let out = render_source(source, &config);
        assert_eq!(out, "| A   | B   |\n|-----|-----|\n| 1   | 22  |\n");
    }

    #[test]
    fn test_alignment_variants_preserved() {
        let source = "| L | C | R |\n|:---|:---:|---:|\n| a | b | c |\n";
        let config = TablesConfig {
            enable: true,
            cell_padding: 1,
            min_column_width: 1,
        };
        let out = render_source(source, &config);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[1], "|:--|:-:|--:|");
    }

    #[test]
    fn test_unicode_width_aligns_turkish_and_emoji() {
        // "İğüşçö" (6 Turkish letters, display width 6) and "🦀" (1 emoji, display width 2) have
        // byte lengths that don't match their display width; column padding must be computed
        // from UnicodeWidthStr::width, not chars().count() or byte len(), or these would
        // misalign.
        assert_eq!(UnicodeWidthStr::width("İğüşçö"), 6);
        assert_eq!(UnicodeWidthStr::width("🦀"), 2);

        let source = "| Ad | Not |\n|---|---|\n| İğüşçö | 🦀 |\n| x | yy |\n";
        let config = TablesConfig {
            enable: true,
            cell_padding: 1,
            min_column_width: 1,
        };
        let out = render_source(source, &config);
        let lines: Vec<&str> = out.lines().collect();

        // Column 1 width is driven by "İğüşçö" (display width 6, the widest cell in that
        // column). Column 2 width is driven by the header "Not" (display width 3), one wider
        // than both "🦀" and "yy" (display width 2 each) — each needs exactly one fill space.
        assert_eq!(lines[0], "| Ad     | Not |");
        assert_eq!(lines[2], "| İğüşçö | 🦀  |");
        assert_eq!(lines[3], "| x      | yy  |");
    }

    #[test]
    fn test_escaped_pipe_inside_inline_code_keeps_cell_together() {
        // A literal pipe inside a cell (even inside backtick inline code) must be
        // backslash-escaped to survive as part of one cell — see the unescaped-pipe test below
        // for why: pulldown-cmark's own table-cell splitter does not special-case code spans.
        let source = "| Expr | Result |\n|---|---|\n| `a\\|b` | ok |\n";
        let structure = crate::parser::structure::parse_structure(source);
        let span = structure.table_spans[0];
        let block = parse_table_block(source, span).unwrap();
        assert_eq!(block.rows[0].len(), 2);
        assert_eq!(block.rows[0][0], "`a\\|b`");
        assert_eq!(block.rows[0][1], "ok");
    }

    #[test]
    fn test_unescaped_pipe_inside_inline_code_still_splits_column() {
        // Verified against pulldown-cmark's own event stream: a table row is split into cells
        // on every raw, unescaped `|`, even one inside a backtick code span — the parser does
        // not treat inline code as a protected zone at the table-row-splitting stage (this
        // matches GitHub's own GFM table behavior too). So this is intentionally NOT a bug: an
        // unescaped `|` inside `` `code` `` in a cell splits into two cells, same as upstream.
        // The row now has 3 raw cells against a 2-column header, so per GFM's "extra cells are
        // dropped" rule the trailing "ok" cell is truncated away.
        let source = "| Expr | Result |\n|---|---|\n| `a|b` | ok |\n";
        let structure = crate::parser::structure::parse_structure(source);
        let span = structure.table_spans[0];
        let block = parse_table_block(source, span).unwrap();
        assert_eq!(block.rows[0], vec!["`a", "b`"]);
    }

    #[test]
    fn test_render_is_idempotent() {
        let source = "|Name|Value|\n|:--|--:|\n|a|1|\n|bb|22|\n";
        let config = TablesConfig::default();
        let pass1 = render_source(source, &config);
        let pass2 = render_source(&pass1, &config);
        assert_eq!(pass1, pass2);
    }
}
