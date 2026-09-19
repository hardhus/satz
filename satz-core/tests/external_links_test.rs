//! Links that point outside the vault (`mailto:`, `tel:`, `obsidian://` ...) are not notes:
//! they can neither be broken nor be graph edges or backlinks.

use satz_core::{Index, LinkKind, VaultGraph, parse_document};
use std::path::Path;

const EXTERNAL: &[&str] = &[
    "[mail](mailto:a@b.c)",
    "[call](tel:+905551112233)",
    "[ftp](ftp://host/file.txt)",
    "[app](obsidian://open?vault=v&file=x)",
    "[file](file:///tmp/x.md)",
    "[web](https://example.com/a/b#frag)",
    "[web](http://example.com)",
];

fn index_of(body: &str) -> Index {
    let a = parse_document(body, Path::new("a.md"));
    let b = parse_document("# B\n", Path::new("b.md"));
    Index::build(vec![a, b])
}

#[test]
fn external_targets_are_never_broken_links() {
    for link in EXTERNAL {
        let index = index_of(link);
        assert_eq!(index.docs_with_broken_links().count(), 0, "{link}");
    }
    // An unresolvable internal target still is.
    assert_eq!(
        index_of("[x](missing.md)").docs_with_broken_links().count(),
        1
    );
}

#[test]
fn external_targets_are_never_graph_edges_or_backlinks() {
    for link in EXTERNAL {
        let index = index_of(link);
        let graph = VaultGraph::build(&index);
        assert_eq!(graph.edge_count(), 0, "{link}");
        assert_eq!(graph.node_count(), 2, "{link}");
    }
    // Sanity: a real internal link is an edge.
    let index = index_of("[x](b.md)");
    assert_eq!(VaultGraph::build(&index).edge_count(), 1);
}

#[test]
fn an_external_url_is_kept_whole_including_its_fragment() {
    for (md, url) in [
        ("[w](https://a.b/c#frag)", "https://a.b/c#frag"),
        ("[m](mailto:a@b.c)", "mailto:a@b.c"),
        (
            "[o](obsidian://open?vault=v#x)",
            "obsidian://open?vault=v#x",
        ),
    ] {
        let doc = parse_document(md, Path::new("a.md"));
        assert_eq!(doc.links.len(), 1, "{md}");
        assert_eq!(doc.links[0].kind, LinkKind::Markdown);
        assert_eq!(doc.links[0].target_doc, url, "{md}");
        assert_eq!(doc.links[0].target_heading, None, "{md}");
    }
    // Internal links keep splitting the heading off.
    let doc = parse_document("[n](note.md#Sec)", Path::new("a.md"));
    assert_eq!(doc.links[0].target_doc, "note.md");
    assert_eq!(doc.links[0].target_heading.as_deref(), Some("Sec"));
}

#[test]
fn a_percent_encoded_link_to_a_note_with_a_space_is_not_broken() {
    let a = parse_document(
        "[t](my%20note.md) [u](my%20note.md#Sec%20Tion)\n",
        Path::new("a.md"),
    );
    let b = parse_document("# B\n\n## Sec Tion\n", Path::new("my note.md"));
    let index = Index::build(vec![a, b]);
    assert_eq!(index.docs_with_broken_links().count(), 0);
}
