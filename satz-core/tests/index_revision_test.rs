//! `Index::revision` changes exactly when the index changes: it names the state diagnostics were
//! computed from, so "nothing changed" can be answered without computing anything.

use satz_core::{DocId, Index, parse_document};
use std::path::Path;

fn doc(text: &str, path: &str) -> satz_core::Document {
    parse_document(text, Path::new(path))
}

#[test]
fn every_change_to_the_index_moves_the_revision() {
    let mut index = Index::build(vec![doc("# A\n", "a.md")]);
    let mut last = index.revision();

    index.replace_doc(doc("# A\n\n[[b]]\n", "a.md")); // same identity: an incremental edit
    assert_ne!(index.revision(), last, "editing a note");
    last = index.revision();

    index.replace_doc(doc("# B\n", "b.md")); // a new note
    assert_ne!(index.revision(), last, "adding a note");
    last = index.revision();

    index.replace_docs(vec![doc("# C\n", "c.md"), doc("# D\n", "d.md")]);
    assert_ne!(index.revision(), last, "adding several notes");
    last = index.revision();

    index.remove_doc(&DocId::new("c.md"));
    assert_ne!(index.revision(), last, "removing a note");
    last = index.revision();

    index.remove_docs(&[DocId::new("d.md"), DocId::new("b.md")]);
    assert_ne!(index.revision(), last, "removing several notes");
}

#[test]
fn changes_that_change_nothing_keep_the_revision() {
    let mut index = Index::build(vec![doc("# A\n", "a.md")]);
    let last = index.revision();
    index.remove_doc(&DocId::new("nope.md"));
    index.remove_docs(&[]);
    index.remove_docs(&[DocId::new("nope.md"), DocId::new("nada.md")]);
    index.replace_docs(Vec::new());
    assert_eq!(index.revision(), last);
}

#[test]
fn a_bulk_change_is_one_step_and_two_indexes_built_alike_are_independent() {
    let mut a = Index::build(vec![doc("# A\n", "a.md")]);
    let before = a.revision();
    a.replace_docs((0..50).map(|i| doc("# n\n", &format!("n{i}.md"))).collect());
    assert_eq!(a.revision(), before + 1, "one rebuild, one step");
    // The revision is a property of one index's history, not of its content.
    let b = Index::build(vec![doc("# A\n", "a.md")]);
    assert_eq!(
        b.revision(),
        Index::build(vec![doc("# A\n", "a.md")]).revision()
    );
    assert!(Index::default().revision() <= b.revision());
}
