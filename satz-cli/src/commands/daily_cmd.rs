use std::fs;
use std::path::PathBuf;

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
    #[arg(short, long, default_value_t = true, action = ArgAction::Set)]
    pub create: bool,
}

pub fn run(args: DailyArgs) -> Result<()> {
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

    if args.create && !target_file.exists() {
        // `format` may contain `/` (e.g. `%Y/%m/%d`), so the note's own parent directory is
        // created, not just the configured folder.
        fs::create_dir_all(&target_dir)
            .with_context(|| format!("Failed to create directory {}", target_dir.display()))?;

        let title = filename.trim_end_matches(".md");
        let date_str = now.format("%Y-%m-%d").to_string();
        let initial_content = satz_core::generate_document_template(title, Some(&date_str));

        fs::write(&target_file, initial_content)
            .with_context(|| format!("Failed to write daily note at {}", target_file.display()))?;
    }

    println!("{}", target_file.display());
    Ok(())
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
    use super::relative_parts;

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
