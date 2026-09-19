//! Turning a pile of timings into one number.
//!
//! This is the file to edit. Every variant has the same shape - look at a
//! run, name a workload, return an estimate of its per-iteration cost - so
//! adding one is a function plus a line in [`all`]. `lab compare` then
//! scores every variant against every other on the same recorded data.
//!
//! The thing being scored is **reproducibility**: how much the estimate
//! moves between independent runs of the same code. Lower is better, and
//! the number that matters is a fraction of a percent, not a percent.

use crate::timing::Sample;
use std::collections::HashMap;

/// The canaries, by name. If you rename them in `workloads.rs`, rename them
/// here too.
pub const CPU: &str = "cpu_canary";
pub const MEM: &str = "mem_canary";

/// One run's worth of data, already divided by the calibrated iteration
/// count.
///
/// Per *iteration*, not per sample, and that is not a detail. Calibration
/// happens once, at whatever clock speed prevailed at that instant, so
/// iteration counts differ by 10-25% between runs. Comparing per-sample
/// durations across runs inherits all of that and can make two physically
/// identical workloads look uncorrelated.
#[derive(Debug, Clone)]
pub struct Run {
    pub names: Vec<String>,
    /// Every sample, **in the order it was taken**, per iteration.
    ///
    /// Kept as a sequence rather than collapsed into one vector per
    /// workload, because the order is data. The position within a round is
    /// reshuffled deliberately - a memory canary immediately before a
    /// payload leaves that payload's cache cold - so an estimator that wants
    /// to ask about position, or about wall-clock time, still can.
    pub samples: Vec<Sample>,
    /// Per-iteration timings in round order, by workload. A convenience
    /// built from `samples` for the estimators that do not care about order.
    by_name: HashMap<String, Vec<f64>>,
    /// The calibrated batch size each name was measured at. Needed to undo
    /// the per-iteration division for anything that works in batch times.
    pub iters: HashMap<String, usize>,
}

impl Run {
    pub fn load(path: &str) -> Run {
        let rec = crate::timing::read(path);
        let samples: Vec<Sample> = rec
            .samples
            .into_iter()
            .map(|s| {
                let n = *rec.iters.get(&s.workload).unwrap_or(&1) as f64;
                Sample { ns: s.ns / n, ..s }
            })
            .collect();

        // Round order per workload. `samples` is already in execution order,
        // and rounds only ever increase, so a plain scan preserves it.
        let mut by_name: HashMap<String, Vec<f64>> = HashMap::new();
        for s in &samples {
            by_name.entry(s.workload.clone()).or_default().push(s.ns);
        }
        let mut names: Vec<String> = by_name.keys().cloned().collect();
        names.sort();
        Run {
            names,
            samples,
            by_name,
            iters: rec.iters,
        }
    }

    pub fn get(&self, name: &str) -> &[f64] {
        self.by_name.get(name).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// Raw batch times by round, undoing the per-iteration division that
    /// [`Run::load`] applies.
    ///
    /// The per-iteration number is the wrong currency for asking about a
    /// *fixed* cost: dividing by the batch size spreads a constant across the
    /// iterations and makes it look like a per-iteration cost that happens to
    /// shrink as the batch grows. The fixed part only stands still in batch
    /// times.
    pub fn batches(&self, name: &str) -> HashMap<usize, f64> {
        let n = *self.iters.get(name).unwrap_or(&1) as f64;
        self.samples
            .iter()
            .filter(|s| s.workload == name)
            .map(|s| (s.round, s.ns * n))
            .collect()
    }

    pub fn timings(&self, name: &str) -> Vec<(f64, f64)> {
        self.samples
            .iter()
            .filter(|s| s.workload == name)
            .map(|s| (s.t_ns as f64 * 1e-9, s.ns))
            .collect()
    }

    /// The per-round ratio of a workload to a canary. Both were measured
    /// microseconds apart in the same round, so they shared a clock, and
    /// dividing cancels whatever the clock was doing.
    ///
    /// Paired by round explicitly rather than by zipping two sequences: if a
    /// workload is ever absent from a round, zipping would silently divide
    /// by the wrong round's canary from there on.
    pub fn ratio(&self, name: &str, canary: &str) -> Vec<f64> {
        let mut num: HashMap<usize, f64> = HashMap::new();
        let mut den: HashMap<usize, f64> = HashMap::new();
        for s in &self.samples {
            // Two independent tests, not `else if`: a canary divided by
            // itself must come out as a column of exact ones, which is the
            // sanity check that the pairing works at all.
            if s.workload == name {
                num.insert(s.round, s.ns);
            }
            if s.workload == canary {
                den.insert(s.round, s.ns);
            }
        }
        let mut rounds: Vec<usize> = num
            .keys()
            .copied()
            .filter(|r| den.contains_key(r))
            .collect();
        rounds.sort_unstable();
        rounds.iter().map(|r| num[r] / den[r]).collect()
    }

    /// Samples of one workload, in order, with their slot within the round.
    /// For asking whether position matters.
    pub fn with_slot(&self, name: &str) -> Vec<(usize, f64)> {
        self.samples
            .iter()
            .filter(|s| s.workload == name)
            .map(|s| (s.slot, s.ns))
            .collect()
    }

    /// The timescale over which this workload's samples stay correlated, in
    /// seconds. `NaN` when no correlation is resolvable.
    ///
    /// In seconds rather than in lags, because a lag is worth a different
    /// amount of time in every subset - a round of five workloads is twenty
    /// times longer than a round of one, so the same lag means twenty times
    /// the wall clock.
    ///
    /// Works from the variogram: over pairs separated by `dt`, the mean of
    /// `(x_i - x_j)^2` climbs from a floor at short separation to a plateau
    /// once the samples are independent. [`Run::variogram_ratio`] compares
    /// the near bin `[0, tau)` against the far bin `[tau, 2*tau)`, so it sits
    /// below 1 while the variogram is still climbing and returns to 1 once
    /// both bins are on the plateau. The `tau` where it dips lowest is where
    /// the climb happens.
    ///
    /// **The grid is absolute, not derived from the recording.** An earlier
    /// version swept from `4 * mean spacing` to `span / 8`, which made the
    /// answer a function of how long a round happened to be: across the 31
    /// subsets the same workload reported values 150x to 10000x apart, and
    /// three different workloads in one subset all reported exactly
    /// `0.10196s` - which was that subset's lower bound to five digits. A
    /// fixed grid cannot do that. A subset whose sampling cannot resolve a
    /// given `tau` now contributes nothing at that `tau` instead of being
    /// silently rescaled.
    pub fn autocorrelation_time(&self, name: &str) -> f64 {
        let timings = self.timings(name);
        if timings.len() < 32 {
            return f64::NAN;
        }
        let span = timings[timings.len() - 1].0 - timings[0].0;
        let spacing = span / (timings.len() - 1) as f64;

        let (mut best_tau, mut best) = (f64::NAN, f64::INFINITY);
        for k in 0..=GRID_STEPS {
            let tau = GRID_LO * (GRID_HI / GRID_LO).powf(k as f64 / GRID_STEPS as f64);
            // Below a few samples apart there is nothing to compare, and
            // above a fraction of the run the far bin runs out of pairs.
            if tau < 4.0 * spacing || 2.0 * tau > span / 4.0 {
                continue;
            }
            let (c, rel_se) = self.variogram_ratio(&timings, tau, spacing);
            // Require the dip to be deeper than the noise on the estimate of
            // it. Without this the argmin of a flat, noisy curve gets
            // reported as a timescale, which is how a search bound ended up
            // being presented as a property of the machine.
            if c.is_finite() && c < 1.0 - 2.0 * rel_se && c < best {
                best = c;
                best_tau = tau;
            }
        }
        best_tau
    }

    /// Mean squared difference over `[0, tau)` against `[tau, 2*tau)`, and
    /// the relative standard error of that ratio.
    ///
    /// Strides the outer loop so the pair count stays bounded: taken whole
    /// this is O(n^2), which at forty thousand samples is a billion pairs
    /// for every `tau`.
    ///
    /// The error is computed against the number of *samples* used, not the
    /// number of pairs. Pairs drawn from the same samples are not
    /// independent, so counting them would understate the error by orders of
    /// magnitude - which is exactly the mistake that would let a flat curve
    /// look significant.
    fn variogram_ratio(&self, timings: &[(f64, f64)], tau: f64, spacing: f64) -> (f64, f64) {
        const MAX_PAIRS: f64 = 200_000.0;
        let per_i = 2.0 * tau / spacing;
        let stride = ((timings.len() as f64 * per_i) / MAX_PAIRS).ceil().max(1.0) as usize;

        let (mut s1, mut q1, mut n1) = (0.0, 0.0, 0usize);
        let (mut s2, mut q2, mut n2) = (0.0, 0.0, 0usize);
        let mut used = 0usize;
        for i in (0..timings.len()).step_by(stride) {
            used += 1;
            for j in i + 1..timings.len() {
                let dt = timings[j].0 - timings[i].0;
                let d2 = (timings[i].1 - timings[j].1).powi(2);
                if dt < tau {
                    s1 += d2;
                    q1 += d2 * d2;
                    n1 += 1;
                } else if dt < 2.0 * tau {
                    s2 += d2;
                    q2 += d2 * d2;
                    n2 += 1;
                } else {
                    break;
                }
            }
        }
        if n1 == 0 || n2 == 0 || used < 2 {
            return (f64::NAN, f64::INFINITY);
        }
        let (m1, m2) = (s1 / n1 as f64, s2 / n2 as f64);
        let rel = |m: f64, q: f64, n: usize| {
            let var = (q / n as f64 - m * m).max(0.0);
            if m > 0.0 {
                var.sqrt() / m / (used as f64).sqrt()
            } else {
                f64::INFINITY
            }
        };
        let (r1, r2) = (rel(m1, q1, n1), rel(m2, q2, n2));
        (m1 / m2, (r1 * r1 + r2 * r2).sqrt())
    }
}

/// The absolute `tau` grid the variogram is probed on: 100us to 10s.
///
/// Fixed rather than derived from the recording, so a correlation time means
/// the same thing in every subset and can be compared between them.
const GRID_LO: f64 = 100e-6;
const GRID_HI: f64 = 10.0;
const GRID_STEPS: usize = 24;

// ------------------------------------------------------------- estimators

pub type Estimator = fn(&Run, &str) -> f64;

pub fn variance(v: &[f64], mean: f64) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    v.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / v.len() as f64
}

pub fn mean(v: &[f64]) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    v.iter().sum::<f64>() / v.len() as f64
}

pub fn median(v: &[f64]) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    let mut s: Vec<f64> = v.iter().copied().filter(|v| v.is_finite()).collect();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap());
    s[s.len() / 2]
}

/// Trimmed mean, dropping `frac` from each end.
pub fn trimmed_mean(v: &[f64], frac: f64) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    let mut s: Vec<f64> = v.iter().copied().filter(|v| v.is_finite()).collect();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let k = (s.len() as f64 * frac) as usize;
    mean(&s[k..s.len() - k])
}

/// Relative spread, used both for reporting and for choosing a canary.
pub fn rel_spread(v: &[f64]) -> f64 {
    let m = mean(v);
    if v.len() < 2 || m == 0.0 {
        return f64::NAN;
    }
    let var = variance(v, m);
    var.sqrt() / m
}

/// Does it matter *where* in the round this workload ran?
///
/// Groups the samples by slot, takes a trimmed mean of each, and returns the
/// relative spread between slots. Near zero means position is irrelevant and
/// the reshuffling is costing nothing. A large value means it is not: the
/// usual cause is the workload immediately before, and the memory canary is
/// the obvious suspect, since it leaves whatever follows it with a cold
/// cache.
///
/// This is a diagnostic rather than an estimator - it says something about
/// the experiment, not about the answer - which is why it gets its own
/// column in `compare` instead of a line in [`all`].
pub fn slot_effect(r: &Run, w: &str) -> f64 {
    let mut by_slot: HashMap<usize, Vec<f64>> = HashMap::new();
    for (slot, ns) in r.with_slot(w) {
        by_slot.entry(slot).or_default().push(ns);
    }
    if by_slot.len() < 2 {
        return f64::NAN;
    }
    let means: Vec<f64> = by_slot.values().map(|v| trimmed_mean(v, 0.10)).collect();
    rel_spread(&means)
}

fn mean_estimator(r: &Run, w: &str) -> f64 {
    mean(r.get(w))
}
fn median_estimator(r: &Run, w: &str) -> f64 {
    median(r.get(w))
}
fn trim10_estimator(r: &Run, w: &str) -> f64 {
    trimmed_mean(r.get(w), 0.10)
}

fn ratio_cpu(r: &Run, w: &str) -> f64 {
    trimmed_mean(&r.ratio(w, CPU), 0.10)
}
fn ratio_mem(r: &Run, w: &str) -> f64 {
    trimmed_mean(&r.ratio(w, MEM), 0.10)
}

/// Pearson correlation, used to ask which canary a workload moves with.
pub fn corr(a: &[f64], b: &[f64]) -> f64 {
    if a.len() < 2 || a.len() != b.len() {
        return 0.0;
    }
    let (ma, mb) = (mean(a), mean(b));
    let num: f64 = a.iter().zip(b).map(|(x, y)| (x - ma) * (y - mb)).sum();
    let da: f64 = a.iter().map(|x| (x - ma) * (x - ma)).sum::<f64>().sqrt();
    let db: f64 = b.iter().map(|y| (y - mb) * (y - mb)).sum::<f64>().sqrt();
    if da * db == 0.0 {
        0.0
    } else {
        num / (da * db)
    }
}

/// Works for a clear-cut workload, fails for a borderline one.
///
/// Picks the canary whose per-round ratio has the smaller spread *within
/// this run*. Note that is not the rule that was validated nine times out of
/// nine - that one compared spread *across* runs, which one run cannot do.
///
/// On workloads clearly dominated by one bottleneck it agrees with itself
/// run after run and costs nothing. On one sitting between the two canaries
/// it flips, and then each run has divided by a different denominator and is
/// reporting a different quantity: measured at 122% where naming the canary
/// by hand gave 0.145%. `compare` prints MIXED when that has happened, which
/// is the column to check before believing this estimator's number.
///
/// So the open question is not whether automatic selection works but whether
/// it fails *safely*, and right now it does not - it fails silently unless
/// somebody reads the MIXED column.
fn ratio_auto(r: &Run, w: &str) -> f64 {
    let (c, m) = (r.ratio(w, CPU), r.ratio(w, MEM));
    if rel_spread(&c) <= rel_spread(&m) {
        trimmed_mean(&c, 0.10)
    } else {
        trimmed_mean(&m, 0.10)
    }
}

/// Pick the canary this workload actually moves *with*, by correlation.
///
/// A different within-run criterion, and the one worth testing: a payload
/// and the canary that shares its bottleneck should rise and fall together
/// round by round, whether or not their ratios happen to have similar
/// spreads.
fn ratio_corr(r: &Run, w: &str) -> f64 {
    let canary = pick_by_corr(r, w);
    trimmed_mean(&r.ratio(w, canary), 0.10)
}

pub fn pick_by_corr(r: &Run, w: &str) -> &'static str {
    let v = r.get(w);
    if corr(v, r.get(CPU)).abs() >= corr(v, r.get(MEM)).abs() {
        CPU
    } else {
        MEM
    }
}

/// Which canary each selector picked, so a surprising row can be explained
/// rather than just noticed.
pub fn chosen_canary(r: &Run, w: &str) -> &'static str {
    let (c, m) = (r.ratio(w, CPU), r.ratio(w, MEM));
    if rel_spread(&c) <= rel_spread(&m) {
        "cpu"
    } else {
        "mem"
    }
}

pub fn chosen_by_corr(r: &Run, w: &str) -> &'static str {
    if pick_by_corr(r, w) == CPU {
        "cpu"
    } else {
        "mem"
    }
}

/// Every variant, in one place. Add yours here.
///
/// Not yet here, and the obvious next one: the additive decomposition
/// `P = a*cpu + b*mem`, which beats either canary alone by ~10x on a
/// genuinely mixed workload. It does not fit this signature, because `a` and
/// `b` cannot be fitted from one run - within-run jitter is mostly each
/// canary's own noise, so the fit is attenuated and differs per run. It
/// needs several runs, so it belongs in `compare`.
pub fn all() -> Vec<(&'static str, Estimator)> {
    vec![
        ("mean", mean_estimator as Estimator),
        ("median", median_estimator),
        ("trim10", trim10_estimator),
        ("ratio_cpu", ratio_cpu),
        ("ratio_mem", ratio_mem),
        ("ratio_auto", ratio_auto),
        ("ratio_corr", ratio_corr),
    ]
}

// ---------------------------------------------------------------------------
// Turning a recording into a number.
//
// These lived in `main.rs` beside the command-line plumbing, which is why
// every new analysis grew its own copy rather than reusing them - and why
// the protocol harness ended up a second program with a second format
// instead of another caller of this one. A recording is the interface; this
// is what reads it.
// ---------------------------------------------------------------------------
pub fn rung_name(name: &str, k: usize) -> String {
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
pub struct Rung {
    /// Batch size, in iterations.
    pub n: f64,
    /// Trimmed mean batch time, ns.
    pub mean: f64,
    /// Standard error of that mean, ns.
    pub se: f64,
    /// The same thing divided by the cpu canary in the same round, which
    /// removes whatever the clock was doing between one round and the next.
    pub ratio: f64,
}

/// Trimmed mean and the standard error of that mean.
pub fn mean_se(v: &[f64]) -> (f64, f64) {
    let mut s: Vec<f64> = v.iter().copied().filter(|x| x.is_finite()).collect();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let k = (s.len() as f64 * 0.10) as usize;
    let s = &s[k..s.len() - k];
    let m = mean(s);
    (m, variance(s, m).sqrt() / (s.len() as f64).sqrt())
}

/// One rung, over the rounds in `range`.
///
/// The range is what lets the same code answer two different questions: the
/// whole run, for the size of the fixed cost, and a block of it, for how
/// noisy an estimator is at a given number of rounds.
pub fn rung(
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
pub fn rungs_present(r: &Run, base: &str) -> Vec<usize> {
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
pub fn wide_of(v: &[Rung], y: &dyn Fn(&Rung) -> f64) -> f64 {
    let l = v.len() - 1;
    (y(&v[l]) - y(&v[0])) / (v[l].n - v[0].n)
}

/// Slope from an unweighted least-squares line through every rung.
///
/// The textbook alternative to [`wide_of`], and on this data a dead heat
/// with it. Under multiplicative noise - where a batch's error scales with
/// its duration - the extreme pair is very nearly the optimal design, and an
/// unweighted fit slightly over-trusts the noisiest rung.
pub fn lsq_of(v: &[Rung], y: &dyn Fn(&Rung) -> f64) -> f64 {
    let k = v.len() as f64;
    let xm = v.iter().map(|r| r.n).sum::<f64>() / k;
    let ym = v.iter().map(|r| y(r)).sum::<f64>() / k;
    let num: f64 = v.iter().map(|r| (r.n - xm) * (y(r) - ym)).sum();
    let den: f64 = v.iter().map(|r| (r.n - xm).powi(2)).sum();
    num / den
}

/// The intercept the widest pair implies: the part of a batch's cost that
/// its iterations do not account for.
pub fn fixed_of(v: &[Rung], y: &dyn Fn(&Rung) -> f64) -> f64 {
    y(&v[0]) - wide_of(v, y) * v[0].n
}

/// Is the line a line? The slope over the bottom half of the ladder against
/// the slope over the top half, which agree if and only if the fixed cost is
/// genuinely fixed.
///
/// A constant intercept makes these equal. Anything else - a warm-up that
/// keeps on warming, a queue that fills as the batch runs - shows up here,
/// and means no two-point subtraction can be right, however tidy its answer.
pub fn lin_of(v: &[Rung], y: &dyn Fn(&Rung) -> f64) -> f64 {
    let mid = v.len() / 2;
    wide_of(&v[..=mid], y) - wide_of(&v[mid..], y)
}

pub fn mean_v(r: &Rung) -> f64 {
    r.mean
}
pub fn ratio_v(r: &Rung) -> f64 {
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
pub fn canary_per_iter(r: &Run) -> HashMap<usize, f64> {
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
