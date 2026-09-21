use anyhow::Result;
use std::collections::HashSet;
use std::io::{BufWriter, Write};
use std::path::PathBuf;

#[derive(clap::Args, Debug)]
pub struct ListArgs {
    /// Vault root directory
    #[arg(long, short = 'v', default_value = ".")]
    pub vault: PathBuf,

    /// Filter notes by tag (can be specified multiple times for intersection)
    #[arg(long)]
    pub tag: Vec<String>,

    /// List only orphan notes (documents with no incoming backlinks)
    #[arg(long)]
    pub orphans: bool,

    /// List documents that contain broken internal links
    #[arg(long)]
    pub broken: bool,
}

pub fn run(args: ListArgs) -> Result<()> {
    super::with_stdout(|out| run_with_output(args, out))
}

/// `run`, writing what `satz list` prints to `out`.
pub fn run_with_output(args: ListArgs, out: &mut dyn Write) -> Result<()> {
    let index = super::load_index(&args.vault)?;
    // Many lines can follow: they go out in blocks (nothing is written to stderr meanwhile).
    let mut out = BufWriter::new(out);

    // The documents the filters leave; `--broken` looks only at those, like the plain listing.
    let mut results: Vec<_> = index.documents().collect();

    // Filter by tags (AND logic)
    for tag in &args.tag {
        let with_tag: HashSet<_> = index.docs_with_tag(tag).map(|d| &d.id).collect();
        results.retain(|d| with_tag.contains(&d.id));
    }

    // Filter by orphans
    if args.orphans {
        let orphan_ids: HashSet<_> = index.orphan_docs().map(|d| &d.id).collect();
        results.retain(|d| orphan_ids.contains(&d.id));
    }

    if args.broken {
        let kept: HashSet<_> = results.iter().map(|d| &d.id).collect();
        for (doc, broken_links) in index.docs_with_broken_links() {
            if !kept.contains(&doc.id) {
                continue;
            }
            for (link, res) in broken_links {
                let pos = doc.line_index.byte_to_position(link.range.start);
                let line_no = pos.line + 1;
                let link_repr = if link.range.end <= doc.line_index.source().len() {
                    let raw = &doc.line_index.source()[link.range.start..link.range.end];
                    raw.trim().to_string()
                } else if let Some(h) = &link.target_heading {
                    format!("[[{}#{}]]", link.target_doc, h)
                } else if let Some(b) = &link.target_block {
                    format!("[[{}#^{}]]", link.target_doc, b)
                } else {
                    format!("[[{}]]", link.target_doc)
                };

                let reason = match res {
                    satz_core::LinkResolution::AnchorMissing { .. } => {
                        "file exists, heading not found"
                    }
                    satz_core::LinkResolution::DocMissing => "file not found",
                    // `docs_with_broken_links` only yields the two broken outcomes.
                    satz_core::LinkResolution::Resolved { .. } => continue,
                };

                writeln!(
                    out,
                    "{}:{}\t{}\t— {}",
                    doc.path.display(),
                    line_no,
                    link_repr,
                    reason
                )?;
            }
        }
        out.flush()?;
        return Ok(());
    }

    let mut paths: Vec<_> = results
        .iter()
        .map(|d| d.path.display().to_string())
        .collect();
    paths.sort();

    for p in paths {
        writeln!(out, "{}", p)?;
    }
    out.flush()?;

    Ok(())
}
