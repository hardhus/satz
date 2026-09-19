//! The incremental index (`replace_doc` / `remove_doc`) must always agree with an index built from
//! scratch out of the same documents. A deterministic pseudo-random sequence of edits is applied
//! and, after every step, everything observable about the index is compared.

use satz_core::{DocId, Document, Index, parse_document};
use std::collections::BTreeMap;
use std::path::Path;

/// Small xorshift generator: fixed seeds keep the sequences reproducible without a dependency.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[(self.next() % items.len() as u64) as usize]
    }

    fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }
}

const PATHS: &[&str] = &[
    "a.md",
    "b.md",
    "c.md",
    "sub/a.md",
    "sub/x.md",
    "x.md",
    "deep/er/note.md",
    "Books/Rust.md",
    "tlp/2.0121.md",
];

/// Things a link may point at: stems, paths, titles, aliases, spellings with other case.
const TARGETS: &[&str] = &[
    "a",
    "b",
    "c",
    "x",
    "sub/a",
    "sub/x.md",
    "note",
    "Rust",
    "books/rust",
    "tlp/2.0121",
    "Alpha",
    "beta",
    "Gamma Note",
    "missing",
    "SUB/A",
];

const TITLES: &[&str] = &["Alpha", "Beta", "Gamma Note", "Rust", "Untitled thing"];
const ALIASES: &[&str] = &["beta", "alpha", "gam", "note", "Rust"];
const TAGS: &[&str] = &["one", "Two", "two", "three/sub"];

fn random_document(rng: &mut Rng, path: &str) -> Document {
    let mut text = String::new();
    if rng.chance(60) {
        text.push_str("---\n");
        if rng.chance(70) {
            text.push_str(&format!("title: {}\n", rng.pick(TITLES)));
        }
        if rng.chance(50) {
            text.push_str(&format!("aliases: [{}]\n", rng.pick(ALIASES)));
        }
        if rng.chance(40) {
            text.push_str(&format!("tags: [{}]\n", rng.pick(TAGS)));
        }
        text.push_str("---\n");
    }
    if rng.chance(50) {
        text.push_str(&format!("# {}\n\n", rng.pick(TITLES)));
    }
    text.push_str("## Section\n\n");
    for _ in 0..(rng.next() % 4) {
        match rng.next() % 5 {
            0 => text.push_str(&format!("[[{}]] ", rng.pick(TARGETS))),
            1 => text.push_str(&format!("[[{}#Section]] ", rng.pick(TARGETS))),
            2 => text.push_str(&format!("[t]({}.md) ", rng.pick(TARGETS))),
            3 => text.push_str(&format!("#{} ", rng.pick(TAGS))),
            _ => text.push_str("[[#Section]] "),
        }
    }
    text.push('\n');
    parse_document(&text, Path::new(path))
}

/// Everything observable about an index, in a deterministic textual form.
fn observe(index: &Index) -> String {
    let mut ids: Vec<&DocId> = index.documents().map(|d| &d.id).collect();
    ids.sort();

    let mut per_doc = BTreeMap::new();
    for id in &ids {
        let mut backlinks: Vec<String> = index
            .backlinks_of(id)
            .map(|b| b.as_str().to_string())
            .collect();
        backlinks.sort();
        per_doc.insert(id.as_str().to_string(), backlinks);
    }

    let mut resolved = BTreeMap::new();
    for target in TARGETS.iter().chain(PATHS.iter()) {
        resolved.insert(
            target.to_string(),
            index.resolve_link(target).map(|id| id.as_str().to_string()),
        );
    }

    let mut orphans: Vec<String> = index
        .orphan_docs()
        .map(|d| d.id.as_str().to_string())
        .collect();
    orphans.sort();
    let mut tags: Vec<String> = index.all_tags().iter().map(|t| t.to_string()).collect();
    tags.sort();
    let broken = index.docs_with_broken_links().count();

    format!(
        "backlinks={per_doc:?}\nresolved={resolved:?}\norphans={orphans:?}\ntags={tags:?}\nbroken={broken}\n"
    )
}

fn assert_equivalent(incremental: &Index, docs: &BTreeMap<String, Document>, context: &str) {
    let rebuilt = Index::build(docs.values().cloned().collect());
    assert_eq!(
        observe(incremental),
        observe(&rebuilt),
        "incremental index differs from a fresh build {context}"
    );
}

fn run(seed: u64, steps: usize) {
    let mut rng = Rng(seed);
    let mut docs: BTreeMap<String, Document> = BTreeMap::new();
    let mut index = Index::build(Vec::new());
    let mut history: Vec<String> = Vec::new();

    for step in 0..steps {
        let path = *rng.pick(PATHS);
        if docs.contains_key(path) && rng.chance(30) {
            docs.remove(path);
            index.remove_doc(&DocId::new(path));
            history.push(format!("remove {path}"));
        } else {
            let doc = random_document(&mut rng, path);
            docs.insert(path.to_string(), doc.clone());
            index.replace_doc(doc);
            history.push(format!("replace {path}"));
        }
        assert_equivalent(
            &index,
            &docs,
            &format!("(seed {seed}, step {step}: {})", history.join(" -> ")),
        );
    }
}

#[test]
fn incremental_updates_match_a_fresh_build_for_many_random_histories() {
    for seed in 1..=60u64 {
        run(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1, 40);
    }
}

#[test]
fn removing_every_document_leaves_an_empty_index() {
    let mut rng = Rng(42);
    let mut index = Index::build(Vec::new());
    for path in PATHS {
        index.replace_doc(random_document(&mut rng, path));
    }
    for path in PATHS {
        index.remove_doc(&DocId::new(*path));
    }
    assert_eq!(index.doc_count(), 0);
    assert_eq!(observe(&index), observe(&Index::build(Vec::new())));
}

#[test]
fn replacing_a_document_with_itself_changes_nothing() {
    let mut rng = Rng(7);
    let docs: Vec<Document> = PATHS.iter().map(|p| random_document(&mut rng, p)).collect();
    let mut index = Index::build(docs.clone());
    let before = observe(&index);
    for doc in docs {
        index.replace_doc(doc);
        assert_eq!(observe(&index), before);
    }
}
