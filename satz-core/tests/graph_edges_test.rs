//! Graph edges are real note-to-note links: footnotes and empty links are not edges, a genuine
//! link to the note itself is.

use satz_core::{Index, VaultGraph, parse_document};
use std::path::Path;

fn edges(files: &[(&str, &str)]) -> Vec<(String, String, String)> {
    let index = Index::build(
        files
            .iter()
            .map(|(path, text)| parse_document(text, Path::new(path)))
            .collect(),
    );
    let mut edges: Vec<(String, String, String)> = VaultGraph::build(&index)
        .to_data()
        .edges
        .into_iter()
        .map(|e| (e.source, e.target, e.kind))
        .collect();
    edges.sort();
    edges
}

#[test]
fn a_footnote_reference_is_not_an_edge() {
    assert!(edges(&[("a.md", "Claim[^1] and more[^n].\n\n[^1]: one\n[^n]: two\n")]).is_empty());
    // Also when other, real links are present.
    let found = edges(&[
        ("a.md", "Claim[^1] [[b]]\n\n[^1]: one\n"),
        ("b.md", "# B\n"),
    ]);
    assert_eq!(
        found,
        vec![("a.md".into(), "b.md".into(), "wikilink".into())]
    );
}

#[test]
fn genuine_self_links_stay_edges() {
    assert_eq!(
        edges(&[("a.md", "# A\n\n## H\n\n[[#H]]\n")]),
        vec![("a.md".into(), "a.md".into(), "wikilink".into())]
    );
    assert_eq!(
        edges(&[("a.md", "# A\n\n[[a]]\n")]),
        vec![("a.md".into(), "a.md".into(), "wikilink".into())]
    );
}

#[test]
fn empty_links_and_broken_links_and_external_links_make_no_edge() {
    assert!(edges(&[("a.md", "[[]] [[|x]] [[#]] [[nowhere]] [t](nothing.md)\n")]).is_empty());
    assert!(edges(&[("a.md", "[w](https://a.b) [m](mailto:a@b.c)\n")]).is_empty());
}

#[test]
fn every_link_kind_between_two_notes_is_one_labelled_edge() {
    let found = edges(&[("a.md", "[[b]] ![[b]] [t](b.md)\n"), ("b.md", "# B\n")]);
    assert_eq!(
        found,
        vec![
            ("a.md".to_string(), "b.md".to_string(), "embed".to_string()),
            (
                "a.md".to_string(),
                "b.md".to_string(),
                "markdown".to_string()
            ),
            (
                "a.md".to_string(),
                "b.md".to_string(),
                "wikilink".to_string()
            ),
        ]
    );
}
