//! `incoming_from_others` is the backlink count users see: a note that links to itself is not
//! referenced by anyone else.

use satz_core::{DocId, Index, parse_document};
use std::path::Path;

fn index_of(files: &[(&str, &str)]) -> Index {
    Index::build(
        files
            .iter()
            .map(|(path, text)| parse_document(text, Path::new(path)))
            .collect(),
    )
}

fn others(index: &Index, id: &str) -> Vec<String> {
    let id = DocId::new(id);
    let mut found: Vec<String> = index
        .incoming_from_others(&id)
        .map(|d| d.as_str().to_string())
        .collect();
    found.sort();
    found
}

#[test]
fn a_self_link_is_not_an_incoming_link() {
    let index = index_of(&[
        ("a.md", "# A\n\n[[a]] and [[#A]] and [[a#A]]\n"),
        ("b.md", "# B\n"),
    ]);
    assert!(others(&index, "a.md").is_empty());
    // `backlinks_of` still reports it (references/rename need the document itself).
    let id = DocId::new("a.md");
    assert!(index.backlinks_of(&id).any(|d| d == &id));
}

#[test]
fn other_documents_count_and_the_self_link_does_not() {
    let index = index_of(&[
        ("a.md", "# A\n\n[[a]]\n"),
        ("b.md", "[[a]]\n"),
        ("c.md", "[[a]] [[a]]\n"),
        ("d.md", "no links\n"),
    ]);
    assert_eq!(others(&index, "a.md"), vec!["b.md", "c.md"]);
    assert!(others(&index, "d.md").is_empty());
}

#[test]
fn an_unknown_id_has_no_incoming_links() {
    let index = index_of(&[("a.md", "# A\n")]);
    assert!(others(&index, "nope.md").is_empty());
}
