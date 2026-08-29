use anyhow::Result;
use satz_core::{Index, walk_vault};
use std::collections::HashSet;
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
    let docs = walk_vault(&args.vault)?;
    let index = Index::build(docs);

    if args.broken {
        for (doc, broken_links) in index.docs_with_broken_links() {
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
                    satz_core::LinkResolution::AnchorMissing { .. } => "dosya var, başlık yok",
                    satz_core::LinkResolution::DocMissing => "dosya bulunamadı",
                    satz_core::LinkResolution::Resolved { .. } => "",
                };

                println!(
                    "{}:{}\t{}\t— {}",
                    doc.path.display(),
                    line_no,
                    link_repr,
                    reason
                );
            }
        }
        return Ok(());
    }

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

    let mut paths: Vec<_> = results
        .iter()
        .map(|d| d.path.display().to_string())
        .collect();
    paths.sort();

    for p in paths {
        println!("{}", p);
    }

    Ok(())
}
