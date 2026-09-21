use anyhow::Result;
use std::io::Write;
use std::path::PathBuf;

#[derive(clap::Args, Debug)]
pub struct ResolveArgs {
    /// Vault root directory
    #[arg(long, short = 'v', default_value = ".")]
    pub vault: PathBuf,

    /// Target wikilink to resolve, e.g. "[[note]]" or "[[note#heading]]" or "note"
    pub target: String,
}

/// How a `resolve` run ended, apart from errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The target points at a note: its path was written.
    Found,
    /// Nothing the target names exists. Not an error of the command: it says so on stderr and
    /// the caller decides what that means (the `satz` binary exits with status 1).
    NotFound,
}

pub fn run(args: ResolveArgs) -> Result<Outcome> {
    super::with_stdout(|out| run_with_output(args, out))
}

/// `run`, writing the resolved path to `out`.
pub fn run_with_output(args: ResolveArgs, out: &mut dyn Write) -> Result<Outcome> {
    let index = super::load_index(&args.vault)?;

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
        return Ok(Outcome::NotFound);
    };

    let doc = index.get_doc(doc_id).expect("doc must exist in index");
    let path = args.vault.join(&doc.path);

    if let Some(heading_text) = heading {
        if let Some(h) = doc
            .headings
            .iter()
            .find(|h| h.slug == heading_text || h.text.eq_ignore_ascii_case(heading_text))
        {
            let pos = doc.line_index.byte_to_position(h.range.start);
            writeln!(out, "{}:{}", path.display(), pos.line + 1)?;
        } else {
            writeln!(out, "{}", path.display())?;
        }
    } else {
        writeln!(out, "{}", path.display())?;
    }

    Ok(Outcome::Found)
}
