use crate::convert::line_edits_to_text_edits;
use crate::state::SatzState;
use satz_core::formatter::diff::line_diff;
use tower_lsp_server::ls_types::{DocumentFormattingParams, TextEdit};

/// Formats a document according to the vault's FormatterConfig, returning minimal line-range
/// `TextEdit`s (via a line-based diff) rather than one edit replacing the whole document — this
/// keeps the editor's undo history and the LSP payload proportional to what actually changed.
pub fn formatting(params: DocumentFormattingParams, state: &SatzState) -> Option<Vec<TextEdit>> {
    if !state.formatting_allowed() {
        return Some(vec![]);
    }

    let uri = params.text_document.uri.as_str();
    tracing::debug!(uri, "formatting");
    let open_doc = state.open_docs.get(uri)?;
    let original = open_doc.rope.to_string();

    let formatted = satz_core::formatter::format_document(&original, &state.config.formatter);

    if formatted == original {
        return Some(vec![]);
    }

    let line_index = satz_core::LineIndex::new(&original);
    let edits = line_diff(&original, &formatted);
    Some(line_edits_to_text_edits(&line_index, &edits))
}

#[cfg(test)]
mod tests;
