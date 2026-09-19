//! The head-to-head: four measurement protocols, one wall-clock budget.
//!
//! Everything else in this lab measures *properties*. This measures
//! **protocols** - whole strategies for turning a budget of machine time into
//! one number - and scores them the only way that matters to somebody running
//! a benchmark: run it again tomorrow and see if you get the same answer.
//!
//! Three rules make the comparison honest, and each of them is a way the
//! experiment could have been rigged:
//!
//! **The budget covers calibration.** A protocol that spends half its time
//! deciding how to measure has spent half its time. Excluding that would
//! favour whichever protocol calibrates hardest, which is the one being
//! proposed.
//!
//! **The budget is wall-clock, not a round count.** A ladder costs more per
//! round than a single batch, so fixing rounds would hand the advantage to
//! the simplest cell and call it a result.
//!
//! **One process per repeat.** Reproducibility across *processes* is the
//! question - a fresh calibration, fresh allocations, separated in time.
//! Splitting one long run into blocks would score the estimators while hiding
//! calibration wander entirely.
//!
//! The budget clock starts after the workloads are constructed. Building a
//! million-entry map is a fixture, identical in every cell, and at small
//! budgets it would swamp the thing being compared.

use crate::estimate;
use crate::workloads::Workload;
use std::time::{Duration, Instant};

/// What one sample should cost in the cells that fix it in advance.
const SAMPLE_NS: f64 = 100_000.0;

/// How much of the budget a protocol may spend deciding how to measure.
const CALIB_FRACTION: f64 = 0.25;

#[derive(Clone, Copy, PartialEq)]
pub enum Est {
    /// Batch time over batch size, trimmed. What the crate does today.
    Naive,
    /// The two extreme rungs differenced, which removes a fixed cost per
    /// measurement whatever its size.
    Subtract,
}

/// A protocol, as a decision about rungs plus an estimator.
pub struct Cell {
    pub name: &'static str,
    /// Target sample durations as multiples of `SAMPLE_NS`; empty means
    /// "measure the efficiency curve and choose".
    pub rungs: &'static [f64],
    pub est: Est,
}

/// Stop sampling once the reported relative standard error falls below this.
///
/// This is the thing under test, not a tuning knob: a benchmark that stops
/// when `sd/sqrt(n)/mean` looks small is trusting that number, and whether it
/// deserves to be trusted is the question.
pub fn se_target() -> f64 {
    std::env::var("LAB_SE_TARGET")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0.01)
}

pub const CELLS: [Cell; 7] = [
    // A: today. One sample size, chosen by convention, no subtraction.
    Cell {
        name: "A-current",
        rungs: &[1.0],
        est: Est::Naive,
    },
    // B: the proposal in full - find the efficiency optimum by measurement,
    // bracket it, subtract. The only cell that pays for a mini-ladder.
    Cell {
        name: "B-auto",
        rungs: &[],
        est: Est::Subtract,
    },
    // C: small samples, no subtraction. If B wins, C says how much of that
    // was simply using a shorter sample.
    Cell {
        name: "C-small",
        rungs: &[0.05],
        est: Est::Naive,
    },
    // D: today's sample size, with subtraction. The other half of the 2x2,
    // and says how much of B's win was the estimator.
    Cell {
        name: "D-sub",
        rungs: &[1.0, 8.0],
        est: Est::Subtract,
    },
    // E: what actually happens in practice. Same measurement as A, but it
    // stops as soon as the reported standard error looks good enough rather
    // than spending the budget. The budget becomes a cap it usually never
    // reaches, and the number worth reading is not its spread but the ratio
    // of that spread to the error bar it claimed on the way out.
    Cell {
        name: "E-stop",
        rungs: &[1.0],
        est: Est::Naive,
    },
    // F: the short sample that subtraction makes possible. The fixed cost is
    // what forces a long sample - dilute it until the bias is small - so
    // removing it should let the sample shrink, and a shorter sample means
    // more of them per second. Algebra says this wins ~1.8x for a workload
    // whose noise is multiplicative and loses ~1.35x where it is additive,
    // because there a long sample genuinely dilutes the noise. Neither D
    // (100us + 800us) nor B (450ns) tested this middle.
    Cell {
        name: "F-short2",
        rungs: &[0.05, 0.4],
        est: Est::Subtract,
    },
    // G: the algorithm. Everything the lab settled, in one path.
    //
    // Calibration decides the rungs from the *requested accuracy* rather
    // than from an efficiency optimum - the optimum minimises sampling
    // noise, which is the error that stops mattering, while the fixed cost
    // is a bias that no amount of sampling removes and no error bar reveals.
    // So: pick a sample long enough that a/T is negligible against the
    // request; if the budget cannot afford that sample, use two rungs and
    // subtract the bias away instead of diluting it.
    Cell {
        name: "G-algo",
        rungs: &[],
        est: Est::Subtract,
    },
];

/// A mean, and two claims about how well it is known.
///
/// The two differ in what they can see, and that is the whole point.
///
/// `se_naive` is `sd/sqrt(n)` over the trimmed samples - the textbook error
/// bar, and what a stopping rule watches. It assumes samples are independent.
///
/// `se_batch` is the batch-means estimator: cut the sample sequence into
/// blocks *in order*, average each, and take the spread of the block means.
/// If consecutive samples are correlated - a clock ramp, a cache state that
/// persists, anything that makes neighbouring measurements agree more than
/// chance allows - the naive form divides by an `n` it has not really got,
/// and this one does not.
///
/// Neither can see *between-process* variation: a batch size calibrated
/// differently, a different memory layout, whatever makes two runs of the
/// same benchmark disagree. That is invisible from inside a single run by
/// construction. Which of the two floors we are against decides whether an
/// honest error bar is achievable at all from one process, and the point of
/// recording both is to find out.
struct Stat {
    mean: f64,
    se_naive: f64,
    se_batch: f64,
}

fn stat(v: &[f64]) -> Stat {
    if v.len() < 4 {
        return Stat {
            mean: f64::NAN,
            se_naive: f64::NAN,
            se_batch: f64::NAN,
        };
    }
    let mean = estimate::trimmed_mean(v, 0.10);

    let mut s: Vec<f64> = v.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let k = s.len() / 10;
    let t = &s[k..s.len() - k];
    let mu = estimate::mean(t);
    let se_naive = estimate::variance(t, mu).sqrt() / (t.len() as f64).sqrt();

    // Batch means over the sequence as taken, so order is preserved.
    //
    // The block count adapts rather than sitting at twenty, because the
    // sample counts that matter most here are small: a benchmark that stops
    // when its error bar looks good stops after ten or twenty samples, and a
    // fixed twenty blocks would return nothing at all in exactly the regime
    // the estimator exists to fix. Few blocks make a noisy estimate of the
    // spread; no blocks make none.
    let b = (v.len() / 4).clamp(4, 20);
    let se_batch = if v.len() >= 8 {
        let per = v.len() / b;
        let means: Vec<f64> = (0..b)
            .map(|i| estimate::mean(&v[i * per..(i + 1) * per]))
            .collect();
        let m = estimate::mean(&means);
        estimate::variance(&means, m).sqrt() / (b as f64).sqrt()
    } else {
        f64::NAN
    };
    Stat {
        mean,
        se_naive,
        se_batch,
    }
}

/// What a workload ended up measuring, and what it cost to decide.
struct Plan {
    /// Batch size per rung, and the samples already taken at it.
    rungs: Vec<(usize, Vec<f64>)>,
    /// True when one call already exceeds the target, so there was no choice.
    forced: bool,
    /// Sample duration the mini-ladder picked, ns. NaN when it did not run.
    chosen_ns: f64,
}

/// Grow a batch until it reaches `target` ns, cheaply, and say so when there
/// was never a decision to make.
///
/// The early exit is the point. For a function whose single call already
/// costs more than the target, `n = 1` is forced: there is no growth loop to
/// run, no median to take, no ladder to plan. Doing the careful thing anyway
/// cost five calls - fifty milliseconds for a ten-millisecond function, most
/// of a six-sample budget - to learn what the first call had already said.
///
/// **Probes are samples.** Every probe taken at the batch size that ends up
/// being used measures exactly what the measurement loop will measure, so it
/// is returned rather than discarded. For a forced-`n=1` workload that drops
/// calibration's true cost to a single warm-up call, which is thrown away
/// only because it is cold.
fn calibrate_fast(w: &Workload, target: f64) -> (usize, f64, bool, Vec<f64>) {
    // One untimed-but-timed warm-up. It has to happen - it is what stops a
    // first-touch page fault from being mistaken for the cost of the work -
    // but there is no reason not to read the clock while it runs.
    let first = w.time_batch(1)();

    // A tenfold margin, because the largest first-touch inflation seen here
    // was sevenfold (`copy_64mb`, 47 ms against a true 6.4). Above that no
    // plausible cold-start error changes the verdict, so one call is enough.
    if first >= target * 10.0 {
        let one = w.time_batch(1)();
        return (1, one, true, vec![one]);
    }

    let mut n = 1usize;
    let mut probes: Vec<f64> = Vec::new();
    loop {
        let ns = w.time_batch(n)();
        if ns >= target * 0.9 || n >= 1 << 32 {
            // Only probes at the final batch size are measurements.
            probes.clear();
            probes.push(ns);
            return (n, ns / n as f64, n == 1, probes);
        }
        let factor = (target / ns.max(1.0)).clamp(1.5, 50.0);
        n = ((n as f64 * factor) as usize).max(n + 1);
    }
}

/// Measure the efficiency curve and return the sample duration that minimises
/// relative error per unit of machine time.
///
/// Measured rather than modelled. A two-parameter fit of additive against
/// multiplicative noise gets `mem_canary` and `instant_now` right to within a
/// factor of two and `btree_miss` wrong by 400x, because its absolute noise
/// drifts across the range instead of staying flat. The argmin of the curve
/// needs no model to be wrong about and absorbs the tick for free.
///
/// The optimum is a property of the function, not of the round: across 32
/// compositions it moved by at most one rung for five of six workloads. That
/// is what makes it findable here, with the workload running alone.
fn find_optimum(w: &Workload, per_iter: f64, deadline: Instant) -> (f64, Vec<(usize, Vec<f64>)>) {
    const PROBE: [f64; 5] = [0.005, 0.02, 0.08, 0.32, 1.28];
    let mut best = (f64::INFINITY, SAMPLE_NS);
    let mut kept: Vec<(usize, Vec<f64>)> = Vec::new();

    // Equal *time* per rung, not equal count. A fixed sample count spends
    // almost nothing at the cheap rungs and blows the whole calibration
    // budget at the dear ones - with 160 samples each, the top rung of this
    // ladder costs 250 times the bottom one. Worse, it leaves every rung
    // with the same number of samples and therefore the same relative error
    // on its spread estimate, when what the argmin needs is for the
    // *expensive* rungs to be pinned well enough to lose honestly. Splitting
    // the budget evenly by time gave a far steadier optimum: before this,
    // `cpu_canary` picked 100 us on one run and 560 ns on the next, and the
    // bad pick put its estimate out by 18%.
    let start = Instant::now();
    let slice = deadline.saturating_duration_since(start) / PROBE.len() as u32;
    for (i, &m) in PROBE.iter().enumerate() {
        let target = m * SAMPLE_NS;
        let n = ((target / per_iter).round() as usize).max(1);
        let until = start + slice * (i as u32 + 1);
        let mut v = Vec::new();
        // A floor so a spread is estimable at all, and a ceiling so the
        // cheap rungs do not collect a million samples they cannot use.
        while v.len() < 4000 && (v.len() < 30 || Instant::now() < until) {
            v.push(w.time_batch(n)());
            if Instant::now() >= deadline {
                break;
            }
        }
        if v.len() < 16 {
            break;
        }
        let m_ns = estimate::trimmed_mean(&v, 0.10);
        let sd = {
            let mut s = v.clone();
            s.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let k = s.len() / 10;
            let s = &s[k..s.len() - k];
            let mu = estimate::mean(s);
            estimate::variance(s, mu).sqrt()
        };
        if m_ns > 0.0 {
            // Relative error per root-second: rel_sd * sqrt(T).
            let eff = (sd / m_ns) * (m_ns / 1e9).sqrt();
            if eff.is_finite() && eff < best.0 {
                best = (eff, m_ns);
            }
        }
        kept.push((n, v));
    }
    (best.1, kept)
}

/// Write every individual sample time, in the order taken.
///
/// The summary row says what two particular error estimators claimed. That
/// is enough to score those two and nothing else, which is a poor trade when
/// the entire question is *which* estimator tells the truth - a third idea
/// would cost another six hours of measuring rather than a second of
/// arithmetic. This is the same discipline `run` and `compare` were split
/// for, applied to the experiment that most needs it.
///
/// Four bytes a sample, integer nanoseconds. Order within a rung is
/// preserved because that is what any estimator dealing with correlation
/// needs, and the probe count says where calibration ended and measurement
/// began.
fn dump(
    cell: &str,
    budget_s: f64,
    rep: usize,
    ws: &[Workload],
    plans: &[Plan],
    probes: &[Vec<usize>],
    log: &[(u32, u8, u8, f64)],
) {
    use std::fmt::Write as _;
    let Ok(dir) = std::env::var("LAB_PROTO_DUMP") else {
        return;
    };
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let tag = std::env::var("LAB_PROTO_TAG").unwrap_or_default();
    let path = format!("{dir}/{cell}.{budget_s}.{rep}{tag}.bin");

    let mut head = String::from("LABPROTO1\n");
    let _ = writeln!(head, "# cell {cell}\n# budget {budget_s}\n# rep {rep}");
    let mut body: Vec<u8> = Vec::new();
    for (i, w) in ws.iter().enumerate() {
        for (k, (n, v)) in plans[i].rungs.iter().enumerate() {
            let _ = writeln!(
                head,
                "# series {} {k} {n} {} {}",
                w.name,
                probes[i].get(k).copied().unwrap_or(0),
                v.len()
            );
            for &ns in v {
                body.extend_from_slice(
                    &(ns.round().clamp(0.0, u32::MAX as f64) as u32).to_le_bytes(),
                );
            }
        }
    }
    // Ten bytes a sample in execution order: round, workload, rung, ns.
    // Calibration probes are not in here - they were taken with the workload
    // running back to back and alone, which is a different regime from the
    // interleaved rounds, and the `series` lines above say how many there
    // were so they can be found in the grouped block instead.
    let _ = writeln!(
        head,
        "# order {} records of 10 bytes: u32 round, u8 workload, u8 rung, u32 ns",
        log.len()
    );
    head.push_str("DATA\n");
    let mut out = head.into_bytes();
    out.extend_from_slice(&body);
    for (r, w, k, ns) in log {
        out.extend_from_slice(&r.to_le_bytes());
        out.push(*w);
        out.push(*k);
        out.extend_from_slice(&(ns.round().clamp(0.0, u32::MAX as f64) as u32).to_le_bytes());
    }
    let _ = std::fs::write(path, out);
}

/// Run one cell, for one budget, in this process, and print one row per
/// workload.
pub fn run(cell_name: &str, budget_s: f64, rep: usize) {
    let cell = CELLS
        .iter()
        .find(|c| c.name == cell_name)
        .unwrap_or_else(|| panic!("no cell {cell_name:?}"));

    // Two arms, because mixing them answers neither question. A workload
    // whose single call outruns the target is `forced-n1` in every cell, so
    // all four protocols do literally the same thing to it and comparing
    // them on it is vacuous - while its cost still dominates every round and
    // starves the fast workloads of the samples the comparison needs.
    let names: Vec<&str> = std::env::var("LAB_PROTO_SET")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .map(|s| {
            s.split(',')
                .map(|x| Box::leak(x.trim().to_string().into_boxed_str()) as &str)
                .collect()
        })
        .unwrap_or_else(|| vec!["instant_now", "btree_miss", "cpu_canary"]);
    // With `LAB_PROTO_DROP`, each repeat measures a different subset - one
    // member picked out and left behind. This is the harder and more honest
    // arm: nobody re-runs an identical suite, and the company a benchmark
    // keeps changes between one invocation and the next. Composition shifts
    // the naive estimate by design, so a protocol that is immune to it should
    // show the same spread here as with a fixed round, and one that is not
    // should fall apart. Seeded by the repeat, so the draw is reproducible.
    let mut names = names;
    if std::env::var("LAB_PROTO_DROP").is_ok() && names.len() > 2 {
        let mut h = 0x9E3779B97F4A7C15u64 ^ (rep as u64).wrapping_mul(0xD1B54A32D192ED03);
        h = crate::step(h);
        let drop = (h >> 33) as usize % names.len();
        names.remove(drop);
    }
    let ws: Vec<Workload> = names.iter().map(|n| crate::workloads::named(n)).collect();

    // Budget starts here: construction is a fixture, not a protocol choice.
    let start = Instant::now();
    let budget = Duration::from_secs_f64(budget_s);
    let calib_deadline = start + budget.mul_f64(CALIB_FRACTION);

    let mut plans: Vec<Plan> = Vec::new();
    for w in &ws {
        let (n1, per_iter, forced, probes) = calibrate_fast(w, SAMPLE_NS);
        if forced {
            // No sample-size decision exists. Do not calibrate further, do
            // not ladder, do not subtract a fixed cost that is a rounding
            // error against a millisecond call.
            plans.push(Plan {
                rungs: vec![(1, probes)],
                forced: true,
                chosen_ns: f64::NAN,
            });
            continue;
        }
        let (rungs, chosen) = if cell.name == "G-algo" {
            // Two rungs, by default, because at equal machine time they are
            // the better trade. A pair at (T, 8T) costs 4.5T a round - the
            // same as one rung at 4.5T - and buys away the *entire* fixed-cost
            // bias for about 15% more statistical noise. That is worth it
            // whenever the bias exceeds a sixth of the noise floor, which is
            // nearly always: the noise shrinks as you sample and the bias
            // does not, and no error bar ever reveals the bias.
            //
            // An earlier version of this preferred a single long rung, sized
            // so that a/T was a hundredth of the requested accuracy. That
            // margin was arbitrary and far too strict - it forced samples of
            // 0.2 to 1.4 ms where this pair does better on both axes - and it
            // got the ordering backwards: diluting a systematic is strictly
            // worse than removing it when both cost the same.
            //
            // 0.25 and 2.0 samples average 1.125, so a round costs about what
            // a single 100 us sample costs today. Same budget, no bias.
            let lo = ((0.25 * SAMPLE_NS / per_iter).round() as usize).max(1);
            let hi = ((2.0 * SAMPLE_NS / per_iter).round() as usize).max(lo + 1);
            (vec![(lo, Vec::new()), (hi, Vec::new())], f64::NAN)
        } else if cell.rungs.is_empty() {
            let (t, kept) = find_optimum(w, per_iter, calib_deadline);
            // Bracket the optimum: an eightfold lever centred near it.
            let lo = ((0.5 * t / per_iter).round() as usize).max(1);
            let hi = ((4.0 * t / per_iter).round() as usize).max(lo + 1);
            let reuse = |n: usize| {
                kept.iter()
                    .find(|(k, _)| *k == n)
                    .map(|(_, v)| v.clone())
                    .unwrap_or_default()
            };
            (vec![(lo, reuse(lo)), (hi, reuse(hi))], t)
        } else {
            let mut v: Vec<(usize, Vec<f64>)> = cell
                .rungs
                .iter()
                .map(|m| {
                    let n = ((m * SAMPLE_NS / per_iter).round() as usize).max(1);
                    (n, Vec::new())
                })
                .collect();
            // Reuse the growth loop's last probe where it lands on a rung.
            if let Some(slot) = v.iter_mut().find(|(n, _)| *n == n1) {
                slot.1.extend(probes.iter().copied());
            }
            (v, f64::NAN)
        };
        plans.push(Plan {
            rungs,
            forced: false,
            chosen_ns: chosen,
        });
    }
    let calib_s = start.elapsed().as_secs_f64();
    // How many of each rung's samples came from calibration, so a later
    // reader can tell a probe from a measurement.
    let probe_counts: Vec<Vec<usize>> = plans
        .iter()
        .map(|p| p.rungs.iter().map(|(_, v)| v.len()).collect())
        .collect();

    // Measure until the budget runs out: one rung per workload per round,
    // drawn at random, in a random order - the same discipline as the sweeps.
    let mut perm = 0x853C49E6748FEA9Bu64 ^ ((rep as u64) << 32);
    let mut rounds = 0usize;
    // (round, workload, rung, ns) in the order actually executed.
    let mut log: Vec<(u32, u8, u8, f64)> = Vec::new();
    while start.elapsed() < budget {
        let mut order: Vec<usize> = (0..ws.len()).collect();
        for i in (1..order.len()).rev() {
            perm = crate::step(perm);
            order.swap(i, (perm >> 33) as usize % (i + 1));
        }
        for &i in &order {
            perm = crate::step(perm);
            let k = (perm >> 33) as usize % plans[i].rungs.len();
            let n = plans[i].rungs[k].0;
            let ns = ws[i].time_batch(n)();
            plans[i].rungs[k].1.push(ns);
            // Also in execution order, so the round structure survives into
            // the recording. Grouping by workload was enough for the error
            // estimators, and no use at all to anyone later asking whether a
            // payload's sample moves with its neighbours' in the same round,
            // or whether position within a round matters - both of which
            // this project has already found to be real effects. The order
            // is data, and storing it sorted throws it away.
            log.push((rounds as u32, i as u8, k as u8, ns));
        }
        rounds += 1;
        if start.elapsed() >= budget {
            break;
        }
        // Early stopping, as a real benchmark does it: once every workload's
        // reported relative standard error is under target, there is nothing
        // more to learn - or so the error bar says.
        //
        // This applies to every algorithm now, because it is how the crate
        // actually behaves: a budget is a cap that is rarely reached, not a
        // quantity to spend. With stopping universal the comparison is
        // "given the same accuracy request, who answers sooner", which is
        // the question - an algorithm that is more accurate than asked has
        // bought nothing and paid time for it.
        if std::env::var("LAB_STOP_ALL").is_ok() && rounds >= 3 {
            let done = plans.iter().all(|p| {
                let (_, v) = &p.rungs[0];
                let s = stat(v);
                s.mean > 0.0 && s.se_naive / s.mean < se_target()
            });
            if done {
                break;
            }
        }
    }
    let total_s = start.elapsed().as_secs_f64();
    dump(cell.name, budget_s, rep, &ws, &plans, &probe_counts, &log);

    for (i, w) in ws.iter().enumerate() {
        let p = &plans[i];
        let got: Vec<usize> = p.rungs.iter().map(|(_, v)| v.len()).collect();

        // The estimate, what it claims to know about itself, and what the two
        // halves of its own samples say. The halves are the control that
        // separates the two floors: if half-to-half disagreement is as big as
        // run-to-run disagreement, the trouble is inside the run and a better
        // within-run estimator can catch it; if it is far smaller, the
        // trouble is between processes and no within-run estimator ever can.
        let est_of = |vs: &dyn Fn(&(usize, Vec<f64>)) -> Vec<f64>| -> (f64, f64, f64) {
            let parts: Vec<(usize, Stat)> = p.rungs.iter().map(|r| (r.0, stat(&vs(r)))).collect();
            if p.forced || parts.len() < 2 || cell.est == Est::Naive {
                let (n, s) = &parts[0];
                (
                    s.mean / *n as f64,
                    s.se_naive / *n as f64,
                    s.se_batch / *n as f64,
                )
            } else {
                let (lo_n, lo) = &parts[0];
                let (hi_n, hi) = &parts[parts.len() - 1];
                let span = *hi_n as f64 - *lo_n as f64;
                // Independent rungs, so the errors add in quadrature.
                (
                    (hi.mean - lo.mean) / span,
                    (hi.se_naive * hi.se_naive + lo.se_naive * lo.se_naive).sqrt() / span,
                    (hi.se_batch * hi.se_batch + lo.se_batch * lo.se_batch).sqrt() / span,
                )
            }
        };
        let all = |r: &(usize, Vec<f64>)| r.1.clone();
        let first = |r: &(usize, Vec<f64>)| r.1[..r.1.len() / 2].to_vec();
        let second = |r: &(usize, Vec<f64>)| r.1[r.1.len() / 2..].to_vec();
        let (_, se_naive, se_batch) = est_of(&all);
        let (h1, _, _) = est_of(&first);
        let (h2, _, _) = est_of(&second);
        // A forced-n=1 workload is always estimated naively: there is no
        // second rung, and nothing worth subtracting.
        let est = if p.forced || p.rungs.len() < 2 {
            let (n, v) = &p.rungs[0];
            if v.is_empty() {
                f64::NAN
            } else {
                estimate::trimmed_mean(v, 0.10) / *n as f64
            }
        } else {
            match cell.est {
                Est::Naive => {
                    let (n, v) = &p.rungs[0];
                    if v.is_empty() {
                        f64::NAN
                    } else {
                        estimate::trimmed_mean(v, 0.10) / *n as f64
                    }
                }
                Est::Subtract => {
                    let (lo_n, lo_v) = &p.rungs[0];
                    let (hi_n, hi_v) = &p.rungs[p.rungs.len() - 1];
                    if lo_v.len() < 4 || hi_v.len() < 4 {
                        f64::NAN
                    } else {
                        (estimate::trimmed_mean(hi_v, 0.10) - estimate::trimmed_mean(lo_v, 0.10))
                            / (*hi_n as f64 - *lo_n as f64)
                    }
                }
            }
        };
        println!(
            "{},{budget_s},{rep},{},{est:.6},{se_naive:.6},{se_batch:.6},{h1:.6},{h2:.6},{},{},{:.6},{:.6},{}",
            cell.name,
            w.name,
            got.iter().sum::<usize>(),
            rounds,
            calib_s,
            total_s,
            if p.forced {
                "forced-n1".to_string()
            } else if p.chosen_ns.is_finite() {
                // What the mini-ladder picked, so the choice is auditable
                // rather than buried inside the estimate.
                format!("chose-{:.0}ns", p.chosen_ns)
            } else {
                "fixed".to_string()
            }
        );
    }
}
