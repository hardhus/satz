pub mod daily_cmd;
pub mod fmt_cmd;
pub mod graph_cmd;
pub mod index_cmd;
pub mod list_cmd;
pub mod resolve_cmd;
pub mod stats_cmd;

use std::io::{self, Write};
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

/// A writer that takes a closed pipe in stride: once the reader is gone (`satz list | head`), the
/// rest of the output goes nowhere and the command carries on to its own result and exit code.
/// `println!` would panic there (exit code 101). Every other write error is passed on.
pub(crate) struct QuietPipe<W: Write> {
    inner: W,
    closed: bool,
}

impl<W: Write> QuietPipe<W> {
    pub(crate) fn new(inner: W) -> Self {
        Self {
            inner,
            closed: false,
        }
    }

    /// Runs one write on the inner writer unless the pipe is closed already; a broken pipe closes
    /// it and counts as written.
    fn quietly<T>(
        &mut self,
        on_closed: T,
        write: impl FnOnce(&mut W) -> io::Result<T>,
    ) -> io::Result<T> {
        if self.closed {
            return Ok(on_closed);
        }
        match write(&mut self.inner) {
            Err(e) if e.kind() == io::ErrorKind::BrokenPipe => {
                self.closed = true;
                Ok(on_closed)
            }
            other => other,
        }
    }
}

impl<W: Write> Write for QuietPipe<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.quietly(buf.len(), |inner| inner.write(buf))
    }

    fn flush(&mut self) -> io::Result<()> {
        self.quietly((), |inner| inner.flush())
    }
}

/// Runs `body` with a writer for stdout that survives a reader that went away. The output stays
/// line-buffered (std's), so it comes out in step with what a command writes to stderr.
pub(crate) fn with_stdout<T>(body: impl FnOnce(&mut dyn Write) -> Result<T>) -> Result<T> {
    let mut out = QuietPipe::new(io::stdout().lock());
    let result = body(&mut out);
    let flushed = out.flush();
    let value = result?;
    flushed?;
    Ok(value)
}

/// Validates the vault directory and indexes every note in it: the loading step shared by `index`,
/// `stats`, `list`, `resolve` and `graph`, so all of them report a bad vault path the same way.
///
/// `.satz.toml` is read first: its `[vault]` section decides which files the walk sees, so a file
/// that cannot be used is an error here too (as for `fmt` and `daily`), never a silent fallback.
pub(crate) fn load_index(path: &Path) -> Result<satz_core::Index> {
    let root = vault_dir(path)?;
    let config = load_config(&root)?;
    Ok(satz_core::Index::build(satz_core::walk_vault_with(
        &root,
        config.gitignore_mode(),
    )?))
}

/// Resolves the `path` argument of a command that works on a vault (`fmt`, `daily`).
///
/// The vault root must be an existing DIRECTORY: `.satz.toml` is looked up inside it, so a file
/// path would silently skip the config, and a missing path must never be created behind the
/// user's back. The returned path is absolute and, on Windows, free of the `\\?\` prefix that
/// `canonicalize` adds (which shells and editors don't handle well).
pub(crate) fn vault_dir(path: &Path) -> Result<PathBuf> {
    vault_dir_with(path, |p| std::fs::canonicalize(p))
}

/// `vault_dir` with the way a path is resolved handed in, so it can be tried against the errors a
/// file system may give.
fn vault_dir_with(
    path: &Path,
    canonicalize: impl Fn(&Path) -> io::Result<PathBuf>,
) -> Result<PathBuf> {
    let canonical = match canonicalize(path) {
        Ok(p) => p,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            bail!("vault path does not exist: {}", path.display())
        }
        // Some drives cannot say what a path finally is although the files on them can be used:
        // a virtual drive that is not registered with the mount manager (WinFsp, Dokan) answers
        // `canonicalize` with `os error 1005`. Then the path is made absolute without asking the
        // file system, and the file system is only asked whether it is a directory. What
        // `canonicalize` adds is missing there: links in the path are not resolved, and `..` is
        // folded by its text (on Windows). If the file system cannot be used either, it is the
        // original error that is reported.
        Err(e) => {
            let Ok(absolute) = std::path::absolute(path) else {
                bail!("cannot access vault path {}: {}", path.display(), e)
            };
            match std::fs::metadata(&absolute) {
                Ok(_) => absolute,
                Err(m) if m.kind() == io::ErrorKind::NotFound => {
                    bail!("vault path does not exist: {}", path.display())
                }
                Err(_) => bail!("cannot access vault path {}: {}", path.display(), e),
            }
        }
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

/// Reads `<vault>/.satz.toml`. A file that cannot be read or parsed at all is an error; mistakes
/// inside a readable one (an unknown key, a value outside its choices) are printed as
/// `warning: ...` lines on stderr and the rest of the file applies.
pub(crate) fn load_config(vault_root: &Path) -> Result<satz_core::VaultConfig> {
    let (config, warnings) = satz_core::VaultConfig::load_with_warnings(vault_root)?;
    for warning in warnings {
        eprintln!("warning: {warning}");
    }
    Ok(config)
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

    /// Accepts `room` bytes, then fails every call with `kind`.
    struct Failing {
        kept: Vec<u8>,
        room: usize,
        kind: io::ErrorKind,
    }

    impl Write for Failing {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            if self.kept.len() >= self.room {
                return Err(io::Error::new(self.kind, "reader is gone"));
            }
            let n = buf.len().min(self.room - self.kept.len());
            self.kept.extend_from_slice(&buf[..n]);
            Ok(n)
        }

        fn flush(&mut self) -> io::Result<()> {
            if self.kept.len() >= self.room {
                return Err(io::Error::new(self.kind, "reader is gone"));
            }
            Ok(())
        }
    }

    fn failing(room: usize, kind: io::ErrorKind) -> QuietPipe<Failing> {
        QuietPipe::new(Failing {
            kept: Vec::new(),
            room,
            kind,
        })
    }

    #[test]
    fn a_closed_pipe_swallows_the_rest_of_the_output_without_an_error() {
        let mut out = failing(5, io::ErrorKind::BrokenPipe);
        writeln!(out, "abc").unwrap(); // 4 bytes: fits
        writeln!(out, "defgh").unwrap(); // room for one byte, then the pipe breaks
        writeln!(out, "ijk").unwrap(); // closed: nothing reaches the inner writer, no error
        out.flush().unwrap();
        assert!(out.closed);
        assert_eq!(
            out.inner.kept, b"abc\nd",
            "only what was written before it broke"
        );
    }

    #[test]
    fn any_other_write_error_is_passed_on() {
        for kind in [
            io::ErrorKind::PermissionDenied,
            io::ErrorKind::Other,
            io::ErrorKind::WriteZero,
            io::ErrorKind::OutOfMemory,
        ] {
            let mut out = failing(0, kind);
            let e = writeln!(out, "x").unwrap_err();
            assert_eq!(e.kind(), kind, "a {kind:?} error must not be swallowed");
            assert!(!out.closed, "{kind:?}");
            assert_eq!(out.flush().unwrap_err().kind(), kind, "flush, {kind:?}");
        }
    }

    #[test]
    fn a_pipe_that_broke_reports_every_byte_of_later_writes_as_written() {
        let mut out = failing(0, io::ErrorKind::BrokenPipe);
        assert_eq!(out.write(b"hello").unwrap(), 5);
        assert_eq!(out.write(b"").unwrap(), 0);
        assert_eq!(
            out.write(b"more").unwrap(),
            4,
            "closed: all of it counts as written"
        );
        out.write_all(b"and write_all does not loop for ever")
            .unwrap();
        assert!(out.closed);
    }

    #[test]
    fn what_the_body_of_with_stdout_fails_with_is_kept() {
        let e = with_stdout::<()>(|_| bail!("the command's own error")).unwrap_err();
        assert_eq!(e.to_string(), "the command's own error");
        assert_eq!(with_stdout(|_| Ok(7)).unwrap(), 7);
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

    // ---- a drive that cannot tell its final path (WinFsp, Dokan: os error 1005) ----

    /// What `canonicalize` gives on such a drive: an error that is not `NotFound`.
    fn unrecognized_volume(_: &Path) -> io::Result<PathBuf> {
        Err(io::Error::from_raw_os_error(1005))
    }

    #[test]
    fn a_directory_is_accepted_when_its_final_path_cannot_be_asked_for() {
        let dir = temp("fb_ok");
        let got = vault_dir_with(&dir, unrecognized_volume).unwrap();
        assert!(got.is_dir());
        assert!(got.is_absolute());
        assert!(!got.to_string_lossy().starts_with(r"\?\"), "{got:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_relative_path_becomes_absolute_by_the_fallback() {
        let got = vault_dir_with(Path::new("."), unrecognized_volume).unwrap();
        assert!(got.is_absolute(), "{got:?}");
        assert!(got.is_dir());
        assert_eq!(
            got.canonicalize().unwrap(),
            std::env::current_dir().unwrap().canonicalize().unwrap()
        );
    }

    #[test]
    fn the_fallback_still_rejects_a_file_and_a_missing_path() {
        let dir = temp("fb_bad");
        let file = dir.join("note.md");
        std::fs::write(&file, "# x").unwrap();

        let msg = vault_dir_with(&file, unrecognized_volume)
            .unwrap_err()
            .to_string();
        assert!(msg.contains("not a directory"), "{msg}");

        let missing = dir.join("nope");
        let msg = vault_dir_with(&missing, unrecognized_volume)
            .unwrap_err()
            .to_string();
        assert!(msg.contains("does not exist"), "{msg}");
        assert!(!missing.exists(), "must not create the path");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn any_error_but_not_found_gives_the_fallback_a_try() {
        let dir = temp("fb_kinds");
        for kind in [
            io::ErrorKind::PermissionDenied,
            io::ErrorKind::Other,
            io::ErrorKind::Unsupported,
            io::ErrorKind::InvalidInput,
        ] {
            let got = vault_dir_with(&dir, |_| Err(io::Error::from(kind))).unwrap();
            assert!(got.is_dir(), "{kind:?}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_path_that_was_not_found_is_not_looked_for_again() {
        // `NotFound` from the resolver is final, even for a directory that is there: the fallback
        // is for a resolver that cannot answer, not for one that answered "no".
        let dir = temp("fb_notfound");
        let msg = vault_dir_with(&dir, |_| Err(io::Error::from(io::ErrorKind::NotFound)))
            .unwrap_err()
            .to_string();
        assert!(msg.contains("does not exist"), "{msg}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn when_the_fallback_fails_too_the_original_error_is_reported() {
        // A path with a NUL cannot be looked at by any call: the answer is the resolver's error.
        let msg = vault_dir_with(Path::new("bad path"), unrecognized_volume)
            .unwrap_err()
            .to_string();
        assert!(msg.contains("cannot access vault path"), "{msg}");
        assert!(msg.contains("os error 1005"), "{msg}");
    }

    #[test]
    fn a_resolver_that_works_is_used_as_it_was() {
        let dir = temp("fb_resolver");
        let elsewhere = temp("fb_resolver_target");
        let got = vault_dir_with(&dir, |_| Ok(elsewhere.clone())).unwrap();
        assert_eq!(got, elsewhere, "the resolved path, not the given one");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&elsewhere);
    }

    #[test]
    fn a_path_below_a_file_is_never_a_vault_by_the_fallback() {
        let dir = temp("fb_below_file");
        let file = dir.join("note.md");
        std::fs::write(&file, "# x").unwrap();
        let below = file.join("inside");
        let err = vault_dir_with(&below, unrecognized_volume).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("cannot access vault path") || msg.contains("does not exist"),
            "{msg}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(windows)]
    #[test]
    fn a_name_the_file_system_refuses_is_reported_with_the_original_error() {
        // Not `NotFound` but a refusal: the fallback does not turn it into a vault or into
        // "does not exist".
        let dir = temp("fb_refused");
        let msg = vault_dir_with(&dir.join("a<b"), unrecognized_volume)
            .unwrap_err()
            .to_string();
        assert!(msg.contains("cannot access vault path"), "{msg}");
        assert!(msg.contains("os error 1005"), "{msg}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
