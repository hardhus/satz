//! Measurement of `satz.formatWorkspace` (5.2): `cargo test -p satz-lsp --lib --release
//! format_measurement -- --ignored --nocapture`. Not a test of anything; it prints one line per cell
//! (and appends it to `$SATZ_FORMAT_OUT`): how long the command takes end to end, how long a reader
//! and a writer of the state wait at worst while it runs, and what the in-process pieces cost.
//!
//! The vault is built in memory (nothing is written to disk). `SATZ_FORMAT_ONLY=<text>|<text>` keeps
//! the cells whose label contains one of the texts; `SATZ_FORMAT_RUNS` is the number of measured
//! runs per cell (default 3; one more is run first and thrown away); `SATZ_FORMAT_N` lists the
//! vault sizes (default `2000,10000`).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use satz_core::{Index, parse_document};
use tower_lsp_server::LanguageServer;
use tower_lsp_server::ls_types::ExecuteCommandParams;

use crate::backend::tests::{connected_backend_io, read_frame, write_frame};
use crate::handlers::execute_command::compute_format_changes;
use crate::state::{FormatCache, SatzState};

fn root() -> PathBuf {
    if cfg!(windows) {
        PathBuf::from("C:\\vault")
    } else {
        PathBuf::from("/vault")
    }
}

/// A note of about 700 bytes; `dirty` adds what the formatter takes away.
fn note(i: usize, dirty: bool) -> String {
    let filler = "A paragraph of plain prose with a [[link]] and some more words in it. ";
    let (tail, gap) = if dirty {
        ("   ", "\n\n\n\n")
    } else {
        ("", "\n\n")
    };
    format!(
        "# Note {i}{tail}{gap}{}{tail}{gap}- one{tail}\n- two{tail}\n",
        filler.repeat(9).trim_end()
    )
}

fn median(values: &mut [f64]) -> f64 {
    values.sort_by(|a, b| a.partial_cmp(b).unwrap());
    values[values.len() / 2]
}

/// A state of `n` notes, `dirty_per_100` of every hundred needing formatting, `open` of them open.
fn make_state(n: usize, dirty_per_100: usize, open: usize) -> SatzState {
    let mut state = SatzState::default();
    state.set_vault_root(Some(root()));
    state.client_supports_document_changes = true;
    let docs = (0..n)
        .map(|i| {
            parse_document(
                &note(i, i % 100 < dirty_per_100),
                Path::new(&format!("dir{}/n{i}.md", i % 40)),
            )
        })
        .collect();
    state.index = Index::build(docs);
    state.format_cache = FormatCache::new(n * 2);
    for i in 0..open {
        // Open notes are spread over the vault; their buffers are what the index holds.
        let at = i * (n / open.max(1)).max(1);
        let rel = format!("dir{}/n{at}.md", at % 40);
        let text = state
            .index
            .get_doc(&satz_core::DocId::new(rel.as_str()))
            .map(|d| d.line_index.source().to_string())
            .unwrap();
        state.open_document(
            &format!("file:///editor/n{at}.md"),
            &text,
            &root().join(&rel),
            1,
        );
    }
    state
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "a measurement, not a test"]
async fn format_measurement() {
    let only = std::env::var("SATZ_FORMAT_ONLY").unwrap_or_default();
    let runs: usize = std::env::var("SATZ_FORMAT_RUNS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3);
    let out: Option<PathBuf> = std::env::var("SATZ_FORMAT_OUT").ok().map(PathBuf::from);
    let sizes: Vec<usize> = std::env::var("SATZ_FORMAT_N")
        .ok()
        .map(|v| v.split(',').filter_map(|x| x.parse().ok()).collect())
        .unwrap_or_else(|| vec![2000, 10000]);

    let (backend, from_server, mut to_server) = connected_backend_io().await;
    // A client that applies every edit it is sent and answers everything else with nothing.
    {
        let mut from_server = from_server;
        let (answer_tx, mut answer_rx) =
            tokio::sync::mpsc::unbounded_channel::<serde_json::Value>();
        tokio::spawn(async move {
            while let Some(frame) = read_frame(&mut from_server).await {
                if let (Some(method), Some(id)) = (frame["method"].as_str(), frame.get("id")) {
                    let result = if method == "workspace/applyEdit" {
                        serde_json::json!({"applied": true})
                    } else {
                        serde_json::Value::Null
                    };
                    let _ = answer_tx.send(serde_json::json!(
                        {"jsonrpc": "2.0", "id": id, "result": result}));
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
        for (dirty, open) in [(10, 0), (10, 20), (100, 0), (100, 20), (0, 0), (0, 20)] {
            let label = format!("n={n} dirty={dirty}% open={open}");
            if !only.is_empty() && !only.split('|').any(|part| label.contains(part)) {
                continue;
            }
            // In process: the synchronous pass on a cold and on a warm cache, and what learning costs.
            let mut cold_ms = Vec::new();
            let mut warm_ms = Vec::new();
            let mut learn_ms = Vec::new();
            let mut changes = 0;
            for run in 0..=runs {
                let mut state = make_state(n, dirty, open);
                let started = Instant::now();
                let result = compute_format_changes(&state);
                let cold = started.elapsed();
                changes = result.changes.len();
                let started = Instant::now();
                state.apply_format_cache_updates(result.cache_updates);
                let learn = started.elapsed();
                let started = Instant::now();
                let again = compute_format_changes(&state);
                let warm = started.elapsed();
                drop(again);
                if run > 0 {
                    cold_ms.push(cold.as_secs_f64() * 1e3);
                    learn_ms.push(learn.as_secs_f64() * 1e3);
                    warm_ms.push(warm.as_secs_f64() * 1e3);
                }
            }
            let line = format!(
                "{label}: in process: cold {:.2} ms, learning {:.2} ms, warm {:.2} ms ({} notes change)",
                median(&mut cold_ms),
                median(&mut learn_ms),
                median(&mut warm_ms),
                changes
            );
            println!("{line}");
            report.push(line);

            // End to end: the command, with a reader and a writer of the state watching how long
            // they wait.
            let mut wall = Vec::new();
            let mut read_wait = Vec::new();
            let mut write_wait = Vec::new();
            for run in 0..=runs {
                *backend.state.write().await = make_state(n, dirty, open);
                let stop = Arc::new(AtomicBool::new(false));
                let reader = {
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
                let writer = {
                    let (state, stop) = (backend.state.clone(), stop.clone());
                    tokio::spawn(async move {
                        let mut worst = Duration::ZERO;
                        while !stop.load(Ordering::Relaxed) {
                            let asked = Instant::now();
                            drop(state.write().await);
                            worst = worst.max(asked.elapsed());
                            tokio::time::sleep(Duration::from_millis(5)).await;
                        }
                        worst
                    })
                };
                tokio::time::sleep(Duration::from_millis(20)).await;
                let started = Instant::now();
                let answer = backend
                    .execute_command(ExecuteCommandParams {
                        command: "satz.formatWorkspace".to_string(),
                        arguments: Vec::new(),
                        work_done_progress_params: Default::default(),
                    })
                    .await
                    .unwrap();
                let elapsed = started.elapsed();
                stop.store(true, Ordering::Relaxed);
                let (worst_read, worst_write) = (reader.await.unwrap(), writer.await.unwrap());
                assert_eq!(
                    answer
                        .as_ref()
                        .map(|a| a["formatted"].as_u64().unwrap_or(0) as usize),
                    Some(changes),
                    "{label}"
                );
                if run > 0 {
                    wall.push(elapsed.as_secs_f64() * 1e3);
                    read_wait.push(worst_read.as_secs_f64() * 1e3);
                    write_wait.push(worst_write.as_secs_f64() * 1e3);
                }
            }
            let line = format!(
                "{label}: end to end: median {:.2} ms (min {:.2}, max {:.2}); a reader waited at most {:.2} ms, a writer {:.2} ms (medians of runs)",
                median(&mut wall.clone()),
                wall.iter().cloned().fold(f64::MAX, f64::min),
                wall.iter().cloned().fold(0.0, f64::max),
                median(&mut read_wait),
                median(&mut write_wait),
            );
            println!("{line}");
            report.push(line);
        }
    }
    if let Some(out) = out {
        let _ = std::fs::create_dir_all(out.parent().unwrap());
        std::fs::write(out, report.join("\n")).unwrap();
    }
}
