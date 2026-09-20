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
    if !state.indexing_complete {
        tracing::debug!(uri, "pull_document_diagnostics: initial indexing not complete yet");
        return Vec::new();
    }

    match state.doc_for_uri(uri) {
        Some((_, doc)) => compute_diagnostics(doc, &state.index, &state.config),
        None => Vec::new(),
    }
}

/// Computes the pull-mode `workspace/diagnostic` report across every indexed document.
///
/// Same `indexing_complete` gating as [`pull_document_diagnostics`], for the same reason: a
/// workspace-wide scan taken mid-`walk_vault` would only cover a fraction of the vault.
pub fn pull_workspace_diagnostics(state: &SatzState) -> Vec<lsp::WorkspaceDocumentDiagnosticReport> {
    if !state.indexing_complete {
        tracing::debug!("pull_workspace_diagnostics: initial indexing not complete yet");
        return Vec::new();
    }

    let mut items = Vec::new();
    for doc in state.index.documents() {
        let doc_path = match &state.vault_root {
            Some(root) if !doc.path.is_absolute() => root.join(&doc.path),
            _ => doc.path.clone(),
        };
        let Some(uri) = path_to_uri(&doc_path) else {
            continue;
        };
        let diagnostics = compute_diagnostics(doc, &state.index, &state.config);
        let version = state
            .open_docs
            .get(uri.as_str())
            .map(|od| od.version as i64);
        items.push(lsp::WorkspaceDocumentDiagnosticReport::Full(
            lsp::WorkspaceFullDocumentDiagnosticReport {
                uri,
                version,
                full_document_diagnostic_report: lsp::FullDocumentDiagnosticReport {
                    result_id: None,
                    items: diagnostics,
                },
            },
        ));
    }
    items
}

/// Computes all language server diagnostics for a single document.
pub fn compute_diagnostics(
    doc: &Document,
    index: &Index,
    config: &VaultConfig,
) -> Vec<lsp::Diagnostic> {
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
            code: Some(lsp::NumberOrString::String("invalid-frontmatter".to_string())),
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
                d.code == Some(lsp::NumberOrString::String("invalid-frontmatter".to_string()))
            })
            .collect()
    }

    #[test]
    fn a_frontmatter_that_cannot_be_read_is_reported_on_its_block() {
        let found = frontmatter_diagnostics("---\ntitle: Foo: bar\ntags: [a]\n---\n# Real\n");
        assert_eq!(found.len(), 1);
        let d = &found[0];
        assert_eq!(d.severity, Some(lsp::DiagnosticSeverity::WARNING));
        assert!(d.message.starts_with("Invalid frontmatter"), "{}", d.message);
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
        assert_eq!(duplicate_heading_count("# Same\n\n## Same\n\n### Same\n"), 2);
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
        state.vault_root = Some(abs_root());
        let uri = "file:///doc-a.md";
        state.open_docs.insert(
            uri.to_string(),
            OpenDocument::new(uri, Path::new("doc-a.md").to_path_buf(), "", 1),
        );

        // Default `SatzState` starts with `indexing_complete: false` — the initial vault
        // scan hasn't finished, so a real broken link must not be reported yet.
        assert!(pull_document_diagnostics(uri, &state).is_empty());

        state.indexing_complete = true;
        // broken-link (missing-note) + orphan-note (nothing links back to doc-a)
        assert_eq!(pull_document_diagnostics(uri, &state).len(), 2);
    }

    #[test]
    fn test_pull_workspace_diagnostics_empty_while_indexing_incomplete() {
        let doc_a = parse_document("# Orphan Doc", Path::new("doc-a.md"));
        let mut state = SatzState::default();
        state.index = Index::build(vec![doc_a]);
        state.vault_root = Some(abs_root());

        assert!(pull_workspace_diagnostics(&state).is_empty());

        state.indexing_complete = true;
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
}
