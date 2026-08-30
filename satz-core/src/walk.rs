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

    let walker = WalkBuilder::new(vault_root)
        .hidden(false)
        .git_ignore(true)
        .git_global(true)
        .follow_links(false)
        .build();

    let mut md_paths: Vec<PathBuf> = Vec::new();

    for result in walker {
        match result {
            Ok(entry) => {
                let path = entry.path();
                if is_ignored_entry(path, vault_root) {
                    continue;
                }
                if entry.file_type().is_some_and(|ft| ft.is_file())
                    && path
                        .extension()
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("md"))
                {
                    md_paths.push(path.to_path_buf());
                }
            }
            Err(e) => {
                tracing::warn!("error traversing vault entry: {}", e);
            }
        }
    }

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

    #[test]
    fn test_walk_fixtures_dir() {
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let docs = walk_vault(&fixtures).expect("fixtures dir should exist");
        assert!(docs.len() >= 4);
    }
}
