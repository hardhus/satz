use satz_core::{Document, Frontmatter, Index, LinkKind, VaultConfig};
use tower_lsp_server::ls_types as lsp;

use crate::convert::byte_range_to_lsp;
use crate::state::SatzState;

/// Computes the pull-mode `textDocument/diagnostic` report for a single open document.
///
/// Returns an empty result while the vault's initial `walk_vault` scan is still running
/// (`!state.indexing_complete`): at that point the index may only contain whichever documents
/// happened to already be open, so any link to a not-yet-indexed peer would be reported as
/// spuriously broken. The `workspace/diagnostic/refresh` push sent once indexing finishes makes
/// the client re-pull the real results.
pub fn pull_document_diagnostics(uri: &str, state: &SatzState) -> Vec<lsp::Diagnostic> {
    match pull_document_report(uri, None, state) {
        DocumentPull::Full { items, .. } => items,
        DocumentPull::Unchanged { .. } => Vec::new(),
    }
}

/// What a `textDocument/diagnostic` pull answers.
#[derive(Debug, Clone, PartialEq)]
pub enum DocumentPull {
    /// The diagnostics, tagged with the id a later pull can send back (`None`: an incomplete answer
    /// that must not be reused).
    Full {
        items: Vec<lsp::Diagnostic>,
        result_id: Option<String>,
    },
    /// The client's `previous_result_id` is still current: nothing was computed.
    Unchanged { result_id: String },
}

/// The id of the state diagnostics are computed from. A diagnostic depends on every note (a link is
/// broken because another note is gone), so ANY change to the index or the configuration gives a new
/// id -- coarse, but never wrong.
pub fn diagnostics_result_id(state: &SatzState) -> String {
    format!("{}.{}", state.index.revision(), state.config_revision)
}

/// Computes the pull-mode `textDocument/diagnostic` report for a single open document, or says it
/// is unchanged when `previous_result_id` is still current.
///
/// Empty (and without an id) while the vault's initial scan is still running
/// (`!state.indexing_complete`): at that point the index may only contain whichever documents
/// happened to already be open, so any link to a not-yet-indexed peer would be reported as
/// spuriously broken. The `workspace/diagnostic/refresh` sent once indexing finishes makes the
/// client re-pull the real results.
pub fn pull_document_report(
    uri: &str,
    previous_result_id: Option<&str>,
    state: &SatzState,
) -> DocumentPull {
    if !state.is_indexing_complete() {
        tracing::debug!(
            uri,
            "pull_document_report: initial indexing not complete yet"
        );
        return DocumentPull::Full {
            items: Vec::new(),
            result_id: None,
        };
    }
    let result_id = diagnostics_result_id(state);
    if previous_result_id == Some(result_id.as_str()) {
        return DocumentPull::Unchanged { result_id };
    }
    let items = match state.doc_for_uri(uri) {
        Some((_, doc)) => compute_diagnostics(doc, &state.index, &state.config),
        None => Vec::new(),
    };
    DocumentPull::Full {
        items,
        result_id: Some(result_id),
    }
}

/// The pull-mode `workspace/diagnostic` report across every indexed document (no previous ids).
pub fn pull_workspace_diagnostics(
    state: &SatzState,
) -> Vec<lsp::WorkspaceDocumentDiagnosticReport> {
    pull_workspace_report(&std::collections::HashMap::new(), state)
}

/// The `workspace/diagnostic` report: a note whose entry in `previous` (uri -> result id) is still
/// current is `Unchanged` and costs nothing; every other note gets its full diagnostics and the id
/// to send back next time. Same `indexing_complete` gating as [`pull_document_report`]: a
/// workspace-wide scan taken mid-scan would only cover a fraction of the vault.
pub fn pull_workspace_report(
    previous: &std::collections::HashMap<String, String>,
    state: &SatzState,
) -> Vec<lsp::WorkspaceDocumentDiagnosticReport> {
    if !state.is_indexing_complete() {
        tracing::debug!("pull_workspace_report: initial indexing not complete yet");
        return Vec::new();
    }

    let result_id = diagnostics_result_id(state);
    let mut items = Vec::new();
    for doc in state.index.documents() {
        let Some(uri) = state.doc_uri(doc) else {
            continue;
        };
        let version = state
            .open_docs
            .get(uri.as_str())
            .map(|od| od.version as i64);
        if previous.get(uri.as_str()) == Some(&result_id) {
            items.push(lsp::WorkspaceDocumentDiagnosticReport::Unchanged(
                lsp::WorkspaceUnchangedDocumentDiagnosticReport {
                    uri,
                    version,
                    unchanged_document_diagnostic_report: lsp::UnchangedDocumentDiagnosticReport {
                        result_id: result_id.clone(),
                    },
                },
            ));
            continue;
        }
        let diagnostics = compute_diagnostics(doc, &state.index, &state.config);
        items.push(lsp::WorkspaceDocumentDiagnosticReport::Full(
            lsp::WorkspaceFullDocumentDiagnosticReport {
                uri,
                version,
                full_document_diagnostic_report: lsp::FullDocumentDiagnosticReport {
                    result_id: Some(result_id.clone()),
                    items: diagnostics,
                },
            },
        ));
    }
    items
}

#[cfg(test)]
thread_local! {
    /// How many times diagnostics were computed on this thread (tests assert that answering
    /// "unchanged" computes nothing).
    static COMPUTE_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn compute_calls() -> usize {
    COMPUTE_CALLS.with(|c| c.get())
}

/// Whether a fragment that is not a heading is still a real anchor: `#top` (every viewer knows it)
/// or an HTML `id`/`name` that the note itself defines (`<a id="x">`, `<h2 id='x'>`).
fn names_a_non_heading_anchor(target: &Document, fragment: Option<&str>) -> bool {
    let Some(fragment) = fragment.map(str::trim).filter(|f| !f.is_empty()) else {
        return false;
    };
    if fragment.eq_ignore_ascii_case("top") {
        return true;
    }
    let source = target.line_index.source();
    ["id", "name"].iter().any(|attribute| {
        ['"', '\'']
            .iter()
            .any(|quote| source.contains(&format!("{attribute}={quote}{fragment}{quote}")))
    })
}

/// What the resolution of `link` comes to for what the user is TOLD about it (a diagnostic, a
/// colour, a hint): a Markdown link whose fragment is no heading but a real anchor (`[t](#top)`,
/// `[t](note.md#custom-id)`: a viewer's own anchor, or an HTML id the note defines itself) is not a
/// broken reference, so it counts as resolved. What the link IS (where go-to-definition leads,
/// what hover shows, what a rename touches) is still the plain resolution.
pub(crate) fn as_the_user_sees_it<'a>(
    link: &satz_core::Link,
    resolution: satz_core::LinkResolution<'a>,
) -> satz_core::LinkResolution<'a> {
    match resolution {
        satz_core::LinkResolution::AnchorMissing { doc }
            if link.kind == LinkKind::Markdown
                && names_a_non_heading_anchor(doc, link.target_heading.as_deref()) =>
        {
            satz_core::LinkResolution::Resolved { doc, anchor: None }
        }
        other => other,
    }
}

/// Whether a frontmatter block has a line that starts like a YAML key (`title:`, `my-key: value`).
fn looks_like_yaml_keys(block: &str) -> bool {
    block.lines().any(|line| {
        let line = line.trim_start();
        line.split_once(':').is_some_and(|(key, _)| {
            !key.is_empty()
                && key
                    .chars()
                    .all(|c| c.is_alphanumeric() || matches!(c, '_' | '-'))
        })
    })
}

/// Computes all language server diagnostics for a single document.
pub fn compute_diagnostics(
    doc: &Document,
    index: &Index,
    config: &VaultConfig,
) -> Vec<lsp::Diagnostic> {
    #[cfg(test)]
    COMPUTE_CALLS.with(|c| c.set(c.get() + 1));
    tracing::trace!(doc_id = ?doc.id, "compute_diagnostics");
    let mut diagnostics = Vec::new();

    // 1. Link diagnostics (broken wikilinks, broken heading references, broken internal markdown links)
    for link in &doc.links {
        match link.kind {
            LinkKind::WikiLink | LinkKind::Embed | LinkKind::Markdown => {
                if satz_core::model::link::is_external_target(&link.target_doc) {
                    continue;
                }
                let range = byte_range_to_lsp(link.range, &doc.line_index);
                match as_the_user_sees_it(
                    link,
                    index.resolve_link_full_with_config(link, Some(doc), Some(config)),
                ) {
                    satz_core::LinkResolution::DocMissing => {
                        let code = if link.kind == LinkKind::Embed {
                            "broken-embed"
                        } else {
                            "broken-link"
                        };
                        let message = if link.kind == LinkKind::Embed {
                            format!("Broken embed: '{}' could not be resolved", link.target_doc)
                        } else {
                            format!("Broken link: '{}' could not be resolved", link.target_doc)
                        };
                        diagnostics.push(lsp::Diagnostic {
                            range,
                            severity: Some(lsp::DiagnosticSeverity::WARNING),
                            code: Some(lsp::NumberOrString::String(code.to_string())),
                            source: Some("satz".to_string()),
                            message,
                            ..Default::default()
                        });
                    }
                    satz_core::LinkResolution::AnchorMissing { .. } => {
                        let message = if let Some(h) = &link.target_heading {
                            if link.target_doc.is_empty() {
                                format!(
                                    "Broken heading reference: no heading '{}' in current document",
                                    h
                                )
                            } else {
                                format!(
                                    "Broken heading reference: '{}' has no heading '{}'",
                                    link.target_doc, h
                                )
                            }
                        } else if let Some(b) = &link.target_block {
                            if link.target_doc.is_empty() {
                                format!(
                                    "Broken block reference: no block '^{}' in current document",
                                    b
                                )
                            } else {
                                format!(
                                    "Broken block reference: '{}' has no block '^{}'",
                                    link.target_doc, b
                                )
                            }
                        } else {
                            "Broken reference".to_string()
                        };
                        diagnostics.push(lsp::Diagnostic {
                            range,
                            severity: Some(lsp::DiagnosticSeverity::WARNING),
                            code: Some(lsp::NumberOrString::String("broken-heading".to_string())),
                            source: Some("satz".to_string()),
                            message,
                            ..Default::default()
                        });
                    }
                    satz_core::LinkResolution::Resolved { .. } => {}
                }
            }
            // Pulldown-cmark only ever emits a `LinkKind::Footnote` here for a `[^label]`
            // reference that already has a matching definition -- an undefined one is left as
            // plain text with no event at all, so it can't be caught in this loop. See the
            // `doc.broken_footnote_refs` loop below instead (populated by a manual text scan).
            LinkKind::Footnote => {}
        }
    }

    // 1b. Broken footnote references, found by a manual text scan independent of the
    // structural parser (see `doc.broken_footnote_refs`'s doc comment for why).
    for link in &doc.broken_footnote_refs {
        let range = byte_range_to_lsp(link.range, &doc.line_index);
        diagnostics.push(lsp::Diagnostic {
            range,
            severity: Some(lsp::DiagnosticSeverity::WARNING),
            code: Some(lsp::NumberOrString::String("broken-footnote".to_string())),
            source: Some("satz".to_string()),
            message: format!(
                "Broken footnote reference: '[^{}]' has no matching definition",
                link.display.as_deref().unwrap_or("")
            ),
            ..Default::default()
        });
    }

    // 1b. A frontmatter block that cannot be read: its title, aliases and tags are ignored, which
    // would otherwise be invisible.
    // A pair of horizontal rules around ordinary text (`---`, words, `---`) is read as a block too,
    // but nobody meant it as frontmatter: only a block with a `key:` line is worth a warning.
    if let (Some(error), Some(range)) = (&doc.frontmatter_error, doc.frontmatter_range)
        && looks_like_yaml_keys(&doc.line_index.source()[range.start..range.end])
    {
        diagnostics.push(lsp::Diagnostic {
            range: byte_range_to_lsp(range, &doc.line_index),
            severity: Some(lsp::DiagnosticSeverity::WARNING),
            code: Some(lsp::NumberOrString::String(
                "invalid-frontmatter".to_string(),
            )),
            source: Some("satz".to_string()),
            message: format!("Invalid frontmatter: {error}; title, aliases and tags are ignored"),
            ..Default::default()
        });
    }

    // 2. Missing required frontmatter fields
    for required_field in &config.frontmatter.required_fields {
        if is_missing_frontmatter_field(&doc.frontmatter, required_field) {
            diagnostics.push(make_missing_field_diagnostic(required_field));
        }
    }

    // 3. Duplicate heading slugs
    let mut seen_slugs = std::collections::HashSet::new();
    for heading in &doc.headings {
        // A heading with no slug (`# 🙂`, `# !!!`, an empty one) cannot be linked to by name, so
        // two of them are not "ambiguous targets".
        if !heading.slug.is_empty() && !seen_slugs.insert(&heading.slug) {
            let range = byte_range_to_lsp(heading.range, &doc.line_index);
            diagnostics.push(lsp::Diagnostic {
                range,
                severity: Some(lsp::DiagnosticSeverity::WARNING),
                code: Some(lsp::NumberOrString::String("duplicate-heading".to_string())),
                source: Some("satz".to_string()),
                message: format!(
                    "Duplicate heading slug: '{}' (reference targets may be ambiguous)",
                    heading.slug
                ),
                ..Default::default()
            });
        }
    }

    // 4. Orphan note diagnostic (HINT)
    let is_moc = doc.tags.iter().any(|t| {
        let tag_clean = satz_core::fold_key(t.name.trim_start_matches('#'));
        config
            .diagnostics
            .moc_tags
            .iter()
            .any(|moc| satz_core::fold_key(moc) == tag_clean)
    });

    if !is_moc
        && index.incoming_from_others(&doc.id).next().is_none()
        && (!doc.links.is_empty() || !doc.headings.is_empty())
    {
        diagnostics.push(lsp::Diagnostic {
            range: lsp::Range {
                start: lsp::Position {
                    line: 0,
                    character: 0,
                },
                end: lsp::Position {
                    line: 0,
                    character: 0,
                },
            },
            severity: Some(lsp::DiagnosticSeverity::HINT),
            code: Some(lsp::NumberOrString::String("orphan-note".to_string())),
            source: Some("satz".to_string()),
            message: "Orphan note: No incoming backlinks from any document".to_string(),
            ..Default::default()
        });
    }

    diagnostics
}

fn is_missing_frontmatter_field(frontmatter: &Frontmatter, field: &str) -> bool {
    match field {
        "title" => frontmatter.title.is_none(),
        "date" => frontmatter.date.is_none(),
        "tags" => frontmatter.tags.is_empty(),
        "aliases" | "alias" => frontmatter.aliases.is_empty(),
        other => !frontmatter.extra.contains_key(other),
    }
}

fn make_missing_field_diagnostic(field: &str) -> lsp::Diagnostic {
    lsp::Diagnostic {
        range: lsp::Range {
            start: lsp::Position {
                line: 0,
                character: 0,
            },
            end: lsp::Position {
                line: 0,
                character: 0,
            },
        },
        severity: Some(lsp::DiagnosticSeverity::WARNING),
        code: Some(lsp::NumberOrString::String(
            "missing-frontmatter-field".to_string(),
        )),
        source: Some("satz".to_string()),
        message: format!("Missing required frontmatter field: '{}'", field),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests;
