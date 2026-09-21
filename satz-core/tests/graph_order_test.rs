//! The graph export is a function of the vault: the same notes give the same nodes and edges in
//! the same order, whatever order the index was fed them in or hashed them in.
//!
//! The order is: nodes by note id; edges by the id of their source note, then in the order the
//! links stand in that note.

use satz_core::{GraphData, Index, VaultGraph, parse_document};
use std::path::Path;

fn graph_of(index: &Index) -> GraphData {
    VaultGraph::build(index).to_data()
}

fn edge_list(data: &GraphData) -> Vec<(&str, &str, &str, Option<&str>)> {
    data.edges
        .iter()
        .map(|e| {
            (
                e.source.as_str(),
                e.target.as_str(),
                e.kind.as_str(),
                e.label.as_deref(),
            )
        })
        .collect()
}

/// About forty notes whose ids order differently by byte, by case, by folder and by non-ASCII
/// letters, and that link to one another in several ways (two links to one target included).
fn a_vault() -> Vec<satz_core::Document> {
    let mut paths: Vec<String> = vec!["a.md".into(), "a/b.md".into(), "Zeta/z.md".into()];
    for i in 0..40 {
        let folder = ["", "sub/", "Zeta/", "ş/", "a/"][i % 5];
        paths.push(format!("{folder}n{i:02}.md"));
    }
    paths
        .iter()
        .enumerate()
        .map(|(i, path)| {
            let next = (i + 1) % 40;
            let far = (i + 7) % 40;
            let mut text = format!("# Note {i}\n\n[[n{next:02}]] ![[n{far:02}]] [[n{next:02}]]\n");
            if i % 3 == 0 {
                text.push_str("[[a]] [[b#Section]]\n");
            }
            parse_document(&text, Path::new(path))
        })
        .collect()
}

/// The same documents in six different orders.
fn orders(docs: &[satz_core::Document]) -> Vec<Vec<satz_core::Document>> {
    let n = docs.len();
    let by = |pick: &dyn Fn(usize) -> usize| -> Vec<satz_core::Document> {
        (0..n).map(|i| docs[pick(i) % n].clone()).collect()
    };
    // 5 and 11 share no factor with 43 (3 + 40 notes), so the strides visit every note once.
    assert_eq!(n, 43);
    vec![
        by(&|i| i),
        by(&|i| n - 1 - i),
        by(&|i| i + 13),
        by(&|i| n - 1 - (i + 29) % n),
        by(&|i| i * 5),
        by(&|i| i * 11),
    ]
}

#[test]
fn the_graph_does_not_depend_on_the_order_the_notes_came_in_or_on_hashing() {
    let docs = a_vault();
    let graphs: Vec<GraphData> = orders(&docs)
        .into_iter()
        .map(|order| graph_of(&Index::build(order)))
        .collect();

    for (n, graph) in graphs.iter().enumerate().skip(1) {
        // (`assert!`, not `assert_eq!`: a failure must not print two whole graphs.)
        assert!(
            graph == &graphs[0],
            "build #{n} of the same notes gave another graph"
        );
    }

    let first = &graphs[0];
    assert_eq!(first.nodes.len(), 43);
    assert!(first.edges.len() > 100, "the vault is well connected");

    let ids: Vec<&str> = first.nodes.iter().map(|n| n.id.as_str()).collect();
    let mut sorted = ids.clone();
    sorted.sort();
    assert_eq!(ids, sorted, "nodes are listed by note id");

    let rank = |id: &str| sorted.binary_search(&id).expect("an edge names a node");
    let sources: Vec<usize> = first.edges.iter().map(|e| rank(&e.source)).collect();
    assert!(
        sources.windows(2).all(|w| w[0] <= w[1]),
        "edges are grouped by the id of their source note, in that order"
    );
}

#[test]
fn a_note_s_edges_follow_its_links_in_the_order_of_the_text() {
    // Inserted c, a, b: neither the insertion order nor the ids order the edges of `a`.
    let c = parse_document("# C\n", Path::new("c.md"));
    let a = parse_document("# A\n\n[[c]] [[b]] [[b#Sec]] [[b]]\n", Path::new("a.md"));
    let b = parse_document("# B\n\n## Sec\n\n[[c]] [[a]]\n", Path::new("b.md"));
    let data = graph_of(&Index::build(vec![c, a, b]));

    assert_eq!(
        data.nodes.iter().map(|n| n.id.as_str()).collect::<Vec<_>>(),
        ["a.md", "b.md", "c.md"]
    );
    assert_eq!(
        edge_list(&data),
        vec![
            ("a.md", "c.md", "wikilink", None),
            ("a.md", "b.md", "wikilink", None),
            ("a.md", "b.md", "wikilink", Some("Sec")),
            ("a.md", "b.md", "wikilink", None),
            ("b.md", "c.md", "wikilink", None),
            ("b.md", "a.md", "wikilink", None),
        ],
        "every link is an edge, each source's edges in text order, sources by id"
    );
}

#[test]
fn the_json_and_dot_exports_are_identical_for_identical_notes() {
    let docs = a_vault();
    let exports: Vec<(String, String)> = orders(&docs)
        .into_iter()
        .map(|order| {
            let graph = VaultGraph::build(&Index::build(order));
            (graph.export_json().unwrap(), graph.export_dot())
        })
        .collect();
    for (n, export) in exports.iter().enumerate().skip(1) {
        assert_eq!(export.0, exports[0].0, "JSON of build #{n}");
        assert_eq!(export.1, exports[0].1, "DOT of build #{n}");
    }
}

#[test]
fn an_empty_and_a_one_note_vault_have_the_plain_shapes() {
    let empty = VaultGraph::build(&Index::build(Vec::new()));
    assert_eq!(empty.node_count(), 0);
    assert_eq!(empty.edge_count(), 0);
    assert_eq!(
        empty.export_json().unwrap(),
        "{\n  \"nodes\": [],\n  \"edges\": []\n}"
    );
    let dot = empty.export_dot();
    assert!(dot.starts_with("digraph \"satz\" {\n"), "{dot}");
    assert!(dot.ends_with("}\n"), "{dot}");
    assert!(!dot.contains("->") && !dot.contains("[label="), "{dot}");

    let one = VaultGraph::build(&Index::build(vec![parse_document(
        "# Only\n",
        Path::new("only.md"),
    )]));
    assert_eq!((one.node_count(), one.edge_count()), (1, 0));
    assert!(one.export_dot().contains("\"only.md\" [label="));
}
