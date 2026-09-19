pub mod daily_cmd;
pub mod fmt_cmd;
pub mod graph_cmd;
pub mod index_cmd;
pub mod list_cmd;
pub mod resolve_cmd;
pub mod stats_cmd;

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

/// Resolves the `path` argument of a command that works on a vault (`fmt`, `daily`).
///
/// The vault root must be an existing DIRECTORY: `.satz.toml` is looked up inside it, so a file
/// path would silently skip the config, and a missing path must never be created behind the
/// user's back. The returned path is absolute and, on Windows, free of the `\\?\` prefix that
/// `canonicalize` adds (which shells and editors don't handle well).
pub(crate) fn vault_dir(path: &Path) -> Result<PathBuf> {
    let canonical = match std::fs::canonicalize(path) {
        Ok(p) => p,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            bail!("vault path does not exist: {}", path.display())
        }
        Err(e) => bail!("cannot access vault path {}: {}", path.display(), e),
    };
    if !canonical.is_dir() {
        bail!(
            "expected a vault directory, but {} is not a directory",
            path.display()
        );
    }
    Ok(match canonical.to_str() {
        Some(s) => PathBuf::from(strip_verbatim_prefix(s)),
        None => canonical,
    })
}

/// Turns a Windows extended-length path into its ordinary form: `\\?\C:\x` -> `C:\x` and
/// `\\?\UNC\srv\share\x` -> `\\srv\share\x`. Anything else (including other `\\?\` forms such as
/// `\\?\Volume{..}\`, ordinary paths, and non-Windows paths) is returned unchanged.
pub(crate) fn strip_verbatim_prefix(path: &str) -> String {
    if let Some(rest) = path.strip_prefix(r"\\?\UNC\")
        && !rest.is_empty()
    {
        return format!(r"\\{rest}");
    }
    if let Some(rest) = path.strip_prefix(r"\\?\") {
        let b = rest.as_bytes();
        if b.len() >= 2
            && b[0].is_ascii_alphabetic()
            && b[1] == b':'
            && (b.len() == 2 || b[2] == b'\\')
        {
            return rest.to_string();
        }
    }
    path.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_verbatim_prefix_table() {
        for (input, expected) in [
            (r"\\?\C:\a\b", r"C:\a\b"),
            (r"\\?\c:\a", r"c:\a"),
            (r"\\?\C:", "C:"),
            (r"\\?\UNC\srv\share\x", r"\\srv\share\x"),
            // Everything else is left alone.
            (r"C:\a\b", r"C:\a\b"),
            ("/unix/path", "/unix/path"),
            (r"\\srv\share", r"\\srv\share"),
            (r"\\?\Volume{1234}\x", r"\\?\Volume{1234}\x"),
            (r"\\?\C:relative", r"\\?\C:relative"),
            (r"\\?\1:\x", r"\\?\1:\x"),
            (r"\\?\UNC\", r"\\?\UNC\"),
            (r"\\?\", r"\\?\"),
            ("", ""),
        ] {
            assert_eq!(strip_verbatim_prefix(input), expected, "input: {input:?}");
            // Applying it again must not change anything.
            assert_eq!(
                strip_verbatim_prefix(expected),
                strip_verbatim_prefix(&strip_verbatim_prefix(input)),
                "not idempotent for {input:?}"
            );
        }
    }

    fn temp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("satz_vd_{tag}_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn vault_dir_accepts_a_directory_and_returns_it_without_verbatim_prefix() {
        let dir = temp("ok");
        let got = vault_dir(&dir).unwrap();
        assert!(got.is_dir());
        assert!(got.is_absolute());
        assert!(!got.to_string_lossy().starts_with(r"\\?\"), "{got:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn vault_dir_rejects_a_file_and_a_missing_path() {
        let dir = temp("bad");
        let file = dir.join("note.md");
        std::fs::write(&file, "# x").unwrap();

        let msg = vault_dir(&file).unwrap_err().to_string();
        assert!(msg.contains("not a directory"), "{msg}");

        let missing = dir.join("nope");
        let msg = vault_dir(&missing).unwrap_err().to_string();
        assert!(msg.contains("does not exist"), "{msg}");
        assert!(!missing.exists(), "must not create the path");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
