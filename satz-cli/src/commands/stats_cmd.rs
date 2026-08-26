use anyhow::Result;
use satz_core::{Index, walk_vault};
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
    let docs = walk_vault(&args.vault)?;
    let index = Index::build(docs);
    let stats = index.stats();

    if args.json {
        println!("{}", serde_json::to_string_pretty(&stats)?);
    } else {
        println!("Vault Stats: {}", args.vault.display());
        println!("  Documents:    {}", stats.doc_count);
        println!("  Total links:  {}", stats.total_links);
        println!("  Broken links: {}", stats.broken_links);
        println!("  Unique tags:  {}", stats.unique_tags);
        println!("  Orphan docs:  {}", stats.orphan_docs);
        println!("  Headings:     {}", stats.total_headings);
        println!("  ~Words:       {}", format_number(stats.total_words));
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
