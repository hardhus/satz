//! One resolution order for every kind of link target: exact path, path + `.md`, path ignoring
//! case, file stem, then title/alias -- whether or not the target looks like a path.

use satz_core::{Index, parse_document};
use std::path::Path;

fn index_of(files: &[(&str, &str)]) -> Index {
    Index::build(
        files
            .iter()
            .map(|(path, text)| parse_document(text, Path::new(path)))
            .collect(),
    )
}

fn resolve(index: &Index, target: &str) -> Option<String> {
    index.resolve_link(target).map(|id| id.as_str().to_string())
}

#[test]
fn a_file_at_the_vault_root_beats_a_same_named_file_in_a_folder() {
    // `sub/x.md` sorts first and would win a bare stem lookup.
    let index = index_of(&[
        ("sub/x.md", "# Sub"),
        ("x.md", "# Root"),
        ("zz/x.md", "# Z"),
    ]);
    assert_eq!(resolve(&index, "x"), Some("x.md".into()));
    assert_eq!(resolve(&index, "x.md"), Some("x.md".into()));
    assert_eq!(resolve(&index, "sub/x"), Some("sub/x.md".into()));
    // Without a root file the stem still finds one.
    let only_folders = index_of(&[("sub/x.md", "# Sub"), ("zz/x.md", "# Z")]);
    assert!(resolve(&only_folders, "x").is_some());
}

#[test]
fn paths_match_regardless_of_letter_case() {
    let index = index_of(&[("books/rust.md", "# Rust"), ("Notes/İş Notu.md", "# İş")]);
    for target in [
        "books/rust",
        "Books/Rust",
        "BOOKS/RUST.md",
        "books/Rust.MD",
        "books\\rust",
        "Books\\Rust",
    ] {
        assert_eq!(
            resolve(&index, target),
            Some("books/rust.md".into()),
            "{target:?}"
        );
    }
    assert_eq!(
        resolve(&index, "notes/iş notu"),
        Some("Notes/İş Notu.md".into())
    );
}

#[test]
fn surrounding_whitespace_is_not_part_of_the_target() {
    let index = index_of(&[
        ("books/rust.md", "# Rust"),
        ("x.md", "# X"),
        ("z.md", "---\ntitle: My Title\n---\n"),
    ]);
    for target in [
        " books/rust ",
        "\tbooks/rust",
        "books/rust  ",
        " x ",
        "  My Title ",
        " my title",
    ] {
        assert!(resolve(&index, target).is_some(), "{target:?}");
    }
    assert_eq!(resolve(&index, " x "), Some("x.md".into()));
    assert_eq!(resolve(&index, "  My Title "), Some("z.md".into()));
}

#[test]
fn a_dotted_decimal_name_is_a_name_not_an_extension() {
    let index = index_of(&[("tlp/2.md", "# Two"), ("tlp/2.0121.md", "# Long")]);
    assert_eq!(resolve(&index, "tlp/2.0121"), Some("tlp/2.0121.md".into()));
    assert_eq!(resolve(&index, "tlp/2"), Some("tlp/2.md".into()));
    // A dotted target whose file does not exist must not fall back to the chopped stem.
    let only_short = index_of(&[("tlp/2.md", "# Two")]);
    assert_eq!(resolve(&only_short, "tlp/2.0121"), None);
}

#[test]
fn stem_beats_title_and_a_title_still_works_when_nothing_else_matches() {
    let index = index_of(&[
        ("a.md", "---\ntitle: Other\n---\n"),
        ("other.md", "# Different"),
        ("p.md", "---\ntitle: Dr. Smith\naliases: [ds]\n---\n"),
    ]);
    assert_eq!(resolve(&index, "other"), Some("other.md".into()));
    assert_eq!(resolve(&index, "Other"), Some("other.md".into()));
    // Titles and aliases with dots or spaces resolve (they are not paths).
    assert_eq!(resolve(&index, "Dr. Smith"), Some("p.md".into()));
    assert_eq!(resolve(&index, "DS"), Some("p.md".into()));
    assert_eq!(resolve(&index, "nothing here"), None);
}

#[test]
fn an_exact_path_beats_a_folded_or_stem_match() {
    // `Notes.md` and `notes.md` differ only in case; each exact spelling finds its own file.
    let index = index_of(&[("Notes.md", "# Big"), ("notes.md", "# Small")]);
    assert_eq!(resolve(&index, "Notes.md"), Some("Notes.md".into()));
    assert_eq!(resolve(&index, "notes.md"), Some("notes.md".into()));
}

// ---- the short-path rule and dotted targets (documented in docs/lsp.md) ----

#[test]
fn a_wrong_folder_still_finds_the_note_by_its_file_name() {
    // The Obsidian "shortest path" habit: the folder part is a hint, the file name decides.
    let index = index_of(&[(
        "other/note.md",
        "# N
",
    )]);
    assert_eq!(
        resolve(&index, "wrong/note").as_deref(),
        Some("other/note.md")
    );
    assert_eq!(
        resolve(&index, "wrong/deeper/note.md").as_deref(),
        Some("other/note.md")
    );
    assert_eq!(resolve(&index, "wrong/other-name"), None);
}

#[test]
fn a_title_with_dots_and_spaces_is_found_by_that_title() {
    let index = index_of(&[(
        "plans/p1.md",
        "---
title: v1.2 plan
---
# P
",
    )]);
    assert_eq!(resolve(&index, "v1.2 plan").as_deref(), Some("plans/p1.md"));
    assert_eq!(resolve(&index, "V1.2 PLAN").as_deref(), Some("plans/p1.md"));
    assert_eq!(resolve(&index, "v1.2"), None, "a title is matched whole");
}

#[test]
fn a_title_that_looks_like_a_path_is_still_only_a_title() {
    let index = index_of(&[(
        "x.md",
        "---
title: a/b.c
---
# X
",
    )]);
    assert_eq!(resolve(&index, "a/b.c").as_deref(), Some("x.md"));
    assert_eq!(resolve(&index, "b.c"), None);
}
