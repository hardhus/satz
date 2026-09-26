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

#[cfg(test)]
mod tests {
    use super::*;

    /// A vault in a temp folder, removed when dropped.
    struct Vault(PathBuf);

    impl Vault {
        fn new(tag: &str) -> Self {
            use std::sync::atomic::{AtomicUsize, Ordering};
            static N: AtomicUsize = AtomicUsize::new(0);
            let dir = std::env::temp_dir().join(format!(
                "satz_resolve_{tag}_{}_{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn write(&self, rel: &str, text: &str) {
            let path = self.0.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
    }

    impl Drop for Vault {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// The note with the headings, the blocks and the duplicates.
    const H: &str = "# H\n\n## Heading\n\ntext\n\n## Bölüm Başlığı\n\n## İş Planı\n\n## What?\n\n## Dup\n\nfirst\n\n## Dup\n\nsecond ^blk\n\nA paragraph\nof two lines ^multi\n\n## ???\n";

    fn vault() -> Vault {
        let v = Vault::new("forms");
        v.write("h.md", H);
        v.write("b.md", "# B\n");
        v.write("sub/x.md", "# X\n");
        v.write(
            "alias.md",
            "---\naliases: [Other Name]\n---\n# Alias note\n",
        );
        v.write("ş/kayıt.md", "# Kayıt\n");
        v.write("Çalışma.md", "# Çalışma\n");
        v
    }

    /// What `resolve` says for `target`: how it ended and what it wrote, with the vault's folder taken
    /// off the front of the path and `/` for a separator (`""` when nothing was written).
    fn resolve(v: &Vault, target: &str) -> (Outcome, String) {
        let mut written = Vec::new();
        let outcome = run_with_output(
            ResolveArgs {
                vault: v.0.clone(),
                target: target.to_string(),
            },
            &mut written,
        )
        .expect("a vault that can be read");
        let text = String::from_utf8(written).unwrap();
        let prefix = format!("{}", v.0.join("").display());
        let shown = text
            .trim_end()
            .strip_prefix(&prefix)
            .unwrap_or(text.trim_end())
            .replace('\\', "/");
        (outcome, shown)
    }

    /// The line (1-based) of the `n`th (from 0) line of `H` that starts with `start`.
    fn line_of(start: &str, n: usize) -> usize {
        H.lines()
            .enumerate()
            .filter(|(_, l)| l.starts_with(start))
            .nth(n)
            .unwrap()
            .0
            + 1
    }

    #[test]
    fn a_note_is_found_by_path_file_name_title_or_alias_in_any_case_and_any_letters() {
        let v = vault();
        for (target, file) in [
            ("b", "b.md"),
            ("[[b]]", "b.md"),
            ("[[b.md]]", "b.md"),
            ("b.md", "b.md"),
            ("[[sub/x]]", "sub/x.md"),
            ("sub/x.md", "sub/x.md"),
            ("x", "sub/x.md"),
            ("[[Other Name]]", "alias.md"),
            ("[[OTHER NAME]]", "alias.md"),
            ("[[other name]]", "alias.md"),
            ("alias", "alias.md"),
            ("[[Alias note]]", "alias.md"),
            ("[[ş/kayıt]]", "ş/kayıt.md"),
            ("[[kayıt]]", "ş/kayıt.md"),
            ("[[Kayıt]]", "ş/kayıt.md"),
            ("[[Çalışma]]", "Çalışma.md"),
            ("[[çalışma]]", "Çalışma.md"),
        ] {
            assert_eq!(
                resolve(&v, target),
                (Outcome::Found, file.to_string()),
                "{target:?}"
            );
        }
    }

    #[test]
    fn a_heading_is_found_by_its_text_or_its_slug_and_the_first_of_equal_ones_counts() {
        let v = vault();
        for (target, heading_line) in [
            ("[[h#Heading]]", line_of("## Heading", 0)),
            ("h#heading", line_of("## Heading", 0)),
            ("h#HEADING", line_of("## Heading", 0)),
            ("h#  Heading  ", line_of("## Heading", 0)),
            ("[[h# Heading ]]", line_of("## Heading", 0)),
            ("h#What?", line_of("## What", 0)),
            ("h#what?", line_of("## What", 0)),
            // the slug, as a link written by hand or by a tool has it
            ("h#what", line_of("## What", 0)),
            ("h#bölüm-başlığı", line_of("## Bölüm", 0)),
            ("h#Bölüm Başlığı", line_of("## Bölüm", 0)),
            ("h#bölüm başlığı", line_of("## Bölüm", 0)),
            ("h#İş Planı", line_of("## İş", 0)),
            // two headings with the same text: the first is the one every link means
            ("h#Dup", line_of("## Dup", 0)),
            ("h#dup", line_of("## Dup", 0)),
            ("h#???", line_of("## ???", 0)),
        ] {
            assert_eq!(
                resolve(&v, target),
                (Outcome::Found, format!("h.md:{heading_line}")),
                "{target:?}"
            );
        }
    }

    #[test]
    fn a_heading_the_note_does_not_have_leaves_the_path_and_the_note_is_still_found() {
        let v = vault();
        for target in ["h#nope", "[[h#nope]]", "h#a#b", "h#Heading and more"] {
            assert_eq!(
                resolve(&v, target),
                (Outcome::Found, "h.md".to_string()),
                "{target:?}"
            );
        }
    }

    #[test]
    fn a_note_that_is_not_there_writes_nothing_whatever_follows_its_name() {
        let v = vault();
        for target in [
            "nothing",
            "[[nothing]]",
            "[[nothing#h]]",
            "[[nothing#Heading]]",
        ] {
            assert_eq!(
                resolve(&v, target),
                (Outcome::NotFound, String::new()),
                "{target:?}"
            );
        }
    }

    #[test]
    fn relative_daily_aliases_are_not_resolved_here_only_by_the_language_server() {
        // (`docs/cli.md` says so: the command has no configuration of the day's note.)
        let v = vault();
        v.write("daily/2024-01-01.md", "# Day\n");
        for target in ["[[today]]", "[[bugün]]", "today"] {
            assert_eq!(
                resolve(&v, target),
                (Outcome::NotFound, String::new()),
                "{target:?}"
            );
        }
    }
}
