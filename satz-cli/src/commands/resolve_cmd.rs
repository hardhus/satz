use anyhow::Result;
use satz_core::{Index, walk_vault};
use std::path::PathBuf;

#[derive(clap::Args, Debug)]
pub struct ResolveArgs {
    /// Vault root directory
    #[arg(long, short = 'v', default_value = ".")]
    pub vault: PathBuf,

    /// Target wikilink to resolve, e.g. "[[note]]" or "[[note#heading]]" or "note"
    pub target: String,
}

pub fn run(args: ResolveArgs) -> Result<()> {
    let docs = walk_vault(&args.vault)?;
    let index = Index::build(docs);

    // Strip [[ and ]] if present
    let raw = args
        .target
        .trim()
        .trim_start_matches("[[")
        .trim_end_matches("]]");

    let (doc_target, heading) = if let Some((doc, h)) = raw.split_once('#') {
        (doc.trim(), Some(h.trim()))
    } else {
        (raw.trim(), None)
    };

    let Some(doc_id) = index.resolve_link(doc_target) else {
        eprintln!("not found: {}", doc_target);
        std::process::exit(1);
    };

    let doc = index.get_doc(doc_id).expect("doc must exist in index");
    let abs_path = args.vault.join(&doc.path);

    if let Some(heading_text) = heading {
        if let Some(h) = doc
            .headings
            .iter()
            .find(|h| h.slug == heading_text || h.text.eq_ignore_ascii_case(heading_text))
        {
            let pos = doc.line_index.byte_to_position(h.range.start);
            println!("{}:{}", abs_path.display(), pos.line + 1);
        } else {
            println!("{}", abs_path.display());
        }
    } else {
        println!("{}", abs_path.display());
    }

    Ok(())
}
