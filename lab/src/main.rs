//! A bench for building a benchmarker.
//!
//! ```none
//! lab run 4000 runs/a.csv     # measure, write a recording
//! lab compare runs/*.csv      # score every estimator against the others
//! ```
//!
//! `PROBLEMS.md` lists the four sources of error this lab exists to tell
//! apart - a moving clock, a fixed cost per measurement, sensitivity to what
//! else shares the round, and a wandering calibrated batch size - with what
//! is known about each so far. Read it before trusting a number from here.
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
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;
use std::time::{Duration, Instant};
use workloads::{Kind, Workload};

/// What one sample of one workload should cost. Everything is calibrated to
/// this, so the members of a round are comparable and share a noise regime.
const SAMPLE: Duration = Duration::from_micros(100);

/// The rungs of the ladder, **as multiples of [`SAMPLE`] - that is, as
/// durations, not as iteration counts.**
///
/// Duration is the thing that actually matters. The fixed cost we are trying
/// to separate out is paid per *measurement*, so what determines how visible
/// it is at a given rung is how long that rung's batch runs, not how many
/// iterations fit inside it. Two workloads a thousandfold apart in cost
/// should be laddering over the same range of times, not the same range of
/// counts.
///
/// Counts follow from the target durations, and a workload too slow to fit
/// even one iteration into a rung gets the tightest ladder that still has
/// distinct rungs. `copy_64mb` at 4.5 ms an iteration cannot have a 50 us
/// batch, so asking for `0.5, 1, 2, 4` samples gives it `1, 2, 3, 4`
/// iterations rather than `1, 2, 4, 8` - a shorter lever, but a third of the
/// machine time, and crucially a largest batch of 18 ms rather than 36 ms.
/// Long batches are where interrupts and migrations land, so a geometric
/// blowup on a slow workload buys lever arm and pays for it in exactly the
/// tail events that wreck a subtraction.
///
/// Two rungs are enough to *use* - a straight line has two parameters, and
/// under multiplicative noise the lowest-variance design puts both
/// measurements at the extremes rather than spreading them evenly, so a
/// least-squares fit over an even ladder is a worse use of the same machine
/// time than the outer pair alone. The rungs in between are there to check
/// that the line is a line: they cost little and they are the only warning
/// that a workload's cost is not affine in its batch size, which several of
/// ours are not.
const LADDER: [f64; 4] = [0.5, 1.0, 2.0, 4.0];

/// The workloads whose powerset `ladder` sweeps by default.
///
/// Chosen to span *cache footprint*, because that is the axis a cold start
/// should care about, and to stay cheap enough that 31 subsets are
/// affordable - no `copy_64mb`, whose 4.5 ms iterations would dominate the
/// sweep and appear in half of it.
///
/// | workload | footprint it leaves behind |
/// | --- | --- |
/// | `nothing` | none at all: the harness floor |
/// | `cpu_canary` | none, but it does occupy time |
/// | `instant_now` | a vDSO page |
/// | `btree_miss` | a 1M-entry map, walked one path at a time |
/// | `mem_canary` | tens of MiB, streamed, evicting everything |
///
/// `nothing` and `cpu_canary` are the pair that separates the two
/// explanations: both occupy a slot and take time, and only one of them
/// touches memory. If a neighbour's *time* is what matters they behave
/// alike; if its *footprint* is what matters, neither should do much and
/// `mem_canary` should do everything.
const LADDER_SET: &str = "nothing,cpu_canary,instant_now,btree_miss,mem_canary";

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
                run(&canaries, payloads, rounds, &dir, &[1.0], None, None, 0);
            }
        }
        Some("ladder") => {
            let rounds: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(2000);
            let dir = args.get(3).cloned().unwrap_or_else(|| "ladder".to_string());
            if let Err(e) = std::fs::create_dir_all(&dir) {
                eprintln!("could not create {dir}: {e}");
                std::process::exit(2);
            }
            // Every subset, for the reason `run` sweeps them: a fixed cost
            // that is a property of a workload has to come out the same
            // whatever else shares the round, and the only way to see that
            // is to vary what shares the round and compare. One composition
            // can tell you the size of a fixed cost; it cannot tell you
            // whether it is a property of the workload or of the company it
            // was keeping.
            //
            // **The canaries are not forced in**, which is the one place in
            // this program where that is right. Everywhere else they are the
            // instrument; here the composition is the independent variable,
            // and `mem_canary` is the most cache-destroying workload we
            // have - in every round, it would hold the thing under study
            // pinned at its maximum.
            //
            // Built once and shared, so a subset measures the same workload
            // rather than a fresh copy, exactly as in `run`.
            let ws: Vec<Arc<Workload>> = args
                .get(4)
                .map(|s| s.as_str())
                .unwrap_or(LADDER_SET)
                .split(',')
                .map(str::trim)
                .filter(|n| !n.is_empty())
                .map(|n| Arc::new(workloads::named(n)))
                .collect();
            let passes: usize = std::env::var("LAB_PASSES")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(3);
            sweep(ws, rounds, &dir, &ladder_from_env(), passes);
        }
        Some("intercept") if args.len() > 2 => intercept(&args[2..]),
        Some("shift") if args.len() > 2 => shift(&args[2..]),
        Some("compare") if args.len() > 2 => compare(&args[2..]),
        _ => {
            eprintln!(
                "usage:\n  lab run [rounds] [dir]        measure every subset of the best workloads\n\
                 \x20 lab ladder [rounds] [dir] [names]  every subset, at several batch sizes each\n\
                 \x20 lab compare <run.csv>...      score the estimators\n\
                 \x20 lab intercept <ladder.csv>... size up the fixed per-measurement cost\\n\\x20 lab shift <ladder.csv>...     does a number move with the round it sits in\n\n\
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

/// The ladder from `LAB_LADDER`, as multiples of [`SAMPLE`].
fn ladder_from_env() -> Vec<f64> {
    match std::env::var("LAB_LADDER") {
        Ok(s) => {
            let mut v: Vec<f64> = s
                .split(',')
                .filter_map(|x| x.trim().parse().ok())
                .filter(|m: &f64| *m > 0.0)
                .collect();
            v.sort_by(|a, b| a.partial_cmp(b).unwrap());
            if v.len() < 2 {
                eprintln!("LAB_LADDER wants at least two durations; got {s:?}");
                std::process::exit(2);
            }
            v
        }
        Err(_) => LADDER.to_vec(),
    }
}

/// Turn target durations into iteration counts for one workload.
///
/// `cal` is what one [`SAMPLE`] costs in iterations, so `target * cal` is the
/// count that would take `target` samples of time. Two things then have to be
/// imposed on the result.
///
/// **At least one iteration.** You cannot run a third of a memcpy.
///
/// **Strictly increasing.** Once rounding has flattened several targets onto
/// the same count - which is what happens to any workload whose single
/// iteration already costs more than the largest rung - the ladder has to be
/// pushed apart again or the rungs are not different measurements at all.
/// Pushing apart by the smallest possible step is deliberate: it keeps the
/// rungs distinct at the least possible cost in time, which for a 4.5 ms
/// iteration is the difference between a ladder ending at 4 iterations and
/// one ending at 8.
fn counts_for(cal: usize, ladder: &[f64]) -> Vec<usize> {
    let mut out: Vec<usize> = Vec::with_capacity(ladder.len());
    for &t in ladder {
        let want = (t * cal as f64).round().max(1.0) as usize;
        let floor = out.last().map(|&p| p + 1).unwrap_or(1);
        out.push(want.max(floor));
    }
    out
}

/// Sweep every subset, several times over.
///
/// Two decisions here, both of which have a wrong version that looks
/// reasonable.
///
/// **Calibrate once, up front, and share the counts.** Letting each subset
/// calibrate for itself means `btree_miss` is measured at 8762 iterations in
/// one subset and 9100 in another - and since the whole question is whether
/// per-iteration cost depends on batch size, that would mix the effect under
/// study into the comparison meant to measure it. Problem 4 in `PROBLEMS.md`,
/// held fixed here rather than left to vary alongside problem 3.
///
/// **Contiguous blocks, in a fresh random order each pass.** The tempting
/// alternative is to interleave subsets in short slices so that slow drift
/// cannot load onto any one of them. That is worse, because the start of a
/// round is the end of the one before it: a composition establishes a regime
/// - clock, cache occupancy - that takes time to settle, and interleaving
/// keeps every subset in a permanent transient and drags them all toward the
/// average, masking exactly the composition effect it was meant to protect.
///
/// So drift is handled by **replication** instead. Each pass visits every
/// subset once, in a new random order, so subset identity is decorrelated
/// from wall-clock time without any subset losing its contiguous stretch.
/// The passes are written as separate files, which is what makes them worth
/// having: the spread of one composition *across passes* is the null against
/// which its spread *across compositions* has to be judged. Without it, a
/// composition effect and a night of thermal drift look identical.
fn sweep(ws: Vec<Arc<Workload>>, rounds: usize, dir: &str, ladder: &[f64], passes: usize) {
    let warmup: usize = std::env::var("LAB_WARMUP")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    let mut seed = 0x9E3779B97F4A7C15u64;
    let mut counts: HashMap<&'static str, (usize, f64)> = HashMap::new();
    eprintln!(
        "calibrating {} workloads once, to share across every subset",
        ws.len()
    );
    for w in ws.iter() {
        counts.insert(w.name, calibrate(w, &mut seed));
    }
    eprintln!(
        "ladder {ladder:?} x {SAMPLE:?}, {passes} passes of {rounds} rounds, \
         warmup {warmup}\nmachine state -> {dir}/machine.csv\n\nladder plan:"
    );
    // Printed once here rather than per subset, and in *durations*, because
    // the ladder is specified in time and the counts are only how each
    // workload gets there. A workload whose rungs are 1,2,3,4 rather than
    // geometric is one whose single iteration already outruns the target,
    // and that is worth seeing before committing a night to it.
    for w in ws.iter() {
        let (cal, per) = counts[w.name];
        let plan: Vec<String> = counts_for(cal, ladder)
            .iter()
            .map(|&c| {
                // Two decimals below ten microseconds. The bottom rungs of a
                // small-sample ladder are hundreds of nanoseconds, and a plan
                // that reports every one of them as "0us" hides exactly what
                // it exists to show.
                let us = c as f64 * per / 1e3;
                if us < 10.0 {
                    format!("{c}={us:.2}us")
                } else {
                    format!("{c}={us:.0}us")
                }
            })
            .collect();
        eprintln!("  {:>16}  {}", w.name, plan.join("  "));
    }

    let subsets: Vec<Vec<Arc<Workload>>> = ws
        .into_iter()
        .powerset()
        .filter(|s| !s.is_empty())
        .collect();
    let mut perm = 0xD1B54A32D192ED03u64;
    let started = Instant::now();
    for pass in 0..passes {
        let mut order: Vec<usize> = (0..subsets.len()).collect();
        for i in (1..order.len()).rev() {
            perm = step(perm);
            order.swap(i, (perm >> 33) as usize % (i + 1));
        }
        for (k, &i) in order.iter().enumerate() {
            eprintln!(
                "\n[pass {}/{passes}, subset {}/{}] {:.2} h elapsed",
                pass + 1,
                k + 1,
                order.len(),
                started.elapsed().as_secs_f64() / 3600.0
            );
            run(
                &[],
                subsets[i].clone(),
                rounds,
                dir,
                ladder,
                Some(&counts),
                Some(pass),
                warmup,
            );
        }
    }
    eprintln!(
        "\nsweep finished in {:.2} h",
        started.elapsed().as_secs_f64() / 3600.0
    );
}

/// Measure, and write a recording.
#[allow(clippy::too_many_arguments)]
fn run(
    canaries: &[Arc<Workload>],
    mut payloads: Vec<Arc<Workload>>,
    rounds: usize,
    dir: &str,
    ladder: &[f64],
    counts: Option<&HashMap<&'static str, (usize, f64)>>,
    pass: Option<usize>,
    warmup: usize,
) {
    // Sorted, so a subset gets the same name however the powerset happened
    // to order it - `compare out/*/a+b.csv` then lines up the same subset
    // across repetitions. Taking the names from the built workloads rather
    // than from a parallel list means the filename cannot drift out of step
    // with what was actually measured.
    payloads.sort_by_key(|w| w.name);
    let out = csv_name(dir, &payloads, pass);

    // The canaries are never optional: every ratio estimator divides by one
    // of them. They go first so the report reads with them at the top, and
    // they are left out of the filename because they are in every run.
    let mut ws: Vec<Arc<Workload>> = canaries.to_vec();
    ws.extend(payloads);

    eprintln!("\n=== {out} ===");
    let mut t = timing::Timing::from_env();

    // Calibrate each workload to SAMPLE, unless the sweep already did it
    // once for everybody - which is the point of `counts`, so that a
    // workload is measured at the same batch size in every subset and the
    // composition is the only thing varying.
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
    //
    // A workload appears **once** per round, at one randomly chosen rung of
    // the ladder. Measuring the same workload at `n` and at `4n` in the same
    // round would be the more precise design and the wrong one: whichever
    // rung ran first would pay the cold start and leave the others running
    // warm, so a round would contain one warm-up shared between three
    // measurements instead of three. The fit assumes every measurement pays
    // it, so it would come out about threefold too small - and the warm-up
    // is half of what the ladder is here to find.
    //
    // The cost of one rung per round is that `n` and `4n` are no longer
    // paired, so drift does not cancel between them. Random assignment is
    // what makes that safe: it turns drift into a random effect that
    // averages out over rounds rather than a systematic one that loads onto
    // whichever rung ran later.
    let mut seed = 0x9E3779B97F4A7C15u64;
    let mut rungs: Vec<Vec<(usize, String)>> = Vec::with_capacity(ws.len());
    for w in ws.iter() {
        let (cal, per) = match (t.replaying(), counts.and_then(|c| c.get(w.name))) {
            (true, _) => (*t.iters.get(w.name).unwrap_or(&1), f64::NAN),
            (false, Some(&(n, p))) => (n, p),
            (false, None) => calibrate(w, &mut seed),
        };
        // Replaying takes the rung counts straight from the recording rather
        // than deriving them again. Deriving them would re-apply the ladder
        // to a number that already has it: `t.iters[name]` is *rung zero's*
        // count, which equalled the calibration count only while ladders
        // were integer multiples starting at one. With a rung at half a
        // sample it silently halved every count, so replayed timings came
        // back attached to the wrong batch sizes and every per-iteration
        // cost doubled.
        let derived = counts_for(cal, ladder);
        let mut this: Vec<(usize, String)> = Vec::with_capacity(ladder.len());
        for k in 0..ladder.len() {
            let name = rung_name(w.name, k);
            let n = if t.replaying() {
                *t.iters
                    .get(&name)
                    .unwrap_or_else(|| panic!("replay recording has no rung {name}"))
            } else {
                derived[k]
            };
            t.iters.insert(name.clone(), n);
            // Only when this call did its own calibrating. A sweep has
            // already printed the plan once, and repeating it for every
            // subset of every pass buries the log.
            if counts.is_none() && !t.replaying() {
                eprintln!("  {name:>16} {n:>12} iters  ~{:>8.0} us", n as f64 * per / 1e3);
            }
            this.push((n, name));
        }
        rungs.push(this);
    }

    // Round-robin, in a fresh random order each round.
    //
    // A random permutation rather than alternating the sweep direction: the
    // alternating version removes position bias and substitutes a period-2
    // oscillation, which lands as a large negative lag-1 autocorrelation and
    // corrupts any variance estimate taken later.
    let mut perm = 0x853C49E6748FEA9Bu64;
    let start = Instant::now();

    // One shared log for the whole sweep, appended to. Keyed by composition
    // and pass so a sample can be lined up with the rounds it sat between.
    const MACHINE_EVERY: usize = 200;
    let mach_path = format!("{dir}/machine.csv");
    let fresh = std::fs::metadata(&mach_path).map(|m| m.len() == 0).unwrap_or(true);
    let mut mach = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&mach_path)
        .ok();
    if let (true, Some(f)) = (fresh, mach.as_mut()) {
        use std::io::Write as _;
        let _ = writeln!(f, "t_ns,composition,pass,round,khz,temp_mC,procs_running");
    }
    let composition = composition_of(&out);

    for r in 0..rounds {
        if r % MACHINE_EVERY == 0 {
            if let Some(f) = mach.as_mut() {
                use std::io::Write as _;
                let t_ns = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0);
                let _ = writeln!(
                    f,
                    "{t_ns},{composition},{},{r},{}",
                    pass.map(|p| p as i64).unwrap_or(-1),
                    machine_state()
                );
            }
        }
        // Which rung each workload runs at this round, drawn independently
        // so the rungs are not correlated with each other either.
        let pick: Vec<usize> = rungs
            .iter()
            .map(|r| {
                perm = step(perm);
                (perm >> 33) as usize % r.len()
            })
            .collect();
        let mut order: Vec<usize> = (0..ws.len()).collect();
        for i in (1..ws.len()).rev() {
            perm = step(perm);
            order.swap(i, (perm >> 33) as usize % (i + 1));
        }
        for (slot, &i) in order.iter().enumerate() {
            seed = step(seed);
            let (count, name) = &rungs[i][pick[i]];
            // Untimed iterations first, to separate the two things that
            // could make a measurement cost more than its iterations do.
            // Harness overhead - the two clock reads, the boxed call - is
            // paid per measurement whatever ran before it, so a prefix
            // cannot touch it. A cold start is the workload's own working
            // set being pulled back after neighbours evicted it, so a prefix
            // pays it *outside* the timer and the timed batch comes out
            // warm. Sweep the prefix and whatever decays is the cold start.
            if warmup > 0 {
                ws[i].time_batch(warmup)();
            }
            let time_me = ws[i].time_batch(*count);
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
    for (i, name) in rungs
        .iter()
        .enumerate()
        .flat_map(|(i, r)| r.iter().map(move |(_, name)| (i, name)))
    {
        let v = run.get(name);
        let kind = match ws[i].kind {
            Kind::CpuCanary => "cpu*",
            Kind::MemCanary => "mem*",
            Kind::Payload => "",
        };
        println!(
            "{name:>16} {kind:>8} {:>12.4} {:>11.2}%",
            estimate::trimmed_mean(v, 0.10),
            100.0 * estimate::rel_spread(v)
        );
    }
}

/// What one rung of the ladder is called in the recording.
///
/// A suffix on the workload name rather than a new column, so a ladder
/// recording is an ordinary recording: `compare` and every estimator read it
/// without knowing the ladder exists, and each rung is scored on its own.
/// Rungs are named by their **index**, not by their batch size, because with
/// a time-based ladder the batch size is a different number for every
/// workload. The count lives in the `# iters` header, which is where every
/// reader already gets it.
fn rung_name(name: &str, k: usize) -> String {
    if k == 0 {
        name.to_string()
    } else {
        format!("{name}@{k}")
    }
}

/// One rung of a ladder, summarised over the rounds that happened to draw it.
///
/// A mean and its standard error, because the whole question is whether a
/// difference between two rungs is bigger than the noise on it. The rungs
/// are not paired - each round runs a workload at one rung only - so there
/// is no per-round difference to take, and the uncertainty has to be carried
/// explicitly instead.
struct Rung {
    /// Batch size, in iterations.
    n: f64,
    /// Trimmed mean batch time, ns.
    mean: f64,
    /// Standard error of that mean, ns.
    se: f64,
    /// The same thing divided by the cpu canary in the same round, which
    /// removes whatever the clock was doing between one round and the next.
    ratio: f64,
}

/// Trimmed mean and the standard error of that mean.
fn mean_se(v: &[f64]) -> (f64, f64) {
    let mut s: Vec<f64> = v.iter().copied().filter(|x| x.is_finite()).collect();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let k = (s.len() as f64 * 0.10) as usize;
    let s = &s[k..s.len() - k];
    let m = estimate::mean(s);
    (m, estimate::variance(s, m).sqrt() / (s.len() as f64).sqrt())
}

/// One rung, over the rounds in `range`.
///
/// The range is what lets the same code answer two different questions: the
/// whole run, for the size of the fixed cost, and a block of it, for how
/// noisy an estimator is at a given number of rounds.
fn rung(
    r: &Run,
    base: &str,
    m: usize,
    canary: &HashMap<usize, f64>,
    range: &std::ops::Range<usize>,
) -> Option<Rung> {
    let name = rung_name(base, m);
    let n = *r.iters.get(&name)? as f64;
    let b: HashMap<usize, f64> = r
        .batches(&name)
        .into_iter()
        .filter(|(k, _)| range.contains(k))
        .collect();
    if b.len() < 32 {
        return None;
    }
    let v: Vec<f64> = b.values().copied().collect();
    let (mean, se) = mean_se(&v);
    let rat: Vec<f64> = b
        .iter()
        .filter_map(|(k, t)| canary.get(k).map(|c| t / c))
        .collect();
    Some(Rung {
        n,
        mean,
        se,
        ratio: mean_se(&rat).0,
    })
}

/// Which rungs this recording actually holds for `base`, by batch multiple.
///
/// Read back from the names rather than assumed from [`LADDER`], so an
/// analysis works on a recording made with any ladder - including one made
/// before the ladder was changed.
fn rungs_present(r: &Run, base: &str) -> Vec<usize> {
    let prefix = format!("{base}@");
    let mut ms: Vec<usize> = r
        .names
        .iter()
        .filter_map(|n| n.strip_prefix(&prefix)?.parse().ok())
        // Index 0, not 1: `rung_name` gives rung 0 the bare workload name.
        // This said 1 while naming was still by batch multiple, and the
        // effect after the switch was to drop the smallest rung from every
        // analysis while leaving the tables looking entirely reasonable -
        // losing exactly the rung where a fixed cost is most visible.
        .chain(r.names.iter().any(|n| n == base).then_some(0))
        .collect();
    ms.sort_unstable();
    ms.dedup();
    ms
}

/// Slope from the two extreme rungs: the two-point subtraction, longest lever.
fn wide_of(v: &[Rung], y: &dyn Fn(&Rung) -> f64) -> f64 {
    let l = v.len() - 1;
    (y(&v[l]) - y(&v[0])) / (v[l].n - v[0].n)
}

/// Slope from an unweighted least-squares line through every rung.
///
/// The textbook alternative to [`wide_of`], and on this data a dead heat
/// with it. Under multiplicative noise - where a batch's error scales with
/// its duration - the extreme pair is very nearly the optimal design, and an
/// unweighted fit slightly over-trusts the noisiest rung.
fn lsq_of(v: &[Rung], y: &dyn Fn(&Rung) -> f64) -> f64 {
    let k = v.len() as f64;
    let xm = v.iter().map(|r| r.n).sum::<f64>() / k;
    let ym = v.iter().map(|r| y(r)).sum::<f64>() / k;
    let num: f64 = v.iter().map(|r| (r.n - xm) * (y(r) - ym)).sum();
    let den: f64 = v.iter().map(|r| (r.n - xm).powi(2)).sum();
    num / den
}

/// The intercept the widest pair implies: the part of a batch's cost that
/// its iterations do not account for.
fn fixed_of(v: &[Rung], y: &dyn Fn(&Rung) -> f64) -> f64 {
    y(&v[0]) - wide_of(v, y) * v[0].n
}

/// Is the line a line? The slope over the bottom half of the ladder against
/// the slope over the top half, which agree if and only if the fixed cost is
/// genuinely fixed.
///
/// A constant intercept makes these equal. Anything else - a warm-up that
/// keeps on warming, a queue that fills as the batch runs - shows up here,
/// and means no two-point subtraction can be right, however tidy its answer.
fn lin_of(v: &[Rung], y: &dyn Fn(&Rung) -> f64) -> f64 {
    let mid = v.len() / 2;
    wide_of(&v[..=mid], y) - wide_of(&v[mid..], y)
}

fn mean_v(r: &Rung) -> f64 {
    r.mean
}
fn ratio_v(r: &Rung) -> f64 {
    r.ratio
}

/// The cpu canary's **per-iteration** time, by round.
///
/// Per-iteration and not per-batch, which is the trap: the canary is
/// recalibrated every run and lands on a different batch size each time, so
/// dividing by its batch time would make the ratio carry that calibration
/// difference and be worse than not normalising at all. Asking this the
/// wrong way round scored every ratio at ~20% between runs, against ~1% for
/// the raw nanoseconds.
fn canary_per_iter(r: &Run) -> HashMap<usize, f64> {
    // Every rung, not just the first. The canary draws a random rung each
    // round like everything else, so taking only rung 1 leaves most rounds
    // with no canary to divide by - which showed up as the ratio columns
    // going NaN for any ladder longer than a couple of rungs.
    //
    // Each rung is converted to per-iteration by its own count before
    // pooling. That leaves the canary's own fixed cost in the reference,
    // varying a little by rung; second order against the clock signal the
    // ratio is here to cancel, but it is there.
    let mut out = HashMap::new();
    for m in rungs_present(r, "cpu_canary") {
        let name = rung_name("cpu_canary", m);
        let n = *r.iters.get(&name).unwrap_or(&1) as f64;
        for (round, t) in r.batches(&name) {
            out.insert(round, t / n);
        }
    }
    out
}

/// How big the fixed per-measurement cost is, and what subtracting it costs.
///
/// Four numbers per workload, all per-iteration:
///
/// - `naive`, the batch time divided by the batch size, which is what the
///   lab has reported until now and includes the fixed cost spread thin;
/// - `wide`, `(T4-T1)/3n`: the two extreme rungs, which is the two-point
///   subtraction with the longest lever the ladder offers;
/// - `fixed`, the intercept itself, in nanoseconds per *measurement*;
/// - `lin`, the disagreement between the near pair `(T2-T1)/n` and the far
///   pair `(T4-T2)/2n`, relative to `wide`. This is the assumption test. A
///   straight line makes these two the same estimate of the same slope, so
///   anything much past the noise means the fixed cost is not fixed and no
///   two-point subtraction can be right.
fn intercept(paths: &[String]) {
    let runs: Vec<Run> = paths.iter().map(|p| Run::load(p)).collect();
    let mut bases: Vec<String> = runs
        .iter()
        .flat_map(|r| r.names.iter().cloned())
        .filter(|n| !n.contains('@'))
        .collect::<BTreeSet<String>>()
        .into_iter()
        .collect();
    bases.sort();

    // Per run, so that three recordings give three independent answers
    // rather than one answer from a pile of pooled samples. With `blocks`
    // above 1 each run is cut into that many consecutive stretches, each
    // scored as its own pseudo-run - which is the only way to get enough
    // replicates to say whether one estimator is noisier than another, since
    // nobody is going to sit through thirty real runs.
    let split = |blocks: usize| -> Vec<Vec<(String, Vec<Rung>)>> {
        runs.iter()
            .flat_map(|r| {
                let canary = canary_per_iter(r);
                let last = r.samples.iter().map(|s| s.round).max().unwrap_or(0) + 1;
                let per = last.div_ceil(blocks);
                (0..blocks)
                    .map(|b| {
                        let range = b * per..((b + 1) * per).min(last);
                        bases
                            .iter()
                            .filter_map(|base| {
                                let want = rungs_present(r, base);
                                let v: Vec<Rung> = want
                                    .iter()
                                    .filter_map(|&m| rung(r, base, m, &canary, &range))
                                    .collect();
                                (v.len() == want.len() && v.len() >= 2)
                                    .then(|| (base.clone(), v))
                            })
                            .collect()
                    })
                    .collect::<Vec<_>>()
            })
            .collect()
    };
    let ladder = split(1);

    // The shape of the cost, rung by rung. This is the table to read first:
    // per-iteration cost against batch size tells you *what kind* of fixed
    // cost you have, which no single summary number can.
    //
    //   flat                  -> no fixed cost; the slope is the whole story
    //   drops, each drop half -> a constant intercept, `b + a/n`; subtracting
    //                            two rungs removes it exactly
    //   drops, drops equal    -> logarithmic in batch length: something that
    //                            keeps on warming. No subtraction fixes this
    //   rises                 -> superlinear; the batch degrades as it runs
    println!("\nper-iteration cost at each rung (ns)\n");
    print!("{:>14}", "workload");
    let widest = ladder
        .iter()
        .flat_map(|p| p.iter())
        .map(|(_, v)| v.len())
        .max()
        .unwrap_or(0);
    for k in 0..widest {
        print!("{:>13}", format!("rung {k}"));
    }
    println!();
    for base in &bases {
        let Some((_, v)) = ladder.iter().flat_map(|p| p.iter()).find(|(b, _)| b == base) else {
            continue;
        };
        print!("{base:>14}");
        for r in v {
            print!("{:>13.4}", r.mean / r.n);
        }
        println!("      n={:.0}..{:.0}", v[0].n, v[v.len() - 1].n);
    }

    println!(
        "\n{:>14}{:>15}{:>15}{:>9}{:>13}{:>16}",
        "workload", "naive/it", "wide/it", "bias", "fixed/meas", "linearity"
    );
    for base in bases.iter() {
        let mut got: Vec<[f64; 6]> = Vec::new();
        for per_run in &ladder {
            let Some((_, v)) = per_run.iter().find(|(b, _)| b == base) else {
                continue;
            };
            let l = v.len() - 1;
            let n = v[0].n;
            let wide = wide_of(v, &mean_v);
            let fixed = fixed_of(v, &mean_v);
            // Propagated from the two rungs the estimate actually uses:
            // fixed = y0 - (yl - y0) * n0/(nl - n0).
            let g = n / (v[l].n - n);
            let fixed_se =
                ((1.0 + g) * (1.0 + g) * v[0].se * v[0].se + g * g * v[l].se * v[l].se).sqrt();
            let lin = lin_of(v, &mean_v);
            // Every rung contributes to `lin` through one half or the other;
            // this is the conservative sum rather than the exact propagation.
            let lin_se = v.iter().map(|r| r.se * r.se).sum::<f64>().sqrt()
                / (v[l].n - v[0].n)
                * 2.0;
            got.push([v[0].mean / n, wide, fixed, fixed_se, lin, lin_se]);
        }
        if got.is_empty() {
            continue;
        }
        let col = |k: usize| estimate::mean(&got.iter().map(|g| g[k]).collect::<Vec<f64>>());
        // Errors average down across runs; spreads do not add.
        let se = |k: usize| {
            (got.iter().map(|g| g[k] * g[k]).sum::<f64>() / got.len() as f64).sqrt()
                / (got.len() as f64).sqrt()
        };
        let (naive, wide) = (col(0), col(1));
        println!(
            "{base:>14}{naive:>15.4}{wide:>15.4}{:>8.2}%{:>8.1} ±{:>3.0}{:>9.2}% ±{:>4.2}%",
            100.0 * (naive - wide) / wide,
            col(2),
            se(3),
            100.0 * col(4) / wide,
            100.0 * se(5) / wide,
        );
    }

    // Reproducibility: the only test that says which estimator to prefer.
    // Everything above is each run talking about itself.
    if runs.len() < 2 {
        println!("\n(one recording: pass several to see between-run spread)");
        return;
    }
    // Three ways to get a slope out of the same three rungs.
    //
    // `naive` ignores the fixed cost, `wide` subtracts the two extreme
    // rungs, and `lsq` is the unweighted least-squares line through all
    // three - the alternative worth beating, since fitting a series and
    // reading the slope off it is the obvious textbook move.
    //
    // Each also in units of a cpu-canary iteration rather than of
    // nanoseconds, so a stretch that ran at a different clock speed is not
    // penalised for it.
    let est: [(&str, fn(&[Rung]) -> f64); 6] = [
        ("naive", |v| v[0].mean / v[0].n),
        ("wide", |v| wide_of(v, &mean_v)),
        ("lsq", |v| lsq_of(v, &mean_v)),
        ("naive/cpu", |v| v[0].ratio / v[0].n),
        ("wide/cpu", |v| wide_of(v, &ratio_v)),
        ("lsq/cpu", |v| lsq_of(v, &ratio_v)),
    ];

    let table = |what: &str, ladder: &[Vec<(String, Vec<Rung>)>]| {
        println!("\nspread of the estimate across {what}, lower is better\n");
        print!("{:>14}", "workload");
        for (n, _) in &est {
            print!("{n:>11}");
        }
        println!();
        for base in &bases {
            let pick = |f: fn(&[Rung]) -> f64| -> Vec<f64> {
                ladder
                    .iter()
                    .filter_map(|p| p.iter().find(|(b, _)| b == base).map(|(_, v)| f(v)))
                    .collect()
            };
            if pick(est[0].1).len() < 2 {
                continue;
            }
            print!("{base:>14}");
            for (_, f) in &est {
                print!("{:>10.3}%", 100.0 * estimate::rel_spread(&pick(*f)));
            }
            println!();
        }
    };

    table(&format!("{} runs", runs.len()), &ladder);
    // The same comparison with enough replicates to mean something. Blocks
    // within a run cannot see differences in how a run was calibrated, so
    // they understate the between-run spread - but they are the right test
    // for which *estimator* is noisier, which is the question here.
    let blocks = 10;
    table(
        &format!(
            "{} blocks of ~{} rounds",
            blocks * runs.len(),
            20000 / blocks
        ),
        &split(blocks),
    );
}

/// Does a workload's number move because of what shares its round?
///
/// The acceptance test for a benchmark, and a different question from the
/// one [`intercept`] asks. `intercept` pools recordings as replicates of one
/// thing; this keeps them apart, because here the recordings differ *on
/// purpose* and the difference between them is the measurement.
///
/// The spread at the bottom of each block is the number to read: it is how
/// much this workload's answer changes when the only thing that changed was
/// the company it was keeping. Any of that is pure error - nothing about
/// `btree_miss` is different because `mem_canary` was also being measured.
fn shift(paths: &[String]) {
    const EST: [(&str, fn(&[Rung]) -> f64); 5] = [
        ("naive/it", |v| v[0].mean / v[0].n),
        ("wide/it", |v| wide_of(v, &mean_v)),
        ("lsq/it", |v| lsq_of(v, &mean_v)),
        // In units of a cpu-canary iteration rather than of nanoseconds. The
        // canary is a dependent ALU chain, so its cost is essentially one
        // over the clock: if a neighbour changes a number only by changing
        // what the clock was doing, this cancels it and the raw columns do
        // not. NaN where the round had no canary.
        ("naive/cpu", |v| v[0].ratio / v[0].n),
        ("wide/cpu", |v| wide_of(v, &ratio_v)),
    ];

    // workload -> composition -> one entry per pass
    let mut by: BTreeMap<String, BTreeMap<String, Vec<([f64; 5], f64)>>> = BTreeMap::new();
    for p in paths {
        let r = Run::load(p);
        let composition = composition_of(p);
        let canary = canary_per_iter(&r);
        let full = 0..usize::MAX;
        let bases: Vec<String> = r.names.iter().filter(|n| !n.contains('@')).cloned().collect();
        for b in bases {
            let want = rungs_present(&r, &b);
            let v: Vec<Rung> = want
                .iter()
                .filter_map(|&m| rung(&r, &b, m, &canary, &full))
                .collect();
            if v.len() != want.len() || v.len() < 2 {
                continue;
            }
            let mut e = [0.0; 5];
            for (k, (_, f)) in EST.iter().enumerate() {
                e[k] = f(&v);
            }
            by.entry(b)
                .or_default()
                .entry(composition.clone())
                .or_default()
                .push((e, fixed_of(&v, &mean_v)));
        }
    }

    for (base, comps) in by {
        if comps.len() < 2 {
            continue;
        }
        // Fewest neighbours first, so the gradient reads down the column.
        let mut rows: Vec<(&String, &Vec<([f64; 5], f64)>)> = comps.iter().collect();
        rows.sort_by_key(|(l, _)| (l.matches('+').count(), (*l).clone()));

        println!("\n=== {base} ===");
        print!("{:>44}", "round");
        for (n, _) in &EST {
            print!("{n:>12}");
        }
        println!("{:>12}{:>9}{:>8}", "fixed/meas", "passes", "pass sd");

        let avg = |v: &[([f64; 5], f64)], k: usize| {
            estimate::mean(&v.iter().map(|(e, _)| e[k]).collect::<Vec<f64>>())
        };
        for (label, v) in &rows {
            print!("{label:>44}");
            for k in 0..EST.len() {
                print!("{:>12.4}", avg(v, k));
            }
            // The spread of *this* composition across passes, which is the
            // local null: how much this number moves when nothing changed.
            let pass_sd = 100.0
                * estimate::rel_spread(&v.iter().map(|(e, _)| e[0]).collect::<Vec<f64>>());
            println!(
                "{:>12.0}{:>9}{:>7.2}%",
                estimate::mean(&v.iter().map(|(_, f)| *f).collect::<Vec<f64>>()),
                v.len(),
                pass_sd
            );
        }

        // The headline, and the null it has to beat.
        //
        // Spread across compositions is only evidence of a composition
        // effect insofar as it exceeds the spread across *passes* of the
        // same composition - which is the same measurement repeated hours
        // apart. Without that second line the first one cannot be read: a
        // night of thermal drift and a real composition effect produce the
        // same number.
        let across = |k: usize, rows: &[(&String, &Vec<([f64; 5], f64)>)]| {
            let means: Vec<f64> = rows.iter().map(|(_, v)| avg(v, k)).collect();
            100.0 * estimate::rel_spread(&means)
        };
        let null = |k: usize, rows: &[(&String, &Vec<([f64; 5], f64)>)]| {
            let each: Vec<f64> = rows
                .iter()
                .filter(|(_, v)| v.len() > 1)
                .map(|(_, v)| {
                    estimate::rel_spread(&v.iter().map(|(e, _)| e[k]).collect::<Vec<f64>>())
                })
                .filter(|x| x.is_finite())
                .collect();
            if each.is_empty() {
                return f64::NAN;
            }
            100.0 * (each.iter().map(|x| x * x).sum::<f64>() / each.len() as f64).sqrt()
        };
        let line = |what: String, rows: &[(&String, &Vec<([f64; 5], f64)>)], f: &dyn Fn(usize, &[(&String, &Vec<([f64; 5], f64)>)]) -> f64| {
            print!("{what:>44}");
            for k in 0..EST.len() {
                print!("{:>11.2}%", f(k, rows));
            }
            println!();
        };
        // Only compositions that have a canary, so raw and ratio columns are
        // scored on exactly the same rounds. Crediting the ratio for being
        // asked about a different set of subsets would be no test at all.
        let with: Vec<_> = rows
            .iter()
            .filter(|(_, v)| v.iter().all(|(e, _)| e[3].is_finite()))
            .cloned()
            .collect();
        let scored = if with.len() > 1 { &with } else { &rows };
        line(
            format!("spread across {} compositions:", scored.len()),
            scored,
            &across,
        );
        line(
            "pass-to-pass spread (the null):".to_string(),
            scored,
            &null,
        );
    }
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
    print!("{:>8}{:>8}{:>10}", "auto", "corr", "slot");
    println!("{:>12} ± {:>6} ", "time", "stddev");

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
        print!(
            "{sample_mean:>12.2} ± {:>6.2}%",
            sample_stdev / sample_mean * 100.0
        );
        let autocorrelation_s = estimate::mean(
            &runs
                .iter()
                .map(|r| r.autocorrelation_time(w))
                .collect::<Vec<f64>>(),
        );
        println!("{autocorrelation_s:>12.8}s");
    }
}

/// Grow the batch until the *timed* part takes about [`SAMPLE`].
///
/// Only `run` is timed, exactly as in the measurement loop. Timing `prepare`
/// too would aim at the size of the work plus its setup, so a payload with
/// an expensive generator - and a generator can easily cost more than the
/// thing it feeds - would end up with a batch far too small.
fn calibrate(w: &Workload, seed: &mut u64) -> (usize, f64) {
    let target = SAMPLE.as_secs_f64() * 1e9;
    // A ceiling on generation, because `prepare` allocates one input per
    // iteration. Without it a benchmark whose timed part the optimiser
    // deleted would never reach `target`, and the batch would grow until
    // generating it exhausted memory.
    let prepare_ceiling = Duration::from_millis(50);

    // One untimed batch before anything is measured.
    //
    // A workload's first execution pays whatever its allocations deferred.
    // `copy_64mb` was the loud case - 64 MiB of untouched zero pages, 47 ms
    // to fault in against a true cost of 6.4 ms - but any workload that
    // allocates is exposed, and the damage is not just a wrong number. The
    // growth loop below uses each probe to decide how much to grow, so a
    // first probe inflated sevenfold makes it stop growing almost at once
    // and settle on a batch far too small. That is silent, and it would look
    // like a property of the workload.
    *seed = step(*seed);
    w.time_batch(1)();

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
            // Median of three fresh probes rather than the single one that
            // happened to end the loop. This number sets the rung durations
            // for an entire sweep, and one sample is one sample.
            let mut again: Vec<f64> = (0..3)
                .map(|_| {
                    *seed = step(*seed);
                    w.time_batch(n)()
                })
                .collect();
            again.sort_by(|a, b| a.partial_cmp(b).unwrap());
            return (n, again[1] / n as f64);
        }
        let factor = (target / ns.max(1.0)).clamp(1.5, 50.0);
        n = ((n as f64 * factor) as usize).max(n + 1);
    }
}

/// Where a subset's recording goes: its payload names, in order, joined.
///
/// Derived rather than passed so that the same subset always lands in the
/// same file, which is what makes two sweeps comparable subset by subset.
fn csv_name(dir: &str, payloads: &[Arc<Workload>], pass: Option<usize>) -> String {
    // Binary unless asked otherwise. `read` sniffs the magic rather than the
    // extension, so old CSV recordings keep working either way.
    let ext = match std::env::var("LAB_FORMAT").as_deref() {
        Ok("csv") => "csv",
        _ => "bin",
    };
    let names: Vec<&str> = payloads.iter().map(|w| w.name).collect();
    match pass {
        // A suffix rather than a directory per pass, so one glob picks up
        // every pass of one composition and `shift` can tell them apart by
        // name. Passes must stay distinguishable: pooling them would throw
        // away the replication that makes the composition effect testable.
        Some(p) => format!("{dir}/{}.p{p}.{ext}", names.join("+")),
        None => format!("{dir}/{}.{ext}", names.join("+")),
    }
}

/// The composition a recording measured, with any pass suffix removed.
fn composition_of(path: &str) -> String {
    let stem = std::path::Path::new(path)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string());
    let stem = stem.strip_suffix(".csv").or_else(|| stem.strip_suffix(".bin")).unwrap_or(&stem);
    match stem.rsplit_once(".p") {
        Some((head, tail)) if tail.chars().all(|c| c.is_ascii_digit()) => head.to_string(),
        _ => stem.to_string(),
    }
}

/// What the machine was doing, sampled beside the measurements.
///
/// Recorded rather than inferred. A whole evening went into deducing the
/// clock from `cpu_canary`'s timings; having the number the kernel reports
/// turns that inference into a check, and means an unexplained step at 3am
/// can be attributed instead of guessed at.
///
/// Sampled between rounds, never inside a timed batch: these are three file
/// reads and a few microseconds, which is nothing against a round but a
/// large fraction of a 100 us sample.
fn machine_state() -> String {
    let read = |p: &str| {
        std::fs::read_to_string(p)
            .ok()
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|| "-".to_string())
    };
    let temp = (0..)
        .map(|z| format!("/sys/class/thermal/thermal_zone{z}"))
        .take_while(|z| std::path::Path::new(z).exists())
        .find(|z| read(&format!("{z}/type")) == "x86_pkg_temp")
        .map(|z| read(&format!("{z}/temp")))
        .unwrap_or_else(|| "-".to_string());
    let procs = std::fs::read_to_string("/proc/stat")
        .ok()
        .and_then(|s| {
            s.lines()
                .find_map(|l| l.strip_prefix("procs_running ").map(|v| v.trim().to_string()))
        })
        .unwrap_or_else(|| "-".to_string());
    format!(
        "{},{},{}",
        read("/sys/devices/system/cpu/cpu0/cpufreq/scaling_cur_freq"),
        temp,
        procs
    )
}

fn step(x: u64) -> u64 {
    x.wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407)
}
