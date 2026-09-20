//! Math (`$...$`, `$$...$$`) is never changed by the formatter: pulldown-cmark does not know it, so
//! without care its `*`, `_`, `-`, numbers and line breaks look like emphasis, lists and prose.

use satz_core::config::FormatterConfig;
use satz_core::formatter::format_document;

/// A configuration that changes as much as possible, so math that is not protected shows it.
fn busy_config(width: usize) -> FormatterConfig {
    let mut cfg = FormatterConfig::default();
    cfg.emphasis.italic_marker = "_".to_string();
    cfg.emphasis.bold_marker = "__".to_string();
    cfg.lists.marker = "*".to_string();
    cfg.wrap.enable = true;
    cfg.line_width = width;
    cfg
}

fn fmt(src: &str) -> String {
    format_document(src, &busy_config(30))
}

fn wide(src: &str) -> String {
    format_document(src, &busy_config(500))
}

fn assert_kept(src: &str) {
    let out = fmt(src);
    assert_eq!(out, src, "changed:\n{src}\n--- into ---\n{out}");
    assert_eq!(wide(src), src, "changed with a wide line:\n{src}");
}

#[test]
fn emphasis_looking_text_inside_inline_math_is_left_alone() {
    assert_kept("$x*y*z$\n");
    assert_kept("Let $a *b* c$ hold.\n");
    assert_kept("Let $a**b**c$ hold.\n");
    assert_eq!(
        wide("Norm $|x|_*$ and\n$\\frac{a}{b}$ and\n$a_1 * b_2$.\n"),
        "Norm $|x|_*$ and $\\frac{a}{b}$ and $a_1 * b_2$.\n"
    );
    assert_kept("Display $$x*y*z$$ on one line.\n");
}

#[test]
fn text_around_inline_math_is_still_formatted() {
    assert_eq!(
        wide("An *outer* word and $a*b*c$ inside.\n"),
        "An _outer_ word and $a*b*c$ inside.\n"
    );
}

#[test]
fn a_display_block_keeps_every_line_byte_for_byte() {
    assert_kept("$$\n- a\n- b\n$$\n");
    assert_kept("$$\n1. x\n3. y\n$$\n");
    assert_kept("$$\nx = y   \n\\\\\n% a comment\nz\n$$\n");
    assert_kept("$$\na\n\n\n\nb\n$$\n");
    assert_kept("$$\n  \\begin{aligned}\n  a &= b \\\\\n  c &= d\n  \\end{aligned}\n$$\n");
    assert_kept("Before.\n\n$$ x + y $$\n\nAfter.\n");
    assert_kept("$$x\ny$$\n");
}

#[test]
fn a_display_block_is_not_joined_with_the_prose_around_it() {
    let src = "some text before the block that is long enough\n$$\nx = y\n$$\nand some text after the block that is long\n";
    let out = fmt(src);
    assert!(out.contains("\n$$\nx = y\n$$\n"), "{out}");
    // The prose around it wraps as usual, on its own.
    for line in out
        .lines()
        .filter(|l| !l.starts_with('$') && !l.contains('='))
    {
        assert!(line.chars().count() <= 30, "{line:?}");
    }
}

#[test]
fn long_math_is_never_split_by_wrapping() {
    let inline = "$a + b + c + d + e + f + g + h + i + j + k$";
    let out = fmt(&format!(
        "Words {inline} more words that follow the formula.\n"
    ));
    assert!(out.contains(inline), "{out}");
    let block = "$$\na + b + c + d + e + f + g + h + i + j + k + l + m + n + o + p\n$$";
    let out = fmt(&format!("Intro.\n\n{block}\n\nOutro.\n"));
    assert!(out.contains(block), "{out}");
}

#[test]
fn dollar_signs_that_are_not_math_are_formatted_as_text() {
    // Currency, spaced, escaped and unclosed dollars are prose: emphasis after them is normalised.
    assert_eq!(
        wide("It costs $5 and $10 and *more*.\n"),
        "It costs $5 and $10 and _more_.\n"
    );
    assert_eq!(
        wide("A $ x $ is not math *here*.\n"),
        "A $ x $ is not math _here_.\n"
    );
    assert_eq!(
        wide("Escaped \\$5 and \\$6 with *e*.\n"),
        "Escaped \\$5 and \\$6 with _e_.\n"
    );
    assert_eq!(
        wide("Only one $ sign *here*.\n"),
        "Only one $ sign _here_.\n"
    );
    assert_eq!(
        wide("$$ never closed\n\n*after*\n"),
        "$$ never closed\n\n_after_\n"
    );
}

#[test]
fn math_inside_code_is_code_and_is_not_processed_twice() {
    assert_kept("```\n$$\n- a\n$$\n```\n");
    assert_kept("Use `$x*y*z$` literally.\n");
    assert_kept("    $$\n    - indented code\n    $$\n");
}

#[test]
fn math_in_list_items_headings_and_tables() {
    assert_eq!(
        wide("- item $a*b*c$ here\n- item two\n"),
        "* item $a*b*c$ here\n* item two\n"
    );
    assert_kept("# Title $a*b*c$\n");
    let table = "| a | b |\n| --- | --- |\n| $x*y*z$ | text |\n";
    let out = wide(table);
    assert!(out.contains("$x*y*z$"), "{out}");
}

#[test]
fn a_display_block_inside_a_list_item_stays_inside_it() {
    let src = "* item text\n\n  $$\n  - a\n  - b\n  $$\n\n* next\n";
    assert_kept(src);
}

#[test]
fn line_endings_and_unicode_do_not_matter() {
    let lf = "Ünal $a*b*c$ 🦀 and *e*.\n\n$$\n- ığ\n$$\n";
    let crlf = lf.replace('\n', "\r\n");
    let out_lf = wide(lf);
    let out_crlf = wide(&crlf);
    assert!(
        out_lf.contains("$a*b*c$") && out_lf.contains("$$\n- ığ\n$$"),
        "{out_lf}"
    );
    assert_eq!(out_crlf, out_lf.replace('\n', "\r\n"));
}

#[test]
fn formatting_is_idempotent_and_math_survives_in_mixed_documents() {
    let src = "# Notes $E=mc^2$\n\nSome *emphasis* and $a*b*$ and $$c*d*$$ text   \n\n\n\n$$\n- x\n1. y\n$$\n\n- list $q*r*$\n- another\n\n| t | u |\n|---|---|\n| $z$ | w |\n\n```\n$$ code $$\n```\n\nEnd $1 and $2.\n";
    for width in [20, 40, 500] {
        let cfg = busy_config(width);
        let once = format_document(src, &cfg);
        assert_eq!(format_document(&once, &cfg), once, "width {width}\n{once}");
        for piece in [
            "$E=mc^2$",
            "$a*b*$",
            "$$c*d*$$",
            "$$\n- x\n1. y\n$$",
            "$q*r*$",
            "$$ code $$",
        ] {
            assert!(
                once.contains(piece),
                "width {width} lost {piece:?}:\n{once}"
            );
        }
    }
}

#[test]
fn a_document_with_the_reserved_characters_is_formatted_as_before_without_masking() {
    // Private-use characters in the source: the formatter does not risk a clash and treats math as
    // prose, exactly as it did before math support (the text is still formatted, not refused).
    let src = "Has \u{E000} inside and *emph* and $x$.\n";
    assert_eq!(wide(src), "Has \u{E000} inside and _emph_ and $x$.\n");
}

#[test]
fn thousands_of_formulas_do_not_corrupt_anything() {
    let mut src = String::new();
    for i in 0..7000 {
        src.push_str(&format!("Line {i} with $x_{i}*a*$ and *e*.\n\n"));
    }
    let out = wide(&src);
    assert!(out.contains("$x_0*a*$") && out.contains("$x_6999*a*$"));
    assert_eq!(out.matches("$x_").count(), 7000);
}

#[test]
fn unclosed_empty_and_quoted_math_are_handled_without_damage() {
    // Unclosed `$$` does not swallow the rest of the note.
    let out = wide("$$\nnever closed\n\n*after* it\n");
    assert!(out.contains("$$") && out.contains("never closed"), "{out}");
    assert!(out.contains("_after_ it"), "{out}");
    // Empty and adjacent display markers.
    assert_kept("$$ $$\n");
    assert_kept("$$$$\n");
    // A formula inside a quote and one with a pipe in a table are kept as written.
    assert_kept("> quoted $a*b*c$ text\n");
    let table = "| a | b |\n| --- | --- |\n| $|x|$ | y |\n";
    let out = wide(table);
    assert!(out.contains("$|x|$"), "{out}");
}

/// Deterministic pseudo-random documents: every formula must come out exactly as it went in, and
/// a second formatting pass must change nothing.
#[test]
fn random_mixed_documents_keep_every_formula_and_reach_a_fixed_point() {
    let pieces = [
        "plain words here",
        "*emph*",
        "**strong**",
        "$a*b*c$",
        "$x_1 + y_2$",
        "$$E = mc^2$$",
        "$5 and $6",
        "\\$7",
        "`code $x*y*$`",
        "[[link]]",
        "Ünal 🦀",
        "trailing   ",
    ];
    let blocks = [
        "$$\n- a\n1. b\n$$",
        "$$\nx   \n\n\ny\n$$",
        "- item $q*r*$\n- other",
        "1. one\n3. three",
        "# Head $h*i*$",
        "```\n$$ raw $$\n- x\n```",
        "> quote $u*v*$",
    ];
    let mut seed = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for round in 0..150 {
        let mut doc = String::new();
        for _ in 0..(3 + next() % 6) {
            if next() % 3 == 0 {
                doc.push_str(blocks[(next() % blocks.len() as u64) as usize]);
                doc.push_str("\n\n");
            } else {
                for _ in 0..(1 + next() % 5) {
                    doc.push_str(pieces[(next() % pieces.len() as u64) as usize]);
                    doc.push(' ');
                }
                doc.push_str("\n\n");
            }
        }
        for width in [25, 500] {
            let cfg = busy_config(width);
            let once = format_document(&doc, &cfg);
            assert_eq!(
                format_document(&once, &cfg),
                once,
                "round {round} width {width} not idempotent:\n{doc}\n--- once ---\n{once}"
            );
            for formula in [
                "$a*b*c$",
                "$x_1 + y_2$",
                "$$E = mc^2$$",
                "$$\n- a\n1. b\n$$",
            ] {
                assert_eq!(
                    once.matches(formula).count(),
                    doc.matches(formula).count(),
                    "round {round} width {width}: {formula:?} changed\n{doc}\n--- once ---\n{once}"
                );
            }
        }
    }
}
