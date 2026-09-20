//! Tag queries: a tag matches itself and its sub-tags (`rust` -> `rust/async`) but not a longer
//! name that merely starts with it (`rustic`); answers come in a fixed order.

use satz_core::{Index, parse_document};
use std::path::Path;

fn note(path: &str, tags: &str) -> satz_core::Document {
    parse_document(&format!("---\ntags: [{tags}]\n---\n# N\n"), Path::new(path))
}

fn paths<'a>(docs: impl Iterator<Item = &'a satz_core::Document>) -> Vec<String> {
    docs.map(|d| d.path.to_string_lossy().replace('\\', "/"))
        .collect()
}

fn vault() -> Index {
    Index::build(vec![
        note("d.md", "rust/async"),
        note("a.md", "rust"),
        note("c.md", "rustic"),
        note("b.md", "Rust/Web, other"),
        note("e.md", "İş"),
    ])
}

#[test]
fn a_tag_matches_itself_and_its_sub_tags_but_not_longer_names() {
    let index = vault();
    assert_eq!(
        paths(index.docs_with_tag("rust")),
        vec!["a.md", "b.md", "d.md"]
    );
    assert_eq!(paths(index.docs_with_tag("rust/async")), vec!["d.md"]);
    assert_eq!(paths(index.docs_with_tag("rustic")), vec!["c.md"]);
    assert_eq!(paths(index.docs_with_tag("rus")), Vec::<String>::new());
}

#[test]
fn the_hash_and_the_case_of_the_query_do_not_matter() {
    let index = vault();
    assert_eq!(
        paths(index.docs_with_tag("#RUST")),
        vec!["a.md", "b.md", "d.md"]
    );
}

#[test]
fn turkish_dotted_capital_i_is_folded_like_the_vault_does() {
    let index = vault();
    assert_eq!(paths(index.docs_with_tag("iş")), vec!["e.md"]);
    assert_eq!(paths(index.docs_with_tag("İŞ")), vec!["e.md"]);
}

#[test]
fn an_empty_or_unknown_tag_matches_nothing() {
    let index = vault();
    for query in ["", "#", "/", "nope", "rust/", "rust//async"] {
        assert!(
            paths(index.docs_with_tag(query)).is_empty(),
            "query {query:?}"
        );
    }
}

#[test]
fn answers_come_in_the_order_of_the_document_ids_every_time() {
    for _ in 0..30 {
        let index = vault();
        assert_eq!(
            paths(index.docs_with_tag("rust")),
            vec!["a.md", "b.md", "d.md"]
        );
    }
}

#[test]
fn all_tags_are_sorted_and_follow_edits_and_removals() {
    let mut index = vault();
    assert_eq!(
        index.all_tags(),
        vec!["iş", "other", "rust", "rust/async", "rust/web", "rustic"]
    );
    index.replace_doc(note("a.md", "python"));
    assert!(index.docs_with_tag("python").count() == 1);
    assert_eq!(paths(index.docs_with_tag("rust")), vec!["b.md", "d.md"]);
    index.remove_doc(&satz_core::DocId::new("b.md"));
    assert_eq!(paths(index.docs_with_tag("rust")), vec!["d.md"]);
    assert!(!index.all_tags().contains(&"other"));
}

#[test]
fn a_big_vault_answers_tag_queries_and_stats_quickly() {
    let docs: Vec<_> = (0..5000)
        .map(|i| {
            note(
                &format!("n/{i:05}.md"),
                &format!("t{}, group/{}", i % 500, i % 7),
            )
        })
        .collect();
    let index = Index::build(docs);
    let start = std::time::Instant::now();
    for i in 0..500 {
        assert_eq!(index.docs_with_tag(&format!("t{i}")).count(), 10);
    }
    let stats = index.stats();
    assert_eq!(stats.doc_count, 5000);
    assert_eq!(stats.unique_tags, 500 + 7);
    assert!(
        start.elapsed() < std::time::Duration::from_secs(1),
        "{:?}",
        start.elapsed()
    );
}
