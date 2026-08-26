use std::path::Path;

use satz_core::walk::walk_vault;
use satz_core::{DocId, Index, LinkKind};

#[test]
fn test_obsidian_vault_compatibility() {
    let obsidian_vault = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/obsidian");
    let docs = walk_vault(&obsidian_vault).expect("Vault walk should succeed");

    assert_eq!(docs.len(), 3);

    let index = Index::build(docs);

    // 1. Alias Resolution (singular `alias` and plural `aliases`)
    // "Quick Thought" -> Inbox/fleeting.md
    let quick_thought_id = index
        .resolve_link("Quick Thought")
        .expect("Quick Thought alias should resolve");
    assert_eq!(quick_thought_id, &DocId::new("Inbox/fleeting.md"));

    // "Fundamentals" -> Literature/concept.md
    let fundamentals_id = index
        .resolve_link("Fundamentals")
        .expect("Fundamentals alias should resolve");
    assert_eq!(fundamentals_id, &DocId::new("Literature/concept.md"));

    // "Index" -> MOC.md
    let index_id = index
        .resolve_link("Index")
        .expect("Index alias should resolve");
    assert_eq!(index_id, &DocId::new("MOC.md"));

    // 2. Block Reference parsing & targeting
    let concept_doc = index
        .get_doc(&DocId::new("Literature/concept.md"))
        .expect("Concept doc should exist");
    assert_eq!(concept_doc.blocks.len(), 1);
    assert_eq!(concept_doc.blocks[0].id, "quote-1");

    // 3. Embed link recognition
    let fleeting_doc = index
        .get_doc(&DocId::new("Inbox/fleeting.md"))
        .expect("Fleeting doc should exist");
    let embed = fleeting_doc
        .links
        .iter()
        .find(|l| l.kind == LinkKind::Embed)
        .expect("Embed link should exist");
    assert_eq!(embed.target_doc, "Attachment/image.png");

    // 4. Tag Indexing (Frontmatter list, string, and body tags)
    let philosophy_docs: Vec<_> = index.docs_with_tag("philosophy").collect();
    assert_eq!(philosophy_docs.len(), 1);
    assert_eq!(philosophy_docs[0].id, DocId::new("Literature/concept.md"));

    let moc_docs: Vec<_> = index.docs_with_tag("moc").collect();
    assert_eq!(moc_docs.len(), 1);
    assert_eq!(moc_docs[0].id, DocId::new("MOC.md"));

    // 5. Backlink Verification
    let fleeting_backlinks: Vec<_> = index
        .backlinks_of(&DocId::new("Inbox/fleeting.md"))
        .collect();
    assert!(fleeting_backlinks.contains(&&DocId::new("MOC.md")));

    let concept_backlinks: Vec<_> = index
        .backlinks_of(&DocId::new("Literature/concept.md"))
        .collect();
    assert!(concept_backlinks.contains(&&DocId::new("Inbox/fleeting.md")));
    assert!(concept_backlinks.contains(&&DocId::new("MOC.md")));
}
