use satz_core::{Document, Frontmatter, Index, Link, LinkKind, VaultConfig};
use tower_lsp_server::ls_types as lsp;

use crate::convert::byte_range_to_lsp;

/// Computes all language server diagnostics for a single document.
pub fn compute_diagnostics(
    doc: &Document,
    index: &Index,
    config: &VaultConfig,
) -> Vec<lsp::Diagnostic> {
    let mut diagnostics = Vec::new();

    // 1. Link diagnostics (broken wikilinks, broken heading references, broken internal markdown links)
    for link in &doc.links {
        match link.kind {
            LinkKind::WikiLink | LinkKind::Embed => {
                if link.target_doc.is_empty() {
                    // Intra-document heading reference
                    if let Some(heading_target) = &link.target_heading {
                        let heading_exists = doc.headings.iter().any(|h| {
                            h.slug == *heading_target || h.text.eq_ignore_ascii_case(heading_target)
                        });
                        if !heading_exists {
                            let range = byte_range_to_lsp(link.range, &doc.line_index);
                            diagnostics.push(lsp::Diagnostic {
                                range,
                                severity: Some(lsp::DiagnosticSeverity::WARNING),
                                code: Some(lsp::NumberOrString::String(
                                    "broken-heading".to_string(),
                                )),
                                source: Some("satz".to_string()),
                                message: format!(
                                    "Broken heading reference: no heading '{}' in current document",
                                    heading_target
                                ),
                                ..Default::default()
                            });
                        }
                    }
                    continue;
                }
                diagnose_wikilink(link, index, doc, &mut diagnostics);
            }
            LinkKind::Markdown => {
                if link.target_doc.starts_with("http://")
                    || link.target_doc.starts_with("https://")
                    || link.target_doc.is_empty()
                {
                    continue;
                }
                diagnose_internal_md_link(link, index, doc, &mut diagnostics);
            }
            LinkKind::Footnote => {}
        }
    }

    // 2. Missing required frontmatter fields
    for required_field in &config.frontmatter.required_fields {
        if is_missing_frontmatter_field(&doc.frontmatter, required_field) {
            diagnostics.push(make_missing_field_diagnostic(required_field));
        }
    }

    diagnostics
}

fn diagnose_wikilink(link: &Link, index: &Index, doc: &Document, out: &mut Vec<lsp::Diagnostic>) {
    let range = byte_range_to_lsp(link.range, &doc.line_index);

    match index.resolve_link(&link.target_doc) {
        None => {
            out.push(lsp::Diagnostic {
                range,
                severity: Some(lsp::DiagnosticSeverity::WARNING),
                code: Some(lsp::NumberOrString::String("broken-link".to_string())),
                source: Some("satz".to_string()),
                message: format!("Broken link: '{}' could not be resolved", link.target_doc),
                ..Default::default()
            });
        }
        Some(target_id) => {
            if let (Some(heading_target), Some(target_doc)) =
                (&link.target_heading, index.get_doc(target_id))
            {
                let heading_exists = target_doc.headings.iter().any(|h| {
                    h.slug == *heading_target || h.text.eq_ignore_ascii_case(heading_target)
                });
                if !heading_exists {
                    out.push(lsp::Diagnostic {
                        range,
                        severity: Some(lsp::DiagnosticSeverity::WARNING),
                        code: Some(lsp::NumberOrString::String("broken-heading".to_string())),
                        source: Some("satz".to_string()),
                        message: format!(
                            "Broken heading reference: '{}' has no heading '{}'",
                            link.target_doc, heading_target
                        ),
                        ..Default::default()
                    });
                }
            }
        }
    }
}

fn diagnose_internal_md_link(
    link: &Link,
    index: &Index,
    doc: &Document,
    out: &mut Vec<lsp::Diagnostic>,
) {
    if index.resolve_link(&link.target_doc).is_none() {
        let range = byte_range_to_lsp(link.range, &doc.line_index);
        out.push(lsp::Diagnostic {
            range,
            severity: Some(lsp::DiagnosticSeverity::WARNING),
            code: Some(lsp::NumberOrString::String("broken-link".to_string())),
            source: Some("satz".to_string()),
            message: format!("Broken link: '{}' could not be resolved", link.target_doc),
            ..Default::default()
        });
    }
}

fn is_missing_frontmatter_field(frontmatter: &Frontmatter, field: &str) -> bool {
    match field {
        "title" => frontmatter.title.is_none(),
        "date" => frontmatter.date.is_none(),
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
mod tests {
    use super::*;
    use satz_core::parse_document;
    use std::path::Path;

    #[test]
    fn test_valid_link_no_diagnostics() {
        let doc_a = parse_document("# Doc A\n\n[[doc-b]]", Path::new("doc-a.md"));
        let doc_b = parse_document("# Doc B\n\nContent", Path::new("doc-b.md"));
        let index = Index::build(vec![doc_a.clone(), doc_b]);
        let config = VaultConfig::default();

        let diagnostics = compute_diagnostics(&doc_a, &index, &config);
        assert!(diagnostics.is_empty());
    }

    #[test]
    fn test_broken_wikilink_diagnostic() {
        let doc_a = parse_document("# Doc A\n\n[[missing-note]]", Path::new("doc-a.md"));
        let index = Index::build(vec![doc_a.clone()]);
        let config = VaultConfig::default();

        let diagnostics = compute_diagnostics(&doc_a, &index, &config);
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(
            diagnostics[0].code,
            Some(lsp::NumberOrString::String("broken-link".to_string()))
        );
    }

    #[test]
    fn test_broken_heading_ref_diagnostic() {
        let doc_a = parse_document(
            "# Doc A\n\n[[doc-b#missing-heading]]",
            Path::new("doc-a.md"),
        );
        let doc_b = parse_document("# Doc B\n\n## Existing Heading", Path::new("doc-b.md"));
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
        let index = Index::build(vec![doc_a.clone()]);
        let config = VaultConfig::default();

        let diagnostics = compute_diagnostics(&doc_a, &index, &config);
        assert!(diagnostics.is_empty());
    }

    #[test]
    fn test_missing_required_field_diagnostic() {
        let doc_a = parse_document("# Doc A\n\nNo frontmatter", Path::new("doc-a.md"));
        let index = Index::build(vec![doc_a.clone()]);
        let mut config = VaultConfig::default();
        config.frontmatter.required_fields = vec!["date".to_string(), "author".to_string()];

        let diagnostics = compute_diagnostics(&doc_a, &index, &config);
        assert_eq!(diagnostics.len(), 2);
    }
}
