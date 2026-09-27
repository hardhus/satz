// Test states are built field by field so each test shows exactly what it sets up.
#![allow(clippy::field_reassign_with_default)]

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
        .filter(|d| d.code == Some(lsp::NumberOrString::String("duplicate-heading".to_string())))
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
    crate::convert::path_to_uri(&state.vault_root().unwrap().join(rel))
        .unwrap()
        .as_str()
        .to_string()
}

fn full_id(report: &DocumentPull) -> String {
    match report {
        DocumentPull::Full { result_id, .. } => result_id.clone().expect("a full report has an id"),
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
                lsp::WorkspaceDocumentDiagnosticReport::Unchanged(u) if u.uri.as_str() == uri => {
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

// ---- Markdown links are only checked for their note, not for `#fragment`s that are not headings ----

fn codes_of(files: &[(&str, &str)], note: &str) -> Vec<String> {
    let state = ready_state(files);
    let doc = state.index.get_doc_by_path(Path::new(note)).unwrap();
    compute_diagnostics(doc, &state.index, &state.config)
        .into_iter()
        .filter_map(|d| match d.code {
            Some(lsp::NumberOrString::String(c)) if c != "orphan-note" => Some(c),
            _ => None,
        })
        .collect()
}

#[test]
fn a_markdown_link_to_an_html_anchor_or_top_is_not_a_broken_heading() {
    let files = [
        (
            "a.md",
            "# A\n\n[top](#top) and [x](b.md#custom-id) and [y](#own-id)\n\n<a id=\"own-id\"></a>\n",
        ),
        ("b.md", "# B\n\n<h2 id='custom-id'>Styled</h2>\n"),
    ];
    assert_eq!(codes_of(&files, "a.md"), Vec::<String>::new());
}

#[test]
fn a_markdown_link_to_a_fragment_that_is_neither_a_heading_nor_an_anchor_is_still_reported() {
    let files = [
        ("a.md", "# A\n\n[x](b.md#nope) and [y](#nothing)\n"),
        ("b.md", "# B\n\n<a id=\"other\"></a>\n"),
    ];
    assert_eq!(
        codes_of(&files, "a.md"),
        vec!["broken-heading", "broken-heading"]
    );
}

#[test]
fn a_markdown_link_to_a_missing_note_is_still_broken() {
    let files = [("a.md", "# A\n\n[x](missing.md#top) and [y](nope.md)\n")];
    assert_eq!(codes_of(&files, "a.md"), vec!["broken-link", "broken-link"]);
}

#[test]
fn a_wikilink_to_a_missing_heading_is_still_reported() {
    let files = [
        ("a.md", "# A\n\n[[b#Nope]] and [[#Also nope]]\n"),
        ("b.md", "# B\n"),
    ];
    assert_eq!(
        codes_of(&files, "a.md"),
        vec!["broken-heading", "broken-heading"]
    );
}

// ---- a `---` ... `---` pair around ordinary text is horizontal rules, not broken frontmatter ----

#[test]
fn prose_between_two_horizontal_rules_at_the_top_is_not_reported_as_frontmatter() {
    for text in [
        "---\nJust some words\nand more words\n---\n\n# Title\n",
        "---\n\n---\n# Empty block\n",
        "---\n* a\n* b\n---\nbody\n",
    ] {
        assert_eq!(
            codes_of(&[("a.md", text)], "a.md"),
            Vec::<String>::new(),
            "{text:?}"
        );
    }
}

#[test]
fn frontmatter_that_looks_like_yaml_but_is_broken_is_still_reported() {
    for text in [
        "---\ntitle: [unclosed\ntags: x\n---\n# T\n",
        "---\ntitle: a: b: c\n---\n# T\n",
        "---\nkey: value\n  bad indent: [\n---\n# T\n",
    ] {
        assert_eq!(
            codes_of(&[("a.md", text)], "a.md"),
            vec!["invalid-frontmatter"],
            "{text:?}"
        );
    }
}
