#![allow(clippy::field_reassign_with_default)]
use satz_core::{Index, parse_document};
use satz_lsp::handlers::definition::goto_definition;
use satz_lsp::handlers::diagnostics::compute_diagnostics;
use satz_lsp::handlers::hover::hover;
use satz_lsp::handlers::references::find_references;
use satz_lsp::handlers::rename::rename;
use satz_lsp::state::{OpenDocument, SatzState};
use std::path::{Path, PathBuf};
use tower_lsp_server::ls_types::*;

fn create_zettel_state() -> (SatzState, PathBuf) {
    let vault_root = PathBuf::from(if cfg!(windows) {
        "C:\\zettel_vault"
    } else {
        "/zettel_vault"
    });

    let content_ana = include_str!("../../satz-core/tests/fixtures/zettel/Ana Dizin.md");
    let content_lsp = include_str!("../../satz-core/tests/fixtures/zettel/Kavramlar/LSP.md");
    let content_gunluk = include_str!("../../satz-core/tests/fixtures/zettel/Gunluk/2026-08-27.md");
    let content_unutulan = include_str!("../../satz-core/tests/fixtures/zettel/Unutulmus Fikir.md");

    let doc_ana = parse_document(content_ana, Path::new("Ana Dizin.md"));
    let doc_lsp = parse_document(content_lsp, Path::new("Kavramlar/LSP.md"));
    let doc_gunluk = parse_document(content_gunluk, Path::new("Gunluk/2026-08-27.md"));
    let doc_unutulan = parse_document(content_unutulan, Path::new("Unutulmus Fikir.md"));

    let docs = vec![
        doc_ana.clone(),
        doc_lsp.clone(),
        doc_gunluk.clone(),
        doc_unutulan.clone(),
    ];
    let index = Index::build(docs);

    let mut state = SatzState::default();
    state.vault_root = Some(vault_root.clone());
    state.index = index;

    // Register open docs
    let uri_ana = if cfg!(windows) {
        "file:///C:/zettel_vault/Ana%20Dizin.md"
    } else {
        "file:///zettel_vault/Ana%20Dizin.md"
    };
    let uri_lsp = if cfg!(windows) {
        "file:///C:/zettel_vault/Kavramlar/LSP.md"
    } else {
        "file:///zettel_vault/Kavramlar/LSP.md"
    };
    let uri_gunluk = if cfg!(windows) {
        "file:///C:/zettel_vault/Gunluk/2026-08-27.md"
    } else {
        "file:///zettel_vault/Gunluk/2026-08-27.md"
    };
    let uri_unutulan = if cfg!(windows) {
        "file:///C:/zettel_vault/Unutulmus%20Fikir.md"
    } else {
        "file:///zettel_vault/Unutulmus%20Fikir.md"
    };

    state.open_docs.insert(
        uri_ana.to_string(),
        OpenDocument::new(uri_ana, vault_root.join("Ana Dizin.md"), content_ana, 1),
    );
    state.open_docs.insert(
        uri_lsp.to_string(),
        OpenDocument::new(uri_lsp, vault_root.join("Kavramlar/LSP.md"), content_lsp, 1),
    );
    state.open_docs.insert(
        uri_gunluk.to_string(),
        OpenDocument::new(
            uri_gunluk,
            vault_root.join("Gunluk/2026-08-27.md"),
            content_gunluk,
            1,
        ),
    );
    state.open_docs.insert(
        uri_unutulan.to_string(),
        OpenDocument::new(
            uri_unutulan,
            vault_root.join("Unutulmus Fikir.md"),
            content_unutulan,
            1,
        ),
    );

    (state, vault_root)
}

#[test]
fn zettel_vault_expected_diagnostics() {
    let (state, _) = create_zettel_state();

    // 1. Ana Dizin: 2 warnings (Yapay Zeka Destekli LSP, olmayan-gorsel.png) + 1 orphan hint
    let doc_ana = state
        .index
        .get_doc(&satz_core::DocId::new("Ana Dizin.md"))
        .unwrap();
    let diags_ana = compute_diagnostics(doc_ana, &state.index, &state.config);
    let warnings_ana: Vec<_> = diags_ana
        .iter()
        .filter(|d| d.severity == Some(DiagnosticSeverity::WARNING))
        .collect();
    let hints_ana: Vec<_> = diags_ana
        .iter()
        .filter(|d| d.severity == Some(DiagnosticSeverity::HINT))
        .collect();
    assert_eq!(
        warnings_ana.len(),
        2,
        "Ana Dizin should have 2 broken link warnings"
    );
    assert_eq!(hints_ana.len(), 1, "Ana Dizin should have 1 orphan hint");
    assert!(
        warnings_ana
            .iter()
            .any(|d| d.message.contains("Yapay Zeka Destekli LSP"))
    );
    assert!(
        warnings_ana
            .iter()
            .any(|d| d.message.contains("olmayan-gorsel.png"))
    );

    // 2. Kavramlar/LSP: 1 warning (duplicate heading 'Mimari')
    let doc_lsp = state
        .index
        .get_doc(&satz_core::DocId::new("Kavramlar/LSP.md"))
        .unwrap();
    let diags_lsp = compute_diagnostics(doc_lsp, &state.index, &state.config);
    assert_eq!(diags_lsp.len(), 1);
    assert_eq!(diags_lsp[0].severity, Some(DiagnosticSeverity::WARNING));
    assert!(diags_lsp[0].message.to_lowercase().contains("mimari"));

    // 3. Gunluk/2026-08-27: 0 diagnostics
    let doc_gunluk = state
        .index
        .get_doc(&satz_core::DocId::new("Gunluk/2026-08-27.md"))
        .unwrap();
    let diags_gunluk = compute_diagnostics(doc_gunluk, &state.index, &state.config);
    assert_eq!(diags_gunluk.len(), 0, "Gunluk should have 0 diagnostics");

    // 4. Unutulmus Fikir: 1 warning (broken heading 'İstemciler') + 1 orphan hint
    let doc_unutulan = state
        .index
        .get_doc(&satz_core::DocId::new("Unutulmus Fikir.md"))
        .unwrap();
    let diags_unutulan = compute_diagnostics(doc_unutulan, &state.index, &state.config);
    let warnings_unutulan: Vec<_> = diags_unutulan
        .iter()
        .filter(|d| d.severity == Some(DiagnosticSeverity::WARNING))
        .collect();
    let hints_unutulan: Vec<_> = diags_unutulan
        .iter()
        .filter(|d| d.severity == Some(DiagnosticSeverity::HINT))
        .collect();
    assert_eq!(
        warnings_unutulan.len(),
        1,
        "Unutulmus Fikir should have 1 broken heading warning"
    );
    assert_eq!(
        hints_unutulan.len(),
        1,
        "Unutulmus Fikir should have 1 orphan hint"
    );
    assert!(warnings_unutulan[0].message.contains("İstemciler"));
}

#[test]
fn zettel_vault_navigation_handlers() {
    let (state, _) = create_zettel_state();

    let uri_ana = if cfg!(windows) {
        "file:///C:/zettel_vault/Ana%20Dizin.md"
    } else {
        "file:///zettel_vault/Ana%20Dizin.md"
    };
    let uri_lsp = if cfg!(windows) {
        "file:///C:/zettel_vault/Kavramlar/LSP.md"
    } else {
        "file:///zettel_vault/Kavramlar/LSP.md"
    };
    let uri_gunluk = if cfg!(windows) {
        "file:///C:/zettel_vault/Gunluk/2026-08-27.md"
    } else {
        "file:///zettel_vault/Gunluk/2026-08-27.md"
    };

    // 1. Definition on [[LSP]] in Ana Dizin (line 8: Burada [[LSP]]...)
    let pos_lsp_link = Position::new(8, 10);
    let def_resp = goto_definition(
        GotoDefinitionParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: uri_ana.parse().unwrap(),
                },
                position: pos_lsp_link,
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        },
        &state,
    );
    assert!(
        def_resp.is_some(),
        "Goto definition on [[LSP]] should return target"
    );

    // 2. References on ^mimari-tanim in Kavramlar/LSP (line 9: ... ^mimari-tanim)
    let pos_block_def = Position::new(9, 35);
    let refs_block = find_references(
        ReferenceParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: uri_lsp.parse().unwrap(),
                },
                position: pos_block_def,
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: ReferenceContext {
                include_declaration: true,
            },
        },
        &state,
    );
    let refs = refs_block.expect("References on block anchor should return results");
    assert_eq!(
        refs.len(),
        2,
        "Expected 2 references (def in LSP.md + link in Gunluk)"
    );

    // 3. Hover on [[LSP#^mimari-tanim]] in Gunluk (line 9: Bugün [[LSP#^mimari-tanim]]...)
    let pos_gunluk_link = Position::new(9, 12);
    let hover_resp = hover(
        HoverParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: uri_gunluk.parse().unwrap(),
                },
                position: pos_gunluk_link,
            },
            work_done_progress_params: Default::default(),
        },
        &state,
    );
    let h = hover_resp.expect("Hover on block link should return Some");
    if let HoverContents::Markup(m) = h.contents {
        assert!(
            m.value
                .contains("LSP istemci-sunucu mimarisidir. ^mimari-tanim")
        );
        assert!(!m.value.contains("---"));
    } else {
        panic!("Expected Markup hover");
    }

    // 4. References on #yazilim/araclar tag
    let pos_tag = Position::new(3, 12); // in Gunluk frontmatter tags: [yazilim/araclar]
    let refs_tag = find_references(
        ReferenceParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: uri_gunluk.parse().unwrap(),
                },
                position: pos_tag,
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: ReferenceContext {
                include_declaration: true,
            },
        },
        &state,
    );
    let tag_locations = refs_tag.expect("Tag references should return results");
    assert_eq!(
        tag_locations.len(),
        2,
        "Expected 2 tag occurrences across files"
    );
}

#[test]
fn zettel_vault_rename_heading_and_doc() {
    let (state, _) = create_zettel_state();

    let uri_lsp = if cfg!(windows) {
        "file:///C:/zettel_vault/Kavramlar/LSP.md"
    } else {
        "file:///zettel_vault/Kavramlar/LSP.md"
    };

    // Rename ## Mimari heading to ## Yeni Mimari
    let pos_heading = Position::new(8, 4);
    let edit = rename(
        RenameParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: uri_lsp.parse().unwrap(),
                },
                position: pos_heading,
            },
            new_name: "Yeni Mimari".to_string(),
            work_done_progress_params: Default::default(),
        },
        &state,
    );
    let we = edit.expect("Heading rename should produce WorkspaceEdit");
    let changes = we.changes.expect("Changes should be present");
    assert!(
        changes
            .values()
            .any(|edits| edits.iter().any(|e| e.new_text.contains("Yeni Mimari")))
    );
}

#[tokio::test]
async fn test_pull_diagnostics_matches_compute_diagnostics() {
    use std::sync::Arc;
    use tokio::sync::RwLock;

    let (state, _) = create_zettel_state();
    let state_arc = Arc::new(RwLock::new(state));

    let uri_ana = if cfg!(windows) {
        "file:///C:/zettel_vault/Ana%20Dizin.md"
    } else {
        "file:///zettel_vault/Ana%20Dizin.md"
    };

    // 1. Direct compute
    let state_guard = state_arc.read().await;
    let doc_ana = state_guard
        .index
        .get_doc(&satz_core::DocId::new("Ana Dizin.md"))
        .unwrap();
    let expected_diags = compute_diagnostics(doc_ana, &state_guard.index, &state_guard.config);
    drop(state_guard);

    // 2. Mock pull diagnostics call via state logic
    let state_guard = state_arc.read().await;
    let open_doc = state_guard.open_docs.get(uri_ana).unwrap();
    let rel_path = SatzState::get_rel_path(&open_doc.path, state_guard.vault_root.as_deref());
    let rel_path_str = rel_path.to_string_lossy().replace('\\', "/");
    let doc_id = satz_core::DocId::new(&rel_path_str);
    let doc = state_guard.index.get_doc(&doc_id).unwrap();
    let pull_diags = compute_diagnostics(doc, &state_guard.index, &state_guard.config);

    assert_eq!(expected_diags.len(), pull_diags.len());
    assert_eq!(expected_diags, pull_diags);
}
