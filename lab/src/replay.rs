//! Calibration at analysis time.
//!
//! Every measurement run this lab did before now baked three decisions into
//! the machine time: how to calibrate, which rungs to stand on, and when to
//! stop. Changing any one of them meant measuring again, so each variant was
//! scored against *different* noise, and a day of quiet machine bought one
//! comparison.
//!
//! The way out is to record a ladder wide enough that every rung an
//! algorithm might plausibly choose is already on disk, and then make
//! calibration, rung choice and stopping into arithmetic over that
//! recording. One night of collection then scores any number of algorithms
//! against *byte-identical* noise, which is the only way a difference
//! between two of them is attributable to the algorithm.
//!
//! **What this cannot do.** A replayed sample at rung k was taken in a round
//! that also contained the other rungs. A real run that used only rung k
//! would establish a different regime - different cache occupancy, different
//! round period - and we have measured composition shifting costs by
//! percent-scale amounts. So simulated timings inherit the wide-ladder
//! round, not the round the algorithm would have created. That is a bias of
//! the same order as the effects under study, and it is why `validate`
//! exists: the simulator's answer for an algorithm we *also* measured
//! directly has to match, or the simulation is not measuring that algorithm.

use crate::estimate::Run;

/// One workload's recorded ladder: every rung, with its batch times in the
/// order they were taken.
pub struct Tape {
    pub workload: String,
    /// Sorted by iteration count, ascending.
    pub rungs: Vec<Rung>,
}

pub struct Rung {
    pub n: usize,
    /// Whole-batch times in ns, in recorded order. Batch times rather than
    /// per-iteration, because a fixed cost per measurement only stands still
    /// in this currency - dividing by `n` smears it across the iterations
    /// and disguises it as a per-iteration cost that shrinks with batch size.
    pub batch_ns: Vec<f64>,
}

impl Tape {
    /// The rung whose count is closest to `want`, measured in log space
    /// because the ladder is geometric and being a factor of two high is the
    /// same mistake as being a factor of two low.
    pub fn nearest(&self, want: f64) -> usize {
        let mut best = 0;
        let mut best_d = f64::INFINITY;
        for (i, r) in self.rungs.iter().enumerate() {
            let d = (r.n as f64 / want).ln().abs();
            if d < best_d {
                best_d = d;
                best = i;
            }
        }
        best
    }
}

/// Split a recording into one tape per workload.
///
/// Rung names are `base` and `base@k`, so the base name is everything before
/// the `@`. Counts come from the recording's own header rather than being
/// re-derived from the ladder: re-deriving was the bug that halved every
/// count when the ladder stopped starting at one.
pub fn tapes(r: &Run) -> Vec<Tape> {
    let mut by_base: std::collections::BTreeMap<String, Vec<Rung>> = Default::default();
    for name in &r.names {
        let base = name.split('@').next().unwrap_or(name).to_string();
        let n = *r.iters.get(name).unwrap_or(&1);
        let batch_ns: Vec<f64> = r.get(name).iter().map(|x| x * n as f64).collect();
        if batch_ns.is_empty() {
            continue;
        }
        by_base.entry(base).or_default().push(Rung { n, batch_ns });
    }
    by_base
        .into_iter()
        .map(|(workload, mut rungs)| {
            rungs.sort_by_key(|x| x.n);
            Tape { workload, rungs }
        })
        .collect()
}

/// A cursor over one tape, handing out recorded samples and keeping the bill.
///
/// Each rung has its own position, so drawing from a cheap rung does not
/// consume the expensive one. Positions wrap, which lets a long simulation
/// reuse a short recording; a trial that wraps is reusing noise it has
/// already seen, so `wrapped` says when that happened rather than leaving it
/// to be discovered as suspiciously good agreement between trials.
pub struct Player<'a> {
    tape: &'a Tape,
    pos: Vec<usize>,
    pub spent_ns: f64,
    pub draws: usize,
    pub wrapped: bool,
}

impl<'a> Player<'a> {
    /// `start` in [0,1) picks where in the recording this trial begins, so
    /// one recording yields many trials that see different noise.
    pub fn new(tape: &'a Tape, start: f64) -> Player<'a> {
        let pos = tape
            .rungs
            .iter()
            .map(|r| ((r.batch_ns.len() as f64 * start) as usize) % r.batch_ns.len())
            .collect();
        Player {
            tape,
            pos,
            spent_ns: 0.0,
            draws: 0,
            wrapped: false,
        }
    }

    /// Take the next recorded batch time at rung `k`, and charge for it.
    pub fn draw(&mut self, k: usize) -> f64 {
        let r = &self.tape.rungs[k];
        let ns = r.batch_ns[self.pos[k]];
        self.pos[k] += 1;
        if self.pos[k] >= r.batch_ns.len() {
            self.pos[k] = 0;
            self.wrapped = true;
        }
        self.spent_ns += ns;
        self.draws += 1;
        ns
    }

    pub fn n(&self, k: usize) -> f64 {
        self.tape.rungs[k].n as f64
    }
}

/// What an algorithm is, as far as replay is concerned.
pub struct Policy {
    pub name: &'static str,
    /// Rung durations as multiples of `SAMPLE`. Empty means "whatever
    /// calibration lands on", which is the auto case.
    pub rungs: &'static [f64],
    /// Relative standard error to stop at.
    pub target: f64,
    pub budget_s: f64,
}

pub struct Outcome {
    /// ns per iteration.
    pub est: f64,
    /// Absolute standard error on `est`, as the algorithm itself would
    /// report it - not as we know it to be. Judging an algorithm against a
    /// bar it did not claim tells you nothing about the algorithm.
    pub se: f64,
    pub seconds: f64,
    /// Stopped because the budget ran out, not because the target was met.
    /// A distinct outcome from being wrong: the answer is fine, it is just
    /// slow, and it gets reported as `> budget` rather than as a failure.
    pub capped: bool,
    pub wrapped: bool,
    /// Iteration counts actually used.
    pub used: Vec<usize>,
}

/// Fewest samples before a standard error means anything.
const MIN_SAMPLES: usize = 5;

/// Standard error of a mean via batch means.
///
/// `sd/sqrt(n)` assumes the samples are independent, and consecutive timings
/// are not - they share a clock frequency, a cache state, a scheduler phase.
/// Cutting the sequence into contiguous blocks *in order*, averaging each and
/// taking the spread of the block means divides out whatever is correlated
/// within a block, so the number degrades gracefully as autocorrelation grows
/// instead of being confidently too small.
pub fn batch_se(v: &[f64]) -> f64 {
    if v.len() < MIN_SAMPLES {
        return f64::INFINITY;
    }
    let b = (v.len() / 4).clamp(4, 20);
    let per = v.len() / b;
    if per == 0 {
        return f64::INFINITY;
    }
    let means: Vec<f64> = (0..b)
        .map(|i| {
            let s = &v[i * per..(i + 1) * per];
            s.iter().sum::<f64>() / s.len() as f64
        })
        .collect();
    let m = means.iter().sum::<f64>() / b as f64;
    let var = means.iter().map(|x| (x - m) * (x - m)).sum::<f64>() / (b as f64 - 1.0);
    (var / b as f64).sqrt()
}

/// Replay the growth loop in `calibrate`, on the grid of recorded rungs.
///
/// The real loop grows by `clamp(target/ns, 1.5, 50)` rather than doubling,
/// so it asks for counts that are not on the ladder. Replay walks to the
/// nearest recorded rung instead of inventing a count - the measurement it
/// would need does not exist on disk, and interpolating one would be making
/// up data at exactly the point the experiment is about.
///
/// Returns the chosen count and the per-iteration estimate, and leaves what
/// it spent on the player's bill, because calibration is not free and an
/// algorithm that calibrates elaborately should be charged for it.
fn calibrate(p: &mut Player, target_ns: f64) -> (f64, f64) {
    let mut k = 0usize;
    loop {
        let ns = p.draw(k);
        let n = p.n(k);
        let at_top = k + 1 >= p.tape.rungs.len();
        if ns >= target_ns * 0.9 || at_top {
            // Median of three fresh probes, as the real calibrator does:
            // this number sets the rung durations for everything after it,
            // and one sample is one sample.
            let mut again = [p.draw(k), p.draw(k), p.draw(k)];
            again.sort_by(|a, b| a.partial_cmp(b).unwrap());
            return (n, again[1] / n);
        }
        let factor = (target_ns / ns.max(1.0)).clamp(1.5, 50.0);
        let want = n * factor;
        let next = p.tape.nearest(want).max(k + 1);
        k = next.min(p.tape.rungs.len() - 1);
    }
}

/// Run one algorithm once, from one starting point in the recording.
pub fn simulate(tape: &Tape, pol: &Policy, start: f64) -> Outcome {
    let sample_ns = crate::SAMPLE.as_secs_f64() * 1e9;
    let budget_ns = pol.budget_s * 1e9;
    let mut p = Player::new(tape, start);

    let (cal_n, per_iter) = calibrate(&mut p, sample_ns);

    // Which rungs to stand on. An explicit ladder is in multiples of
    // SAMPLE and has to be turned into counts through the calibrated
    // per-iteration cost; the auto case just keeps what calibration found.
    let ks: Vec<usize> = if pol.rungs.is_empty() {
        vec![tape.nearest(cal_n)]
    } else {
        pol.rungs
            .iter()
            .map(|m| tape.nearest(m * sample_ns / per_iter.max(1e-9)))
            .collect()
    };
    // A ladder that collapses onto one rung is a single-rung measurement,
    // and has to be treated as one: subtracting a rung from itself is a
    // division by zero that would otherwise surface as a plausible-looking
    // infinity.
    let mut ks = ks;
    ks.dedup();

    let mut vals: Vec<f64> = Vec::new();
    let mut est = f64::NAN;
    let mut se = f64::INFINITY;
    let mut capped = false;

    loop {
        if ks.len() >= 2 {
            // Pair a low and a high draw and subtract, which removes
            // whatever the measurement costs regardless of batch size. The
            // two came from different rounds in the recording, but their
            // cursors advance together, so they stay close in wall-clock
            // time - the property the real alternation is buying.
            let lo = p.draw(ks[0]);
            let hi = p.draw(ks[1]);
            let dn = p.n(ks[1]) - p.n(ks[0]);
            vals.push((hi - lo) / dn);
        } else {
            let ns = p.draw(ks[0]);
            vals.push(ns / p.n(ks[0]));
        }

        if vals.len() >= MIN_SAMPLES {
            est = vals.iter().sum::<f64>() / vals.len() as f64;
            se = batch_se(&vals);
            if est > 0.0 && se / est <= pol.target {
                break;
            }
        }
        if p.spent_ns >= budget_ns {
            capped = true;
            break;
        }
    }

    Outcome {
        est,
        se,
        seconds: p.spent_ns * 1e-9,
        capped,
        wrapped: p.wrapped,
        used: ks.iter().map(|&k| tape.rungs[k].n).collect(),
    }
}

/// The best estimate of a workload's true per-iteration cost, from the whole
/// recording at once.
///
/// Two rungs far apart, pooled over every sample there is, so the fixed cost
/// per measurement subtracts out and the remaining noise is divided by tens
/// of thousands. This is not a ground truth in the sense of being correct by
/// construction - nothing here is - but it uses orders of magnitude more
/// machine time than any algorithm under test is allowed, which is the only
/// sense in which one measurement can referee another.
pub fn truth(tape: &Tape) -> (f64, f64) {
    if tape.rungs.len() < 2 {
        let r = &tape.rungs[0];
        let per: Vec<f64> = r.batch_ns.iter().map(|x| x / r.n as f64).collect();
        let m = per.iter().sum::<f64>() / per.len() as f64;
        return (m, batch_se(&per));
    }
    // The widest pair that both have enough samples to be worth pooling.
    let lo = 0;
    let hi = tape.rungs.len() - 1;
    let a = &tape.rungs[lo];
    let b = &tape.rungs[hi];
    let ma = a.batch_ns.iter().sum::<f64>() / a.batch_ns.len() as f64;
    let mb = b.batch_ns.iter().sum::<f64>() / b.batch_ns.len() as f64;
    let dn = (b.n - a.n) as f64;
    let est = (mb - ma) / dn;
    let pa = batch_se(&a.batch_ns);
    let pb = batch_se(&b.batch_ns);
    (est, (pa * pa + pb * pb).sqrt() / dn)
}

/// Accuracy targets to report against, as relative standard error.
const TARGETS: [f64; 3] = [0.02, 0.01, 0.005];

/// The algorithms under test. All of them get every target and the same
/// budget, so the only thing that differs is the algorithm.
fn policies(target: f64) -> Vec<Policy> {
    vec![
        Policy { name: "one-rung", rungs: &[1.0], target, budget_s: 10.0 },
        Policy { name: "auto", rungs: &[], target, budget_s: 10.0 },
        Policy { name: "small", rungs: &[0.05], target, budget_s: 10.0 },
        Policy { name: "sub-wide", rungs: &[1.0, 8.0], target, budget_s: 10.0 },
        Policy { name: "sub-short", rungs: &[0.05, 0.4], target, budget_s: 10.0 },
        Policy { name: "sub-quarter", rungs: &[0.25, 2.0], target, budget_s: 10.0 },
    ]
}

/// How many independent starting points to replay each policy from.
const TRIALS: usize = 200;

/// A run is judged against the bar *it reported*, and an honest 1-sigma bar
/// is inside its own error about this often.
const EXPECT_COVERAGE: f64 = 0.68;
/// Below this, the reported uncertainty is not a standard error.
const COVERAGE_FLOOR: f64 = 0.50;
/// An error this many times the goal is not a wide tail, it is a wrong answer.
const BLOWUP: f64 = 4.0;
const BLOWUP_MAX: f64 = 0.01;
/// Fraction of runs that must land inside the goal. Half, because the goal
/// is a standard error and not a guarantee.
const PASS_WITHIN: f64 = 0.50;
/// Above this fraction of trials reusing the recording, a cell is not
/// reporting statistics, it is reporting the same noise several times.
const WRAP_MAX: f64 = 0.10;

fn pct(x: f64) -> String {
    format!("{:.0}%", 100.0 * x)
}

pub fn report(paths: &[String]) {
    let mut tapes: Vec<Tape> = Vec::new();
    for p in paths {
        let r = Run::load(p);
        tapes.extend(self::tapes(&r));
    }
    if tapes.is_empty() {
        eprintln!("no usable recordings");
        return;
    }

    println!(
        "calibration replayed at analysis time: {TRIALS} trials per cell, \
         from different starting points in the same recording.\n\
         within = fraction landing inside the accuracy goal (want >={})\n\
         cover  = fraction inside the run's own reported error bar (want ~{})\n\
         blow   = fraction off by more than {BLOWUP}x the goal (want <={})\n",
        pct(PASS_WITHIN),
        pct(EXPECT_COVERAGE),
        pct(BLOWUP_MAX),
    );

    for tape in &tapes {
        let (truth_ns, truth_se) = truth(tape);
        if !(truth_ns.is_finite() && truth_ns > 0.0) {
            continue;
        }
        println!(
            "===== {} =====  truth {:.4} ns/iter +- {:.2}%   ({} rungs, n={}..{})",
            tape.workload,
            truth_ns,
            100.0 * truth_se / truth_ns,
            tape.rungs.len(),
            tape.rungs.first().map(|r| r.n).unwrap_or(0),
            tape.rungs.last().map(|r| r.n).unwrap_or(0),
        );
        for &target in &TARGETS {
            println!("  goal {:.1}%", 100.0 * target);
            println!(
                "    {:>12} {:>10} {:>8} {:>8} {:>8} {:>8}  {}",
                "policy", "time", "within", "cover", "blow", "capped", "verdict"
            );
            for pol in policies(target) {
                let outs: Vec<Outcome> = (0..TRIALS)
                    .map(|i| simulate(tape, &pol, i as f64 / TRIALS as f64))
                    .collect();
                let n = outs.len() as f64;
                let good: Vec<&Outcome> = outs.iter().filter(|o| o.est.is_finite()).collect();
                if good.is_empty() {
                    continue;
                }
                let rel = |o: &Outcome| (o.est - truth_ns).abs() / truth_ns;
                let within = good.iter().filter(|o| rel(o) <= target).count() as f64 / n;
                let cover = good
                    .iter()
                    .filter(|o| (o.est - truth_ns).abs() <= o.se)
                    .count() as f64
                    / n;
                let blow = good.iter().filter(|o| rel(o) > BLOWUP * target).count() as f64 / n;
                let capped = good.iter().filter(|o| o.capped).count() as f64 / n;
                // A trial that ran off the end of the recording wrapped
                // around and re-consumed noise it had already seen, which
                // makes independent-looking trials agree for a reason that
                // has nothing to do with the algorithm. Where that is
                // common the cell has no statistics in it, so the numbers
                // are not printed: a row of figures with a caveat beside it
                // is a row of figures I will read and the caveat I will skip.
                let wrapped = good.iter().filter(|o| o.wrapped).count() as f64 / n;
                if wrapped > WRAP_MAX {
                    println!(
                        "    {:>12} {:>10} {:>8} {:>8} {:>8} {:>8}  recording too thin ({} of trials wrapped)",
                        pol.name, "-", "-", "-", "-", "-", pct(wrapped)
                    );
                    continue;
                }
                let mut times: Vec<f64> = good.iter().map(|o| o.seconds).collect();
                times.sort_by(|a, b| a.partial_cmp(b).unwrap());
                let med = times[times.len() / 2];
                // The cap is not a failure. A run that used its whole
                // budget gave a fine answer and merely took a while, so it
                // is reported as a lower bound on time rather than as a
                // wrong result.
                let time = if capped > 0.5 {
                    format!("> {:.2}s", med)
                } else if med >= 1.0 {
                    format!("{:.2}s", med)
                } else {
                    format!("{:.0}ms", med * 1e3)
                };
                let mut bad: Vec<&str> = Vec::new();
                if within < PASS_WITHIN {
                    bad.push("within");
                }
                if cover < COVERAGE_FLOOR {
                    bad.push("cover");
                }
                if blow > BLOWUP_MAX {
                    bad.push("blow");
                }
                let verdict = if bad.is_empty() {
                    "pass".to_string()
                } else {
                    format!("FAIL: {}", bad.join(","))
                };
                println!(
                    "    {:>12} {:>10} {:>8} {:>8} {:>8} {:>8}  {}",
                    pol.name,
                    time,
                    pct(within),
                    pct(cover),
                    pct(blow),
                    pct(capped),
                    verdict
                );
            }
        }
        println!();
    }
}
