//! Backlinks, orphans, broken-link counts and graph edges must judge a Markdown link exactly like
//! link resolution does: relative to the folder of the note that contains it, and broken when the
//! path leaves the vault.

use satz_core::{DocId, Index, LinkKind, LinkResolution, VaultGraph, parse_document};
use std::collections::BTreeSet;
use std::path::Path;

fn index(files: &[(&str, &str)]) -> Index {
    Index::build(
        files
            .iter()
            .map(|(path, text)| parse_document(text, Path::new(path)))
            .collect(),
    )
}

fn backlinks(index: &Index, of: &str) -> BTreeSet<String> {
    index
        .backlinks_of(&DocId::new(of))
        .map(|id| id.as_str().to_string())
        .collect()
}

fn set(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|s| s.to_string()).collect()
}

fn graph_edges(index: &Index) -> BTreeSet<(String, String)> {
    VaultGraph::build(index)
        .to_data()
        .edges
        .into_iter()
        .map(|e| (e.source, e.target))
        .collect()
}

#[test]
fn a_link_that_leaves_the_vault_is_broken_everywhere_not_a_backlink() {
    let idx = index(&[("sub/a.md", "[t](../../out.md)\n"), ("out.md", "# out\n")]);
    assert!(backlinks(&idx, "out.md").is_empty());
    assert!(idx.orphan_docs().any(|d| d.id.as_str() == "out.md"));
    let broken: Vec<&str> = idx
        .docs_with_broken_links()
        .map(|(d, _)| d.id.as_str())
        .collect();
    assert_eq!(broken, vec!["sub/a.md"]);
    assert_eq!(idx.stats().broken_links, 1);
    assert!(graph_edges(&idx).is_empty(), "{:?}", graph_edges(&idx));
}

#[test]
fn the_same_file_name_in_two_folders_is_told_apart() {
    let idx = index(&[
        ("sub/a.md", "[t](b.md) [u](./b.md)\n"),
        ("sub/b.md", "# sub b\n"),
        ("b.md", "# root b\n"),
    ]);
    assert_eq!(backlinks(&idx, "sub/b.md"), set(&["sub/a.md"]));
    assert!(backlinks(&idx, "b.md").is_empty());
    assert_eq!(
        graph_edges(&idx),
        BTreeSet::from([("sub/a.md".to_string(), "sub/b.md".to_string())])
    );

    let idx = index(&[
        ("sub/a.md", "[t](../b.md)\n"),
        ("sub/b.md", "# sub b\n"),
        ("b.md", "# root b\n"),
    ]);
    assert_eq!(backlinks(&idx, "b.md"), set(&["sub/a.md"]));
    assert!(backlinks(&idx, "sub/b.md").is_empty());
    assert_eq!(
        graph_edges(&idx),
        BTreeSet::from([("sub/a.md".to_string(), "b.md".to_string())])
    );
}

#[test]
fn wikilinks_and_title_or_alias_links_keep_the_vault_wide_rule() {
    let idx = index(&[
        ("sub/a.md", "[[b]] and [t](Root Title)\n"),
        ("sub/b.md", "# sub b\n"),
        ("b.md", "---\ntitle: Root Title\n---\n# root b\n"),
    ]);
    // `[[b]]` does not start in the note's folder; the Markdown link reaches the note by title.
    assert_eq!(backlinks(&idx, "b.md"), set(&["sub/a.md"]));
    assert!(backlinks(&idx, "sub/b.md").is_empty());
}

#[test]
fn a_target_added_or_removed_later_moves_the_backlink() {
    let mut idx = index(&[("sub/a.md", "[t](b.md)\n"), ("b.md", "# root b\n")]);
    // Only the root `b.md` exists: the folder-relative path is missing, the vault-relative
    // fallback finds it.
    assert_eq!(backlinks(&idx, "b.md"), set(&["sub/a.md"]));
    idx.replace_doc(parse_document("# sub b\n", Path::new("sub/b.md")));
    assert_eq!(backlinks(&idx, "sub/b.md"), set(&["sub/a.md"]));
    assert!(backlinks(&idx, "b.md").is_empty());
    idx.remove_doc(&DocId::new("sub/b.md"));
    assert_eq!(backlinks(&idx, "b.md"), set(&["sub/a.md"]));
    assert!(backlinks(&idx, "sub/b.md").is_empty());
}

#[test]
fn external_empty_and_footnote_targets_are_never_backlinks_or_edges() {
    let idx = index(&[
        (
            "a.md",
            "[x](https://example.com/b.md) [y](#h) [z]() n[^1]\n\n[^1]: note\n",
        ),
        ("b.md", "# b\n"),
    ]);
    assert!(backlinks(&idx, "b.md").is_empty());
    assert!(backlinks(&idx, "a.md").is_empty());
    assert!(graph_edges(&idx).is_empty());
}

/// A backlink exists exactly when link resolution finds the note, for every kind of link, over a
/// deterministic mix of folders, names, escapes and case.
#[test]
fn backlinks_agree_with_link_resolution_on_every_markdown_link() {
    let paths = [
        "a.md",
        "b.md",
        "sub/a.md",
        "sub/b.md",
        "sub/deep/c.md",
        "deep/c.md",
        "Other/Note.md",
    ];
    let targets = [
        "b.md",
        "./b.md",
        "../b.md",
        "../../b.md",
        "../../../b.md",
        "sub/b.md",
        "deep/c",
        "../deep/c.md",
        "C.md",
        "../other/note.md",
        "Other/Note",
        "nope.md",
        "my%20note.md",
        "/abs.md",
        "sub//b.md",
        "b.md#Heading",
        "./../b.md",
    ];
    let mut seed = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for _ in 0..60 {
        let mut docs = Vec::new();
        for path in paths {
            let mut text = String::from("# H\n\n");
            for _ in 0..(next() % 4) {
                let target = targets[(next() % targets.len() as u64) as usize];
                text.push_str(&format!("[t]({target}) "));
            }
            docs.push(parse_document(&text, Path::new(path)));
        }
        let idx = Index::build(docs);
        for doc in idx.documents() {
            for link in doc.links.iter().filter(|l| l.kind == LinkKind::Markdown) {
                let resolved = match idx.resolve_link_full(link, Some(doc)) {
                    LinkResolution::Resolved { doc, .. }
                    | LinkResolution::AnchorMissing { doc } => Some(doc.id.clone()),
                    LinkResolution::DocMissing => None,
                };
                if let Some(target) = resolved
                    && target != doc.id
                {
                    assert!(
                        idx.backlinks_of(&target).any(|b| *b == doc.id),
                        "{} -> {:?} resolves to {target} but is no backlink",
                        doc.id,
                        link.target_doc
                    );
                }
            }
            // The other direction: every recorded backlink comes from a link that resolves there.
            let id = doc.id.clone();
            for source in idx.backlinks_of(&id) {
                let source_doc = idx.get_doc(source).unwrap();
                let has_resolving_link = source_doc.links.iter().any(|l| {
                    matches!(
                        idx.resolve_link_full(l, Some(source_doc)),
                        LinkResolution::Resolved { doc, .. } | LinkResolution::AnchorMissing { doc }
                            if doc.id == id
                    ) || (l.target_doc.is_empty() && *source == id)
                });
                assert!(
                    has_resolving_link,
                    "{source} is a backlink of {id} without a link"
                );
            }
        }
    }
}
