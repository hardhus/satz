use anyhow::Result;
use std::io::Write;
use std::path::PathBuf;
use std::time::Instant;

#[derive(clap::Args, Debug)]
pub struct IndexArgs {
    /// Vault root path (default: current directory)
    #[arg(default_value = ".")]
    pub path: PathBuf,
}

pub fn run(args: IndexArgs) -> Result<()> {
    super::with_stdout(|out| run_with_output(args, out))
}

/// `run`, writing what `satz index` prints to `out` (the broken-links hint goes to stderr).
pub fn run_with_output(args: IndexArgs, out: &mut dyn Write) -> Result<()> {
    let t0 = Instant::now();
    let index = super::load_index(&args.path)?;
    let doc_count = index.doc_count();
    let elapsed = t0.elapsed();
    let stats = index.stats();

    writeln!(out, "Indexing vault: {}", args.path.display())?;
    writeln!(
        out,
        "✓ {} documents indexed in {:.0}ms",
        doc_count,
        elapsed.as_millis()
    )?;
    writeln!(
        out,
        "  Links:        {} total, {} broken",
        stats.total_links, stats.broken_links
    )?;
    writeln!(out, "  Tags:         {} unique", stats.unique_tags)?;
    writeln!(
        out,
        "  Orphans:      {} documents (no backlinks)",
        stats.orphan_docs
    )?;

    if stats.broken_links > 0 {
        eprintln!(
            "⚠ {} broken links — run `satz list --broken` for details",
            stats.broken_links
        );
    }

    Ok(())
}
