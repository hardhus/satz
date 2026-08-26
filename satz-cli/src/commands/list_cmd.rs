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
            for link in broken_links {
                println!("{}: [[{}]]", doc.path.display(), link.target_doc);
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
