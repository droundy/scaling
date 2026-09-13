//! A bench for building a benchmarker.
//!
//! ```none
//! lab run 4000 runs/a.csv     # measure, write a recording
//! lab compare runs/*.csv      # score every estimator against the others
//! ```
//!
//! `run` touches the machine and is the slow, noisy half. `compare` is pure
//! arithmetic over recordings, so a new estimator can be tried in a second
//! without measuring anything again - which is the whole point of splitting
//! them. Collect a handful of runs once, then iterate on `estimate.rs`.

use itertools::Itertools;

mod estimate;
mod timing;
mod workloads;

use estimate::Run;
use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};
use workloads::{Kind, Workload};

/// What one sample of one workload should cost. Everything is calibrated to
/// this, so the members of a round are comparable and share a noise regime.
const SAMPLE: Duration = Duration::from_micros(100);

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(|s| s.as_str()) {
        Some("run") => {
            let rounds: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(4000);
            let dir = args.get(3).cloned().unwrap_or_else(|| "out".to_string());
            if let Err(e) = std::fs::create_dir_all(&dir) {
                eprintln!("could not create {dir}: {e}");
                std::process::exit(2);
            }
            // Every subset, so an estimator can be scored against the whole
            // lattice rather than one hand-picked combination. The empty one
            // is skipped: the canaries alone measure nothing.
            // One instance of each canary for the whole sweep: the chase
            // table is built once rather than thirty-one times, and its
            // cursor walks forward across every subset instead of each one
            // re-treading the same region of the table.
            let canaries = [
                Arc::new(Workload::cpu_canary()),
                Arc::new(Workload::mem_canary()),
            ];
            for payloads in Workload::best().into_iter().powerset() {
                if payloads.is_empty() {
                    continue;
                }
                run(&canaries, payloads, rounds, &dir);
            }
        }
        Some("compare") if args.len() > 2 => compare(&args[2..]),
        _ => {
            eprintln!(
                "usage:\n  lab run [rounds] [dir]      measure every subset of the best workloads\n\
                 \x20 lab compare <run.csv>...   score the estimators\n\n\
                 Each subset writes <dir>/<names joined by +>.csv (default dir `out`),\n\
                 so repeating a\
                 sweep into a second directory and comparing\n\
                 `<dir1>/x+y.csv <dir2>/x+y.csv` scores the same subset across runs."
            );
            eprintln!(
                "\nenv:\n  LAB_REPLAY=<run.csv>  serve recorded timings instead of measuring"
            );
            std::process::exit(2);
        }
    }
}

/// Measure, and write a recording.
fn run(canaries: &[Arc<Workload>], mut payloads: Vec<Arc<Workload>>, rounds: usize, dir: &str) {
    // Sorted, so a subset gets the same name however the powerset happened
    // to order it - `compare out/*/a+b.csv` then lines up the same subset
    // across repetitions. Taking the names from the built workloads rather
    // than from a parallel list means the filename cannot drift out of step
    // with what was actually measured.
    payloads.sort_by_key(|w| w.name);
    let out = csv_name(dir, &payloads);

    // The canaries are never optional: every ratio estimator divides by one
    // of them. They go first so the report reads with them at the top, and
    // they are left out of the filename because they are in every run.
    let mut ws: Vec<Arc<Workload>> = canaries.to_vec();
    ws.extend(payloads);

    eprintln!("\n=== {out} ===");
    let mut t = timing::Timing::from_env();

    // Calibrate each workload to SAMPLE.
    //
    // Note the seed is advanced on every probe, exactly as it is in the
    // measurement loop below. Calibrating a moving-window workload from a
    // *fixed* start lets it go cache resident, which oversizes its count by
    // about six and makes every later sample three times too long.
    //
    // `prepare` is outside the timing here for the same reason it is below:
    // calibration should aim at the size of the work, not of the work plus
    // its setup, or a payload with an expensive generator gets a batch far
    // too small.
    let mut seed = 0x9E3779B97F4A7C15u64;
    let mut counts = Vec::with_capacity(ws.len());
    for w in ws.iter() {
        let name = w.name;
        let n = if t.replaying() {
            *t.iters.get(name).unwrap_or(&1)
        } else {
            calibrate(w, &mut seed)
        };
        t.iters.insert(name.to_string(), n);
        counts.push(n);
        eprintln!("  {:>16} {:>12} iters", name, n);
    }

    // Round-robin, in a fresh random order each round.
    //
    // A random permutation rather than alternating the sweep direction: the
    // alternating version removes position bias and substitutes a period-2
    // oscillation, which lands as a large negative lag-1 autocorrelation and
    // corrupts any variance estimate taken later.
    let mut perm = 0x853C49E6748FEA9Bu64;
    let start = Instant::now();
    for r in 0..rounds {
        let mut order: Vec<usize> = (0..ws.len()).collect();
        for i in (1..ws.len()).rev() {
            perm = step(perm);
            order.swap(i, (perm >> 33) as usize % (i + 1));
        }
        for (slot, &i) in order.iter().enumerate() {
            seed = step(seed);
            let w = &ws[i];
            let name = w.name;
            let time_me = w.time_batch(counts[i]);
            t.time(r, slot, name, time_me);
        }
    }
    eprintln!("{rounds} rounds in {:.2}s", start.elapsed().as_secs_f64());
    t.write(&out);
    eprintln!("wrote {out}");

    // A quick look, so a single run is useful on its own. The real question
    // needs several runs and `compare`.
    let run = Run::load(&out);
    println!(
        "\n{:>16} {:>8} {:>12} {:>12}",
        "workload", "kind", "ns/iter", "within-run"
    );
    for w in &ws {
        let v = run.get(w.name);
        let kind = match w.kind {
            Kind::CpuCanary => "cpu*",
            Kind::MemCanary => "mem*",
            Kind::Payload => "",
        };
        println!(
            "{:>16} {kind:>8} {:>12.4} {:>11.2}%",
            w.name,
            estimate::trimmed_mean(v, 0.10),
            100.0 * estimate::rel_spread(v)
        );
    }
    println!(
        "\n* canary: constant true cost, so its spread is the machine's. Its\n\
         ns/iter is per chunk of work - a canary loops internally, so the\n\
         absolute figure is not per operation and is not meant to be read."
    );
}

/// Score every estimator on every workload, across several recordings.
///
/// The number printed is the spread of the estimate *between* runs, as a
/// percentage. That is reproducibility, and it is the thing to minimise.
fn compare(paths: &[String]) {
    let runs: Vec<Run> = if paths.len() > 1 {
        paths.iter().map(|p| Run::load(p)).collect()
    } else if paths.len() == 1 {
        std::fs::read_dir(&paths[0])
            .unwrap()
            .filter_map(|entry| {
                let entry = entry.unwrap();
                if entry.file_type().unwrap().is_file() {
                    Some(Run::load(&entry.path().to_string_lossy().into_owned()))
                } else {
                    None
                }
            })
            .collect()
    } else {
        eprintln!("compare wants at least two recordings; got {}", paths.len());
        std::process::exit(2);
    };
    if runs.len() < 2 {
        eprintln!("compare wants at least two recordings; got {}", runs.len());
        std::process::exit(2);
    }
    let names: BTreeSet<String> = runs
        .iter()
        .flat_map(|r| r.names.clone().into_iter())
        .collect();
    let ests = estimate::all();

    println!(
        "across {} runs: spread of the estimate between runs, lower is better\n",
        runs.len()
    );
    print!("{:>16}", "workload");
    for (n, _) in &ests {
        print!("{n:>12}");
    }
    println!("{:>8}{:>8}{:>10}", "auto", "corr", "slot");

    for w in &names {
        print!("{w:>16}");
        // Evaluate each workload with only those runs that measure it.
        let runs = runs
            .iter()
            .filter(|r| r.names.contains(w))
            .cloned()
            .collect::<Vec<Run>>();
        for (_, f) in &ests {
            let per_run: Vec<f64> = runs.iter().map(|r| f(r, w)).collect();
            print!("{:>11.3}%", 100.0 * estimate::rel_spread(&per_run));
        }
        // Which canary ratio_auto chose, and whether it agreed across runs.
        let show = |f: fn(&Run, &str) -> &'static str| {
            let picks: Vec<&str> = runs.iter().map(|r| f(r, w)).collect();
            if picks.iter().all(|p| *p == picks[0]) {
                picks[0]
            } else {
                "MIXED"
            }
        };
        let slot: Vec<f64> = runs.iter().map(|r| estimate::slot_effect(r, w)).collect();
        let all_times = runs
            .iter()
            .flat_map(|r| r.get(w))
            .copied()
            .collect::<Vec<f64>>();
        print!(
            "{:>8}{:>8}{:>9.2}%",
            show(estimate::chosen_canary),
            show(estimate::chosen_by_corr),
            100.0 * estimate::mean(&slot)
        );
        let sample_mean: f64 = estimate::mean(&all_times);
        let sample_stdev: f64 = estimate::variance(&all_times, sample_mean).sqrt();
        println!(
            "{sample_mean:>12.2} ± {:>6.2}%",
            sample_stdev / sample_mean * 100.0
        );
    }
}

/// Grow the batch until the *timed* part takes about [`SAMPLE`].
///
/// Only `run` is timed, exactly as in the measurement loop. Timing `prepare`
/// too would aim at the size of the work plus its setup, so a payload with
/// an expensive generator - and a generator can easily cost more than the
/// thing it feeds - would end up with a batch far too small.
fn calibrate(w: &Workload, seed: &mut u64) -> usize {
    let target = SAMPLE.as_secs_f64() * 1e9;
    // A ceiling on generation, because `prepare` allocates one input per
    // iteration. Without it a benchmark whose timed part the optimiser
    // deleted would never reach `target`, and the batch would grow until
    // generating it exhausted memory.
    let prepare_ceiling = Duration::from_millis(50);
    let mut n = 1usize;
    loop {
        *seed = step(*seed);
        let p = Instant::now();
        let job = w.time_batch(n);
        let prepared = p.elapsed();

        let ns = job();

        if ns >= target * 0.9 || prepared > prepare_ceiling || n >= 1 << 32 {
            if ns < target * 0.9 {
                eprintln!(
                    "  note: {} stopped growing at {n} iters ({:.0}us timed, {:.0}ms to generate)",
                    w.name,
                    ns / 1000.0,
                    prepared.as_secs_f64() * 1e3
                );
            }
            return n;
        }
        let factor = (target / ns.max(1.0)).clamp(1.5, 50.0);
        n = ((n as f64 * factor) as usize).max(n + 1);
    }
}

/// Where a subset's recording goes: its payload names, in order, joined.
///
/// Derived rather than passed so that the same subset always lands in the
/// same file, which is what makes two sweeps comparable subset by subset.
fn csv_name(dir: &str, payloads: &[Arc<Workload>]) -> String {
    let names: Vec<&str> = payloads.iter().map(|w| w.name).collect();
    format!("{dir}/{}.csv", names.join("+"))
}

fn step(x: u64) -> u64 {
    x.wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407)
}
