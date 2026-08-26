use anyhow::Result;
use satz_core::{Index, walk_vault};
use std::path::PathBuf;
use std::time::Instant;

#[derive(clap::Args, Debug)]
pub struct IndexArgs {
    /// Vault root path (default: current directory)
    #[arg(default_value = ".")]
    pub path: PathBuf,
}

pub fn run(args: IndexArgs) -> Result<()> {
    let t0 = Instant::now();
    let docs = walk_vault(&args.path)?;
    let doc_count = docs.len();
    let index = Index::build(docs);
    let elapsed = t0.elapsed();
    let stats = index.stats();

    println!("Indexing vault: {}", args.path.display());
    println!(
        "✓ {} documents indexed in {:.0}ms",
        doc_count,
        elapsed.as_millis()
    );
    println!(
        "  Links:        {} total, {} broken",
        stats.total_links, stats.broken_links
    );
    println!("  Tags:         {} unique", stats.unique_tags);
    println!(
        "  Orphans:      {} documents (no backlinks)",
        stats.orphan_docs
    );

    if stats.broken_links > 0 {
        eprintln!(
            "⚠ {} broken links — run `satz list --broken` for details",
            stats.broken_links
        );
    }

    Ok(())
}
