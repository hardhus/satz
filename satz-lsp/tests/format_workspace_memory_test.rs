//! How much memory `satz.formatWorkspace` holds on a vault of thousands of notes.
//!
//! The heap is counted by an allocator of this test binary (a byte count, so unlike a time it is
//! the same on every run and on every machine).

use std::alloc::{GlobalAlloc, Layout, System};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

use satz_core::{Index, parse_document};
use satz_lsp::handlers::execute_command::{build_workspace_edit_versioned, compute_format_changes};
use satz_lsp::state::{FormatCache, SatzState};

struct Counting;

/// Bytes in use now, and the most that were in use since the last `measure` began.
static NOW: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

fn grew(by: usize) {
    let now = NOW.fetch_add(by, Relaxed) + by;
    PEAK.fetch_max(now, Relaxed);
}

// SAFETY: every call is passed on to the system allocator unchanged; only the counts are kept.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            grew(layout.size());
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
        NOW.fetch_sub(layout.size(), Relaxed);
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let new = unsafe { System.realloc(ptr, layout, new_size) };
        if !new.is_null() {
            if new_size >= layout.size() {
                grew(new_size - layout.size());
            } else {
                NOW.fetch_sub(layout.size() - new_size, Relaxed);
            }
        }
        new
    }
}

#[global_allocator]
static COUNTING: Counting = Counting;

/// What `f` needed at most beyond what was held when it began (`peak`), and what it still holds
/// once it has returned, its result included (`kept`).
struct Bytes {
    peak: usize,
    kept: usize,
}

fn measure<T>(f: impl FnOnce() -> T) -> (T, Bytes) {
    let base = NOW.load(Relaxed);
    PEAK.store(base, Relaxed);
    let result = f();
    let bytes = Bytes {
        peak: PEAK.load(Relaxed).saturating_sub(base),
        kept: NOW.load(Relaxed).saturating_sub(base),
    };
    (result, bytes)
}

fn root() -> PathBuf {
    if cfg!(windows) {
        PathBuf::from("C:\\vault")
    } else {
        PathBuf::from("/vault")
    }
}

/// A note of about 600 bytes; `dirty` adds what the formatter takes away (trailing spaces, blank
/// lines).
fn note(i: usize, dirty: bool) -> String {
    let filler = "A paragraph of plain prose with a [[link]] and some more words in it. ";
    let (tail, gap) = if dirty {
        ("   ", "\n\n\n\n")
    } else {
        ("", "\n\n")
    };
    format!(
        "# Note {i}{tail}{gap}{}{tail}{gap}- one{tail}\n- two{tail}\n",
        filler.repeat(7).trim_end()
    )
}

fn state_of(notes: usize, dirty: bool) -> (SatzState, usize) {
    let mut state = SatzState::default();
    let mut total = 0;
    let docs = (0..notes)
        .map(|i| {
            let text = note(i, dirty);
            total += text.len();
            parse_document(&text, Path::new(&format!("dir{}/n{i}.md", i % 20)))
        })
        .collect();
    state.index = Index::build(docs);
    state.set_vault_root(Some(root()));
    state.format_cache = FormatCache::new(notes * 2);
    (state, total)
}

fn kib(bytes: usize) -> String {
    format!("{:>9.1} KiB", bytes as f64 / 1024.0)
}

/// What each stage of the workspace format holds, against the text of all the notes.
///
/// One test on purpose: the counts are those of the whole process, so nothing else may run beside
/// it. The limits are the ratios to the text of the notes, with room for the size of a URI and of
/// the allocator; before the change was made these were 1.10 / 1.08 (formatted notes: computing),
/// 4.67 (dirty notes: what computing holds) and 1.60 (building the edit), which the limits rule
/// out.
#[test]
fn what_the_workspace_format_holds() {
    for (label, dirty) in [
        ("every note already formatted", false),
        ("every note dirty", true),
    ] {
        let (mut state, text) = state_of(3000, dirty);
        let ratio = |bytes: usize| bytes as f64 / text as f64;
        println!("\n{label}: 3000 notes, {} of text", kib(text));

        let (result, computed) = measure(|| compute_format_changes(&state));
        println!(
            "  compute_format_changes   peak {}  kept {}  ({} changes, {} cache updates)   peak/text {:.2}  kept/text {:.2}",
            kib(computed.peak),
            kib(computed.kept),
            result.changes.len(),
            result.cache_updates.len(),
            ratio(computed.peak),
            ratio(computed.kept),
        );
        if dirty {
            assert_eq!(result.changes.len(), 3000);
            // The formatted text is kept once (for the cache), the edits once (to be sent).
            assert!(
                ratio(computed.kept) < 4.0,
                "computing holds {:.2}x the text of the notes",
                ratio(computed.kept)
            );
        } else {
            assert!(result.changes.is_empty());
            // An already formatted note is remembered by its hash, without a copy of its text.
            assert!(
                ratio(computed.peak) < 0.4 && ratio(computed.kept) < 0.4,
                "computing holds {:.2}x (peak) and {:.2}x (kept) the text of the notes",
                ratio(computed.peak),
                ratio(computed.kept)
            );
        }

        let (updates, changes) = (result.cache_updates, result.changes);
        let (edit, built) = measure(|| build_workspace_edit_versioned(changes));
        println!(
            "  build_workspace_edit     peak {}  kept {}   peak/text {:.2}",
            kib(built.peak),
            kib(built.kept),
            ratio(built.peak),
        );
        // The edits are taken over, not copied.
        assert!(
            ratio(built.peak) < 0.5,
            "building the edit needs {:.2}x the text of the notes",
            ratio(built.peak)
        );
        drop(edit);

        let (_, applied) = measure(|| state.apply_format_cache_updates(updates));
        println!(
            "  apply_format_cache       peak {}  kept {}   peak/text {:.2}  kept/text {:.2}",
            kib(applied.peak),
            kib(applied.kept),
            ratio(applied.peak),
            ratio(applied.kept),
        );
        // The cache holds the hashes and, for a note that changes, its formatted text (which the
        // updates carried, so it is the same bytes), and nothing is copied on the way.
        assert!(ratio(applied.kept) < 0.05, "{:.2}", ratio(applied.kept));
    }
}
