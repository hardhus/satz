use satz_core::{Index, parse_document};
use std::path::Path;

#[test]
fn test_index_from_fixtures() {
    let docs = vec![
        parse_document(
            include_str!("fixtures/daily_note.md"),
            Path::new("daily_note.md"),
        ),
        parse_document(
            include_str!("fixtures/book_note.md"),
            Path::new("book_note.md"),
        ),
        parse_document(
            include_str!("fixtures/tractatus_style.md"),
            Path::new("tractatus/1.1.md"),
        ),
        parse_document(
            include_str!("fixtures/edge_case.md"),
            Path::new("tests/edge_case.md"),
        ),
    ];
    let index = Index::build(docs);

    assert_eq!(index.doc_count(), 4);

    // book_note aliases: TLP, Tractatus
    assert!(index.resolve_link("TLP").is_some());
    assert!(index.resolve_link("Tractatus").is_some());

    // tractatus/1.1.md alias: "1.1"
    assert!(index.resolve_link("1.1").is_some());

    // Tags are indexed
    assert!(index.docs_with_tag("felsefe").count() > 0);
    assert!(index.docs_with_tag("daily").count() > 0);

    // Stats
    let stats = index.stats();
    assert_eq!(stats.doc_count, 4);
    assert!(stats.unique_tags > 0);
    assert!(stats.total_links > 0);
}
