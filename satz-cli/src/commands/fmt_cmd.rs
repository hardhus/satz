use std::fs;
use std::path::PathBuf;
use std::time::Instant;

use anyhow::{Result, bail};
use clap::Args;
use rayon::prelude::*;
use satz_core::config::VaultConfig;
use satz_core::walk_vault;

#[derive(Args, Debug)]
pub struct FmtArgs {
    /// Vault root directory
    #[arg(default_value = ".")]
    pub path: PathBuf,

    /// Report which files would change without writing anything; exit status 1 if any would.
    #[arg(long, conflicts_with = "write")]
    pub check: bool,

    /// Format files in place. This is the default behavior when neither flag is given.
    #[arg(long, conflicts_with = "check")]
    pub write: bool,
}

struct FileResult {
    rel_path: PathBuf,
    /// The formatted text differs from what is on disk.
    changed: bool,
    /// Set when `changed` and the file could not be written back (`--write` only).
    write_error: Option<String>,
}

pub fn run(args: FmtArgs) -> Result<()> {
    let vault_root = super::vault_dir(&args.path)?;

    // A config that exists but can't be used must stop the run: formatting with defaults would
    // rewrite every file with settings the user didn't choose.
    let config = VaultConfig::load(&vault_root)?;

    if !config.formatter.enabled {
        println!("Formatter is disabled (formatter.enabled = false in .satz.toml); nothing to do.");
        return Ok(());
    }

    let t0 = Instant::now();
    let docs = walk_vault(&vault_root)?;
    let check_only = args.check;

    let mut results: Vec<FileResult> = docs
        .par_iter()
        .map(|doc| {
            let source = doc.line_index.source();
            let formatted = satz_core::formatter::format_document(source, &config.formatter);
            let changed = formatted != source;

            // Skip the write entirely when the file is already formatted — no unnecessary I/O,
            // no mtime churn.
            let mut write_error = None;
            if changed && !check_only {
                let abs_path = vault_root.join(&doc.path);
                if let Err(e) = fs::write(&abs_path, &formatted) {
                    write_error = Some(e.to_string());
                }
            }

            FileResult {
                rel_path: doc.path.clone(),
                changed,
                write_error,
            }
        })
        .collect();

    results.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));

    // Files that could not be written are neither "formatted" nor "clean".
    let changed_count = results
        .iter()
        .filter(|r| r.changed && r.write_error.is_none())
        .count();
    let clean_count = results.iter().filter(|r| !r.changed).count();
    let elapsed = t0.elapsed();

    if check_only {
        for r in results.iter().filter(|r| r.changed) {
            println!("{}", r.rel_path.display());
        }

        if changed_count > 0 {
            eprintln!(
                "✗ {} file(s) need formatting, {} file(s) already clean ({:.0}ms)",
                changed_count,
                clean_count,
                elapsed.as_millis()
            );
            std::process::exit(1);
        }

        println!(
            "✓ all {} file(s) already formatted ({:.0}ms)",
            clean_count,
            elapsed.as_millis()
        );
    } else {
        println!(
            "✓ {} file(s) formatted, {} file(s) already clean ({:.0}ms)",
            changed_count,
            clean_count,
            elapsed.as_millis()
        );
    }

    let failures: Vec<&FileResult> = results.iter().filter(|r| r.write_error.is_some()).collect();
    if !failures.is_empty() {
        for f in &failures {
            eprintln!(
                "error: cannot write {}: {}",
                vault_root.join(&f.rel_path).display(),
                f.write_error.as_deref().unwrap_or("unknown error")
            );
        }
        bail!("{} file(s) could not be written", failures.len());
    }

    Ok(())
}
