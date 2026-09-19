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
    /// Wall-clock cost of taking one sample at this rung that is *not* the
    /// batch itself: the harness loop, and whatever the workload does to
    /// prepare its inputs.
    ///
    /// Charged for, because a policy that takes sixty million samples at a
    /// 150ns rung pays this sixty million times, and a simulation that only
    /// counted batch time would price tiny rungs at nearly free.
    ///
    /// Measured from the recording rather than assumed, because it is not
    /// one number. Across five of seven workloads it is flat at ~370ns from
    /// n=1 to n=524288 - genuinely a per-measurement constant - but
    /// `f64_sin` prepares an input per iteration at 12.5ns each and
    /// `parse_u64` at ~90ns each, so for those it grows with the batch. A
    /// hard-coded constant would be right for most and wrong by a couple of
    /// hundredfold for `parse_u64` at the top of its ladder, which is more
    /// than enough to invert a ranking.
    pub overhead_ns: f64,
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
    let over = overheads(r);
    let mut by_base: std::collections::BTreeMap<String, Vec<Rung>> = Default::default();
    for name in &r.names {
        let base = name.split('@').next().unwrap_or(name).to_string();
        let n = *r.iters.get(name).unwrap_or(&1);
        let batch_ns: Vec<f64> = r.get(name).iter().map(|x| x * n as f64).collect();
        if batch_ns.is_empty() {
            continue;
        }
        let overhead_ns = over.get(name).copied().unwrap_or(DEFAULT_OVERHEAD_NS);
        by_base.entry(base).or_default().push(Rung { n, batch_ns, overhead_ns });
    }
    by_base
        .into_iter()
        .map(|(workload, mut rungs)| {
            rungs.sort_by_key(|x| x.n);
            Tape { workload, rungs }
        })
        .collect()
}

/// Used only when a recording carries no usable timestamps; see
/// [`Rung::overhead_ns`] for where the number comes from.
const DEFAULT_OVERHEAD_NS: f64 = 370.0;

/// Per-rung overhead, read out of the gaps between consecutive samples.
///
/// The recording stores when each sample started and how long its batch
/// ran, so whatever sits between the end of one batch and the start of the
/// next is everything the harness did that was not the measurement. That is
/// attributed to the sample being *set up*, not the one just finished,
/// because preparing a batch happens before its clock starts.
///
/// Median rather than mean: these gaps carry the occasional scheduler
/// excursion, and a mean would fold a rare millisecond into a number that
/// then gets multiplied by every sample a policy takes.
fn overheads(r: &Run) -> std::collections::HashMap<String, f64> {
    let mut gaps: std::collections::HashMap<String, Vec<f64>> = Default::default();
    for w in r.samples.windows(2) {
        let n0 = *r.iters.get(&w[0].workload).unwrap_or(&1) as f64;
        let gap = (w[1].t_ns as f64 - w[0].t_ns as f64) - w[0].ns * n0;
        if gap.is_finite() && gap >= 0.0 {
            gaps.entry(w[1].workload.clone()).or_default().push(gap);
        }
    }
    gaps.into_iter()
        .filter(|(_, v)| v.len() >= 20)
        .map(|(k, mut v)| {
            v.sort_by(|a, b| a.partial_cmp(b).unwrap());
            (k, v[v.len() / 2])
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
        self.spent_ns += ns + r.overhead_ns;
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

/// A rung choice: stand on one rung, or subtract a low from a high.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Choice {
    One(usize),
    Pair(usize, usize),
}

/// Rungs an algorithm could sensibly stand on.
///
/// The window has a floor because the fixed cost per measurement is about
/// 160ns, so a 100ns batch is more overhead than measurement, and a ceiling
/// because past about a millisecond every batch contains a scheduler tick -
/// contamination stops being a rare catastrophe and becomes a near-constant
/// tax, which is a different regime and a worse one to measure in.
///
/// `n == 1` is kept however long it takes: a workload slower than the
/// ceiling cannot be measured in less than one iteration, so the window
/// would otherwise be empty for it.
///
/// `n == 2` is kept up to a second, so that a moderately slow workload still
/// has *some* pair to subtract. Without it the window admits exactly one
/// rung for anything past the ceiling, and subtraction - the thing that
/// removes the fixed cost per measurement - becomes unavailable precisely
/// where the ladder is shortest. Two samples of 4ms is a cheap way to keep
/// the option; two samples of a minute is not, hence the ceiling on it.
pub fn reasonable(tape: &Tape) -> Vec<usize> {
    tape.rungs
        .iter()
        .enumerate()
        .filter(|(_, r)| {
            let d = r.batch_ns.iter().sum::<f64>() / r.batch_ns.len() as f64 + r.overhead_ns;
            (d >= MIN_RUNG_NS && d <= MAX_RUNG_NS)
                || (r.n == 1 && d > MAX_RUNG_NS)
                || (r.n == 2 && d <= SLOW_PAIR_NS)
        })
        .map(|(i, _)| i)
        .collect()
}

/// Shortest batch worth standing on: below this the per-measurement cost is
/// most of what is being timed.
const MIN_RUNG_NS: f64 = 100.0;
/// Longest: past here every batch carries a scheduler tick.
const MAX_RUNG_NS: f64 = 2e6;
/// How long a second rung may run for a workload too slow for the window,
/// so that subtraction stays possible there.
const SLOW_PAIR_NS: f64 = 1e9;

/// Measure at a fixed rung choice, charging nothing for calibration.
///
/// This is the oracle: how fast a choice *could* be if it already knew where
/// to stand. Separating it from the calibrated path is the whole point -
/// otherwise a policy that picks well and a policy that calibrates cheaply
/// are scored as one number, and there is no way to tell which half is doing
/// the work.
pub fn measure(tape: &Tape, c: Choice, target: f64, budget_s: f64, start: f64) -> Outcome {
    let mut p = Player::new(tape, start);
    run_choice(&mut p, c, target, budget_s)
}

fn run_choice(p: &mut Player, c: Choice, target: f64, budget_s: f64) -> Outcome {
    let budget_ns = budget_s * 1e9;
    let mut vals: Vec<f64> = Vec::new();
    let mut est = f64::NAN;
    let mut se = f64::INFINITY;
    let mut capped = false;
    // Next sample count at which to test the stopping rule.
    //
    // Testing it on every draw makes the simulation quadratic: `batch_se`
    // is linear in the samples so far, and a tight target at a small rung
    // runs for tens of thousands of draws. Checking on a geometric schedule
    // costs a constant factor of the draws instead, and is what a real
    // implementation would do anyway - recomputing a standard error to
    // decide whether to take one more 150ns sample is not free either.
    let mut check = MIN_SAMPLES;
    loop {
        match c {
            Choice::One(k) => {
                let ns = p.draw(k);
                vals.push(ns / p.n(k));
            }
            Choice::Pair(a, b) => {
                // Low then high, paired and subtracted, which removes
                // whatever a measurement costs regardless of its size.
                let lo = p.draw(a);
                let hi = p.draw(b);
                vals.push((hi - lo) / (p.n(b) - p.n(a)));
            }
        }
        if vals.len() >= check {
            check = ((vals.len() as f64 * 1.3) as usize).max(vals.len() + 1);
            est = vals.iter().sum::<f64>() / vals.len() as f64;
            se = batch_se(&vals);
            if est > 0.0 && se / est <= target {
                break;
            }
        }
        if p.spent_ns >= budget_ns {
            capped = true;
            break;
        }
        // Off the end of the recording: from here on it would be re-reading
        // noise it has already seen, which is not another trial.
        if p.wrapped {
            break;
        }
    }
    let used = match c {
        Choice::One(k) => vec![p.n(k) as usize],
        Choice::Pair(a, b) => vec![p.n(a) as usize, p.n(b) as usize],
    };
    Outcome { est, se, seconds: p.spent_ns * 1e-9, capped, wrapped: p.wrapped, used }
}

/// Replay a calibration, then let it choose its own rungs and measure.
///
/// The counterpart to [`measure`]: this one pays for calibration and has to
/// find the rung without being told, which is the situation any real
/// algorithm is in.
pub fn calibrated(tape: &Tape, pol: &Policy, start: f64) -> Outcome {
    let sample_ns = crate::SAMPLE.as_secs_f64() * 1e9;
    let mut p = Player::new(tape, start);
    let (cal_n, per_iter) = calibrate(&mut p, sample_ns);
    let ok = reasonable(tape);
    let pick = |want: f64| -> usize {
        let k = tape.nearest(want);
        // Never stand outside the window, however calibration came out.
        *ok.iter().min_by_key(|&&i| (i as i64 - k as i64).abs()).unwrap_or(&k)
    };
    let c = if pol.rungs.is_empty() {
        Choice::One(pick(cal_n))
    } else if pol.rungs.len() == 1 {
        Choice::One(pick(pol.rungs[0] * sample_ns / per_iter.max(1e-9)))
    } else {
        let a = pick(pol.rungs[0] * sample_ns / per_iter.max(1e-9));
        let b = pick(pol.rungs[1] * sample_ns / per_iter.max(1e-9));
        if a == b { Choice::One(a) } else { Choice::Pair(a.min(b), a.max(b)) }
    };
    run_choice(&mut p, c, pol.target, pol.budget_s)
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

/// Starting points per cell for the oracle sweep, which runs a hundred-odd
/// choices per workload per goal, and for the calibrated policies, of which
/// there are three.
const ORACLE_TRIALS: usize = 60;
const CAL_TRIALS: usize = 200;

/// An honest 1-sigma bar contains the truth about this often.
const EXPECT_COVERAGE: f64 = 0.68;
const COVERAGE_FLOOR: f64 = 0.50;
/// An error this many times the goal is not a wide tail, it is a wrong answer.
const BLOWUP: f64 = 4.0;
const BLOWUP_MAX: f64 = 0.01;
/// Half, because the goal is a standard error and not a guarantee.
const PASS_WITHIN: f64 = 0.50;
/// Above this fraction of trials running off the end of the recording, a
/// cell is reporting the same noise repeatedly rather than statistics.
const WRAP_MAX: f64 = 0.10;

fn pct(x: f64) -> String {
    format!("{:.0}%", 100.0 * x)
}

fn fmt_time(s: f64, capped: bool) -> String {
    // The cap is not a failure. A run that spent its whole budget gave a
    // fine answer and merely took a while, so it reads as a lower bound on
    // time rather than as a wrong result.
    let t = if s >= 1.0 {
        format!("{s:.2}s")
    } else if s >= 1e-3 {
        format!("{:.1}ms", s * 1e3)
    } else {
        format!("{:.0}us", s * 1e6)
    };
    if capped {
        format!("> {t}")
    } else {
        t
    }
}

struct Score {
    label: String,
    time: f64,
    capped: bool,
    within: f64,
    cover: f64,
    blow: f64,
    thin: bool,
    pass: bool,
}

fn score(label: String, outs: &[Outcome], truth_ns: f64, target: f64) -> Score {
    let n = outs.len() as f64;
    let good: Vec<&Outcome> = outs.iter().filter(|o| o.est.is_finite() && o.est > 0.0).collect();
    let thin = good.len() as f64 / n < 0.9
        || good.iter().filter(|o| o.wrapped).count() as f64 / n > WRAP_MAX;
    if good.is_empty() {
        return Score { label, time: f64::INFINITY, capped: false, within: 0.0, cover: 0.0,
                       blow: 1.0, thin: true, pass: false };
    }
    let rel = |o: &Outcome| (o.est - truth_ns).abs() / truth_ns;
    let within = good.iter().filter(|o| rel(o) <= target).count() as f64 / n;
    let cover = good.iter().filter(|o| (o.est - truth_ns).abs() <= o.se).count() as f64 / n;
    let blow = good.iter().filter(|o| rel(o) > BLOWUP * target).count() as f64 / n;
    let capped = good.iter().filter(|o| o.capped).count() as f64 / n > 0.5;
    let mut times: Vec<f64> = good.iter().map(|o| o.seconds).collect();
    times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let time = times[times.len() / 2];
    let pass = !thin && within >= PASS_WITHIN && cover >= COVERAGE_FLOOR && blow <= BLOWUP_MAX;
    Score { label, time, capped, within, cover, blow, thin, pass }
}

fn line(s: &Score) -> String {
    format!(
        "{:>22} {:>9} {:>8} {:>8} {:>8}",
        s.label,
        fmt_time(s.time, s.capped),
        pct(s.within),
        pct(s.cover),
        pct(s.blow)
    )
}

/// The calibrated algorithms: these pay for calibration and have to find
/// their own rungs, which is the situation a real one is in.
fn cal_policies(target: f64) -> Vec<Policy> {
    vec![
        Policy { name: "cal one-rung", rungs: &[1.0], target, budget_s: 10.0 },
        Policy { name: "cal auto", rungs: &[], target, budget_s: 10.0 },
        Policy { name: "cal two-rung", rungs: &[0.25, 2.0], target, budget_s: 10.0 },
    ]
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
        "Rung choice, calibration and stopping replayed from recordings.\n\
         A rung is usable if one sample costs between {:.0}ns and {:.0}ms, or if n=1.\n\n\
         oracle     = told which rung to stand on, and charged nothing for knowing\n\
         calibrated = had to find the rung, and charged for the probes\n\
         within     = landed inside the accuracy goal (want >={})\n\
         cover      = landed inside the bar the run itself claimed (want ~{})\n\
         blow       = off by more than {BLOWUP}x the goal (want <={})\n",
        MIN_RUNG_NS, MAX_RUNG_NS / 1e6, pct(PASS_WITHIN), pct(EXPECT_COVERAGE), pct(BLOWUP_MAX),
    );

    for tape in &tapes {
        let (truth_ns, truth_se) = truth(tape);
        if !(truth_ns.is_finite() && truth_ns > 0.0) {
            continue;
        }
        let ok = reasonable(tape);
        if ok.is_empty() {
            continue;
        }
        println!(
            "===== {} =====  truth {:.4} ns/iter +- {:.2}%",
            tape.workload, truth_ns, 100.0 * truth_se / truth_ns
        );
        println!(
            "  usable rungs: n={}..{} ({} of {}), overhead {:.0}ns/sample",
            tape.rungs[ok[0]].n,
            tape.rungs[*ok.last().unwrap()].n,
            ok.len(),
            tape.rungs.len(),
            tape.rungs[ok[0]].overhead_ns,
        );

        // Every usable rung, and every usable pair.
        let mut choices: Vec<Choice> = ok.iter().map(|&k| Choice::One(k)).collect();
        for (x, &a) in ok.iter().enumerate() {
            for &b in &ok[x + 1..] {
                choices.push(Choice::Pair(a, b));
            }
        }

        for &target in &TARGETS {
            println!("  goal {:.1}%", 100.0 * target);
            println!(
                "    {:>22} {:>9} {:>8} {:>8} {:>8}",
                "choice", "time", "within", "cover", "blow"
            );
            let mut scores: Vec<Score> = choices
                .iter()
                .map(|&c| {
                    let label = match c {
                        Choice::One(k) => format!("n={}", tape.rungs[k].n),
                        Choice::Pair(a, b) => {
                            format!("n={},{}", tape.rungs[a].n, tape.rungs[b].n)
                        }
                    };
                    let outs: Vec<Outcome> = (0..ORACLE_TRIALS)
                        .map(|i| {
                            measure(tape, c, target, 10.0, i as f64 / ORACLE_TRIALS as f64)
                        })
                        .collect();
                    score(label, &outs, truth_ns, target)
                })
                .collect();
            scores.retain(|s| !s.thin);
            let passing: Vec<&Score> = {
                let mut v: Vec<&Score> = scores.iter().filter(|s| s.pass).collect();
                v.sort_by(|a, b| a.time.partial_cmp(&b.time).unwrap());
                v
            };
            if passing.is_empty() {
                // Nothing to rank, so nothing is printed in rank order. The
                // useful fact is which choice came closest and how far short
                // it fell, not a list of failures sorted by a time that was
                // never earned.
                let best = scores.iter().max_by(|a, b| {
                    a.within.partial_cmp(&b.within).unwrap()
                });
                match best {
                    Some(b) => println!(
                        "      no usable choice; closest was {} at within {}",
                        b.label, pct(b.within)
                    ),
                    None => println!("      recording too thin at this goal"),
                }
            } else {
                for s in passing.iter().take(3) {
                    println!("    {}  oracle", line(s));
                }
            }
            for pol in cal_policies(target) {
                let outs: Vec<Outcome> = (0..CAL_TRIALS)
                    .map(|i| calibrated(tape, &pol, i as f64 / CAL_TRIALS as f64))
                    .collect();
                let s = score(pol.name.to_string(), &outs, truth_ns, target);
                if s.thin {
                    continue;
                }
                println!("    {}  {}", line(&s), if s.pass { "pass" } else { "FAIL" });
            }
        }
        println!();
    }
}
