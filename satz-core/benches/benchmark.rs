use std::path::PathBuf;
use std::time::Instant;

use satz_core::{DocId, Index, parse_document};

fn main() {
    println!("================ SATZ PERFORMANCE BENCHMARK ================");

    // 1. Generate synthetic vault of 1000 notes
    let t0 = Instant::now();
    let mut docs = Vec::with_capacity(1000);
    for i in 0..1000 {
        let content = format!(
            "---\ntitle: Note {i}\naliases: [N{i}, NoteAlias{i}]\ntags: [tag{}, project/sub]\n---\n\n# Note {i}\n\nLink to [[Note {}]] and [[books/note-{}.md#Heading]] and [[Note {}#^block-1]].\n\nSome paragraph text here. ^block-1\n",
            i % 10,
            (i + 1) % 1000,
            (i + 2) % 1000,
            (i + 3) % 1000
        );
        let path = PathBuf::from(format!("folder_{}/note_{i}.md", i % 10));
        let doc = parse_document(&content, &path);
        docs.push(doc);
    }
    let parse_time = t0.elapsed();
    println!("1. Parsed 1,000 markdown documents: {:?}", parse_time);

    // 2. Build Index
    let t1 = Instant::now();
    let index = Index::build(docs);
    let build_time = t1.elapsed();
    println!("2. Built Index from 1,000 documents: {:?}", build_time);

    // 3. Resolve links (10,000 lookups)
    let t2 = Instant::now();
    let mut resolved_count = 0;
    for i in 0..10000 {
        let target = format!("note_{}", i % 1000);
        if index.resolve_link(&target).is_some() {
            resolved_count += 1;
        }
    }
    let resolve_time = t2.elapsed();
    println!(
        "3. Resolved 10,000 links (resolved: {}): {:?} (avg: {:?}/lookup)",
        resolved_count,
        resolve_time,
        resolve_time / 10000
    );

    // 4. Backlinks query (1,000 queries)
    let t3 = Instant::now();
    let mut total_backlinks = 0;
    for i in 0..1000 {
        let id = DocId::new(format!("folder_{}/note_{i}.md", i % 10));
        total_backlinks += index.backlinks_of(&id).count();
    }
    let backlink_time = t3.elapsed();
    println!(
        "4. Queried backlinks for 1,000 notes (found: {}): {:?}",
        total_backlinks, backlink_time
    );

    println!("============================================================");
}
