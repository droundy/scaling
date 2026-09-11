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

mod estimate;
mod timing;
mod workloads;

use estimate::Run;
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
            let out = args
                .get(3)
                .cloned()
                .unwrap_or_else(|| "run.csv".to_string());
            run(rounds, &out);
        }
        Some("compare") if args.len() > 2 => compare(&args[2..]),
        _ => {
            eprintln!("usage:\n  lab run [rounds] [out.csv]\n  lab compare <run.csv>...");
            eprintln!(
                "\nenv:\n  LAB_REPLAY=<run.csv>  serve recorded timings instead of measuring"
            );
            std::process::exit(2);
        }
    }
}

/// Measure, and write a recording.
fn run(rounds: usize, out: &str) {
    let mut ws = workloads::all();
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
    for w in ws.iter_mut() {
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
            let (w, n) = (&mut ws[i], counts[i]);
            let name = w.name;
            // Generate this batch's inputs first, with the clock stopped.
            if !t.replaying() {
                w.prepare(n, seed);
            }
            t.time(r, slot, name, || w.run());
        }
    }
    eprintln!("{rounds} rounds in {:.2}s", start.elapsed().as_secs_f64());
    t.write(out);
    eprintln!("wrote {out}");

    // A quick look, so a single run is useful on its own. The real question
    // needs several runs and `compare`.
    let run = Run::load(out);
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
            estimate::trim_of(v, 0.10),
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
    let runs: Vec<Run> = paths.iter().map(|p| Run::load(p)).collect();
    if runs.len() < 2 {
        eprintln!("compare wants at least two recordings; got {}", runs.len());
        std::process::exit(2);
    }
    let names = runs[0].names.clone();
    // Selecting payloads per run is easy, so mixing recordings that measured
    // different sets is easy too. Say so rather than quietly reporting NaN
    // for whatever the first recording happened to contain.
    for (path, r) in paths.iter().zip(&runs) {
        if r.names != names {
            eprintln!(
                "warning: {path} measured a different set of workloads\n                          ({:?} against {:?})",
                r.names, names
            );
        }
    }
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
        println!(
            "{:>8}{:>8}{:>9.2}%",
            show(estimate::chosen_canary),
            show(estimate::chosen_by_corr),
            100.0 * estimate::mean_of(&slot)
        );
    }
    println!(
        "\nThe last two columns are which canary each selector chose. MIXED means\n\
         the runs disagreed, and an estimator that picked a different denominator\n\
         in different runs is not reporting the same quantity in each, so its\n\
         percentage on that row means nothing. Compare it against naming the right\n\
         canary by hand (ratio_cpu / ratio_mem) to see what the flipping cost.\n\
         `slot` is how much this workload's timing depends on where in the round\n\
         it ran. Near zero means position does not matter; a large value usually\n\
         means whatever ran before it left the cache in a different state."
    );
}

/// Grow the batch until the *timed* part takes about [`SAMPLE`].
///
/// Only `run` is timed, exactly as in the measurement loop. Timing `prepare`
/// too would aim at the size of the work plus its setup, so a payload with
/// an expensive generator - and a generator can easily cost more than the
/// thing it feeds - would end up with a batch far too small.
fn calibrate(w: &mut Workload, seed: &mut u64) -> u64 {
    let target = SAMPLE.as_secs_f64() * 1e9;
    // A ceiling on generation, because `prepare` allocates one input per
    // iteration. Without it a benchmark whose timed part the optimiser
    // deleted would never reach `target`, and the batch would grow until
    // generating it exhausted memory.
    let prepare_ceiling = Duration::from_millis(50);
    let mut n = 64u64;
    loop {
        *seed = step(*seed);
        let p = Instant::now();
        w.prepare(n, *seed);
        let prepared = p.elapsed();

        let t = Instant::now();
        let sink = w.run();
        let ns = t.elapsed().as_nanos() as f64;
        std::hint::black_box(sink);

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
        n = ((n as f64 * factor) as u64).max(n + 1);
    }
}

fn step(x: u64) -> u64 {
    x.wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407)
}
