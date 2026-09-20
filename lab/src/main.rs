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

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use std::time::{Duration, Instant};
use timing::rung_name;
use workloads::{Kind, Workload};

/// What one sample of one workload should cost. Everything is calibrated to
/// this, so the members of a round are comparable and share a noise regime.
const SAMPLE: Duration = Duration::from_micros(100);

/// The workloads measured when none are named.
///
/// Two with real ladders - nine rungs and six - where rung choice,
/// subtraction and calibration all matter, and three past the rung window
/// with two rungs each and no choice to make. The powerset is exponential
/// in this list, so it is short on purpose: five payloads is 31 subsets,
/// where seven would be 127.
const LADDER_SET: &str = "f64_sin,btree_miss,urandom_read,str_find,copy_64mb";

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

/// Shortest batch worth recording.
///
/// The harness costs about 370ns per measurement outside the timer, so a
/// batch below this is mostly overhead - the machine time buys almost no
/// information about the workload.
const RUNG_MIN_NS: f64 = 100.0;

/// Longest batch worth recording.
///
/// Scheduler ticks arrive every millisecond and add ~5us to whatever batch
/// they land in, so the chance of a batch being hit is its length over the
/// tick period. Below about 20us a hit is a rare outlier that a trimmed
/// estimate removes; by 100us it is 17% and trimming leaves a bias behind;
/// past a millisecond every batch is hit and the contamination is
/// indistinguishable from the workload being slower. Sweeping this against
/// a known answer put the boundary at 20us.
///
/// Nothing is lost by stopping here. A fit takes its lever arm from the
/// *ratio* of iteration counts, not from absolute duration, so a ladder
/// reaching 8192 iterations at 20us has the same lever as one reaching 2ms
/// at a hundredth of the cost.
///
/// This is a budget decision and can be revisited. What it is for is keeping
/// a recording quick enough that the whole powerset is affordable and every
/// rung ends up with far more samples than any replay needs - not for making
/// a claim about precision. Widening it costs machine time in proportion to
/// the largest rung, which is why it is set once, here, rather than
/// negotiated per analysis.
const RUNG_MAX_NS: f64 = 2e4;

/// How long a second rung may run for a workload too slow for the window.
///
/// A workload slower than `RUNG_MAX_NS` has only n=1 inside it, and one rung
/// admits no subtraction. Recording n=2 as well keeps that option open, up
/// to a point: two samples of 4ms is cheap, two samples of a minute is not.
const SLOW_PAIR_NS: f64 = 1e9;

/// How rung draws are weighted; see [`weighted_rung`].
static RUNG_WEIGHT: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Draw a rung so that, over many rounds, every rung gets the same *wall
/// time* rather than the same number of samples.
///
/// This is the shape a replay wants. An algorithm with a time budget takes
/// `B/d` samples at a rung costing `d` - many at the cheap rungs, few at the
/// expensive ones - so a recording weighted by inverse duration holds
/// samples roughly in the proportion that replays consume them. Equal counts
/// over-collect the top of the ladder and starve the bottom, where an
/// algorithm will take orders of magnitude more samples.
///
/// I argued the other way earlier, when the ladder ran from n=1 to n=524288
/// and inverse weighting would have given the top rung about one sample per
/// million rounds. The recorder's window has since cut that span to roughly
/// a hundredfold, so the starvation that argument rested on is gone.
///
/// The cost is real though: the longest rung now gets the fewest samples, so
/// a replayed algorithm that parks on it exhausts the recording soonest.
/// `LAB_RUNG_WEIGHT=count` restores equal sample counts.
fn weighted_rung(r: &[(usize, String, f64, u8)], bits: u64) -> usize {
    if matches!(
        RUNG_WEIGHT
            .get_or_init(|| std::env::var("LAB_RUNG_WEIGHT").unwrap_or_default())
            .as_str(),
        "count"
    ) {
        return (bits >> 33) as usize % r.len();
    }
    // The cost of a sample is its batch *plus* the per-measurement overhead,
    // not the batch alone.
    //
    // Weighting by the batch alone says a cpu_canary sample at n=1 costs
    // 2.36ns when it really costs 372ns - over-weighting the cheapest rung
    // by 158x and pushing the dearest rung's draw probability down to
    // 1.2e-4. Filling the top rung then took 83 million rounds instead of
    // 1.7 million, which buffered 166 million samples and ran the machine
    // out of memory before anything could be written. `round_cost` had the
    // overhead in all along; this is the half that disagreed.
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
    // One canary, not two.
    //
    // `mem_canary` was here to be the reference that moves with memory
    // pressure, but it reads ~20% differently between processes because its
    // chase table lands at a different address each time, and it allocates
    // 64 MiB in every round it sits in. A reference whose own answer is not
    // reproducible cannot referee anything, and it was excluded from every
    // summary it appeared in. Still available by name for a workload that
    // wants it.
    let canaries = [Arc::new(Workload::cpu_canary())];

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

    // What is about to be measured, in durations as well as counts. A
    // workload whose rungs come out 1,2 rather than geometric is one whose
    // single iteration already outruns the top of the ladder, and that is
    // worth seeing before committing hours to it.
    eprintln!("\nladder plan:");
    let mut seen = BTreeSet::new();
    for w in ws.iter().chain(canaries.iter()).filter(|w| seen.insert(w.name)) {
        let (_cal, per) = counts[w.name];
        let plan: Vec<String> = rungs_for(per)
            .iter()
            .map(|&c| {
                let ns = c as f64 * per;
                if ns < 1e3 {
                    format!("{c}={ns:.0}ns")
                } else {
                    format!("{c}={:.1}us", ns / 1e3)
                }
            })
            .collect();
        // What this workload adds to a round, which is what decides
        // whether it can share one with the others: every workload gets a
        // sample per round, so the slowest member sets everyone's sample
        // rate.
        let rungs = rungs_for(per);
        let inv: f64 = rungs
            .iter()
            .map(|&n| 1.0 / (n as f64 * per + OVERHEAD_NS))
            .sum();
        let cost = rungs.len() as f64 / inv;
        let cost_s = if cost >= 1e6 {
            format!("{:.2}ms", cost / 1e6)
        } else if cost >= 1e3 {
            format!("{:.1}us", cost / 1e3)
        } else {
            format!("{cost:.0}ns")
        };
        eprintln!(
            "  {:>14} {:>9}/round  {}",
            w.name,
            cost_s,
            plan.join("  ")
        );
    }
    eprintln!();

    let subsets = subsets_of(&ws, budget, passes, &counts, canaries[0].clone());
    eprintln!(
        "{} subsets x {passes} passes, ~{:.1}s each to start, {:.1} min total",
        subsets.len(),
        budget.as_secs_f64() / (subsets.len() * passes) as f64,
        budget.as_secs_f64() / 60.0,
    );

    let end = Instant::now() + budget;
    // Slices still to run, including this pass's.
    let mut left = subsets.len() * passes;
    let mut perm = 0xD1B54A32D192ED03u64;
    // Cheapest compositions first, so a run cut short loses the expensive
    // ones rather than a random half of everything.
    //
    // The cost of a subset is set almost entirely by whether the slowest
    // workloads are in it - copy_64mb and str_find are 99.7% of a full
    // round - so this sorts into a few natural tiers rather than a smooth
    // gradient. Within a tier the order is still shuffled afresh each pass,
    // which is what stops a subset's position in the pass from being
    // confounded with when in the run it was measured. Across tiers that
    // confound is accepted deliberately: the slow subsets do always sit
    // late in a pass, and being able to stop the run at any point and still
    // have whole cheap compositions is worth more.
    let mut cost: Vec<f64> = subsets.iter().map(|s| round_cost(s, &counts)).collect();
    let mut order_by_cost: Vec<usize> = (0..subsets.len()).collect();
    order_by_cost.sort_by(|&a, &b| cost[a].partial_cmp(&cost[b]).unwrap());
    cost.sort_by(|a, b| a.partial_cmp(b).unwrap());

    for pass in 0..passes {
        let mut order = order_by_cost.clone();
        // Shuffle only within runs of near-equal cost.
        let mut i = 0;
        while i < order.len() {
            let mut j = i + 1;
            while j < order.len() && cost[j] <= cost[i] * 1.5 {
                j += 1;
            }
            for k in (i + 1..j).rev() {
                perm = step(perm);
                let t = i + (perm >> 33) as usize % (k - i + 1);
                order.swap(k, t);
            }
            i = j;
        }
        for &i in &order {
            let now = Instant::now();
            if now >= end || left == 0 {
                eprintln!("budget spent");
                return;
            }
            // Slice from what is *left*, over what is left to do, rather
            // than a fixed share worked out up front.
            //
            // The duration is the one input to this program, and a fixed
            // share stopped honouring it once subsets could finish early:
            // a cheap composition fills its rung cap in seconds and would
            // simply hand its remaining twelve minutes back to nobody, so
            // `collect 20h` quietly became a fifteen-hour run whose real
            // length depended on which subsets happened to fill up.
            //
            // Recomputing spreads that surplus over whatever has not run
            // yet. With compositions ordered cheapest-first, the ones that
            // finish early are exactly the ones with time to give, and the
            // expensive ones that receive it are the ones that are starved
            // - they reach a few hundred samples at their dearest rung
            // where a cheap subset reaches ten thousand.
            let stop = now + (end - now) / left as u32;
            left -= 1;
            run(
                &canaries,
                subsets[i].clone(),
                usize::MAX,
                dir,
                Some(&counts),
                Some(pass),
                0,
                Some(stop),
            );
        }
    }
}

/// What one round of a composition costs, in ns.
///
/// One sample per workload, at the average of its rungs, plus the harness
/// overhead paid per measurement whatever the batch size.
fn round_cost(ws: &[Arc<Workload>], counts: &HashMap<&'static str, (usize, f64)>) -> f64 {
    ws.iter()
        .map(|w| {
            let per = counts.get(w.name).map(|c| c.1).unwrap_or(0.0);
            let rungs = rungs_for(per);
            // Harmonic mean, because rungs are drawn weighted by inverse
            // duration: the expected cost of a draw is K / sum(1/d), not
            // the arithmetic mean of the durations.
            let inv: f64 = rungs
                .iter()
                .map(|&n| 1.0 / (n as f64 * per + OVERHEAD_NS))
                .sum();
            rungs.len() as f64 / inv
        })
        .sum()
}

/// Roughly what a measurement costs outside the timer; see `Rung::overhead_ns`
/// in `replay.rs`, where it is measured rather than assumed.
const OVERHEAD_NS: f64 = 370.0;

/// Most samples worth recording at any one rung.
///
/// Not a stopping rule - nothing here looks at the numbers - but a capacity
/// one, of a kind with the rung window. A cheap composition runs at nearly a
/// million rounds a second, so an equal *time* slice buys it far more data
/// than any replay can read and about 33MB a second of disk: thirty seconds
/// of `f64_sin` alone produced a gigabyte. Past this point a subset has
/// nothing left to learn and moves on, which also means the expensive
/// subsets are reached sooner.
///
/// Twenty-five times `MIN_SAMPLES_PER_RUNG`, so there is a wide margin
/// between the least a rung may have and the most it may keep.
const MAX_SAMPLES_PER_RUNG: usize = 10_000;

/// Fewest samples a rung should end up with, for the most expensive subset.
///
/// A replay draws samples in recorded order and stops when it runs off the
/// end, so a rung with too few samples yields nothing at all - the analysis
/// reports the cell blank rather than reporting recycled noise. This is the
/// floor that makes a collection worth starting.
const MIN_SAMPLES_PER_RUNG: f64 = 400.0;

/// Which compositions to measure.
///
/// The powerset is what answers "does this number move with its company",
/// and nothing else does. The reason to fall back from it is not that 2^n
/// gets large but that the slice per subset gets too short to leave each
/// rung usable - so that is what is checked, rather than a count.
///
/// The check matters because equal-*time* slices already absorb most of the
/// cost objection: an expensive composition simply completes fewer rounds in
/// its slice. Splitting workloads into a fast group and a slow group to make
/// the powerset affordable is the wrong trade, because it removes exactly
/// the compositions worth having - a memory-destroying workload sharing a
/// round with a cheap one is the case most likely to move a number, and
/// grouping by cost guarantees it never happens.
fn subsets_of(
    ws: &[Arc<Workload>],
    budget: Duration,
    passes: usize,
    counts: &HashMap<&'static str, (usize, f64)>,
    canary: Arc<Workload>,
) -> Vec<Vec<Arc<Workload>>> {
    let full: Vec<Arc<Workload>> = ws.to_vec();
    // One composition, sampled hard. The powerset answers whether a number
    // moves with its company; developing an algorithm against recorded data
    // is a different question that wants depth in the composition a user
    // would actually get, not breadth across compositions they would not.
    match std::env::var("LAB_SUBSETS").unwrap_or_default().as_str() {
        // One composition, sampled hard: how an algorithm behaves, in the
        // round a user would actually get.
        "full" => {
            eprintln!("one composition: the full set");
            return vec![full];
        }
        // Singletons, every pair, and the full set.
        //
        // For a workload too expensive to put in all 2^n subsets, this is
        // most of what the powerset would have told us: alone is the
        // control, each pair isolates one neighbour's effect on it, and the
        // full set is the crowd. What it gives up is interaction between
        // *three or more* specific neighbours, which is a second-order
        // question we have no evidence matters.
        "pairs" => {
            let mut out: Vec<Vec<Arc<Workload>>> =
                ws.iter().map(|w| vec![w.clone()]).collect();
            for (i, a) in ws.iter().enumerate() {
                for b in &ws[i + 1..] {
                    out.push(vec![a.clone(), b.clone()]);
                }
            }
            out.push(full);
            eprintln!("singletons, pairs and the full set: {} subsets", out.len());
            return out;
        }
        _ => {}
    }
    let n_subsets = (1usize << ws.len()) - 1;
    let slice_ns = budget.as_secs_f64() * 1e9 / (n_subsets * passes) as f64;
    // The full set is the most expensive composition, so it is where every
    // workload collects the least: one sample per round for everyone means
    // the slowest member sets the sample rate for all of them.
    let rounds = slice_ns / round_cost(&full, counts);

    // The binding number is the *dearest* rung, not the average rung.
    //
    // Rungs are drawn weighted by inverse duration, so every rung of a
    // workload gets the same wall time but wildly different sample counts -
    // for instant_now the cheapest rung takes 28% of the draws and the
    // dearest 0.68%. Reporting the mean described a distribution that is
    // nowhere near it and overstated the dearest rung by about sixteenfold.
    // A replayed algorithm that runs off the end of a rung yields nothing at
    // all, so the thinnest rung is what decides whether a cell is usable.
    let worst = ws
        .iter()
        .chain(std::iter::once(&canary))
        .map(|w| {
            let per = counts.get(w.name).map(|c| c.1).unwrap_or(0.0);
            let d: Vec<f64> = rungs_for(per)
                .iter()
                .map(|&n| n as f64 * per + OVERHEAD_NS)
                .collect();
            let inv: f64 = d.iter().map(|x| 1.0 / x).sum();
            // Equal time per rung: samples at rung k are t_per_rung / d_k,
            // and t_per_rung is the same for every rung by construction.
            let t_per_rung = rounds / inv;
            let dearest = d.iter().cloned().fold(0.0, f64::max);
            (w.name, t_per_rung / dearest, t_per_rung)
        })
        .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap())
        .map(|(n, s, t)| (n, s, t))
        .unwrap_or(("?", 0.0, 0.0));

    if worst.1 >= MIN_SAMPLES_PER_RUNG {
        eprintln!(
            "powerset: {n_subsets} subsets; thinnest rung is {}'s dearest at \
             ~{:.0} samples ({:.1}ms of it)",
            worst.0, worst.1, worst.2 / 1e6
        );
        return ws.iter().cloned().powerset().filter(|s| !s.is_empty()).collect();
    }
    eprintln!(
        "powerset would leave {}'s dearest rung at ~{:.0} samples (want {MIN_SAMPLES_PER_RUNG:.0}); \
         measuring singletons and the full set instead",
        worst.0, worst.1
    );
    let mut out: Vec<Vec<Arc<Workload>>> = ws.iter().map(|w| vec![w.clone()]).collect();
    out.push(full);
    out
}

/// The rungs to record for a workload, given what one iteration costs.
///
/// Powers of two in count, because the calibration a replayed algorithm has
/// to reproduce probes a count and then grows it - a ladder of duration
/// multiples lands on counts like 348 and 1740, so a replayed probe could
/// never stand where it asked to.
///
/// **This filter lives here, in the recorder, and not in the analysis.**
/// Which rungs exist is a property of the recording. Deciding it twice -
/// recording a wide ladder and then discarding most of it when reading -
/// spends machine time on batches nothing will ever look at. The only thing
/// it says to an analysis is which algorithms can be replayed at all: one
/// that wants a rung outside this window needs fresh data, not a different
/// query.
///
/// `n = 1` is always recorded, whatever it costs. For a slow workload it is
/// the only rung inside reach, and for a fast one it is where a growth loop
/// starts - so leaving it out would make calibration unreplayable and would
/// throw away the cheapest, most informative point for the intercept.
fn rungs_for(per_ns: f64) -> Vec<usize> {
    let mut out = vec![1usize];
    let mut n = 1usize;
    while n < (1usize << 40) {
        n *= 2;
        let d = n as f64 * per_ns;
        if d > RUNG_MAX_NS {
            break;
        }
        if d >= RUNG_MIN_NS {
            out.push(n);
        }
    }
    // A workload too slow for the window has only n=1 so far. Give it a
    // partner if one is affordable, so subtraction stays possible.
    if out.len() < 2 && 2.0 * per_ns <= SLOW_PAIR_NS {
        out.push(2);
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
    // A canary named in the workload list as well as added structurally
    // would be measured twice a round under one name, and the two series
    // would be silently merged on load - a workload interleaved with
    // itself, which is not what any of the estimates assume.
    let mut seen = BTreeSet::new();
    ws.retain(|w| seen.insert(w.name));

    eprintln!("\n=== {out} ===");
    let mut t = timing::Timing::new();

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
    let mut rungs: Vec<Vec<(usize, String, f64, u8)>> = Vec::with_capacity(ws.len());
    for w in ws.iter() {
        let (_cal, per) = match counts.and_then(|c| c.get(w.name)) {
            Some(&(n, p)) => (n, p),
            None => calibrate(w, &mut seed),
        };
        let mut this: Vec<(usize, String, f64, u8)> = Vec::with_capacity(8);
        for (k, &n) in rungs_for(per).iter().enumerate() {
            let name = rung_name(w.name, k);
            let dur = n as f64 * per;
            // A rung is only worth recording if some algorithm could pick
            // it, and nothing can pick a batch that overruns the whole
            // budget. `n == 1` is exempt: one iteration is the least a
            // workload can be measured in, so however long it takes, that
            // is the measurement.
            if n > 1 && dur > MAX_RUNG.as_nanos() as f64 {
                continue;
            }
            let overhead_ns = overhead_of(w, n);
            t.rungs.insert(
                name.clone(),
                crate::timing::RungMeta {
                    n,
                    overhead_ns,
                },
            );
            if counts.is_none() {
                eprintln!("  {name:>16} {n:>12} iters  ~{:>8.0} us", dur / 1e3);
            }
            // The *cost* of a sample, batch plus overhead, because that is
            // what the draw is weighted by and what a slice is spent on.
            this.push((n, name, dur + overhead_ns, 0));
        }
        rungs.push(this);
    }

    // The header names every rung, so it can be written once the ladder is
    // fixed and before any sample is taken. Resolving each rung's index now
    // keeps a hash lookup out of the measuring loop.
    let idx = t.open(&out);
    for w in rungs.iter_mut() {
        for r in w.iter_mut() {
            r.3 = match idx.get(&r.1) {
                Some(&i) => i,
                None => continue,
            };
        }
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
    // How often to snapshot the machine's state, **in time rather than in
    // rounds**.
    //
    // This was every 200 rounds, which is a trap: a round is 2.1us for
    // f64_sin alone and 11.5ms for the full set, so the same round count
    // meant a /proc read every 420us in one composition and every 2.3s in
    // another. Reading CPU frequency, package temperature and
    // procs_running is far from free - /proc/stat alone walks every CPU -
    // so cheap compositions were spending a large fraction of each round
    // inside the instrumentation.
    //
    // That is the worst shape a bug can have here. The contamination scales
    // inversely with round cost, so it lands hardest on exactly the cheap
    // compositions the powerset exists to compare, and it would be
    // indistinguishable from the composition effect being measured. It also
    // wrote 36MB of machine.csv in five minutes.
    const MACHINE_EVERY: Duration = Duration::from_secs(1);
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
    let mut last_machine = Instant::now() - MACHINE_EVERY;
    // Samples taken at each rung, so a subset can stop once every rung has
    // more than any replay will read. The thinnest rung is what counts: the
    // rest are cheaper and fill up sooner.
    let mut taken: Vec<Vec<usize>> = rungs.iter().map(|r| vec![0; r.len()]).collect();
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
        if last_machine.elapsed() >= MACHINE_EVERY {
            last_machine = Instant::now();
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
        for &i in order.iter() {
            seed = step(seed);
            let (count, _, _, ridx) = &rungs[i][pick[i]];
            let ridx = *ridx;
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
            t.time(ridx, time_me);
            taken[i][pick[i]] += 1;
        }
        done += 1;
        // Checked once a round, and only against the cheapest thing to
        // check: whether the thinnest rung anywhere has filled up.
        if done % 4096 == 0
            && taken
                .iter()
                .all(|w| w.iter().all(|&c| c >= MAX_SAMPLES_PER_RUNG))
        {
            eprintln!("  every rung full at {done} rounds");
            break;
        }
    }
    // What was actually run, not what was asked for: a deadline-driven run
    // is handed `usize::MAX` and would otherwise report it.
    eprintln!("{done} rounds in {:.2}s", start.elapsed().as_secs_f64());
    t.finish();
    eprintln!("wrote {out} ({} samples)", t.written);

}

/// What a sample at this rung costs beyond its batch.
///
/// The difference between the wall time of the whole measuring call and the
/// time it reports: building the closure, whatever the workload does to
/// prepare its inputs, and the two clock reads.
///
/// This used to be derived at analysis time from the gaps between
/// consecutive samples, which is why every sample carried an eight-byte
/// timestamp. Measuring it here costs a handful of probes per rung and puts
/// one number per rung in the header instead.
///
/// Median of a few, because a probe that catches a scheduler excursion would
/// otherwise be multiplied by every sample a replay takes.
fn overhead_of(w: &Workload, n: usize) -> f64 {
    // Enough that the median is stable. Five gave cpu_canary 214ns at n=1
    // against 106ns at n=64, which is not a real difference between two
    // rungs of the same workload - it is a median of five.
    const PROBES: usize = 25;
    let mut v: Vec<f64> = (0..PROBES)
        .map(|_| {
            let start = Instant::now();
            let job = w.time_batch(n);
            let timed = job();
            (start.elapsed().as_secs_f64() * 1e9 - timed).max(0.0)
        })
        .collect();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
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
    // Located once. Finding it means stat-ing every thermal zone and
    // reading each one's `type`, which is a dozen file opens to answer a
    // question whose answer cannot change while the process runs.
    static TEMP_PATH: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    let temp = TEMP_PATH
        .get_or_init(|| {
            (0..)
                .map(|z| format!("/sys/class/thermal/thermal_zone{z}"))
                .take_while(|z| std::path::Path::new(z).exists())
                .find(|z| read(&format!("{z}/type")) == "x86_pkg_temp")
                .map(|z| format!("{z}/temp"))
        })
        .as_ref()
        .map(|p| read(p))
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
