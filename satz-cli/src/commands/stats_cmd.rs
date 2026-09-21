use anyhow::Result;
use std::io::Write;
use std::path::PathBuf;

#[derive(clap::Args, Debug)]
pub struct StatsArgs {
    /// Vault root directory
    #[arg(long, short = 'v', default_value = ".")]
    pub vault: PathBuf,

    /// Output stats as JSON
    #[arg(long)]
    pub json: bool,
}

pub fn run(args: StatsArgs) -> Result<()> {
    super::with_stdout(|out| run_with_output(args, out))
}

/// `run`, writing what `satz stats` prints to `out`.
pub fn run_with_output(args: StatsArgs, out: &mut dyn Write) -> Result<()> {
    let index = super::load_index(&args.vault)?;
    let stats = index.stats();

    if args.json {
        writeln!(out, "{}", serde_json::to_string_pretty(&stats)?)?;
    } else {
        writeln!(out, "Vault Stats: {}", args.vault.display())?;
        writeln!(out, "  Documents:    {}", stats.doc_count)?;
        writeln!(out, "  Total links:  {}", stats.total_links)?;
        writeln!(out, "  Broken links: {}", stats.broken_links)?;
        writeln!(out, "  Unique tags:  {}", stats.unique_tags)?;
        writeln!(out, "  Orphan docs:  {}", stats.orphan_docs)?;
        writeln!(out, "  Headings:     {}", stats.total_headings)?;
        writeln!(out, "  ~Words:       {}", format_number(stats.total_words))?;
    }

    Ok(())
}

fn format_number(n: usize) -> String {
    let s = n.to_string();
    let mut result = String::new();
    for (i, c) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            result.push(',');
        }
        result.push(c);
    }
    result.chars().rev().collect()
}
