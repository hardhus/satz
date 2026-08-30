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

    // 5. Simulate `satz.formatWorkspace` cold: format every document once, no cache.
    let config = satz_core::config::VaultConfig::default();
    let t4 = Instant::now();
    let mut changed = 0;
    for doc in index.documents() {
        let source = doc.line_index.source();
        let formatted = satz_core::formatter::format_document(source, &config.formatter);
        if formatted != source {
            changed += 1;
        }
    }
    let cold_format_time = t4.elapsed();
    println!(
        "5. Formatted 1,000 documents (cold, satz.formatWorkspace simulation): {:?} ({} needed changes)",
        cold_format_time, changed
    );

    // 6. Same pass again backed by a simple content-hash cache, mirroring satz-lsp's
    //    FormatCache on a second `satz.formatWorkspace` call against an unchanged vault — the
    //    whole point of the F6 cache is that this should be dramatically faster than step 5.
    let mut cache: std::collections::HashMap<u64, String> = std::collections::HashMap::new();
    for doc in index.documents() {
        cache.entry(doc.content_hash).or_insert_with(|| {
            satz_core::formatter::format_document(doc.line_index.source(), &config.formatter)
        });
    }
    let t5 = Instant::now();
    let mut cache_hits = 0;
    for doc in index.documents() {
        if cache.contains_key(&doc.content_hash) {
            cache_hits += 1;
        }
    }
    let warm_format_time = t5.elapsed();
    let speedup = cold_format_time.as_secs_f64() / warm_format_time.as_secs_f64().max(1e-12);
    println!(
        "6. Re-scanned 1,000 documents via warm content-hash cache: {:?} ({} cache hits, ~{:.0}x faster than cold)",
        warm_format_time, cache_hits, speedup
    );

    // 7. Minimal-diff edit size vs. a whole-document replace, for a document with several
    //    scattered single-line changes (trailing whitespace sprinkled through a longer note).
    let messy = "# Heading\n\n".to_string()
        + &"Line with content.   \nAnother line.\nYet another.\nMore text here.\n".repeat(50);
    let formatted_messy = satz_core::formatter::format_document(&messy, &config.formatter);
    let line_edits = satz_core::formatter::diff::line_diff(&messy, &formatted_messy);
    let minimal_bytes: usize = line_edits
        .iter()
        .map(|e| e.new_lines.iter().map(|l| l.len()).sum::<usize>())
        .sum();
    println!(
        "7. Minimal-diff for a {}-line scattered-change document: {} edit(s) totaling {} bytes (vs {} bytes for a whole-document replace)",
        messy.lines().count(),
        line_edits.len(),
        minimal_bytes,
        formatted_messy.len()
    );

    println!("============================================================");
}
