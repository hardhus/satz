//! Benchmark of satz-core: `cargo bench -p satz-core --bench benchmark [-- --quick] [-- --check]`.
//!
//! How to read it, and why it is built this way. A single timing on a machine that is doing other
//! things says little, so:
//! * every measurement is repeated (9 rounds; 3 with `--quick`) after a warm-up, and both the
//!   `min` and the `median` are shown. Noise only ever adds time, so compare runs by `min`; the
//!   median shows the typical case.
//! * what a measurement needs but is not the thing measured (building the input, cloning it) is
//!   done outside the timed part.
//! * "scaling" tables time the same kind of input at doubling sizes, every round in a different
//!   order so that a busy moment hits all sizes alike, and print the exponent
//!   `log2(time(2n) / time(n))`: about 1 is linear, about 2 is quadratic. An exponent above
//!   1.5 over the whole range of sizes is flagged SUPERLINEAR. That is the sign that catches the kind of slowdown a single
//!   size never shows (a note with many code blocks and one `$` once cost time in proportion to
//!   the square of its size). Sizes whose time is too short to judge are not flagged.
//! * when an exponent comes out too high the family is measured again (twice at most) and the best
//!   time of every size over the attempts is kept, so only a cost that stays is flagged.
//! * `--check` makes the exit status 1 if anything was flagged; without it the run only reports.
//!
//! Numbers from a debug build mean nothing; `cargo bench` builds the optimized one.

use std::hint::black_box;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use satz_core::config::FormatterConfig;
use satz_core::{DocId, Document, Index, VaultConfig, parse_document};

/// Makes a document of `n` repeated units.
type Make = fn(usize) -> String;
/// (label, generator, units, must the formatter change it?)
type Family = (&'static str, Make, usize, Option<bool>);

/// An exponent above this is called superlinear.
const SUPERLINEAR: f64 = 1.5;
/// A time below this is too short to say anything about how it grows.
const TOO_SHORT: Duration = Duration::from_micros(300);
/// How many times slower the note with one `$5` may be than the same note without before it is
/// flagged (it is about 1 when all is well).
const DOLLAR_RATIO_LIMIT: f64 = 3.0;

struct Settings {
    rounds: usize,
    /// Sizes are divided by this (2 with `--quick`).
    shrink: usize,
}

impl Settings {
    fn size(&self, n: usize) -> usize {
        (n / self.shrink).max(1)
    }
}

struct Sample {
    min: Duration,
    median: Duration,
}

fn summarize(mut times: Vec<Duration>) -> Sample {
    times.sort();
    Sample {
        min: times[0],
        median: times[times.len() / 2],
    }
}

/// Runs `run(setup())` once to warm up, then `rounds` times, timing only `run`.
fn measure<S, T>(
    rounds: usize,
    mut setup: impl FnMut() -> S,
    mut run: impl FnMut(S) -> T,
) -> Sample {
    black_box(run(setup()));
    let times = (0..rounds)
        .map(|_| {
            let input = setup();
            let started = Instant::now();
            black_box(run(input));
            started.elapsed()
        })
        .collect();
    summarize(times)
}

/// The best time of each of `count` things over `rounds` rounds; in every round the things are
/// timed back to back in a different order.
fn interleaved(
    count: usize,
    rounds: usize,
    mut one: impl FnMut(usize) -> Duration,
) -> Vec<Duration> {
    let mut best = vec![Duration::MAX; count];
    for round in 0..rounds {
        for step in 0..count {
            let i = (step + round) % count;
            best[i] = best[i].min(one(i));
        }
    }
    best
}

/// `log2(time ratio) / log2(size ratio)`: 1 for linear growth, 2 for quadratic.
fn exponent(small: (usize, Duration), large: (usize, Duration)) -> f64 {
    (large.1.as_secs_f64() / small.1.as_secs_f64()).log2()
        / (large.0 as f64 / small.0 as f64).log2()
}

fn shown(d: Duration) -> String {
    let ns = d.as_nanos() as f64;
    if ns < 1_000.0 {
        format!("{ns:.0} ns")
    } else if ns < 1_000_000.0 {
        format!("{:.1} us", ns / 1_000.0)
    } else if ns < 1_000_000_000.0 {
        format!("{:.2} ms", ns / 1_000_000.0)
    } else {
        format!("{:.2} s", ns / 1_000_000_000.0)
    }
}

fn mb_per_s(bytes: usize, d: Duration) -> f64 {
    bytes as f64 / 1_048_576.0 / d.as_secs_f64().max(1e-12)
}

fn row(what: &str, sample: &Sample, note: &str) {
    println!(
        "  {what:<60} min {:>10}  median {:>10}  {note}",
        shown(sample.min),
        shown(sample.median)
    );
}

/// The exponent over the whole range of sizes, if the first and last times are long enough to judge.
fn overall_exponent(sizes: &[usize], best: &[Duration]) -> Option<f64> {
    let (first, last) = (*sizes.first()?, *sizes.last()?);
    let (t_first, t_last) = (*best.first()?, *best.last()?);
    (sizes.len() >= 2 && t_first >= TOO_SHORT && t_last >= TOO_SHORT)
        .then(|| exponent((first, t_first), (last, t_last)))
}

/// Times `one(i)` for every size (see `interleaved`) and reports the scaling. Noise only ever
/// adds time, so when the exponent comes out too high the whole family is measured again (twice
/// at most) and the best time of every size over all the attempts is kept: only a cost that stays
/// is flagged.
fn scaling_family(
    name: &str,
    sizes: &[usize],
    bytes: &[usize],
    rounds: usize,
    known: Option<&str>,
    flagged: &mut Vec<String>,
    mut one: impl FnMut(usize) -> Duration,
) {
    let mut best = interleaved(sizes.len(), rounds, &mut one);
    for _ in 0..2 {
        if overall_exponent(sizes, &best).is_none_or(|e| e <= SUPERLINEAR) {
            break;
        }
        let again = interleaved(sizes.len(), rounds, &mut one);
        for (kept, new) in best.iter_mut().zip(again) {
            *kept = (*kept).min(new);
        }
    }
    report_scaling_with(name, sizes, bytes, &best, known, flagged);
}

/// Prints a scaling table; with `known` set, a superlinear exponent is printed with that explanation and
/// is not flagged: a cost that is understood and not fixed yet must not hide new ones.
fn report_scaling_with(
    name: &str,
    sizes: &[usize],
    bytes: &[usize],
    best: &[Duration],
    known: Option<&str>,
    flagged: &mut Vec<String>,
) {
    println!("\n  scaling: {name}");
    println!(
        "  {:>8} {:>10} {:>11} {:>9}",
        "n", "bytes", "best", "exponent"
    );
    for (i, ((n, size), time)) in sizes.iter().zip(bytes).zip(best).enumerate() {
        let step = match i.checked_sub(1) {
            Some(before) if best[before] >= TOO_SHORT && *time >= TOO_SHORT => {
                format!(
                    "{:.2}",
                    exponent((sizes[before], best[before]), (*n, *time))
                )
            }
            Some(_) => String::from("(too short)"),
            None => String::from("-"),
        };
        println!("  {n:>8} {size:>10} {:>11} {step}", shown(*time));
    }
    // Judged over the whole range, not step by step: one noisy point moves a single step a lot and
    // the whole range hardly at all (a real quadratic cost gives about 2 either way).
    let (Some(&first), Some(&last)) = (sizes.first(), sizes.last()) else {
        return;
    };
    let (Some(&t_first), Some(&t_last)) = (best.first(), best.last()) else {
        return;
    };
    if sizes.len() < 2 || t_first < TOO_SHORT || t_last < TOO_SHORT {
        println!("  overall: too short to judge");
        return;
    }
    let overall = exponent((first, t_first), (last, t_last));
    if overall <= SUPERLINEAR {
        println!("  overall, n={first} to n={last}: {overall:.2}");
    } else if let Some(why) = known {
        println!("  overall, n={first} to n={last}: {overall:.2}  <-- superlinear, known: {why}");
    } else {
        println!("  overall, n={first} to n={last}: {overall:.2}  <-- SUPERLINEAR");
        flagged.push(format!(
            "{name}: exponent {overall:.2} from n={first} to n={last}"
        ));
    }
}

// ---- documents ----------------------------------------------------------------------------

/// A small note with front matter, aliases, tags and links to other notes.
fn small_note(i: usize, total: usize) -> (PathBuf, String) {
    let content = format!(
        "---\ntitle: Note {i}\naliases: [N{i}, NoteAlias{i}]\ntags: [tag{}, project/sub]\n---\n\n# Note {i}\n\nLink to [[Note {}]] and [[books/note-{}.md#Heading]] and [[Note {}#^block-1]].\n\nSome paragraph text here. ^block-1\n",
        i % 10,
        (i + 1) % total,
        (i + 2) % total,
        (i + 3) % total
    );
    (
        PathBuf::from(format!("folder_{}/note_{i}.md", i % 10)),
        content,
    )
}

fn small_notes(total: usize) -> Vec<Document> {
    (0..total)
        .map(|i| {
            let (path, content) = small_note(i, total);
            parse_document(&content, &path)
        })
        .collect()
}

/// A repeated block, with the end tidied so that an already formatted block stays unchanged.
fn repeated(block: &str, n: usize) -> String {
    let mut text = block.repeat(n);
    text.truncate(text.trim_end().len());
    text.push('\n');
    text
}

fn prose_clean(n: usize) -> String {
    repeated(
        "## Section\n\nA paragraph of plain prose with a [[link]] and a [link](other.md), already formatted.\n\n- one\n- two\n\n",
        n,
    )
}

fn prose_dirty(n: usize) -> String {
    repeated(
        "## Section\nA paragraph with trailing spaces.   \nAnd _italic_ text and __bold__ words.\n\n* item one\n* item two\n\n",
        n,
    )
}

fn prose_dirty_crlf(n: usize) -> String {
    prose_dirty(n).replace('\n', "\r\n")
}

fn tables(n: usize) -> String {
    repeated(
        "| Name | Değer | Not |\n|---|:-:|--:|\n| Şeker | 1 | çay ☕ |\n| a | 22 | b |\n\n",
        n,
    )
}

fn lists(n: usize) -> String {
    repeated(
        "1. one\n1. two\n   - nested\n   * nested two\n5. three\n\n",
        n,
    )
}

fn code_heavy(n: usize) -> String {
    "text `code` here\n\n```\ncode\n```\n\n".repeat(n)
}

/// `code_heavy` under a single `$5`: the case in which masking math once went quadratic.
fn code_heavy_with_a_dollar(n: usize) -> String {
    format!("costs $5 and more\n\n{}", code_heavy(n))
}

/// Inline math and inline code side by side on every line: the case where masking math once
/// looked through every code span for every `$`.
fn inline_math_beside_inline_code(n: usize) -> String {
    "`c` and $x$ y
"
    .repeat(n)
}

fn math_heavy(n: usize) -> String {
    "Inline $x^2$ and `code` here, then\n\n$$\na = b\n$$\n\n".repeat(n)
}

/// One note of `n` paragraphs made of `unit`, under front matter and a heading.
fn prose_note(unit: &str, n: usize) -> String {
    format!(
        "---\ntitle: Big note\ntags: [a, b]\n---\n\n# Big note\n\n{}",
        unit.repeat(n)
    )
}

// ---- checks of the harness itself ---------------------------------------------------------

fn check_the_harness() {
    let ms = Duration::from_millis;
    let close = |a: f64, b: f64| (a - b).abs() < 1e-9;
    assert!(close(exponent((1, ms(1)), (2, ms(2))), 1.0), "linear is 1");
    assert!(
        close(exponent((1, ms(1)), (2, ms(4))), 2.0),
        "quadratic is 2"
    );
    assert!(
        close(exponent((10, ms(1)), (40, ms(16))), 2.0),
        "the size ratio counts"
    );
    let sample = summarize(vec![ms(3), ms(1), ms(2)]);
    assert_eq!((sample.min, sample.median), (ms(1), ms(2)));
    let order: Vec<usize> = {
        let mut seen = Vec::new();
        interleaved(3, 2, |i| {
            seen.push(i);
            ms(1)
        });
        seen
    };
    assert_eq!(
        order,
        vec![0, 1, 2, 1, 2, 0],
        "the order rotates every round"
    );
}

// ---- the benchmark --------------------------------------------------------------------------

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let quick = args.iter().any(|a| a == "--quick");
    let check = args.iter().any(|a| a == "--check");
    let settings = Settings {
        rounds: if quick { 3 } else { 9 },
        shrink: if quick { 2 } else { 1 },
    };
    let mut flagged: Vec<String> = Vec::new();
    check_the_harness();

    println!("================ SATZ PERFORMANCE BENCHMARK ================");
    if cfg!(debug_assertions) {
        println!("!!! DEBUG BUILD: these numbers mean nothing. Use `cargo bench`. !!!");
    }
    println!(
        "{} rounds{}; compare runs by `min`",
        settings.rounds,
        if quick { " (--quick)" } else { "" }
    );

    parsing_and_index(&settings, &mut flagged);
    formatter_families(&settings, &mut flagged);
    formatter_scaling(&settings, &mut flagged);
    near_the_server(&settings);

    println!("\n============================================================");
    if flagged.is_empty() {
        println!("nothing flagged");
    } else {
        println!("FLAGGED ({}):", flagged.len());
        for line in &flagged {
            println!("  - {line}");
        }
        if check {
            std::process::exit(1);
        }
    }
}

fn parsing_and_index(settings: &Settings, flagged: &mut Vec<String>) {
    println!("\n--- parsing and the index ---");
    let rounds = settings.rounds;

    let notes: Vec<(PathBuf, String)> = (0..1000).map(|i| small_note(i, 1000)).collect();
    let parse = measure(
        rounds,
        || (),
        |()| {
            notes
                .iter()
                .map(|(path, content)| parse_document(content, path))
                .collect::<Vec<_>>()
        },
    );
    row(
        "parse 1,000 small notes",
        &parse,
        &format!(
            "{:.1} us/note (best)",
            parse.min.as_secs_f64() * 1e6 / 1000.0
        ),
    );

    for total in [1000, 5000] {
        let total = settings.size(total).max(200);
        let docs = small_notes(total);
        let build = measure(rounds, || docs.clone(), Index::build);
        row(
            &format!("Index::build, {total} notes (clone not timed)"),
            &build,
            &format!(
                "{:.1} us/note (best)",
                build.min.as_secs_f64() * 1e6 / total as f64
            ),
        );
    }

    let index = Index::build(small_notes(1000));
    let resolve = measure(
        rounds,
        || (),
        |()| {
            let mut found = 0;
            for i in 0..10_000 {
                let target = format!("note_{}", i % 1000);
                if index.resolve_link(black_box(&target)).is_some() {
                    found += 1;
                }
            }
            found
        },
    );
    row(
        "resolve_link x 10,000 (target string built each time)",
        &resolve,
        &format!(
            "{:.0} ns/lookup (best)",
            resolve.min.as_secs_f64() * 1e9 / 10_000.0
        ),
    );
    let backlinks = measure(
        rounds,
        || (),
        |()| {
            (0..1000)
                .map(|i| {
                    let id = DocId::new(format!("folder_{}/note_{i}.md", i % 10));
                    index.backlinks_of(&id).count()
                })
                .sum::<usize>()
        },
    );
    row(
        "backlinks_of x 1,000",
        &backlinks,
        &format!(
            "{:.0} ns/query (best)",
            backlinks.min.as_secs_f64() * 1e9 / 1000.0
        ),
    );

    // Editing one note of a big vault: ordinary typing keeps the note's title, aliases and file
    // name; changing the title (typing in the H1) makes the index rebuild what it derives.
    let total = settings.size(5000).max(500);
    let mut big = Index::build(small_notes(total));
    let path = PathBuf::from("folder_42/note_42.md");
    let mut counter = 0u64;
    let typing = measure(
        rounds.max(9),
        || {
            counter += 1;
            format!(
                "---\ntitle: Note 42\naliases: [N42, NoteAlias42]\ntags: [tag2, project/sub]\n---\n\n# Note 42\n\nLink to [[Note 43]] and more words {counter}.\n"
            )
        },
        |text| big.replace_doc(parse_document(&text, &path)),
    );
    let mut counter = 0u64;
    let retitle = measure(
        rounds.max(9),
        || {
            counter += 1;
            format!(
                "---\ntitle: Note 42 v{counter}\naliases: [N42, NoteAlias42]\ntags: [tag2, project/sub]\n---\n\n# Note 42 v{counter}\n\nLink to [[Note 43]] and more words.\n"
            )
        },
        |text| big.replace_doc(parse_document(&text, &path)),
    );
    row(
        &format!("replace_doc in a {total}-note vault: ordinary typing"),
        &typing,
        "(parse included)",
    );
    row(
        &format!("replace_doc in a {total}-note vault: title changed"),
        &retitle,
        &format!(
            "{:.0}x typing (best)",
            retitle.min.as_secs_f64() / typing.min.as_secs_f64().max(1e-12)
        ),
    );

    // Does parsing one big note grow in proportion to its size? By what the paragraphs hold, so
    // that a growing cost can be tied to links or tags.
    // (what the paragraphs hold, one paragraph, a known cost to explain a superlinear exponent)
    let variants: &[(&str, &str, Option<&str>)] = &[
        (
            "plain paragraphs",
            "A paragraph of plain prose with some words in it.\n\n",
            None,
        ),
        (
            "paragraphs with links",
            "A paragraph with a [[link]] and a [md](other.md) in it.\n\n",
            None,
        ),
        (
            "paragraphs with tags",
            "A paragraph with a #tag and some words in it.\n\n",
            None,
        ),
        (
            "paragraphs with links and tags",
            "A paragraph of prose with a [[link]] and a [md](other.md) and #tag words.\n\n",
            // Each tag is checked against every link span of the note. Only a note with thousands
            // of tags AND thousands of links shows it; not fixed, not worth it for real notes.
            Some("every tag is checked against every link of the note"),
        ),
    ];
    for (what, unit, known) in variants {
        let sizes: Vec<usize> = [4000, 8000, 16000]
            .iter()
            .map(|&n| settings.size(n))
            .collect();
        let docs: Vec<String> = sizes.iter().map(|&n| prose_note(unit, n)).collect();
        let bytes: Vec<usize> = docs.iter().map(String::len).collect();
        scaling_family(
            &format!("parse_document, one note of {what}"),
            &sizes,
            &bytes,
            rounds,
            *known,
            flagged,
            |i| {
                let started = Instant::now();
                black_box(parse_document(
                    black_box(&docs[i]),
                    &PathBuf::from("big.md"),
                ));
                started.elapsed()
            },
        );
    }

    let sizes: Vec<usize> = [1000, 2000, 4000, 8000]
        .iter()
        .map(|&n| settings.size(n).max(200))
        .collect();
    let sets: Vec<Vec<Document>> = sizes.iter().map(|&n| small_notes(n)).collect();
    let bytes: Vec<usize> = sizes.clone();
    scaling_family(
        "Index::build, n notes (bytes column = notes)",
        &sizes,
        &bytes,
        rounds,
        None,
        flagged,
        |i| {
            let docs = sets[i].clone();
            let started = Instant::now();
            black_box(Index::build(docs));
            started.elapsed()
        },
    );
}

fn formatter_families(settings: &Settings, flagged: &mut Vec<String>) {
    println!("\n--- the formatter on one note of about the same size, by kind of content ---");
    let config = FormatterConfig::default();
    let families: &[Family] = &[
        (
            "clean prose (nothing to change)",
            prose_clean,
            4000,
            Some(false),
        ),
        (
            "dirty prose (spaces, _emphasis_, * lists)",
            prose_dirty,
            4000,
            Some(true),
        ),
        (
            "dirty prose, CRLF line endings",
            prose_dirty_crlf,
            4000,
            Some(true),
        ),
        ("tables (misaligned, non-ASCII)", tables, 4000, Some(true)),
        ("messy lists (numbering, nested)", lists, 6000, Some(true)),
        (
            "code blocks and inline code, no `$`",
            code_heavy,
            5000,
            None,
        ),
        (
            "code blocks and inline code, one `$5`",
            code_heavy_with_a_dollar,
            5000,
            None,
        ),
        (
            "math: inline formulas and $$ blocks",
            math_heavy,
            5000,
            None,
        ),
    ];
    let mut without_dollar = None;
    for (label, make, units, must_change) in families {
        let text = make(settings.size(*units));
        let out = satz_core::formatter::format_document(&text, &config);
        if let Some(expected) = must_change {
            assert_eq!(
                out != text,
                *expected,
                "the `{label}` document is not what its label says"
            );
        }
        let sample = measure(
            settings.rounds,
            || (),
            |()| satz_core::formatter::format_document(black_box(&text), &config),
        );
        row(
            &format!("{label} ({:.0} KB)", text.len() as f64 / 1024.0),
            &sample,
            &format!("{:.0} MB/s (best)", mb_per_s(text.len(), sample.min)),
        );
        if label.ends_with("no `$`") {
            without_dollar = Some((sample.min, text.len()));
        }
        if label.ends_with("one `$5`")
            && let Some((plain, plain_bytes)) = without_dollar
        {
            // Same number of blocks, so the times are comparable.
            let ratio = sample.min.as_secs_f64() / plain.as_secs_f64().max(1e-12);
            println!(
                "  {:<60} {ratio:.2}x the time of the note without it ({} vs {} bytes)",
                "  -> the `$5` costs",
                text.len(),
                plain_bytes
            );
            if ratio > DOLLAR_RATIO_LIMIT {
                flagged.push(format!(
                    "one `$5` makes the note {ratio:.1}x slower to format"
                ));
            }
        }
    }

    // A long note of prose with links, wrapping on: the worst case of the wrap pass.
    let mut wrap = FormatterConfig::default();
    wrap.wrap.enable = true;
    let long = "A paragraph of prose with a [[link|alias]] and some more words to wrap at eighty columns, so that every line has to be reflowed.\n\n"
        .repeat(settings.size(30_000));
    let sample = measure(
        settings.rounds.min(5),
        || (),
        |()| satz_core::formatter::format_document(black_box(&long), &wrap),
    );
    row(
        &format!(
            "prose with links, wrapping on ({:.1} MB)",
            long.len() as f64 / 1_048_576.0
        ),
        &sample,
        &format!("{:.0} MB/s (best)", mb_per_s(long.len(), sample.min)),
    );
}

fn formatter_scaling(settings: &Settings, flagged: &mut Vec<String>) {
    println!("\n--- does the formatter's time grow in proportion to the size of the note? ---");
    let config = FormatterConfig::default();
    let families: &[(&str, Make)] = &[
        ("code blocks and one `$5`", code_heavy_with_a_dollar),
        ("math: inline formulas and $$ blocks", math_heavy),
        (
            "inline math beside inline code (dense)",
            inline_math_beside_inline_code,
        ),
        ("code blocks and inline code, no `$`", code_heavy),
        ("tables", tables),
        ("dirty prose", prose_dirty),
        ("messy lists", lists),
    ];
    for (name, make) in families {
        let sizes: Vec<usize> = [1000, 2000, 4000, 8000]
            .iter()
            .map(|&n| settings.size(n))
            .collect();
        let docs: Vec<String> = sizes.iter().map(|&n| make(n)).collect();
        let bytes: Vec<usize> = docs.iter().map(String::len).collect();
        scaling_family(name, &sizes, &bytes, settings.rounds, None, flagged, |i| {
            let started = Instant::now();
            black_box(satz_core::formatter::format_document(
                black_box(&docs[i]),
                &config,
            ));
            started.elapsed()
        });
    }
}

/// What `satz.formatWorkspace` and formatting edits do with the formatter, without the server.
fn near_the_server(settings: &Settings) {
    println!("\n--- close to what the server does (without the server) ---");
    let config = VaultConfig::default();
    let docs = small_notes(1000);
    let cold = measure(
        settings.rounds,
        || (),
        |()| {
            docs.iter()
                .filter(|doc| {
                    let source = doc.line_index.source();
                    satz_core::formatter::format_document(source, &config.formatter) != source
                })
                .count()
        },
    );
    row(
        "format 1,000 small notes that are already clean",
        &cold,
        &format!(
            "{:.1} us/note (best)",
            cold.min.as_secs_f64() * 1e6 / 1000.0
        ),
    );

    // Only the look-up and the copy of a text kept by content hash -- NOT the server's
    // `FormatCache`, which lives in satz-lsp. It says what a hit costs at the very least.
    let cache: std::collections::HashMap<u64, String> = docs
        .iter()
        .map(|doc| (doc.content_hash, doc.line_index.source().to_string()))
        .collect();
    let lookup = measure(
        settings.rounds,
        || (),
        |()| {
            docs.iter()
                .filter_map(|doc| cache.get(&doc.content_hash).cloned())
                .map(|text| text.len())
                .sum::<usize>()
        },
    );
    row(
        "hash look-up + copy of 1,000 texts (not the server's cache)",
        &lookup,
        &format!(
            "{:.0} ns/note (best)",
            lookup.min.as_secs_f64() * 1e9 / 1000.0
        ),
    );

    // Formatting edits: the formatter, then the line diff that turns the result into small edits.
    let messy = "# Heading\n\n".to_string()
        + &"Line with content. \nAnother line.\nYet another.\nMore text here.\n".repeat(50);
    let formatted = satz_core::formatter::format_document(&messy, &config.formatter);
    let edits = satz_core::formatter::diff::line_diff(&messy, &formatted);
    let minimal_bytes: usize = edits
        .iter()
        .map(|e| e.new_lines.iter().map(|l| l.len()).sum::<usize>())
        .sum();
    let diff = measure(
        settings.rounds,
        || (),
        |()| {
            let out = satz_core::formatter::format_document(black_box(&messy), &config.formatter);
            satz_core::formatter::diff::line_diff(&messy, &out)
        },
    );
    row(
        &format!(
            "format + line_diff of a {}-line note",
            messy.lines().count()
        ),
        &diff,
        &format!(
            "{} edits, {minimal_bytes} bytes (a whole replace: {} bytes)",
            edits.len(),
            formatted.len()
        ),
    );
}
