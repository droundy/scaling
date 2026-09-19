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

mod replay;
mod timing;
mod workloads;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use timing::rung_name;
use workloads::{Kind, Workload};

/// What one sample of one workload should cost. Everything is calibrated to
/// this, so the members of a round are comparable and share a noise regime.
const SAMPLE: Duration = Duration::from_micros(100);

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
        Some("selftest") => {
            let dir = args.get(2).cloned().unwrap_or_else(|| "selftest".to_string());
            replay::selftest(&dir);
        }
        Some("analyze") => {
            replay::report(&args[2..]);
        }
        Some("collect") => {
            let budget = args.get(2).and_then(|s| parse_duration(s)).unwrap_or_else(|| {
                eprintln!("usage: lab collect <duration, e.g. 30m> <dir> [workloads]");
                std::process::exit(2);
            });
            let dir = args.get(3).cloned().unwrap_or_else(|| "collect".to_string());
            if let Err(e) = std::fs::create_dir_all(&dir) {
                eprintln!("could not create {dir}: {e}");
                std::process::exit(2);
            }
            let ws: Vec<Arc<Workload>> = args
                .get(4)
                .map(|s| s.as_str())
                .unwrap_or(LADDER_SET)
                .split(',')
                .map(str::trim)
                .filter(|n| !n.is_empty())
                .map(|n| Arc::new(workloads::named(n)))
                .collect();
            collect(ws, budget, &dir);
        }
        _ => {
            eprintln!(
                "usage:\n  \
                 lab collect <duration> <dir> [workloads]   measure; e.g. `lab collect 30m out`\n  \
                 lab analyze <recording.bin>...            replay algorithms against a recording\n  \
                 lab selftest [dir]                        write fake data whose answer is in its name\n\n\
                 The runner decides nothing: it picks no rung, tests no convergence, and lets\n\
                 no workload leave the round early. Every other choice - calibration, rung\n\
                 selection, estimator, stopping - is replayed from the recording by `analyze`."
            );
            std::process::exit(2);
        }
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
/// Longest batch worth recording, for any rung above a single iteration.
///
/// The ladder is written in multiples of `SAMPLE`, so a slow workload lands
/// on counts like 1,2,3,4 rather than 1,2,4,8 - but the top of a wide ladder
/// can still ask for a batch longer than the budget an algorithm is given to
/// finish in. Such a rung cannot be chosen by anything, and recording it
/// spends collection time on data no replay will ever read.
const MAX_RUNG: Duration = Duration::from_secs(10);

/// Top of a power-of-two ladder, as a multiple of [`SAMPLE`].
///
/// Ten samples is past anything an algorithm should want: a batch that long
/// buys no precision that a shorter one plus more repetitions would not. The
/// rungs near the top exist so the ladder is seen to turn over, rather than
/// being cut off while still useful.
const POW2_TOP: f64 = 10.0;

/// How rung draws are weighted; see [`weighted_rung`].
static RUNG_WEIGHT: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Draw a rung so that, over many rounds, every rung gets the same *wall
/// time* rather than the same number of samples.
///
/// Drawing uniformly spends the run in proportion to rung cost: on a ladder
/// reaching 1000x, the top rung takes nearly all the clock and the bottom
/// rungs - the ones a fast workload's algorithm will actually select - end up
/// with too few samples to say anything about. Weighting by inverse duration
/// equalises the time, which buys many more samples where they are cheap.
///
/// Falls back to uniform when durations are unknown, which is the replay
/// case: `per` is NaN there because nothing was calibrated.
fn weighted_rung(r: &[(usize, String, f64)], bits: u64) -> usize {
    // Equal samples per rung by default, and equal *time* per rung only if
    // asked for.
    //
    // Equalising time is the obvious instinct and it is right for a narrow
    // ladder. On a power-of-two ladder it is a disaster: the span from n=1
    // to n=524288 is a factor of 500000 in cost, so inverse-duration
    // weighting gives the top rung a probability near 1e-6 and a million
    // rounds record about one sample there. The expensive rungs are not
    // decoration - an algorithm that calibrates its way up the ladder stands
    // on them - and a rung with one sample cannot be replayed at all.
    //
    // Equal counts cost little here because the ladder is capped: N samples
    // at every rung costs N * sum(d), and a geometric ladder sums to about
    // twice its top rung. Paying 2x the top rung to sample the whole ladder
    // evenly is the cheaper mistake by far.
    if !matches!(RUNG_WEIGHT.get_or_init(|| std::env::var("LAB_RUNG_WEIGHT").unwrap_or_default()).as_str(), "time") {
        return (bits >> 33) as usize % r.len();
    }
    let total: f64 = r.iter().map(|x| 1.0 / x.2).sum();
    if !total.is_finite() || total <= 0.0 {
        return (bits >> 33) as usize % r.len();
    }
    let u = (bits >> 11) as f64 / (1u64 << 53) as f64 * total;
    let mut acc = 0.0;
    for (k, x) in r.iter().enumerate() {
        acc += 1.0 / x.2;
        if u < acc {
            return k;
        }
    }
    r.len() - 1
}

/// `30s`, `45m`, `2h` - or a bare number, read as seconds.
fn parse_duration(s: &str) -> Option<Duration> {
    let s = s.trim();
    let (num, mult) = match s.chars().last()? {
        's' => (&s[..s.len() - 1], 1.0),
        'm' => (&s[..s.len() - 1], 60.0),
        'h' => (&s[..s.len() - 1], 3600.0),
        _ => (s, 1.0),
    };
    let v: f64 = num.trim().parse().ok()?;
    (v > 0.0).then(|| Duration::from_secs_f64(v * mult))
}

/// Measure everything, decide nothing.
///
/// The runner has no algorithm in it. It does not choose a rung, it does not
/// test for convergence, and no workload ever leaves the round early - which
/// is the point, because a workload dropping out changes the composition for
/// everyone still measuring, and composition moves a number by percent-scale
/// amounts. Every workload appears in every round of its subset, so the
/// recording holds one fixed composition per file and the samples in it are
/// all comparable.
///
/// One parameter: how long to spend. Not a round count, because the cost of
/// a round varies by orders of magnitude with what is in it - 2ms for the
/// fast set, 40ms once `copy_64mb` joins - so the same round count means
/// something different for every subset, while the same duration does not.
///
/// Calibration needs no separate recording. The ladder is powers of two from
/// n=1, so the rounds already contain samples at every count a growth loop
/// would probe; replaying calibration is then a question asked of the
/// recording rather than a phase that had to be captured.
fn collect(ws: Vec<Arc<Workload>>, budget: Duration, dir: &str) {
    let passes: usize = std::env::var("LAB_PASSES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3);
    let canaries = [
        Arc::new(Workload::cpu_canary()),
        Arc::new(Workload::mem_canary()),
    ];

    // Calibrated once and shared, so a workload is measured at the same
    // batch sizes in every subset. Letting each subset calibrate for itself
    // would mix the effect under study - whether a number moves with its
    // company - into the comparison meant to measure it.
    eprintln!(
        "measuring: {}",
        ws.iter()
            .map(|w| match w.kind {
                Kind::CpuCanary => format!("{} (cpu canary)", w.name),
                Kind::MemCanary => format!("{} (mem canary)", w.name),
                Kind::Payload => w.name.to_string(),
            })
            .collect::<Vec<_>>()
            .join(", ")
    );
    eprintln!("calibrating {} workloads once", ws.len());
    let mut seed = 0x9E3779B97F4A7C15u64;
    let mut counts: HashMap<&'static str, (usize, f64)> = HashMap::new();
    for w in ws.iter().chain(canaries.iter()) {
        counts.insert(w.name, calibrate(w, &mut seed));
    }

    let subsets = subsets_of(&ws);
    let slice = budget.div_f64((subsets.len() * passes) as f64);
    eprintln!(
        "{} subsets x {passes} passes, {:.1}s each, {:.1} min total",
        subsets.len(),
        slice.as_secs_f64(),
        budget.as_secs_f64() / 60.0,
    );

    let end = Instant::now() + budget;
    let mut perm = 0xD1B54A32D192ED03u64;
    for pass in 0..passes {
        // Fresh order each pass, so a run cut short does not systematically
        // starve whichever subsets sit at the end of a fixed order.
        let mut order: Vec<usize> = (0..subsets.len()).collect();
        for i in (1..order.len()).rev() {
            perm = step(perm);
            order.swap(i, (perm >> 33) as usize % (i + 1));
        }
        for &i in &order {
            if Instant::now() >= end {
                eprintln!("budget spent");
                return;
            }
            let stop = (Instant::now() + slice).min(end);
            run(
                &canaries,
                subsets[i].clone(),
                usize::MAX,
                dir,
                &[],
                Some(&counts),
                Some(pass),
                0,
                Some(stop),
            );
        }
    }
}

/// Which compositions to measure.
///
/// The powerset is what answers "does this number move with its company",
/// and nothing else does - but it is 2^n, which is 31 subsets for five
/// workloads and 2047 for eleven. Past a threshold the budget per subset
/// gets too thin to say anything, so the choice becomes singletons (each
/// workload alone, the control) plus the full set (what a user actually
/// runs), which is where most of the composition signal lives.
fn subsets_of(ws: &[Arc<Workload>]) -> Vec<Vec<Arc<Workload>>> {
    const MAX_POWERSET: usize = 63;
    if (1usize << ws.len()) - 1 <= MAX_POWERSET {
        return ws
            .iter()
            .cloned()
            .powerset()
            .filter(|s| !s.is_empty())
            .collect();
    }
    let mut out: Vec<Vec<Arc<Workload>>> = ws.iter().map(|w| vec![w.clone()]).collect();
    out.push(ws.to_vec());
    out
}

fn counts_for(cal: usize, ladder: &[f64]) -> Vec<usize> {
    // An empty ladder means powers of two in *count*, from a single
    // iteration up to about `POW2_TOP` samples' worth.
    //
    // Duration multiples are the right way to say where the rungs go when
    // the rungs are the measurement. They are the wrong way when the
    // recording has to support replaying a *calibration*, because the growth
    // loop probes 1 and then doubles: a ladder of duration multiples lands
    // on counts like 348 and 1740, so a replayed probe can never stand where
    // it asked to, and the thing being simulated is not the thing that runs.
    //
    // Powers of two also reach n=1 for every workload by construction, which
    // is where every growth loop starts - rather than by choosing a bottom
    // rung small enough and hoping it was small enough.
    if ladder.is_empty() {
        let top = (POW2_TOP * cal as f64).max(1.0);
        let mut out = vec![1usize];
        while (*out.last().unwrap() as f64) < top {
            out.push(out.last().unwrap() * 2);
        }
        return out;
    }
    let mut out: Vec<usize> = Vec::with_capacity(ladder.len());
    for &t in ladder {
        let want = (t * cal as f64).round().max(1.0) as usize;
        let floor = out.last().map(|&p| p + 1).unwrap_or(1);
        out.push(want.max(floor));
    }
    out
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
    deadline: Option<Instant>,
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
    let mut rungs: Vec<Vec<(usize, String, f64)>> = Vec::with_capacity(ws.len());
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
        let mut this: Vec<(usize, String, f64)> = Vec::with_capacity(derived.len());
        for k in 0..derived.len() {
            let name = rung_name(w.name, k);
            let n = if t.replaying() {
                // A rung absent from the recording was dropped when it was
                // made - see MAX_RUNG - so skip it rather than failing.
                match t.iters.get(&name) {
                    Some(&n) => n,
                    None => continue,
                }
            } else {
                derived[k]
            };
            let dur = n as f64 * per;
            // A rung is only worth recording if some algorithm could pick
            // it, and nothing can pick a batch that overruns the whole
            // budget. `n == 1` is exempt: one iteration is the least a
            // workload can be measured in, so however long it takes, that
            // is the measurement.
            if !t.replaying() && n > 1 && dur > MAX_RUNG.as_nanos() as f64 {
                continue;
            }
            t.iters.insert(name.clone(), n);
            // Only when this call did its own calibrating. A sweep has
            // already printed the plan once, and repeating it for every
            // subset of every pass buries the log.
            if counts.is_none() && !t.replaying() {
                eprintln!(
                    "  {name:>16} {n:>12} iters  ~{:>8.0} us",
                    n as f64 * per / 1e3
                );
            }
            this.push((n, name, dur));
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
    let fresh = std::fs::metadata(&mach_path)
        .map(|m| m.len() == 0)
        .unwrap_or(true);
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

    let mut done = 0usize;
    for r in 0..rounds {
        // The budget running out - *not* a convergence test. Nothing here
        // looks at the numbers it is collecting, and no workload ever
        // leaves the round early, which is what keeps the composition
        // fixed for every sample in the recording. Deciding when enough
        // has been measured is an analysis-time question, asked of the
        // recording afterwards.
        //
        // Checked once per round rather than once per sample, because a
        // round is the unit that keeps every workload's sample count
        // equal; cutting inside one would short whichever workloads sit
        // late in the slot order.
        if let Some(d) = deadline {
            if Instant::now() >= d {
                break;
            }
        }
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
                weighted_rung(r, perm)
            })
            .collect();
        let mut order: Vec<usize> = (0..ws.len()).collect();
        for i in (1..ws.len()).rev() {
            perm = step(perm);
            order.swap(i, (perm >> 33) as usize % (i + 1));
        }
        for (slot, &i) in order.iter().enumerate() {
            seed = step(seed);
            let (count, name, _) = &rungs[i][pick[i]];
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
        done += 1;
    }
    // What was actually run, not what was asked for: a deadline-driven run
    // is handed `usize::MAX` and would otherwise report it.
    eprintln!("{done} rounds in {:.2}s", start.elapsed().as_secs_f64());
    t.write(&out);
    eprintln!("wrote {out}");

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
    let stem = stem
        .strip_suffix(".csv")
        .or_else(|| stem.strip_suffix(".bin"))
        .unwrap_or(&stem);
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
            s.lines().find_map(|l| {
                l.strip_prefix("procs_running ")
                    .map(|v| v.trim().to_string())
            })
        })
        .unwrap_or_else(|| "-".to_string());
    format!(
        "{},{},{}",
        read("/sys/devices/system/cpu/cpu0/cpufreq/scaling_cur_freq"),
        temp,
        procs
    )
}

pub fn step(x: u64) -> u64 {
    x.wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407)
}
