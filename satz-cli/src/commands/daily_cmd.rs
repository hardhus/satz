use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::Local;
use clap::{ArgAction, Args};

#[derive(Args, Debug)]
pub struct DailyArgs {
    /// Path to the vault root directory (defaults to current directory)
    #[arg(default_value = ".")]
    pub path: PathBuf,

    /// Whether to create the daily note file if it doesn't already exist (`--create false` only
    /// prints the path)
    #[arg(short, long, default_value_t = true, action = ArgAction::Set, num_args = 0..=1, default_missing_value = "true")]
    pub create: bool,
}

pub fn run(args: DailyArgs) -> Result<()> {
    super::with_stdout(|out| run_with_output(args, out))
}

/// `run`, writing the path of the daily note to `out`.
pub fn run_with_output(args: DailyArgs, out: &mut dyn Write) -> Result<()> {
    let vault_root = super::vault_dir(&args.path)?;

    // A config that exists but can't be used is an error, never a silent fallback to defaults.
    let config = super::load_config(&vault_root)?;

    let now = Local::now();
    let formatted_date = now.format(&config.daily_note.format).to_string();

    let filename = if formatted_date.ends_with(".md") {
        formatted_date
    } else {
        format!("{}.md", formatted_date)
    };

    // Both settings are joined onto the vault root, so neither may climb out of it: `..` would
    // let a config in a cloned/untrusted vault make `satz daily` write anywhere on disk.
    let mut target_file = vault_root.clone();
    for part in relative_parts("daily_note.folder", &config.daily_note.folder)? {
        target_file.push(part);
    }
    for part in relative_parts("daily_note.format", &filename)? {
        target_file.push(part);
    }
    let target_dir = target_file
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| vault_root.clone());

    if args.create {
        // `format` may contain `/` (e.g. `%Y/%m/%d`), so the note's own parent directory is
        // created, not just the configured folder.
        fs::create_dir_all(&target_dir)
            .with_context(|| format!("Failed to create directory {}", target_dir.display()))?;

        let title = filename.trim_end_matches(".md");
        let date_str = now.format("%Y-%m-%d").to_string();
        let initial_content = satz_core::generate_document_template(title, Some(&date_str));

        // Checking first and writing after would overwrite a note that appears in between.
        create_note(&target_file, &initial_content)
            .with_context(|| format!("Failed to write daily note at {}", target_file.display()))?;
    }

    writeln!(out, "{}", target_file.display())?;
    Ok(())
}

/// Creates the file at `path` with `content` -- only if nothing is there. Returns whether it did.
///
/// Creating and checking are one step (`create_new`): a note that appears while the command runs, a
/// file, a folder, or a link (dangling ones included, which a plain write would go through to
/// wherever they point) is never written to.
fn create_note(path: &Path, content: &str) -> io::Result<bool> {
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(mut file) => {
            file.write_all(content.as_bytes())?;
            Ok(true)
        }
        // Whatever the platform calls it, something is at that path already: leave it.
        Err(_) if path.symlink_metadata().is_ok() => Ok(false),
        Err(e) => Err(e),
    }
}

/// Splits a configured path setting into components that are safe to join onto the vault root.
///
/// Empty components (leading, trailing or doubled separators) are dropped, so an "absolute-looking"
/// value like `/journal` stays inside the vault as `journal`. `.` / `..` and components containing
/// `:` (drive letters, NTFS alternate streams) or NUL are rejected.
fn relative_parts(setting: &str, value: &str) -> Result<Vec<String>> {
    let mut parts = Vec::new();
    for part in value.split(['/', '\\']).filter(|p| !p.is_empty()) {
        if part == "." || part == ".." || part.contains(':') || part.contains('\0') {
            anyhow::bail!("{setting} must stay inside the vault, but {value:?} contains {part:?}");
        }
        parts.push(part.to_string());
    }
    Ok(parts)
}

#[cfg(test)]
mod tests {
    use super::{create_note, relative_parts};
    use std::path::PathBuf;

    /// A unique directory that is removed (read-only files included) when dropped.
    struct Dir(PathBuf);

    impl Dir {
        fn new(tag: &str) -> Self {
            use std::sync::atomic::{AtomicUsize, Ordering};
            static N: AtomicUsize = AtomicUsize::new(0);
            let dir = std::env::temp_dir().join(format!(
                "satz_daily_{tag}_{}_{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            if let Ok(entries) = std::fs::read_dir(&self.0) {
                for e in entries.flatten() {
                    if let Ok(meta) = e.metadata() {
                        let mut permissions = meta.permissions();
                        #[allow(clippy::permissions_set_readonly_false)]
                        permissions.set_readonly(false);
                        let _ = std::fs::set_permissions(e.path(), permissions);
                    }
                }
            }
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn create_note_creates_what_is_not_there_with_all_of_its_content() {
        let d = Dir::new("create");
        let path = d.0.join("new.md");
        assert!(
            create_note(
                &path,
                "# Title

body
"
            )
            .unwrap()
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "# Title

body
"
        );

        // Nothing to write is still a note that now exists.
        let empty = d.0.join("empty.md");
        assert!(create_note(&empty, "").unwrap());
        assert_eq!(std::fs::read(&empty).unwrap(), b"");
    }

    #[test]
    fn create_note_leaves_anything_that_is_there_alone() {
        let d = Dir::new("occupied");

        let file = d.0.join("mine.md");
        std::fs::write(
            &file, "MINE
",
        )
        .unwrap();
        assert!(!create_note(&file, "theirs").unwrap());
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "MINE
"
        );

        let read_only = d.0.join("read-only.md");
        std::fs::write(
            &read_only, "KEEP
",
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&read_only).unwrap().permissions();
        permissions.set_readonly(true);
        std::fs::set_permissions(&read_only, permissions).unwrap();
        assert!(!create_note(&read_only, "theirs").unwrap());
        assert_eq!(
            std::fs::read_to_string(&read_only).unwrap(),
            "KEEP
"
        );

        let folder = d.0.join("folder.md");
        std::fs::create_dir(&folder).unwrap();
        assert!(!create_note(&folder, "theirs").unwrap());
        assert!(folder.is_dir());

        // A second call after a first one that created the note does not create it again.
        let once = d.0.join("once.md");
        assert!(create_note(&once, "first").unwrap());
        assert!(!create_note(&once, "second").unwrap());
        assert_eq!(std::fs::read_to_string(&once).unwrap(), "first");
    }

    #[test]
    fn create_note_reports_a_real_failure_instead_of_saying_it_was_there() {
        let d = Dir::new("failure");
        // The parent folder is missing: nothing is at the path, so this is an error, not "exists".
        let e = create_note(&d.0.join("no-such-folder").join("n.md"), "x").unwrap_err();
        assert_eq!(e.kind(), std::io::ErrorKind::NotFound, "{e}");
    }

    #[test]
    fn relative_parts_splits_and_drops_empty_components() {
        let p = |v: &str| relative_parts("s", v).unwrap();
        assert_eq!(p(""), Vec::<String>::new());
        assert_eq!(p("daily"), ["daily"]);
        assert_eq!(p("2026/09/19.md"), ["2026", "09", "19.md"]);
        assert_eq!(p("a\\b/c"), ["a", "b", "c"]);
        // Absolute-looking values are made relative, never followed.
        assert_eq!(p("/abs/path"), ["abs", "path"]);
        assert_eq!(p("trailing/"), ["trailing"]);
        assert_eq!(p("a//b"), ["a", "b"]);
    }

    #[test]
    fn relative_parts_rejects_anything_that_could_leave_the_vault() {
        for bad in [
            "..",
            "../x",
            "a/../b",
            "a/..",
            "./x",
            "..\\x",
            "C:\\x",
            "C:x",
            "d:",
            "x/y:stream",
            "a\0b",
        ] {
            let err = relative_parts("daily_note.folder", bad)
                .unwrap_err()
                .to_string();
            assert!(err.contains("daily_note.folder"), "{bad:?}: {err}");
            assert!(err.contains("inside the vault"), "{bad:?}: {err}");
        }
    }

    #[test]
    fn relative_parts_accepts_dots_that_are_part_of_a_name() {
        let p = |v: &str| relative_parts("s", v).unwrap();
        assert_eq!(p("v1.2"), ["v1.2"]);
        assert_eq!(p("...hidden"), ["...hidden"]);
        assert_eq!(p("a..b/c"), ["a..b", "c"]);
        assert_eq!(p(".md"), [".md"]);
    }
}
