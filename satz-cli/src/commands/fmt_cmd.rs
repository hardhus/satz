use std::fs;
use std::path::PathBuf;
use std::time::Instant;

use anyhow::{Context, Result};
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
    changed: bool,
}

pub fn run(args: FmtArgs) -> Result<()> {
    let vault_root = fs::canonicalize(&args.path).unwrap_or_else(|_| args.path.clone());

    let config_path = vault_root.join(".satz.toml");
    let config = if config_path.exists() {
        let content = fs::read_to_string(&config_path)
            .with_context(|| format!("Failed to read config at {}", config_path.display()))?;
        VaultConfig::from_toml(&content).unwrap_or_default()
    } else {
        VaultConfig::default()
    };

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
            if changed && !check_only {
                let abs_path = vault_root.join(&doc.path);
                if let Err(e) = fs::write(&abs_path, &formatted) {
                    tracing::warn!("failed to write {}: {}", abs_path.display(), e);
                }
            }

            FileResult {
                rel_path: doc.path.clone(),
                changed,
            }
        })
        .collect();

    results.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));

    let changed_count = results.iter().filter(|r| r.changed).count();
    let clean_count = results.len() - changed_count;
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

    Ok(())
}
