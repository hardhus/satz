use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::Local;
use clap::Args;
use satz_core::config::VaultConfig;

#[derive(Args, Debug)]
pub struct DailyArgs {
    /// Path to the vault root (defaults to current directory)
    #[arg(default_value = ".")]
    pub path: PathBuf,

    /// Create the daily note file if it doesn't already exist
    #[arg(short, long, default_value_t = true)]
    pub create: bool,
}

pub fn run(args: DailyArgs) -> Result<()> {
    let vault_root = fs::canonicalize(&args.path).unwrap_or(args.path);

    // Read config if present
    let config_path = vault_root.join(".satz.toml");
    let config = if config_path.exists() {
        let content = fs::read_to_string(&config_path)
            .with_context(|| format!("Failed to read config at {}", config_path.display()))?;
        VaultConfig::from_toml(&content).unwrap_or_default()
    } else {
        VaultConfig::default()
    };

    let now = Local::now();
    let formatted_date = now.format(&config.daily_note.format).to_string();

    let filename = if formatted_date.ends_with(".md") {
        formatted_date
    } else {
        format!("{}.md", formatted_date)
    };

    let target_dir = if config.daily_note.folder.is_empty() {
        vault_root.clone()
    } else {
        vault_root.join(&config.daily_note.folder)
    };

    let target_file = target_dir.join(&filename);

    if args.create && !target_file.exists() {
        fs::create_dir_all(&target_dir)
            .with_context(|| format!("Failed to create directory {}", target_dir.display()))?;

        let title = filename.trim_end_matches(".md");
        let initial_content = format!(
            "---\ntitle: {}\ndate: {}\n---\n\n# {}\n\n",
            title,
            now.format("%Y-%m-%d"),
            title
        );

        fs::write(&target_file, initial_content)
            .with_context(|| format!("Failed to write daily note at {}", target_file.display()))?;
    }

    println!("{}", target_file.display());
    Ok(())
}
