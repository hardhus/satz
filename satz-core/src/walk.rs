use anyhow::{Result, bail};
use ignore::WalkBuilder;
use rayon::prelude::*;
use std::path::{Path, PathBuf};

use crate::model::Document;
use crate::parser::parse_document;

pub const DEFAULT_IGNORED_DIRS: &[&str] = &[
    ".git",
    ".obsidian",
    "node_modules",
    ".trash",
    ".stversions",
    ".svn",
    ".hg",
];

/// Whether `path` names a note: a `.md` or `.markdown` file (any case).
pub fn is_markdown_path(path: &Path) -> bool {
    path.extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("md") || ext.eq_ignore_ascii_case("markdown"))
}

pub fn is_ignored_entry(path: &Path, root: &Path) -> bool {
    let rel = path.strip_prefix(root).unwrap_or(path);
    for c in rel.components() {
        let s = c.as_os_str().to_string_lossy();
        if DEFAULT_IGNORED_DIRS
            .iter()
            .any(|d| s.eq_ignore_ascii_case(d))
        {
            return true;
        }
    }
    false
}

/// Traverses the given `vault_root` path respecting `.gitignore` rules and parses all `.md` files in parallel.
///
/// Returns a list of `Document`s. Files with read errors or invalid encoding are logged as warnings and skipped.
pub fn walk_vault(vault_root: &Path) -> Result<Vec<Document>> {
    if !vault_root.exists() {
        bail!("vault root does not exist: {}", vault_root.display());
    }
    walk_subtree(vault_root, vault_root)
}

/// Like `walk_vault`, restricted to the folder `dir` inside the vault: the same rules, and the
/// documents' paths stay relative to `vault_root`. Used to index a folder that appeared or moved.
pub fn walk_subtree(vault_root: &Path, dir: &Path) -> Result<Vec<Document>> {
    if !dir.is_dir() {
        bail!("folder does not exist: {}", dir.display());
    }

    let walker = WalkBuilder::new(dir)
        .hidden(false)
        .git_ignore(true)
        .git_global(true)
        .follow_links(false)
        // Ignored folders (`.git`, `node_modules`, ...) are not entered at all, instead of being
        // walked completely and filtered out afterwards. The vault root itself is never pruned.
        .filter_entry(|entry| {
            entry.depth() == 0
                || !entry.file_type().is_some_and(|ft| ft.is_dir())
                || !DEFAULT_IGNORED_DIRS
                    .iter()
                    .any(|d| entry.file_name().to_string_lossy().eq_ignore_ascii_case(d))
        })
        .sort_by_file_name(|a, b| a.cmp(b))
        .build();

    let mut md_paths: Vec<PathBuf> = Vec::new();

    for result in walker {
        match result {
            Ok(entry) => {
                let path = entry.path();
                if is_ignored_entry(path, vault_root) {
                    continue;
                }
                if entry.file_type().is_some_and(|ft| ft.is_file()) && is_markdown_path(path) {
                    md_paths.push(path.to_path_buf());
                }
            }
            Err(e) => {
                tracing::warn!("error traversing vault entry: {}", e);
            }
        }
    }

    // The same order on every platform and file system: by path relative to the vault.
    md_paths.sort_by_cached_key(|path| {
        path.strip_prefix(vault_root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/")
    });

    let docs: Vec<Document> = md_paths
        .par_iter()
        .filter_map(|path| match std::fs::read_to_string(path) {
            Ok(source) => {
                let rel_path = path.strip_prefix(vault_root).unwrap_or(path);
                Some(parse_document(&source, rel_path))
            }
            Err(e) => {
                tracing::warn!("failed to read markdown file {}: {}", path.display(), e);
                None
            }
        })
        .collect();

    Ok(docs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_walk_nonexistent_path() {
        let path = Path::new("nonexistent_vault_dir_12345");
        assert!(walk_vault(path).is_err());
    }

    /// A throw-away directory tree under the system temp dir, removed on drop.
    struct Tree(PathBuf);

    impl Tree {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("satz-walk-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Tree(dir)
        }

        fn write(&self, rel: &str, text: &str) {
            let path = self.0.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }

        /// Relative paths (with `/`) of the documents `walk_vault` returns, in the order returned.
        fn walk(&self) -> Vec<String> {
            walk_vault(&self.0)
                .unwrap()
                .iter()
                .map(|d| d.path.to_string_lossy().replace('\\', "/"))
                .collect()
        }
    }

    impl Drop for Tree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn test_walk_fixtures_dir() {
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let docs = walk_vault(&fixtures).expect("fixtures dir should exist");
        let mut found: Vec<String> = docs
            .iter()
            .map(|d| d.path.to_string_lossy().replace('\\', "/"))
            .collect();
        found.sort();
        assert_eq!(
            found,
            vec![
                "book_note.md",
                "daily_note.md",
                "edge_case.md",
                "mixed_style_note.md",
                "obsidian/Inbox/fleeting.md",
                "obsidian/Literature/concept.md",
                "obsidian/MOC.md",
                "table_note.md",
                "tractatus_style.md",
                "vault/Gunluk/2026-08-27.md",
                "vault/frontmatter_test.md",
                "vault/main.md",
                "zettel/Ana Dizin.md",
                "zettel/Gunluk/2026-08-27.md",
                "zettel/Kavramlar/LSP.md",
                "zettel/Unutulmus Fikir.md",
            ]
        );
    }

    #[test]
    fn ignored_folders_are_skipped_at_every_depth_whatever_their_case() {
        let t = Tree::new("ignored");
        t.write("keep.md", "# keep\n");
        for dir in [
            ".git",
            ".obsidian",
            "node_modules",
            ".trash",
            ".TRASH",
            ".stversions",
            ".svn",
            ".hg",
            "Node_Modules",
            "a/node_modules",
            "a/b/.git",
            ".obsidian/plugins/x",
        ] {
            t.write(&format!("{dir}/hidden.md"), "# hidden\n");
        }
        assert_eq!(t.walk(), vec!["keep.md"]);
    }

    #[test]
    fn names_that_only_resemble_an_ignored_folder_are_kept() {
        let t = Tree::new("lookalikes");
        t.write("node_modules.md", "# a note about modules\n");
        t.write("notes.git/x.md", "# in a folder ending in .git\n");
        t.write("my.obsidian/y.md", "# y\n");
        t.write("git/z.md", "# z\n");
        t.write("UPPER.MD", "# upper extension\n");
        t.write("readme.txt", "not markdown\n");
        assert_eq!(
            t.walk(),
            vec![
                "UPPER.MD",
                "git/z.md",
                "my.obsidian/y.md",
                "node_modules.md",
                "notes.git/x.md",
            ]
        );
    }

    #[test]
    fn a_vault_whose_own_folder_has_an_ignored_name_is_still_walked() {
        let t = Tree::new("rootname");
        let root = t.0.join(".git");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("inside.md"), "# inside\n").unwrap();
        let docs = walk_vault(&root).unwrap();
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].path, Path::new("inside.md"));
    }

    #[test]
    fn the_order_is_the_same_on_every_run_and_sorted_by_path() {
        let t = Tree::new("order");
        for name in [
            "z.md", "a.md", "m/b.md", "m/a.md", "B.md", "ç.md", "10.md", "9.md",
        ] {
            t.write(name, "# n\n");
        }
        let first = t.walk();
        let mut sorted = first.clone();
        sorted.sort();
        assert_eq!(first, sorted);
        for _ in 0..5 {
            assert_eq!(t.walk(), first);
        }
    }

    #[test]
    fn an_unreadable_file_is_skipped_and_the_others_are_returned() {
        let t = Tree::new("badutf8");
        t.write("good.md", "# good\n");
        std::fs::write(t.0.join("bad.md"), [0xff, 0xfe, 0x00, 0xc3, 0x28]).unwrap();
        t.write("other.md", "# other\n");
        assert_eq!(t.walk(), vec!["good.md", "other.md"]);
    }

    #[test]
    fn a_huge_ignored_folder_does_not_slow_the_walk_down() {
        let t = Tree::new("huge");
        t.write("keep.md", "# keep\n");
        for i in 0..1000 {
            t.write(&format!("node_modules/pkg{}/f{i}.md", i % 50), "x\n");
        }
        let start = std::time::Instant::now();
        assert_eq!(t.walk(), vec!["keep.md"]);
        assert!(
            start.elapsed() < std::time::Duration::from_secs(5),
            "{:?}",
            start.elapsed()
        );
    }

    #[test]
    fn is_ignored_entry_looks_at_every_folder_below_the_vault_root() {
        let root = Path::new("/vault");
        for (path, ignored) in [
            ("/vault/a.md", false),
            ("/vault/.git/config.md", true),
            ("/vault/a/node_modules/x.md", true),
            ("/vault/a/NODE_MODULES/x.md", true),
            ("/vault/.Obsidian/x.md", true),
            ("/vault/node_modules.md", false),
            ("/vault/notes.git/x.md", false),
            ("/vault/git/x.md", false),
            ("/vault", false),
            // Outside the root the whole path is looked at.
            ("/elsewhere/.git/x.md", true),
            ("/elsewhere/x.md", false),
        ] {
            assert_eq!(is_ignored_entry(Path::new(path), root), ignored, "{path}");
        }
        // The vault's own folder name is not part of the relative path.
        assert!(!is_ignored_entry(
            Path::new("/home/.git/vault/a.md"),
            Path::new("/home/.git/vault")
        ));
    }

    #[test]
    fn markdown_and_md_extensions_are_both_notes() {
        let t = Tree::new("exts");
        t.write("a.md", "# a\n");
        t.write("b.markdown", "# b\n");
        t.write("c.MARKDOWN", "# c\n");
        t.write("d.mdx", "not a note\n");
        t.write("e.txt", "not a note\n");
        assert_eq!(t.walk(), vec!["a.md", "b.markdown", "c.MARKDOWN"]);
        assert!(is_markdown_path(Path::new("x/y.MD")));
        assert!(is_markdown_path(Path::new("y.markdown")));
        assert!(!is_markdown_path(Path::new("y.mdx")));
        assert!(!is_markdown_path(Path::new("md")));
        assert!(!is_markdown_path(Path::new(".md")));
    }

    #[test]
    fn a_subtree_is_walked_with_paths_relative_to_the_vault() {
        let t = Tree::new("subtree");
        t.write("top.md", "# top\n");
        t.write("sub/one.md", "# one\n");
        t.write("sub/deep/two.md", "# two\n");
        t.write("sub/node_modules/skip.md", "# skip\n");
        t.write("other/three.md", "# three\n");
        let docs = walk_subtree(&t.0, &t.0.join("sub")).unwrap();
        let mut found: Vec<String> = docs
            .iter()
            .map(|d| d.path.to_string_lossy().replace('\\', "/"))
            .collect();
        found.sort();
        assert_eq!(found, vec!["sub/deep/two.md", "sub/one.md"]);
        // The whole vault is the subtree that starts at the root.
        assert_eq!(walk_subtree(&t.0, &t.0).unwrap().len(), 4);
        // A folder that does not exist is an error, like a missing vault.
        assert!(walk_subtree(&t.0, &t.0.join("missing")).is_err());
    }
}
