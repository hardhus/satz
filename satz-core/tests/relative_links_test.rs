//! A Markdown link's path is relative to the note that contains it (`../b.md`, `./c.md`, `d.md`),
//! with the vault-relative and file-name lookups as fallbacks. Wikilinks keep the vault-wide rule.

use satz_core::{Index, LinkResolution, parse_document};
use std::path::Path;

/// The path of the note the first link in `source` resolves to (None: unresolved).
fn resolves_to(files: &[(&str, &str)], source: &str) -> Option<String> {
    let index = Index::build(
        files
            .iter()
            .map(|(path, text)| parse_document(text, Path::new(path)))
            .collect(),
    );
    let doc = index.documents().find(|d| d.path == Path::new(source))?;
    let link = doc.links.first()?;
    match index.resolve_link_full(link, Some(doc)) {
        LinkResolution::Resolved { doc, .. } | LinkResolution::AnchorMissing { doc } => {
            Some(doc.path.to_string_lossy().replace('\\', "/"))
        }
        LinkResolution::DocMissing => None,
    }
}

#[test]
fn a_bare_or_dotted_path_starts_in_the_notes_own_folder() {
    let files = [
        ("sub/a.md", "[t](b.md)\n"),
        ("sub/b.md", "# sub b\n"),
        ("b.md", "# root b\n"),
    ];
    assert_eq!(resolves_to(&files, "sub/a.md"), Some("sub/b.md".into()));
    let dotted = [
        ("sub/a.md", "[t](./c.md)\n"),
        ("sub/c.md", "# sub c\n"),
        ("c.md", "# root c\n"),
    ];
    assert_eq!(resolves_to(&dotted, "sub/a.md"), Some("sub/c.md".into()));
}

#[test]
fn parent_directory_steps_walk_up_from_the_notes_folder() {
    let files = [
        ("deep/er/a.md", "[t](../b.md)\n"),
        ("deep/b.md", "# deep b\n"),
        ("b.md", "# root b\n"),
    ];
    assert_eq!(
        resolves_to(&files, "deep/er/a.md"),
        Some("deep/b.md".into())
    );
    let two_up = [
        ("deep/er/a.md", "[t](../../b.md)\n"),
        ("b.md", "# root b\n"),
    ];
    assert_eq!(resolves_to(&two_up, "deep/er/a.md"), Some("b.md".into()));
    let extension_free = [("sub/a.md", "[t](../b)\n"), ("b.md", "# root b\n")];
    assert_eq!(
        resolves_to(&extension_free, "sub/a.md"),
        Some("b.md".into())
    );
}

#[test]
fn a_path_that_leaves_the_vault_is_broken_not_matched_by_name() {
    let files = [("sub/a.md", "[t](../../out.md)\n"), ("out.md", "# out\n")];
    assert_eq!(resolves_to(&files, "sub/a.md"), None);
    let one_too_many = [("a.md", "[t](../out.md)\n"), ("out.md", "# out\n")];
    assert_eq!(resolves_to(&one_too_many, "a.md"), None);
}

#[test]
fn a_vault_relative_path_still_works_when_the_folder_relative_one_is_missing() {
    let files = [("sub/a.md", "[t](sub/x.md)\n"), ("sub/x.md", "# x\n")];
    assert_eq!(resolves_to(&files, "sub/a.md"), Some("sub/x.md".into()));
    // Folder-relative wins when both exist.
    let both = [
        ("sub/a.md", "[t](sub/x.md)\n"),
        ("sub/x.md", "# vault-relative\n"),
        ("sub/sub/x.md", "# folder-relative\n"),
    ];
    assert_eq!(resolves_to(&both, "sub/a.md"), Some("sub/sub/x.md".into()));
}

#[test]
fn escapes_fragments_and_names_with_spaces_work() {
    let spaced = [
        ("sub/a.md", "[t](my%20note.md)\n"),
        ("sub/my note.md", "# n\n"),
        ("my note.md", "# root\n"),
    ];
    assert_eq!(
        resolves_to(&spaced, "sub/a.md"),
        Some("sub/my note.md".into())
    );
    let fragment = [
        ("sub/a.md", "[t](../b.md#Real)\n"),
        ("b.md", "# B\n\n## Real\n"),
    ];
    assert_eq!(resolves_to(&fragment, "sub/a.md"), Some("b.md".into()));
    // Case of the path is not significant, like everywhere else.
    let case = [("sub/a.md", "[t](B.MD)\n"), ("sub/b.md", "# b\n")];
    assert_eq!(resolves_to(&case, "sub/a.md"), Some("sub/b.md".into()));
}

#[test]
fn wikilinks_keep_the_vault_wide_rule() {
    // `[[b]]` does not start in the note's folder: the root file wins, as before.
    let files = [
        ("sub/a.md", "[[b]]\n"),
        ("sub/b.md", "# sub b\n"),
        ("b.md", "# root b\n"),
    ];
    assert_eq!(resolves_to(&files, "sub/a.md"), Some("b.md".into()));
}

#[test]
fn notes_at_the_vault_root_resolve_like_before() {
    let files = [
        ("a.md", "[t](b.md)\n"),
        ("b.md", "# b\n"),
        ("sub/b.md", "# sub\n"),
    ];
    assert_eq!(resolves_to(&files, "a.md"), Some("b.md".into()));
    let name_only = [
        ("a.md", "[t](deep/where-ever.md)\n"),
        ("x/where-ever.md", "# w\n"),
    ];
    // Not at that path, but the file name still finds it (existing fallback).
    assert_eq!(
        resolves_to(&name_only, "a.md"),
        Some("x/where-ever.md".into())
    );
}
