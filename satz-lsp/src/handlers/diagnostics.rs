use satz_core::{Document, Frontmatter, Index, LinkKind, VaultConfig};
use tower_lsp_server::ls_types as lsp;

use crate::convert::{byte_range_to_lsp, path_to_uri};
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
        let doc_path = match state.vault_root() {
            Some(root) if !doc.path.is_absolute() => root.join(&doc.path),
            _ => doc.path.clone(),
        };
        let Some(uri) = path_to_uri(&doc_path) else {
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
                match index.resolve_link_full_with_config(link, Some(doc), Some(config)) {
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
    if let (Some(error), Some(range)) = (&doc.frontmatter_error, doc.frontmatter_range) {
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
        && index
            .backlinks_of(&doc.id)
            .filter(|id| *id != &doc.id)
            .count()
            == 0
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
// Test states are built field by field so each test shows exactly what it sets up.
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use super::*;
    use crate::state::OpenDocument;
    use satz_core::parse_document;
    use std::path::{Path, PathBuf};

    #[test]
    fn test_valid_link_no_diagnostics() {
        let doc_a = parse_document("# Doc A\n\n[[doc-b]]", Path::new("doc-a.md"));
        let doc_b = parse_document("# Doc B\n\n[[doc-a]]", Path::new("doc-b.md"));
        let index = Index::build(vec![doc_a.clone(), doc_b]);
        let config = VaultConfig::default();

        let diagnostics = compute_diagnostics(&doc_a, &index, &config);
        assert!(diagnostics.is_empty());
    }

    #[test]
    fn test_broken_wikilink_diagnostic() {
        let doc_a = parse_document("# Doc A\n\n[[missing-note]]", Path::new("doc-a.md"));
        let doc_b = parse_document("# Doc B\n\n[[doc-a]]", Path::new("doc-b.md"));
        let index = Index::build(vec![doc_a.clone(), doc_b]);
        let config = VaultConfig::default();

        let diagnostics = compute_diagnostics(&doc_a, &index, &config);
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(
            diagnostics[0].code,
            Some(lsp::NumberOrString::String("broken-link".to_string()))
        );
    }

    #[test]
    fn test_broken_embed_diagnostic() {
        let doc_a = parse_document("# Doc A\n\n![[missing-image]]", Path::new("doc-a.md"));
        let doc_b = parse_document("# Doc B\n\n[[doc-a]]", Path::new("doc-b.md"));
        let index = Index::build(vec![doc_a.clone(), doc_b]);
        let config = VaultConfig::default();

        let diagnostics = compute_diagnostics(&doc_a, &index, &config);
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(
            diagnostics[0].code,
            Some(lsp::NumberOrString::String("broken-embed".to_string()))
        );
    }

    #[test]
    fn test_duplicate_heading_diagnostic() {
        let doc_a = parse_document("# Introduction\n\n# Introduction", Path::new("doc-a.md"));
        let doc_b = parse_document("# Doc B\n\n[[doc-a]]", Path::new("doc-b.md"));
        let index = Index::build(vec![doc_a.clone(), doc_b]);
        let config = VaultConfig::default();

        let diagnostics = compute_diagnostics(&doc_a, &index, &config);
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(
            diagnostics[0].code,
            Some(lsp::NumberOrString::String("duplicate-heading".to_string()))
        );
    }

    /// Number of `duplicate-heading` diagnostics for a document, with a backlink so the orphan
    /// hint does not interfere.
    fn duplicate_heading_count(body: &str) -> usize {
        let doc_a = parse_document(body, Path::new("doc-a.md"));
        let doc_b = parse_document("# B\n\n[[doc-a]]", Path::new("doc-b.md"));
        let index = Index::build(vec![doc_a.clone(), doc_b]);
        compute_diagnostics(&doc_a, &index, &VaultConfig::default())
            .iter()
            .filter(|d| {
                d.code == Some(lsp::NumberOrString::String("duplicate-heading".to_string()))
            })
            .count()
    }

    fn frontmatter_diagnostics(body: &str) -> Vec<lsp::Diagnostic> {
        let doc_a = parse_document(body, Path::new("doc-a.md"));
        let doc_b = parse_document("# B\n\n[[doc-a]]", Path::new("doc-b.md"));
        let index = Index::build(vec![doc_a.clone(), doc_b]);
        compute_diagnostics(&doc_a, &index, &VaultConfig::default())
            .into_iter()
            .filter(|d| {
                d.code
                    == Some(lsp::NumberOrString::String(
                        "invalid-frontmatter".to_string(),
                    ))
            })
            .collect()
    }

    #[test]
    fn a_frontmatter_that_cannot_be_read_is_reported_on_its_block() {
        let found = frontmatter_diagnostics("---\ntitle: Foo: bar\ntags: [a]\n---\n# Real\n");
        assert_eq!(found.len(), 1);
        let d = &found[0];
        assert_eq!(d.severity, Some(lsp::DiagnosticSeverity::WARNING));
        assert!(
            d.message.starts_with("Invalid frontmatter"),
            "{}",
            d.message
        );
        assert!(d.message.contains("ignored"), "{}", d.message);
        // The range is the frontmatter block: from its opening line to its closing line.
        assert_eq!(d.range.start.line, 0);
        assert_eq!(d.range.end.line, 3);
    }

    #[test]
    fn readable_or_missing_frontmatter_gets_no_such_diagnostic() {
        for body in [
            "---\ntitle: T\ntags: [a]\n---\n# H\n",
            "# Just a heading\n",
            "---\n---\n# Empty block\n",
            "---\r\ntitle: T\r\n---\r\n# H\r\n",
        ] {
            assert!(frontmatter_diagnostics(body).is_empty(), "{body:?}");
        }
    }

    #[test]
    fn headings_without_a_slug_are_not_duplicates_of_each_other() {
        // Nothing can link to them by name, so "ambiguous target" makes no sense.
        assert_eq!(duplicate_heading_count("# 🙂\n\n# !!!\n"), 0);
        assert_eq!(duplicate_heading_count("# 🙂\n\n## 🙂\n\n### ...\n"), 0);
        assert_eq!(duplicate_heading_count("#\n\n#\n"), 0);
    }

    #[test]
    fn real_duplicate_headings_are_still_reported() {
        assert_eq!(duplicate_heading_count("# Same\n\n# Same\n"), 1);
        assert_eq!(duplicate_heading_count("# A\n\n## a\n"), 1);
        assert_eq!(
            duplicate_heading_count("# Same\n\n## Same\n\n### Same\n"),
            2
        );
        // A slug-less heading between two real duplicates changes nothing.
        assert_eq!(duplicate_heading_count("# Same\n\n# 🙂\n\n# Same\n"), 1);
    }

    #[test]
    fn test_orphan_note_diagnostic() {
        let doc_a = parse_document("# Orphan Doc", Path::new("doc-a.md"));
        let index = Index::build(vec![doc_a.clone()]);
        let config = VaultConfig::default();

        let diagnostics = compute_diagnostics(&doc_a, &index, &config);
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(
            diagnostics[0].code,
            Some(lsp::NumberOrString::String("orphan-note".to_string()))
        );
        assert_eq!(diagnostics[0].severity, Some(lsp::DiagnosticSeverity::HINT));
    }

    #[test]
    fn test_broken_heading_ref_diagnostic() {
        let doc_a = parse_document(
            "# Doc A\n\n[[doc-b#missing-heading]]",
            Path::new("doc-a.md"),
        );
        let doc_b = parse_document(
            "# Doc B\n\n[[doc-a]]\n## Existing Heading",
            Path::new("doc-b.md"),
        );
        let index = Index::build(vec![doc_a.clone(), doc_b]);
        let config = VaultConfig::default();

        let diagnostics = compute_diagnostics(&doc_a, &index, &config);
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(
            diagnostics[0].code,
            Some(lsp::NumberOrString::String("broken-heading".to_string()))
        );
    }

    #[test]
    fn test_http_link_ignored() {
        let doc_a = parse_document(
            "# Doc A\n\n[Google](https://google.com)",
            Path::new("doc-a.md"),
        );
        let doc_b = parse_document("# Doc B\n\n[[doc-a]]", Path::new("doc-b.md"));
        let index = Index::build(vec![doc_a.clone(), doc_b]);
        let config = VaultConfig::default();

        let diagnostics = compute_diagnostics(&doc_a, &index, &config);
        assert!(diagnostics.is_empty());
    }

    #[test]
    fn test_missing_required_field_diagnostic() {
        let doc_a = parse_document("# Doc A\n\nNo frontmatter", Path::new("doc-a.md"));
        let doc_b = parse_document("# Doc B\n\n[[doc-a]]", Path::new("doc-b.md"));
        let index = Index::build(vec![doc_a.clone(), doc_b]);
        let mut config = VaultConfig::default();
        config.frontmatter.required_fields = vec![
            "date".to_string(),
            "author".to_string(),
            "tags".to_string(),
            "aliases".to_string(),
        ];

        let diagnostics = compute_diagnostics(&doc_a, &index, &config);
        assert_eq!(diagnostics.len(), 4);
    }

    fn abs_root() -> PathBuf {
        if cfg!(windows) {
            Path::new("C:\\vault").to_path_buf()
        } else {
            Path::new("/vault").to_path_buf()
        }
    }

    #[test]
    fn test_pull_document_diagnostics_empty_while_indexing_incomplete() {
        let doc_a = parse_document("# Doc A\n\n[[missing-note]]", Path::new("doc-a.md"));
        let mut state = SatzState::default();
        state.index = Index::build(vec![doc_a]);
        state.set_vault_root(Some(abs_root()));
        let uri = "file:///doc-a.md";
        state.open_docs.insert(
            uri.to_string(),
            OpenDocument::new(uri, Path::new("doc-a.md").to_path_buf(), "", 1),
        );

        // Default `SatzState` starts with `indexing_complete: false` — the initial vault
        // scan hasn't finished, so a real broken link must not be reported yet.
        assert!(pull_document_diagnostics(uri, &state).is_empty());

        state.set_indexing_complete(true);
        // broken-link (missing-note) + orphan-note (nothing links back to doc-a)
        assert_eq!(pull_document_diagnostics(uri, &state).len(), 2);
    }

    #[test]
    fn test_pull_workspace_diagnostics_empty_while_indexing_incomplete() {
        let doc_a = parse_document("# Orphan Doc", Path::new("doc-a.md"));
        let mut state = SatzState::default();
        state.index = Index::build(vec![doc_a]);
        state.set_vault_root(Some(abs_root()));

        assert!(pull_workspace_diagnostics(&state).is_empty());

        state.set_indexing_complete(true);
        assert_eq!(pull_workspace_diagnostics(&state).len(), 1);
    }

    #[test]
    fn test_resolved_footnote_no_diagnostic() {
        let doc_a = parse_document(
            "Ref [^present].\n\n[^present]: Defined.\n",
            Path::new("doc-a.md"),
        );
        let doc_b = parse_document("# Doc B\n\n[[doc-a]]", Path::new("doc-b.md"));
        let index = Index::build(vec![doc_a.clone(), doc_b]);
        let config = VaultConfig::default();

        let diagnostics = compute_diagnostics(&doc_a, &index, &config);
        assert!(diagnostics.is_empty());
    }

    #[test]
    fn test_broken_footnote_diagnostic() {
        let doc_a = parse_document(
            "Ref [^missing].\n\n[^present]: Defined.\n",
            Path::new("doc-a.md"),
        );
        let doc_b = parse_document("# Doc B\n\n[[doc-a]]", Path::new("doc-b.md"));
        let index = Index::build(vec![doc_a.clone(), doc_b]);
        let config = VaultConfig::default();

        let diagnostics = compute_diagnostics(&doc_a, &index, &config);
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(
            diagnostics[0].code,
            Some(lsp::NumberOrString::String("broken-footnote".to_string()))
        );
    }

    #[test]
    fn test_turkish_heading_ref_diagnostic_resolved() {
        let doc_a = parse_document(
            "# Doc A\n\n[[doc-b#Günün Özeti]]\n[[doc-b#günün özeti]]",
            Path::new("doc-a.md"),
        );
        let doc_b = parse_document(
            "# Doc B\n\n[[doc-a]]\n## Günün Özeti",
            Path::new("doc-b.md"),
        );
        let index = Index::build(vec![doc_a.clone(), doc_b]);
        let config = VaultConfig::default();

        let diagnostics = compute_diagnostics(&doc_a, &index, &config);
        assert!(diagnostics.is_empty());
    }

    // ---- pull reports: result ids let an unchanged vault be answered without computing ----

    use std::collections::HashMap;

    fn ready_state(files: &[(&str, &str)]) -> SatzState {
        let mut state = SatzState::default();
        state.index = Index::build(
            files
                .iter()
                .map(|(p, t)| parse_document(t, Path::new(p)))
                .collect(),
        );
        state.set_vault_root(Some(if cfg!(windows) {
            PathBuf::from("C:\\vault")
        } else {
            PathBuf::from("/vault")
        }));
        state.set_indexing_complete(true);
        state
    }

    fn uri_of(state: &SatzState, rel: &str) -> String {
        path_to_uri(&state.vault_root().unwrap().join(rel))
            .unwrap()
            .as_str()
            .to_string()
    }

    fn full_id(report: &DocumentPull) -> String {
        match report {
            DocumentPull::Full { result_id, .. } => {
                result_id.clone().expect("a full report has an id")
            }
            DocumentPull::Unchanged { .. } => panic!("expected a full report"),
        }
    }

    #[test]
    fn the_result_id_changes_with_the_index_and_with_the_configuration() {
        let mut state = ready_state(&[("a.md", "# A\n[[b]]\n"), ("b.md", "# B\n")]);
        let first = diagnostics_result_id(&state);
        assert_eq!(
            diagnostics_result_id(&state),
            first,
            "stable while nothing changes"
        );

        state
            .index
            .replace_doc(parse_document("# B\n\nnew text\n", Path::new("b.md")));
        let after_edit = diagnostics_result_id(&state);
        assert_ne!(after_edit, first);

        state.config_revision += 1;
        assert_ne!(diagnostics_result_id(&state), after_edit);
    }

    #[test]
    fn a_document_pull_with_the_current_id_is_answered_without_computing() {
        let state = ready_state(&[("a.md", "# A\n[[missing]]\n")]);
        let uri = uri_of(&state, "a.md");
        // The document must be open for a per-document pull.
        let mut state = state;
        state.open_document(
            &uri,
            "# A\n[[missing]]\n",
            &state.vault_root().unwrap().join("a.md"),
            1,
        );

        let first = pull_document_report(&uri, None, &state);
        let id = full_id(&first);
        let DocumentPull::Full { items, .. } = &first else {
            unreachable!()
        };
        assert!(
            items.iter().any(|d| d.message.contains("missing")),
            "the broken link: {items:?}"
        );

        let computed_before = compute_calls();
        let again = pull_document_report(&uri, Some(&id), &state);
        assert_eq!(
            again,
            DocumentPull::Unchanged {
                result_id: id.clone()
            }
        );
        assert_eq!(compute_calls(), computed_before, "nothing was computed");

        // A stale, foreign or garbled id gets the full report again.
        for previous in ["", "0.0", "garbage", "999999.999999", &format!("{id}x")] {
            let report = pull_document_report(&uri, Some(previous), &state);
            assert!(matches!(report, DocumentPull::Full { .. }), "{previous:?}");
        }
    }

    #[test]
    fn a_change_anywhere_invalidates_every_result_id() {
        let mut state = ready_state(&[("a.md", "# A\n[[b]]\n"), ("b.md", "# B\n")]);
        let uri = uri_of(&state, "a.md");
        state.open_document(
            &uri,
            "# A\n[[b]]\n",
            &state.vault_root().unwrap().join("a.md"),
            1,
        );
        let id = full_id(&pull_document_report(&uri, None, &state));

        // Removing `b.md` breaks the link in a.md: its diagnostics change though a.md did not.
        state.index.remove_doc(&satz_core::DocId::new("b.md"));
        let report = pull_document_report(&uri, Some(&id), &state);
        let DocumentPull::Full { items, result_id } = report else {
            panic!("the old id must not be accepted")
        };
        assert!(
            items.iter().any(|d| d.message.contains("'b'")),
            "the newly broken link: {items:?}"
        );
        assert_ne!(result_id.unwrap(), id);
    }

    #[test]
    fn before_the_first_index_is_complete_a_pull_is_empty_and_has_no_id() {
        let mut state = ready_state(&[("a.md", "# A\n")]);
        state.set_indexing_complete(false);
        let uri = uri_of(&state, "a.md");
        match pull_document_report(&uri, None, &state) {
            DocumentPull::Full { items, result_id } => {
                assert!(items.is_empty());
                assert_eq!(result_id, None, "an incomplete answer must never be reused");
            }
            other => panic!("{other:?}"),
        }
        // Even a matching-looking id is not accepted while indexing.
        let id = diagnostics_result_id(&state);
        assert!(matches!(
            pull_document_report(&uri, Some(&id), &state),
            DocumentPull::Full { .. }
        ));
        assert!(pull_workspace_report(&HashMap::new(), &state).is_empty());
    }

    #[test]
    fn a_workspace_pull_skips_the_notes_whose_id_is_current() {
        let state = ready_state(&[
            ("a.md", "# A\n[[gone]]\n"),
            ("b.md", "# B\n"),
            ("c.md", "# C\n[[also-gone]]\n"),
        ]);
        let first = pull_workspace_report(&HashMap::new(), &state);
        assert_eq!(first.len(), 3);
        let ids: HashMap<String, String> = first
            .iter()
            .map(|r| match r {
                lsp::WorkspaceDocumentDiagnosticReport::Full(f) => (
                    f.uri.as_str().to_string(),
                    f.full_document_diagnostic_report.result_id.clone().unwrap(),
                ),
                other => panic!("{other:?}"),
            })
            .collect();

        // Everything current: nothing is computed and every entry is `Unchanged`.
        let before = compute_calls();
        let second = pull_workspace_report(&ids, &state);
        assert_eq!(second.len(), 3);
        assert!(
            second
                .iter()
                .all(|r| matches!(r, lsp::WorkspaceDocumentDiagnosticReport::Unchanged(_)))
        );
        assert_eq!(compute_calls(), before);

        // Only some ids known (and one wrong): those are `Unchanged`, the rest `Full`.
        let mut partial = HashMap::new();
        partial.insert(uri_of(&state, "a.md"), ids[&uri_of(&state, "a.md")].clone());
        partial.insert(uri_of(&state, "b.md"), "stale".to_string());
        let mixed = pull_workspace_report(&partial, &state);
        let kind = |rel: &str| {
            let uri = uri_of(&state, rel);
            mixed
                .iter()
                .find_map(|r| match r {
                    lsp::WorkspaceDocumentDiagnosticReport::Full(f) if f.uri.as_str() == uri => {
                        Some("full")
                    }
                    lsp::WorkspaceDocumentDiagnosticReport::Unchanged(u)
                        if u.uri.as_str() == uri =>
                    {
                        Some("unchanged")
                    }
                    _ => None,
                })
                .unwrap()
        };
        assert_eq!(kind("a.md"), "unchanged");
        assert_eq!(kind("b.md"), "full");
        assert_eq!(kind("c.md"), "full");
    }

    #[test]
    fn open_documents_keep_their_version_in_both_report_kinds() {
        let mut state = ready_state(&[("a.md", "# A\n")]);
        let path = state.vault_root().unwrap().join("a.md");
        let uri = uri_of(&state, "a.md");
        state.open_document(&uri, "# A\n", &path, 7);
        let full = pull_workspace_report(&HashMap::new(), &state);
        let lsp::WorkspaceDocumentDiagnosticReport::Full(f) = &full[0] else {
            panic!()
        };
        assert_eq!(f.version, Some(7));
        let ids = HashMap::from([(
            uri.clone(),
            f.full_document_diagnostic_report.result_id.clone().unwrap(),
        )]);
        let unchanged = pull_workspace_report(&ids, &state);
        let lsp::WorkspaceDocumentDiagnosticReport::Unchanged(u) = &unchanged[0] else {
            panic!()
        };
        assert_eq!(u.version, Some(7));
    }

    #[test]
    fn a_big_idle_vault_is_answered_quickly_the_second_time() {
        let files: Vec<(String, String)> = (0..2000)
            .map(|i| {
                (
                    format!("n{i}.md"),
                    format!("# N{i}\n[[n{}]] [[missing{i}]]\n", (i + 1) % 2000),
                )
            })
            .collect();
        let refs: Vec<(&str, &str)> = files
            .iter()
            .map(|(p, t)| (p.as_str(), t.as_str()))
            .collect();
        let state = ready_state(&refs);
        let first = pull_workspace_report(&HashMap::new(), &state);
        let ids: HashMap<String, String> = first
            .iter()
            .map(|r| match r {
                lsp::WorkspaceDocumentDiagnosticReport::Full(f) => (
                    f.uri.as_str().to_string(),
                    f.full_document_diagnostic_report.result_id.clone().unwrap(),
                ),
                _ => unreachable!(),
            })
            .collect();
        let start = std::time::Instant::now();
        let second = pull_workspace_report(&ids, &state);
        assert!(
            start.elapsed() < std::time::Duration::from_secs(1),
            "{:?}",
            start.elapsed()
        );
        assert!(
            second
                .iter()
                .all(|r| matches!(r, lsp::WorkspaceDocumentDiagnosticReport::Unchanged(_)))
        );
    }
}
