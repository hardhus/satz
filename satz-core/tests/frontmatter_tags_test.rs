//! Frontmatter tags carry the byte range of their own spelling in the frontmatter, so anything that
//! points at a tag (references, highlights) lands on it -- never on another key, a comment, or the
//! start of the file.

use satz_core::parse_document;
use std::path::Path;

/// `(tag name, the source text its range covers)` for the frontmatter tags of `source`.
fn fm_tags(source: &str) -> Vec<(String, String)> {
    let doc = parse_document(source, Path::new("n.md"));
    let count = doc.frontmatter.tags.len();
    doc.tags
        .iter()
        .take(count)
        .map(|t| {
            (
                t.name.clone(),
                source[t.range.start..t.range.end].to_string(),
            )
        })
        .collect()
}

fn pairs(items: &[(&str, &str)]) -> Vec<(String, String)> {
    items
        .iter()
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect()
}

/// `(line, column)` of the start of the first frontmatter tag's range.
fn first_tag_position(source: &str) -> (u32, u32) {
    let doc = parse_document(source, Path::new("n.md"));
    let pos = doc.line_index.byte_to_position(doc.tags[0].range.start);
    (pos.line, pos.character)
}

#[test]
fn a_flow_list_a_block_list_and_a_plain_string_each_point_at_their_own_words() {
    assert_eq!(
        fm_tags("---\ntags: [alpha, beta]\n---\n"),
        pairs(&[("alpha", "alpha"), ("beta", "beta")])
    );
    assert_eq!(
        fm_tags("---\ntags:\n  - alpha\n  - beta\n---\n"),
        pairs(&[("alpha", "alpha"), ("beta", "beta")])
    );
    assert_eq!(
        fm_tags("---\ntags:\n- alpha\n- beta\n---\n"),
        pairs(&[("alpha", "alpha"), ("beta", "beta")])
    );
    assert_eq!(
        fm_tags("---\ntags: alpha, beta\n---\n"),
        pairs(&[("alpha", "alpha"), ("beta", "beta")])
    );
    assert_eq!(
        fm_tags("---\ntags: [\"b c\", 'd e']\n---\n"),
        pairs(&[("b c", "b c"), ("d e", "d e")])
    );
}

#[test]
fn a_tag_named_like_its_own_key_is_found_in_the_value_not_in_the_key() {
    let src = "---\ntags: [tags]\n---\n";
    assert_eq!(fm_tags(src), pairs(&[("tags", "tags")]));
    assert_eq!(first_tag_position(src), (1, 7), "after `tags: [`");
    let src = "---\ntag: tag\n---\n";
    assert_eq!(fm_tags(src), pairs(&[("tag", "tag")]));
    assert_eq!(first_tag_position(src), (1, 5));
}

#[test]
fn other_keys_and_comments_never_steal_a_tag() {
    let src = "---\ntitle: rust notes\naliases: [rust]\ntags: [rust]\nsummary: about rust\n---\n";
    assert_eq!(fm_tags(src), pairs(&[("rust", "rust")]));
    assert_eq!(first_tag_position(src), (3, 7));

    let commented = "---\ntags:\n  # rust is great\n  - rust\n---\n";
    assert_eq!(fm_tags(commented), pairs(&[("rust", "rust")]));
    assert_eq!(
        first_tag_position(commented),
        (3, 4),
        "not the comment on line 2"
    );
}

#[test]
fn the_singular_and_the_plural_key_each_locate_their_own_tags() {
    let src = "---\ntag: first\ntags: [second]\n---\n";
    let found = fm_tags(src);
    assert!(
        found.contains(&("first".to_string(), "first".to_string())),
        "{found:?}"
    );
    assert!(
        found.contains(&("second".to_string(), "second".to_string())),
        "{found:?}"
    );
    let doc = parse_document(src, Path::new("n.md"));
    let lines: Vec<u32> = doc
        .tags
        .iter()
        .map(|t| doc.line_index.byte_to_position(t.range.start).line)
        .collect();
    assert!(lines.contains(&1) && lines.contains(&2), "{lines:?}");
}

#[test]
fn duplicates_get_separate_ranges_in_order() {
    let src = "---\ntags: [a, b, a]\n---\n";
    let doc = parse_document(src, Path::new("n.md"));
    let starts: Vec<usize> = doc
        .tags
        .iter()
        .take(doc.frontmatter.tags.len())
        .map(|t| t.range.start)
        .collect();
    let mut sorted = starts.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(
        sorted.len(),
        starts.len(),
        "every tag has its own range: {starts:?}"
    );
    assert_eq!(
        starts,
        {
            let mut s = starts.clone();
            s.sort();
            s
        },
        "ranges follow the order in the file"
    );
}

#[test]
fn spelling_hash_prefixes_case_and_unicode() {
    assert_eq!(
        fm_tags("---\ntags: [\"#proje\", Rust, 'İş', 🦀x]\n---\n"),
        pairs(&[
            ("proje", "proje"),
            ("Rust", "Rust"),
            ("İş", "İş"),
            ("🦀x", "🦀x")
        ])
    );
    // A name the parser stores differently from the source spelling still points at the source.
    let src = "---\ntags: [Rust]\n---\n";
    let doc = parse_document(src, Path::new("n.md"));
    let t = &doc.tags[0];
    assert_eq!(&src[t.range.start..t.range.end], "Rust");
}

#[test]
fn crlf_frontmatter_is_located_like_lf() {
    let lf = "---\ntitle: x\ntags: [a, b]\n---\n";
    let crlf = lf.replace('\n', "\r\n");
    assert_eq!(fm_tags(&crlf), pairs(&[("a", "a"), ("b", "b")]));
    assert_eq!(first_tag_position(&crlf), first_tag_position(lf));
}

#[test]
fn a_tag_that_cannot_be_found_points_at_its_key_line_never_at_nothing() {
    // The escape makes the stored name differ from the source spelling.
    let src = "---\ntitle: x\ntags: [\"\\u00fc\"]\n---\n";
    let doc = parse_document(src, Path::new("n.md"));
    assert_eq!(doc.frontmatter.tags, vec!["ü".to_string()]);
    let t = &doc.tags[0];
    assert!(t.range.start < t.range.end, "not empty: {:?}", t.range);
    let covered = &src[t.range.start..t.range.end];
    assert!(covered.contains("tags:"), "the key line: {covered:?}");
    let line = doc.line_index.byte_to_position(t.range.start).line;
    assert_eq!(line, 2);
}

#[test]
fn no_frontmatter_tag_ever_has_an_empty_range_and_body_tags_are_unchanged() {
    for src in [
        "---\ntags: [a, b]\n---\n#body\n",
        "---\ntags:\n---\ntext #body\n",
        "---\ntitle: t\n---\n#only-body\n",
        "---\ntags: [2024, real]\n---\n",
        "---\ntags: \"x, y\"\n---\n",
    ] {
        let doc = parse_document(src, Path::new("n.md"));
        for tag in &doc.tags {
            assert!(tag.range.start < tag.range.end, "{src:?}: {tag:?}");
            assert!(tag.range.end <= src.len());
        }
    }
    let doc = parse_document("---\ntags: [a]\n---\ntext #body\n", Path::new("n.md"));
    let body = doc.tags.last().unwrap();
    assert_eq!(body.name, "body");
}
