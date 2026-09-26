//! Measurement of what a burst of file events costs (5.1): `cargo test -p satz-lsp --lib --release
//! storm_measurement -- --ignored --nocapture`. Not a test of anything; it prints one line per cell
//! (and appends it to `$SATZ_STORM_OUT`): how long the window takes to apply, what the client is
//! sent, and how long a reader of the state waits at worst while it is applied.
//!
//! `SATZ_STORM_ONLY=<text>|<text>` keeps the cells whose label contains one of the texts; `SATZ_STORM_RUNS` is the
//! number of measured runs per cell (default 5; one more is run first and thrown away);
//! `SATZ_STORM_N` lists the vault sizes (default `2000,5000`).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use satz_core::{GitignoreMode, Index};

use crate::backend::tests::{connected_backend_io, read_frame, write_frame};
use crate::storm_tests::{Dir, run_ready_paths, uri_of};

#[derive(Clone, Copy, Debug, PartialEq)]
enum Kind {
    /// Notes change without changing what they are called: the index updates only their own links.
    Edit,
    /// Notes get another title: what links to them elsewhere changes, so the whole index is redone.
    Retitle,
    /// New notes.
    New,
    /// Notes deleted.
    Delete,
    /// One event for a folder with `k` new notes in it.
    FolderIn,
    /// One event for a folder with `k` notes that is gone.
    FolderOut,
    /// One event for a folder with `k` notes the index already holds (saving a note reports it).
    FolderKnown,
}

fn note(i: usize, n: usize, title: &str) -> String {
    format!(
        "# {title}\n\nSome text about {title}, with [[Note {}]] and [[Note {}]] and [[Note {}]] in it. #tag{}\n\n\
         ## Section\n\nMore words to make the note a little longer than a line or two.\n",
        (i * 7 + 1) % n,
        (i * 13 + 5) % n,
        (i * 31 + 9) % n,
        i % 20
    )
}

struct Vault {
    dir: Dir,
    n: usize,
    docs: Vec<satz_core::Document>,
}

const FOLDER_SIZES: [usize; 3] = [10, 100, 500];

fn make_vault(n: usize) -> Vault {
    let dir = Dir::new("measure");
    for i in 0..n {
        dir.write(
            &format!("f{}/note{i}.md", i / 100),
            &note(i, n, &format!("Note {i}")),
        );
    }
    for k in FOLDER_SIZES {
        for i in 0..k {
            dir.write(
                &format!("del{k}/d{i}.md"),
                &note(i, n, &format!("Del{k} {i}")),
            );
            dir.write(
                &format!("known{k}/k{i}.md"),
                &note(i, n, &format!("Known{k} {i}")),
            );
        }
    }
    let docs = satz_core::walk_vault_with(&dir.0, GitignoreMode::default()).unwrap();
    Vault { dir, n, docs }
}

fn median(values: &mut [f64]) -> f64 {
    values.sort_by(|a, b| a.partial_cmp(b).unwrap());
    values[values.len() / 2]
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "a measurement, not a test"]
async fn storm_measurement() {
    let only = std::env::var("SATZ_STORM_ONLY").unwrap_or_default();
    let runs: usize = std::env::var("SATZ_STORM_RUNS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(5);
    let out: Option<PathBuf> = std::env::var("SATZ_STORM_OUT").ok().map(PathBuf::from);
    let sizes: Vec<usize> = std::env::var("SATZ_STORM_N")
        .ok()
        .map(|v| v.split(',').filter_map(|x| x.parse().ok()).collect())
        .unwrap_or_else(|| vec![2000, 5000]);

    // A push client with 20 open notes and a pull client with one; `SATZ_STORM_FULL` adds the
    // other two combinations.
    let clients: Vec<(bool, usize)> = if std::env::var("SATZ_STORM_FULL").is_ok() {
        vec![(false, 1), (false, 20), (true, 1), (true, 20)]
    } else {
        vec![(false, 20), (true, 1)]
    };

    let (backend, from_server, mut to_server) = connected_backend_io().await;
    // A client that answers what it is asked, and counts what it is sent.
    let counts: Arc<Mutex<HashMap<String, usize>>> = Arc::default();
    let (answer_tx, mut answer_rx) = tokio::sync::mpsc::unbounded_channel::<serde_json::Value>();
    {
        let counts = counts.clone();
        let mut from_server = from_server;
        tokio::spawn(async move {
            while let Some(frame) = read_frame(&mut from_server).await {
                if let Some(method) = frame["method"].as_str() {
                    *counts
                        .lock()
                        .unwrap()
                        .entry(method.to_string())
                        .or_default() += 1;
                    if frame.get("id").is_some() {
                        let _ = answer_tx.send(serde_json::json!(
                            {"jsonrpc": "2.0", "id": frame["id"], "result": null}));
                    }
                }
            }
        });
        tokio::spawn(async move {
            while let Some(answer) = answer_rx.recv().await {
                write_frame(&mut to_server, answer).await;
            }
        });
    }
    tokio::time::sleep(Duration::from_millis(300)).await;

    let mut report = Vec::new();
    for n in sizes {
        let vault = make_vault(n);
        for kind in [
            Kind::Edit,
            Kind::Retitle,
            Kind::New,
            Kind::Delete,
            Kind::FolderIn,
            Kind::FolderOut,
            Kind::FolderKnown,
        ] {
            let ks: &[usize] = match kind {
                Kind::Edit | Kind::Retitle | Kind::New | Kind::Delete => &[1, 4, 10, 100, 500],
                _ => &FOLDER_SIZES,
            };
            for &k in ks {
                for (pull, open) in clients.iter().copied() {
                    let label = format!(
                        "n={n} {kind:?} k={k} {} open={open}",
                        if pull { "pull" } else { "push" }
                    );
                    if !only.is_empty() && !only.split('|').any(|part| label.contains(part)) {
                        continue;
                    }
                    let mut times = Vec::new();
                    let mut waits = Vec::new();
                    let mut sent = (0, 0);
                    for run in 0..=runs {
                        let (elapsed, wait, frames) =
                            one_run(&vault, &backend, &counts, kind, k, pull, open).await;
                        if run > 0 {
                            times.push(elapsed.as_secs_f64() * 1e3);
                            waits.push(wait.as_secs_f64() * 1e3);
                            sent = frames;
                        }
                    }
                    let line = format!(
                        "{label}: median {:.2} ms (min {:.2}, max {:.2}); publish {} refresh {}; reader waited at most {:.2} ms (median of runs)",
                        median(&mut times.clone()),
                        times.iter().cloned().fold(f64::MAX, f64::min),
                        times.iter().cloned().fold(0.0, f64::max),
                        sent.0,
                        sent.1,
                        median(&mut waits)
                    );
                    println!("{line}");
                    report.push(line);
                }
            }
        }
    }
    if let Some(out) = out {
        let _ = std::fs::create_dir_all(out.parent().unwrap());
        std::fs::write(out, report.join("\n")).unwrap();
    }
}

/// One window: the state is what the vault held, the disk is changed, the events are applied.
/// Returns how long that took, how long a reader waited at worst meanwhile, and what the client
/// was sent (`publishDiagnostics`, `workspace/diagnostic/refresh`).
async fn one_run(
    vault: &Vault,
    backend: &Arc<crate::backend::Backend>,
    counts: &Arc<Mutex<HashMap<String, usize>>>,
    kind: Kind,
    k: usize,
    pull: bool,
    open: usize,
) -> (Duration, Duration, (usize, usize)) {
    let dir = &vault.dir;
    let n = vault.n;
    {
        let mut state = backend.state.write().await;
        state.open_docs.clear();
        state.index = Index::build(vault.docs.clone());
        state.set_vault_root(Some(dir.0.clone()));
        state.set_indexing_complete(true);
        state.client_supports_pull_diagnostics = pull;
        // The open notes are the first ones of the vault, and are not the ones that change.
        for i in 0..open {
            let name = format!("f0/note{i}.md");
            let text = std::fs::read_to_string(dir.path(&name)).unwrap();
            state.open_document(&uri_of(&name), &text, &dir.path(&name), 1);
        }
    }
    // The notes that change: spread over the vault, none of the open ones.
    let target = |j: usize| 200 + (j * (n - 200) / k.max(1)).min(n - 201);
    let rel_of = |i: usize| format!("f{}/note{i}.md", i / 100);

    let mut events = Vec::new();
    let mut undo: Vec<Box<dyn FnOnce()>> = Vec::new();
    match kind {
        Kind::Edit | Kind::Retitle => {
            for j in 0..k {
                let i = target(j);
                let path = dir.path(&rel_of(i));
                let original = std::fs::read_to_string(&path).unwrap();
                let title = if kind == Kind::Retitle {
                    format!("Renamed {i}")
                } else {
                    format!("Note {i}")
                };
                let mut text = note(i, n, &title);
                text.push_str("\nAn added line.\n");
                std::fs::write(&path, text).unwrap();
                undo.push(Box::new(move || std::fs::write(path, original).unwrap()));
                events.push(dir.path(&rel_of(i)));
            }
        }
        Kind::New => {
            for j in 0..k {
                let path = dir.write(
                    &format!("fresh/new{j}.md"),
                    &note(j, n, &format!("Fresh {j}")),
                );
                events.push(path);
            }
            undo.push(Box::new({
                let fresh = dir.path("fresh");
                move || std::fs::remove_dir_all(fresh).unwrap()
            }));
        }
        Kind::Delete => {
            for j in 0..k {
                let i = target(j);
                let path = dir.path(&rel_of(i));
                let original = std::fs::read_to_string(&path).unwrap();
                std::fs::remove_file(&path).unwrap();
                undo.push(Box::new({
                    let path = path.clone();
                    move || std::fs::write(path, original).unwrap()
                }));
                events.push(path);
            }
        }
        Kind::FolderIn => {
            for j in 0..k {
                dir.write(
                    &format!("in{k}/x{j}.md"),
                    &note(j, n, &format!("In{k} {j}")),
                );
            }
            undo.push(Box::new({
                let folder = dir.path(&format!("in{k}"));
                move || std::fs::remove_dir_all(folder).unwrap()
            }));
            events.push(dir.path(&format!("in{k}")));
        }
        Kind::FolderOut => {
            let folder = dir.path(&format!("del{k}"));
            let originals: Vec<(PathBuf, String)> = (0..k)
                .map(|i| {
                    let path = folder.join(format!("d{i}.md"));
                    (path.clone(), std::fs::read_to_string(path).unwrap())
                })
                .collect();
            std::fs::remove_dir_all(&folder).unwrap();
            undo.push(Box::new({
                let folder = folder.clone();
                move || {
                    std::fs::create_dir_all(&folder).unwrap();
                    for (path, text) in originals {
                        std::fs::write(path, text).unwrap();
                    }
                }
            }));
            events.push(folder);
        }
        Kind::FolderKnown => events.push(dir.path(&format!("known{k}"))),
    }

    // Everything the client was sent before this window is not part of it.
    tokio::time::sleep(Duration::from_millis(150)).await;
    counts.lock().unwrap().clear();
    let stop = Arc::new(AtomicBool::new(false));
    let probe = {
        let (state, stop) = (backend.state.clone(), stop.clone());
        tokio::spawn(async move {
            let mut worst = Duration::ZERO;
            while !stop.load(Ordering::Relaxed) {
                let asked = Instant::now();
                drop(state.read().await);
                worst = worst.max(asked.elapsed());
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            worst
        })
    };
    let started = Instant::now();
    let again = run_ready_paths(&events, &dir.0, &backend.state, &backend.client).await;
    let elapsed = started.elapsed();
    assert!(again.is_empty());
    stop.store(true, Ordering::Relaxed);
    let worst = probe.await.unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;
    let frames = {
        let counts = counts.lock().unwrap();
        (
            counts
                .get("textDocument/publishDiagnostics")
                .copied()
                .unwrap_or(0),
            counts
                .get("workspace/diagnostic/refresh")
                .copied()
                .unwrap_or(0),
        )
    };

    for step in undo {
        step();
    }
    (elapsed, worst, frames)
}
