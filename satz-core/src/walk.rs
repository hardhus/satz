use anyhow::{Result, bail};
use ignore::WalkBuilder;
use rayon::prelude::*;
use std::path::{Path, PathBuf};

use crate::model::Document;
use crate::parser::parse_document_owned;

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

/// How many threads read and parse the notes of a vault on a machine with `cores` cores. Reading a
/// note waits on the disk (a network or encrypted drive takes milliseconds per file), so far more
/// threads than cores keep the disk busy; between 8 and 64.
pub(crate) fn io_threads_for(cores: usize) -> usize {
    cores.saturating_mul(4).clamp(8, 64)
}

/// The pool that reads and parses notes: sized for waiting on the disk (see `io_threads_for`), made
/// once. `None` if the threads could not be started; the caller then uses rayon's global pool.
fn io_pool() -> Option<&'static rayon::ThreadPool> {
    static POOL: std::sync::OnceLock<Option<rayon::ThreadPool>> = std::sync::OnceLock::new();
    POOL.get_or_init(|| {
        let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
        rayon::ThreadPoolBuilder::new()
            .num_threads(io_threads_for(cores))
            .thread_name(|i| format!("satz-io-{i}"))
            .build()
            .map_err(|e| tracing::warn!("cannot start the note reading threads: {e}"))
            .ok()
    })
    .as_ref()
}

/// Whether the git ignore rules (`.gitignore` files, git's global ignore file) count in a vault that
/// is not inside a git repository (`.satz.toml`: `[vault] gitignore`). `.ignore` files are read
/// either way.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GitignoreMode {
    /// Only inside a git repository (a `.git` folder in the vault or above it): a `.gitignore` in a
    /// folder that is no repository has no effect. What the walk always did, and the default:
    /// notes never disappear because of a `.gitignore` nobody meant for them.
    #[default]
    InRepo,
    /// Also when there is no repository. The rules of `.gitignore` files ABOVE the vault apply too,
    /// as they would inside a repository.
    Always,
}

/// Traverses the given `vault_root` path and parses all `.md` files in parallel, with the default
/// `GitignoreMode` (see there).
///
/// Returns a list of `Document`s. Files with read errors or invalid encoding are logged as warnings and skipped.
pub fn walk_vault(vault_root: &Path) -> Result<Vec<Document>> {
    walk_vault_with(vault_root, GitignoreMode::default())
}

/// `walk_vault` with the given `GitignoreMode`.
pub fn walk_vault_with(vault_root: &Path, gitignore: GitignoreMode) -> Result<Vec<Document>> {
    if !vault_root.exists() {
        bail!("vault root does not exist: {}", vault_root.display());
    }
    walk_subtree_with(vault_root, vault_root, gitignore)
}

/// Like `walk_vault`, restricted to the folder `dir` inside the vault: the same rules, and the
/// documents' paths stay relative to `vault_root`. Used to index a folder that appeared or moved.
pub fn walk_subtree(vault_root: &Path, dir: &Path) -> Result<Vec<Document>> {
    walk_subtree_with(vault_root, dir, GitignoreMode::default())
}

/// `walk_subtree` with the given `GitignoreMode`.
pub fn walk_subtree_with(
    vault_root: &Path,
    dir: &Path,
    gitignore: GitignoreMode,
) -> Result<Vec<Document>> {
    if !dir.is_dir() {
        bail!("folder does not exist: {}", dir.display());
    }

    let walker = WalkBuilder::new(dir)
        .hidden(false)
        .git_ignore(true)
        .git_global(true)
        .require_git(gitignore == GitignoreMode::InRepo)
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

    let discovery_started = std::time::Instant::now();
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

    let discovered = md_paths.len();
    let discovery_time = discovery_started.elapsed();

    let read_started = std::time::Instant::now();
    let read_all = || -> Vec<Document> {
        md_paths
            .par_iter()
            .filter_map(|path| match std::fs::read_to_string(path) {
                Ok(source) => {
                    let rel_path = path.strip_prefix(vault_root).unwrap_or(path);
                    Some(parse_document_owned(source, rel_path))
                }
                Err(e) => {
                    tracing::warn!("failed to read markdown file {}: {}", path.display(), e);
                    None
                }
            })
            .collect()
    };
    let docs = match io_pool() {
        Some(pool) => pool.install(read_all),
        None => read_all(),
    };
    tracing::debug!(
        notes = discovered,
        discovery = ?discovery_time,
        read_and_parse = ?read_started.elapsed(),
        "walk: found the notes, then read and parsed them"
    );

    Ok(docs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wait_on_the_disk_gets_more_threads_than_cores_within_sane_bounds() {
        assert_eq!(
            io_threads_for(0),
            8,
            "an unknown core count still gets the minimum"
        );
        assert_eq!(io_threads_for(1), 8);
        assert_eq!(io_threads_for(2), 8);
        assert_eq!(io_threads_for(4), 16);
        assert_eq!(io_threads_for(12), 48);
        assert_eq!(io_threads_for(16), 64);
        assert_eq!(io_threads_for(128), 64, "never more than 64");
    }

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
    fn a_gitignore_applies_only_inside_a_git_repository_and_an_ignore_file_always() {
        // Git's ignore rules need a repository (the `ignore` crate's default): a `.gitignore` in a
        // folder that is no repository does nothing. docs/cli.md says so.
        let plain = Tree::new("gitignore_plain");
        plain.write("keep.md", "# keep\n");
        plain.write("secret.md", "# secret\n");
        plain.write(".gitignore", "secret.md\n");
        assert_eq!(plain.walk(), vec!["keep.md", "secret.md"]);

        let repo = Tree::new("gitignore_repo");
        repo.write("keep.md", "# keep\n");
        repo.write("secret.md", "# secret\n");
        repo.write("sub/also-secret.md", "# secret\n");
        repo.write(".gitignore", "secret.md\nalso-secret.md\n");
        repo.write(".git/HEAD", "ref: refs/heads/main\n");
        assert_eq!(repo.walk(), vec!["keep.md"]);

        // A `.ignore` file is read whether or not there is a repository.
        let dot_ignore = Tree::new("dot_ignore");
        dot_ignore.write("keep.md", "# keep\n");
        dot_ignore.write("secret.md", "# secret\n");
        dot_ignore.write(".ignore", "secret.md\n");
        assert_eq!(dot_ignore.walk(), vec!["keep.md"]);
    }

    /// Relative paths of the notes under `root` (a vault that may sit inside a bigger tree).
    fn walk_at(root: &Path, mode: GitignoreMode) -> Vec<String> {
        walk_vault_with(root, mode)
            .unwrap()
            .iter()
            .map(|d| d.path.to_string_lossy().replace('\\', "/"))
            .collect()
    }

    #[test]
    fn the_default_mode_is_the_one_that_needs_a_repository() {
        assert_eq!(GitignoreMode::default(), GitignoreMode::InRepo);
        let t = Tree::new("mode_default");
        t.write("keep.md", "# keep\n");
        t.write("secret.md", "# secret\n");
        t.write(".gitignore", "secret.md\n");
        assert_eq!(
            walk_at(&t.0, GitignoreMode::InRepo),
            t.walk(),
            "`walk_vault` is `walk_vault_with` the default mode"
        );
        assert_eq!(t.walk(), vec!["keep.md", "secret.md"]);
    }

    #[test]
    fn always_applies_a_gitignore_without_a_repository() {
        let t = Tree::new("mode_always");
        t.write("keep.md", "# keep\n");
        t.write("secret.md", "# secret\n");
        t.write(".gitignore", "secret.md\n");
        assert_eq!(walk_at(&t.0, GitignoreMode::Always), vec!["keep.md"]);
        assert_eq!(
            walk_at(&t.0, GitignoreMode::InRepo),
            vec!["keep.md", "secret.md"]
        );
    }

    #[test]
    fn a_gitignore_in_a_folder_counts_only_below_that_folder() {
        let t = Tree::new("mode_nested");
        t.write("x.md", "# x at the root\n");
        t.write("sub/x.md", "# x in sub\n");
        t.write("sub/keep.md", "# keep\n");
        t.write("sub/.gitignore", "x.md\n");
        assert_eq!(
            walk_at(&t.0, GitignoreMode::Always),
            vec!["sub/keep.md", "x.md"]
        );
    }

    #[test]
    fn always_also_applies_the_gitignore_of_a_folder_above_the_vault() {
        // The rule the vault's owner may never have meant for it -- why this is not the default.
        let t = Tree::new("mode_above");
        t.write(".gitignore", "secret.md\n");
        t.write("vault/keep.md", "# keep\n");
        t.write("vault/secret.md", "# secret\n");
        let vault = t.0.join("vault");
        assert_eq!(walk_at(&vault, GitignoreMode::Always), vec!["keep.md"]);
        assert_eq!(
            walk_at(&vault, GitignoreMode::InRepo),
            vec!["keep.md", "secret.md"]
        );
    }

    #[test]
    fn inside_a_git_repository_both_modes_give_the_same_notes() {
        // A repository at the vault itself, and one above it.
        let own = Tree::new("mode_repo_own");
        own.write(".git/HEAD", "ref: refs/heads/main\n");
        own.write(".gitignore", "secret.md\n");
        own.write("keep.md", "# keep\n");
        own.write("secret.md", "# secret\n");
        assert_eq!(walk_at(&own.0, GitignoreMode::InRepo), vec!["keep.md"]);
        assert_eq!(walk_at(&own.0, GitignoreMode::Always), vec!["keep.md"]);

        let above = Tree::new("mode_repo_above");
        above.write(".git/HEAD", "ref: refs/heads/main\n");
        above.write(".gitignore", "secret.md\n");
        above.write("vault/keep.md", "# keep\n");
        above.write("vault/secret.md", "# secret\n");
        let vault = above.0.join("vault");
        assert_eq!(walk_at(&vault, GitignoreMode::InRepo), vec!["keep.md"]);
        assert_eq!(walk_at(&vault, GitignoreMode::Always), vec!["keep.md"]);
    }

    #[test]
    fn a_folder_walked_on_its_own_follows_the_mode_too() {
        let t = Tree::new("mode_subtree");
        t.write(".gitignore", "secret.md\n");
        t.write("sub/ok.md", "# ok\n");
        t.write("sub/secret.md", "# secret\n");
        let sub = t.0.join("sub");
        let names = |mode| -> Vec<String> {
            walk_subtree_with(&t.0, &sub, mode)
                .unwrap()
                .iter()
                .map(|d| d.path.to_string_lossy().replace('\\', "/"))
                .collect()
        };
        assert_eq!(names(GitignoreMode::Always), vec!["sub/ok.md"]);
        assert_eq!(
            names(GitignoreMode::InRepo),
            vec!["sub/ok.md", "sub/secret.md"]
        );
        assert_eq!(
            walk_subtree(&t.0, &sub).unwrap().len(),
            2,
            "`walk_subtree` uses the default mode"
        );
    }

    #[test]
    fn an_ignore_file_counts_in_both_modes() {
        let t = Tree::new("mode_dot_ignore");
        t.write("keep.md", "# keep\n");
        t.write("secret.md", "# secret\n");
        t.write(".ignore", "secret.md\n");
        assert_eq!(walk_at(&t.0, GitignoreMode::InRepo), vec!["keep.md"]);
        assert_eq!(walk_at(&t.0, GitignoreMode::Always), vec!["keep.md"]);
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

    #[test]
    fn many_notes_are_read_in_a_fixed_order_with_their_content_intact() {
        let tree = Tree::new("io-pool");
        for i in 0..300 {
            let path = tree.0.join(format!("d{}/n{i:03}.md", i % 7));
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(
                &path,
                format!("# Note {i}\n\nlink [[n{:03}]]\n", (i + 1) % 300),
            )
            .unwrap();
        }
        std::fs::write(tree.0.join("bad.md"), [0xff, 0xfe, 0x00, 0x9f]).unwrap(); // not UTF-8
        let first = walk_vault(&tree.0).unwrap();
        assert_eq!(
            first.len(),
            300,
            "the unreadable note is skipped, the rest is all there"
        );
        let ids: Vec<&str> = first.iter().map(|d| d.id.as_str()).collect();
        let mut sorted = ids.clone();
        sorted.sort();
        assert_eq!(ids, sorted, "in the order of their paths");
        for doc in &first {
            let n: usize = doc.title.trim_start_matches("Note ").parse().unwrap();
            assert_eq!(doc.links.len(), 1, "{}: {n}", doc.id);
        }
        let second = walk_vault(&tree.0).unwrap();
        assert_eq!(
            first.iter().map(|d| d.content_hash).collect::<Vec<_>>(),
            second.iter().map(|d| d.content_hash).collect::<Vec<_>>(),
            "the same every time"
        );
    }

    #[test]
    fn an_empty_vault_and_a_single_note_work_with_the_reading_pool() {
        let tree = Tree::new("io-pool-small");
        assert!(walk_vault(&tree.0).unwrap().is_empty());
        std::fs::write(tree.0.join("only.md"), "# Only\n").unwrap();
        assert_eq!(walk_vault(&tree.0).unwrap().len(), 1);
    }
}
