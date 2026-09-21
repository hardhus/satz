//! The measuring code shared by the benchmarks of satz-core and satz-lsp (each includes this file
//! with `#[path]`): repeated rounds with the best and the median time, families of sizes with the
//! exponent of the growth, and the rules for calling a growth superlinear. See the header of
//! `satz-core/benches/benchmark.rs` for how to read the output.
#![allow(dead_code)]

use std::hint::black_box;
use std::time::{Duration, Instant};

/// An exponent above this is called superlinear.
pub const SUPERLINEAR: f64 = 1.5;
/// A time below this is too short to say anything about how it grows.
pub const TOO_SHORT: Duration = Duration::from_micros(300);

pub struct Settings {
    pub rounds: usize,
    /// Sizes are divided by this (2 with `--quick`).
    pub shrink: usize,
}

impl Settings {
    pub fn size(&self, n: usize) -> usize {
        (n / self.shrink).max(1)
    }
}

pub struct Sample {
    pub min: Duration,
    pub median: Duration,
}

pub fn summarize(mut times: Vec<Duration>) -> Sample {
    times.sort();
    Sample {
        min: times[0],
        median: times[times.len() / 2],
    }
}

/// Runs `run(setup())` once to warm up, then `rounds` times, timing only `run`.
pub fn measure<S, T>(
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
pub fn interleaved(
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
pub fn exponent(small: (usize, Duration), large: (usize, Duration)) -> f64 {
    (large.1.as_secs_f64() / small.1.as_secs_f64()).log2()
        / (large.0 as f64 / small.0 as f64).log2()
}

pub fn shown(d: Duration) -> String {
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

pub fn mb_per_s(bytes: usize, d: Duration) -> f64 {
    bytes as f64 / 1_048_576.0 / d.as_secs_f64().max(1e-12)
}

pub fn row(what: &str, sample: &Sample, note: &str) {
    println!(
        "  {what:<60} min {:>10}  median {:>10}  {note}",
        shown(sample.min),
        shown(sample.median)
    );
}

/// The exponent over the whole range of sizes, if the first and last times are long enough to judge.
pub fn overall_exponent(sizes: &[usize], best: &[Duration]) -> Option<f64> {
    let (first, last) = (*sizes.first()?, *sizes.last()?);
    let (t_first, t_last) = (*best.first()?, *best.last()?);
    (sizes.len() >= 2 && t_first >= TOO_SHORT && t_last >= TOO_SHORT)
        .then(|| exponent((first, t_first), (last, t_last)))
}

/// Times `one(i)` for every size (see `interleaved`) and reports the scaling. Noise only ever
/// adds time, so when the exponent comes out too high the whole family is measured again (twice
/// at most) and the best time of every size over all the attempts is kept: only a cost that stays
/// is flagged.
pub fn scaling_family(
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
pub fn report_scaling_with(
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

// ---- checks of the harness itself ---------------------------------------------------------

pub fn check_the_harness() {
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
