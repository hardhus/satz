use satz_core::{DocId, Index, LinkResolution, parse_document};
use std::path::Path;

fn load_zettel_vault() -> (Index, Vec<satz_core::Document>) {
    let doc_ana = parse_document(
        include_str!("fixtures/zettel/Ana Dizin.md"),
        Path::new("Ana Dizin.md"),
    );
    let doc_lsp = parse_document(
        include_str!("fixtures/zettel/Kavramlar/LSP.md"),
        Path::new("Kavramlar/LSP.md"),
    );
    let doc_gunluk = parse_document(
        include_str!("fixtures/zettel/Gunluk/2026-08-27.md"),
        Path::new("Gunluk/2026-08-27.md"),
    );
    let doc_unutulan = parse_document(
        include_str!("fixtures/zettel/Unutulmus Fikir.md"),
        Path::new("Unutulmus Fikir.md"),
    );

    let docs = vec![doc_ana, doc_lsp, doc_gunluk, doc_unutulan];
    let index = Index::build(docs.clone());
    (index, docs)
}

#[test]
fn test_zettel_vault_navigation() {
    let (index, docs) = load_zettel_vault();

    // 1. [[LSP]] resolves via alias to Kavramlar/LSP.md
    assert_eq!(
        index.resolve_link("LSP"),
        Some(&DocId::new("Kavramlar/LSP.md"))
    );
    assert_eq!(
        index.resolve_link("Language Server Protocol"),
        Some(&DocId::new("Kavramlar/LSP.md"))
    );

    // 2. [[2026-08-27#Günün Özeti]] resolves stem+heading
    let link_gunluk = docs[0]
        .links
        .iter()
        .find(|l| l.target_doc == "2026-08-27")
        .expect("Link to 2026-08-27 should exist in Ana Dizin");
    let res_gunluk = index.resolve_link_full(link_gunluk, Some(&docs[0]));
    assert!(
        matches!(
            res_gunluk,
            LinkResolution::Resolved {
                anchor: Some(_),
                ..
            }
        ),
        "Expected Resolved with heading anchor"
    );

    // 3. [[LSP#^mimari-tanim]] resolves to Kavramlar/LSP.md block anchor
    let link_block = docs[2]
        .links
        .iter()
        .find(|l| l.target_doc == "LSP" && l.target_block.is_some())
        .expect("Block link should exist in Gunluk/2026-08-27");
    let res_block = index.resolve_link_full(link_block, Some(&docs[2]));
    assert!(
        matches!(
            res_block,
            LinkResolution::Resolved {
                anchor: Some(_),
                ..
            }
        ),
        "Expected Resolved with block anchor"
    );

    // 4. [[LSP#İstemciler]] returns AnchorMissing (doc exists, heading missing)
    let link_missing_heading = docs[3]
        .links
        .iter()
        .find(|l| l.target_doc == "LSP")
        .expect("Link to LSP should exist in Unutulmus Fikir");
    let res_missing_heading = index.resolve_link_full(link_missing_heading, Some(&docs[3]));
    assert!(
        matches!(res_missing_heading, LinkResolution::AnchorMissing { .. }),
        "Expected AnchorMissing for İstemciler heading in LSP"
    );

    // 5. [[Yapay Zeka Destekli LSP]] returns DocMissing
    let link_missing_doc = docs[0]
        .links
        .iter()
        .find(|l| l.target_doc == "Yapay Zeka Destekli LSP")
        .expect("Link to Yapay Zeka Destekli LSP should exist");
    let res_missing_doc = index.resolve_link_full(link_missing_doc, Some(&docs[0]));
    assert!(
        matches!(res_missing_doc, LinkResolution::DocMissing),
        "Expected DocMissing"
    );

    // 6. Tags indexed without duplicate ranges
    let tag_docs: Vec<_> = index.docs_with_tag("yazilim/araclar").collect();
    assert_eq!(tag_docs.len(), 2);
}

#[test]
fn test_zettel_index_stats_snapshot() {
    let (index, _) = load_zettel_vault();
    let stats = index.stats();
    insta::assert_yaml_snapshot!(stats);
}
