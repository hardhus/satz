use anyhow::Result;
use std::io::Write;
use std::path::PathBuf;

#[derive(clap::Args, Debug)]
pub struct ResolveArgs {
    /// Vault root directory
    #[arg(long, short = 'v', default_value = ".")]
    pub vault: PathBuf,

    /// Target wikilink to resolve, e.g. "[[note]]", "[[note#heading]]", "[[note#^block]]",
    /// "[[note|shown]]", "![[note]]" or the same without the brackets
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

    let (link, reported) = read_target(&args.target);
    let Some(doc_id) = link
        .as_ref()
        .and_then(|link| index.resolve_link(&link.target_doc))
    else {
        eprintln!("not found: {}", reported);
        return Ok(Outcome::NotFound);
    };

    let doc = index.get_doc(doc_id).expect("doc must exist in index");
    let path = args.vault.join(&doc.path);

    // Where inside the note the link points, found the way the language server finds it: a block by
    // its id (any case), a heading by its text or slug (the first of equal ones). Something the
    // note does not have leaves the note.
    let anchor = link.as_ref().and_then(|link| {
        if let Some(block) = &link.target_block {
            let at = doc.resolve_block(block)?;
            Some(doc.blocks[at].range.start)
        } else if let Some(heading) = &link.target_heading {
            let at = doc.resolve_heading(heading)?;
            Some(doc.headings[at].range.start)
        } else {
            None
        }
    });
    match anchor {
        Some(byte) => {
            let line = doc.line_index.byte_to_position(byte).line + 1;
            writeln!(out, "{}:{}", path.display(), line)?;
        }
        None => writeln!(out, "{}", path.display())?,
    }

    Ok(Outcome::Found)
}

/// The wikilink a target names, and the name to report when nothing is found. The target is what
/// a user copies out of a note: `[[note]]`, `[[note#Heading]]`, `[[note#^block]]`, `[[note|shown]]`,
/// `![[note]]`, or any of them without the brackets. It is read by the parser the language server
/// uses, so a link means the same here as in the editor. `None` when the target names nothing to
/// look for (empty, `[[#]]`, `[[|x]]`).
fn read_target(target: &str) -> (Option<satz_core::Link>, String) {
    let raw = target.trim();
    // The mark of an embed.
    let raw = match raw.strip_prefix('!') {
        Some(rest) if rest.starts_with("[[") => rest,
        _ => raw,
    };
    let raw = raw.trim_start_matches("[[").trim_end_matches("]]").trim();
    let note = satz_core::parse_document(&format!("[[{raw}]]"), std::path::Path::new("target.md"));
    let link = note
        .links
        .into_iter()
        .find(|link| link.kind == satz_core::LinkKind::WikiLink);
    let reported = match &link {
        Some(link) => link.target_doc.clone(),
        // Nothing to look for: what stands before a `#` or a `|` is all there is to report.
        None => raw
            .split(['#', '|'])
            .next()
            .unwrap_or_default()
            .trim()
            .to_string(),
    };
    (link, reported)
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

    #[test]
    fn the_display_part_and_the_embed_mark_of_a_wikilink_are_not_part_of_the_target() {
        let v = vault();
        for target in [
            "[[b|shown]]",
            "b|shown",
            "[[ b | shown ]]",
            "[[b|one|two]]",
            // in a table cell the separator is written `\|`
            "[[b\\|shown]]",
            "![[b]]",
            "![[b|300]]",
            "  ![[ b ]]  ",
        ] {
            assert_eq!(
                resolve(&v, target),
                (Outcome::Found, "b.md".to_string()),
                "{target:?}"
            );
        }
        for target in [
            "[[h#Heading|shown]]",
            "![[h#Heading]]",
            "![[h#Heading|300]]",
        ] {
            assert_eq!(
                resolve(&v, target),
                (Outcome::Found, format!("h.md:{}", line_of("## Heading", 0))),
                "{target:?}"
            );
        }
        for target in [
            "[[nothing|x]]",
            "![[nothing]]",
            "![[nothing#h|x]]",
            "!nothing",
        ] {
            assert_eq!(
                resolve(&v, target),
                (Outcome::NotFound, String::new()),
                "{target:?}"
            );
        }
    }

    #[test]
    fn a_block_anchor_names_the_line_of_its_id() {
        let v = vault();
        let blk = line_of("second ^blk", 0);
        // The anchor of a paragraph of two lines is on its last line.
        let multi = line_of("of two lines ^multi", 0);
        assert_eq!(multi, line_of("A paragraph", 0) + 1);
        for (target, line) in [
            ("[[h#^blk]]", blk),
            ("h#^blk", blk),
            ("[[h#^BLK]]", blk),
            ("[[h#^blk|shown]]", blk),
            ("![[h#^blk]]", blk),
            ("[[h#^multi]]", multi),
        ] {
            assert_eq!(
                resolve(&v, target),
                (Outcome::Found, format!("h.md:{line}")),
                "{target:?}"
            );
        }
        // A block the note does not have, or none at all: the note is where the link points.
        for target in ["[[h#^nope]]", "[[h#^]]", "[[h#^ ]]", "[[b#^blk]]"] {
            assert_eq!(
                resolve(&v, target),
                (
                    Outcome::Found,
                    if target.contains("[[b#") {
                        "b.md"
                    } else {
                        "h.md"
                    }
                    .to_string()
                ),
                "{target:?}"
            );
        }
    }

    #[test]
    fn an_empty_heading_leaves_the_note_and_does_not_find_a_heading_with_no_letters() {
        // (Before, the empty text and the slug of the heading `???` were equal, and `h#` went to it.)
        let v = vault();
        for target in ["h#", "[[h#]]", "[[h# ]]", "h#  "] {
            assert_eq!(
                resolve(&v, target),
                (Outcome::Found, "h.md".to_string()),
                "{target:?}"
            );
        }
    }

    #[test]
    fn a_heading_is_found_as_the_language_server_finds_it() {
        let v = vault();
        let doc = satz_core::parse_document(H, std::path::Path::new("h.md"));
        // Letters of any kind and case, spaces, punctuation and the slug: each is what the language
        // server (and the diagnostics, and the graph) would make of the same link.
        let mut found = 0;
        for reference in [
            "Heading",
            "heading",
            "HEADING",
            "  Heading  ",
            "what",
            "What",
            "what?",
            "WHAT?",
            "bölüm başlığı",
            "BÖLÜM BAŞLIĞI",
            "Bölüm  Başlığı",
            "bölüm-başlığı",
            "iş planı",
            "İş Planı",
            "İŞ PLANI",
            "IŞ PLANI",
            "dup",
            "DUP",
            "???",
            "nothing here",
            "h",
            "bolum basligi",
        ] {
            let expected = match doc.resolve_heading(reference) {
                Some(at) => {
                    found += 1;
                    let line = doc
                        .line_index
                        .byte_to_position(doc.headings[at].range.start)
                        .line
                        + 1;
                    format!("h.md:{line}")
                }
                None => "h.md".to_string(),
            };
            assert_eq!(
                resolve(&v, &format!("[[h#{reference}]]")),
                (Outcome::Found, expected),
                "{reference:?}"
            );
        }
        assert!(found >= 16, "{found} of the references find a heading");
        // Some that the old rule (the slug, or the text in ASCII case) could not find.
        for (reference, start) in [
            ("iş planı", "## İş"),
            ("BÖLÜM BAŞLIĞI", "## Bölüm"),
            ("Bölüm  Başlığı", "## Bölüm"),
            ("What", "## What"),
        ] {
            assert_eq!(
                resolve(&v, &format!("h#{reference}")),
                (Outcome::Found, format!("h.md:{}", line_of(start, 0))),
                "{reference:?}"
            );
        }
    }

    /// The rule `resolve` had for a heading, as it was.
    fn old_heading_rule(doc: &satz_core::Document, reference: &str) -> Option<usize> {
        doc.headings
            .iter()
            .position(|h| h.slug == reference || h.text.eq_ignore_ascii_case(reference))
    }

    #[test]
    fn the_headings_the_old_rule_found_are_all_found_the_same_way() {
        let texts = [
            "Heading",
            "Bölüm Başlığı",
            "İş Planı",
            "ışık",
            "What?",
            "Dup",
            "Some  spaces",
            "C++ notes",
            "100% sure",
            "Ünite 1",
            "ÇALIŞMA",
            "Naïve café",
            "a-b",
            "Tag #x",
            "Trailing.",
            "Dup",
            "日本語",
            "(parenthesis)",
        ];
        let mut text = String::new();
        for t in texts {
            text.push_str(&format!("## {t}\n\nbody\n\n"));
        }
        let doc = satz_core::parse_document(&text, std::path::Path::new("d.md"));
        let (mut both, mut only_now) = (0, 0);
        for h in &doc.headings {
            let variants = [
                h.text.clone(),
                h.text.to_lowercase(),
                h.text.to_uppercase(),
                h.text.to_ascii_lowercase(),
                h.text.to_ascii_uppercase(),
                h.slug.clone(),
                format!("  {}  ", h.text),
                h.text.replace(' ', "  "),
                h.text.trim_end_matches(['?', '.']).to_string(),
            ];
            for reference in variants {
                let (old, now) = (
                    old_heading_rule(&doc, &reference),
                    doc.resolve_heading(&reference),
                );
                match (old, now) {
                    // Anything the old rule found is found, and it is the same heading.
                    (Some(_), _) => {
                        assert_eq!(old, now, "{reference:?} (heading {:?})", h.text);
                        both += 1;
                    }
                    (None, Some(_)) => only_now += 1,
                    (None, None) => {}
                }
            }
        }
        assert!(
            both > 100 && only_now > 20,
            "{both} the same, {only_now} found now"
        );
        // The one thing the old rule found that is not a heading: an empty reference matched a
        // heading without letters (its slug is empty too).
        let doc = satz_core::parse_document("## ???\n", std::path::Path::new("d.md"));
        assert_eq!(old_heading_rule(&doc, ""), Some(0));
        assert_eq!(doc.resolve_heading(""), None);
    }
}
