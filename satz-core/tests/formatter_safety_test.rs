//! Formatter safety net.
//!
//! A formatter may change how a document is *written*, never what it *is*. These tests run the
//! formatter over an adversarial corpus (`tests/formatter_corpus/`, one small document per known
//! way of corrupting content) plus every regular fixture, under several configurations, and check
//! invariants that no legitimate formatting change can violate:
//!
//! 1. **Same rendering** -- pulldown-cmark renders the formatted text to the same HTML as the
//!    original (whitespace collapsed outside `<pre>`, where it is significant).
//! 2. **Idempotency** -- `format(format(x)) == format(x)`.
//! 3. **Line endings** -- a CRLF document formats exactly like its LF twin and stays CRLF.
//! 4. **Verbatim content** -- specific lines that must survive byte for byte (code, table rows).
//!
//! Each violation is reported with the document and configuration that caused it.

use std::path::{Path, PathBuf};

use pulldown_cmark::{Options, Parser, html};
use satz_core::config::FormatterConfig;
use satz_core::formatter::format_document;

fn tests_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests")
}

/// Reads a file with line endings normalised to LF, so the tests behave the same whether git
/// checked the corpus out with LF or CRLF.
fn read_lf(path: &Path) -> String {
    std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
        .replace("\r\n", "\n")
}

fn markdown_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()))
        .flatten()
        .map(|e| e.path())
        .collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            markdown_files(&p, out);
        } else if p.extension().is_some_and(|e| e.eq_ignore_ascii_case("md")) {
            out.push(p);
        }
    }
}

/// `(display name, content)` for the adversarial corpus.
fn corpus() -> Vec<(String, String)> {
    let mut files = Vec::new();
    markdown_files(&tests_dir().join("formatter_corpus"), &mut files);
    assert!(files.len() >= 10, "corpus went missing: {files:?}");
    files
        .iter()
        .map(|p| {
            (
                p.file_stem().unwrap().to_string_lossy().into_owned(),
                read_lf(p),
            )
        })
        .collect()
}

/// `(display name, content)` for the regular fixtures.
fn fixtures() -> Vec<(String, String)> {
    let mut files = Vec::new();
    markdown_files(&tests_dir().join("fixtures"), &mut files);
    assert!(!files.is_empty(), "fixtures went missing");
    files
        .iter()
        .map(|p| {
            (
                format!("fixtures/{}", p.file_name().unwrap().to_string_lossy()),
                read_lf(p),
            )
        })
        .collect()
}

fn all_docs() -> Vec<(String, String)> {
    let mut v = corpus();
    v.extend(fixtures());
    v
}

fn configs() -> Vec<(&'static str, FormatterConfig)> {
    let mut list = vec![("default", FormatterConfig::default())];

    let mut c = FormatterConfig::default();
    c.misc.code_fence_style = "~~~".to_string();
    list.push(("tilde-fences", c));

    let mut c = FormatterConfig::default();
    c.lists.marker = "*".to_string();
    list.push(("star-lists", c));

    let mut c = FormatterConfig::default();
    c.emphasis.italic_marker = "_".to_string();
    c.emphasis.bold_marker = "__".to_string();
    list.push(("underscore-emphasis", c));

    let mut c = FormatterConfig::default();
    c.misc.blockquote_single_space = false;
    list.push(("no-blockquote-spacing", c));

    for width in [20usize, 40, 80] {
        let mut c = FormatterConfig::default();
        c.wrap.enable = true;
        c.line_width = width;
        let name: &'static str = match width {
            20 => "wrap-20",
            40 => "wrap-40",
            _ => "wrap-80",
        };
        list.push((name, c));
    }
    list
}

fn crlf(s: &str) -> String {
    s.replace("\r\n", "\n").replace('\n', "\r\n")
}

fn render_html(src: &str) -> String {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_FOOTNOTES);
    options.insert(Options::ENABLE_TASKLISTS);
    options.insert(Options::ENABLE_HEADING_ATTRIBUTES);
    options.insert(Options::ENABLE_YAML_STYLE_METADATA_BLOCKS);
    let mut out = String::new();
    html::push_html(&mut out, Parser::new_ext(src, options));
    out
}

/// Collapses whitespace runs to one space everywhere EXCEPT inside `<pre>...</pre>`, where
/// whitespace is content.
fn normalize_html(html: &str) -> String {
    let html = html.replace("\r\n", "\n");
    let collapse = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out = String::new();
    let mut rest = html.as_str();
    while let Some(start) = rest.find("<pre") {
        out.push_str(&collapse(&rest[..start]));
        out.push('\u{1}');
        let after = &rest[start..];
        let end = after
            .find("</pre>")
            .map_or(after.len(), |e| e + "</pre>".len());
        out.push_str(&after[..end]);
        out.push('\u{1}');
        rest = &after[end..];
    }
    out.push_str(&collapse(rest));
    out
}

fn report(kind: &str, failures: Vec<String>) {
    assert!(
        failures.is_empty(),
        "{} document/config pair(s) violate `{kind}`:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn formatting_never_changes_how_a_document_renders() {
    let mut failures = Vec::new();
    for (name, src) in corpus() {
        for (config_name, config) in configs() {
            // Wikilink whitespace normalisation intentionally rewrites text pulldown-cmark sees
            // as plain text, so it is switched off for this invariant.
            let mut config = config;
            config.normalize_links = false;
            let out = format_document(&src, &config);
            if normalize_html(&render_html(&out)) != normalize_html(&render_html(&src)) {
                failures.push(format!(
                    "[{name} | {config_name}] renders differently after formatting:\n--- formatted ---\n{out}\n-----------------"
                ));
            }
        }
    }
    report("same rendering", failures);
}

#[test]
fn formatting_is_idempotent_for_every_document_and_config() {
    let mut failures = Vec::new();
    for (name, src) in all_docs() {
        for (config_name, config) in configs() {
            let once = format_document(&src, &config);
            let twice = format_document(&once, &config);
            if once != twice {
                failures.push(format!(
                    "[{name} | {config_name}] second pass changed the output:\n--- once ---\n{once}\n--- twice ---\n{twice}\n------------"
                ));
            }
        }
    }
    report("idempotency", failures);
}

#[test]
fn crlf_documents_format_like_lf_documents_and_stay_crlf() {
    let mut failures = Vec::new();
    for (name, src) in all_docs() {
        for (config_name, config) in configs() {
            let lf_result = format_document(&src, &config);
            let crlf_result = format_document(&crlf(&src), &config);
            if crlf_result != crlf(&lf_result) {
                failures.push(format!(
                    "[{name} | {config_name}] CRLF input is not formatted like its LF twin, or lost its CRLF endings"
                ));
            }
        }
    }
    report("line endings", failures);
}

/// Text that must appear byte for byte in the default-config output of a corpus document.
const MUST_KEEP: &[(&str, &[&str])] = &[
    ("table_extra_cells", &["| 1 | 2 | 3 |"]),
    ("table_wikilink_pipe", &["| [[a|b]] | text |"]),
    (
        "quote_indented_code",
        &[
            ">     indented code inside a quote",
            ">     second code line",
        ],
    ),
    (
        "quote_nested_list",
        &["> - outer item", ">     - nested item"],
    ),
    ("quote_fenced_code", &[">   indented code line"]),
    (
        "fence_long_close",
        &["```\ncode\n```\n\nAfter the fence, this must stay an ordinary paragraph."],
    ),
    ("fence_tilde_info_backtick", &["~~~ js `x`\ncode\n~~~"]),
    (
        "fence_nested_4_backticks",
        &["````\n```\ninner code with trailing spaces   \n```\n````"],
    ),
    (
        "fence_mixed_tilde_inside_backtick",
        &["```\n~~~\n\n\n\nthree blank lines above stay\n```"],
    ),
    (
        "indented_code_hash",
        &["    # not a heading, this is code\n    trailing spaces in code   \n"],
    ),
    (
        "html_block_blank_lines",
        &["<pre>\nline one\n\n\nline two with trailing spaces   \n</pre>"],
    ),
    ("code_span_wikilink", &["`[[ a ]]`", "[[b]]"]),
];

#[test]
fn critical_content_is_kept_verbatim() {
    let docs = corpus();
    let mut failures = Vec::new();
    for (name, needles) in MUST_KEEP {
        let src = &docs
            .iter()
            .find(|(n, _)| n == name)
            .unwrap_or_else(|| panic!("corpus document {name} missing"))
            .1;
        let out = format_document(src, &FormatterConfig::default());
        for needle in *needles {
            if !out.contains(needle) {
                failures.push(format!(
                    "[{name}] lost {needle:?}\n--- formatted ---\n{out}\n-----------------"
                ));
            }
        }
    }
    report("verbatim content", failures);
}

#[test]
fn a_clean_document_is_left_exactly_alone() {
    let docs = corpus();
    let src = &docs.iter().find(|(n, _)| n == "clean_baseline").unwrap().1;
    assert_eq!(format_document(src, &FormatterConfig::default()), *src);
}
