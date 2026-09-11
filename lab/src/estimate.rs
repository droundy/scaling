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
pub struct Run {
    pub names: Vec<String>,
    pub ns: HashMap<String, Vec<f64>>,
}

impl Run {
    pub fn load(path: &str) -> Run {
        let (iters, times) = crate::timing::read(path);
        let mut ns: HashMap<String, Vec<(usize, f64)>> = HashMap::new();
        for ((round, name), t) in times {
            ns.entry(name).or_default().push((round, t));
        }
        let mut names: Vec<String> = ns.keys().cloned().collect();
        names.sort();
        let ns = ns
            .into_iter()
            .map(|(name, mut v)| {
                v.sort_by_key(|(r, _)| *r);
                let n = *iters.get(&name).unwrap_or(&1) as f64;
                (name, v.into_iter().map(|(_, t)| t / n).collect())
            })
            .collect();
        Run { names, ns }
    }

    pub fn get(&self, name: &str) -> &[f64] {
        self.ns.get(name).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// The per-round ratio of a workload to a canary. Both were measured
    /// microseconds apart in the same round, so they shared a clock, and
    /// dividing cancels whatever the clock was doing.
    pub fn ratio(&self, name: &str, canary: &str) -> Vec<f64> {
        let a = self.get(name);
        let b = self.get(canary);
        a.iter().zip(b).map(|(x, y)| x / y).collect()
    }
}

// ------------------------------------------------------------- estimators

pub type Estimator = fn(&Run, &str) -> f64;

pub fn mean_of(v: &[f64]) -> f64 {
    if v.is_empty() { return f64::NAN; }
    v.iter().sum::<f64>() / v.len() as f64
}

pub fn median_of(v: &[f64]) -> f64 {
    if v.is_empty() { return f64::NAN; }
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap());
    s[s.len() / 2]
}

/// Trimmed mean, dropping `frac` from each end.
pub fn trim_of(v: &[f64], frac: f64) -> f64 {
    if v.is_empty() { return f64::NAN; }
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let k = (s.len() as f64 * frac) as usize;
    mean_of(&s[k..s.len() - k])
}

/// Relative spread, used both for reporting and for choosing a canary.
pub fn rel_spread(v: &[f64]) -> f64 {
    let m = mean_of(v);
    if v.len() < 2 || m == 0.0 { return f64::NAN; }
    let var = v.iter().map(|x| (x - m) * (x - m)).sum::<f64>() / v.len() as f64;
    var.sqrt() / m
}

fn raw_mean(r: &Run, w: &str) -> f64 { mean_of(r.get(w)) }
fn raw_median(r: &Run, w: &str) -> f64 { median_of(r.get(w)) }
fn raw_trim(r: &Run, w: &str) -> f64 { trim_of(r.get(w), 0.10) }

fn ratio_cpu(r: &Run, w: &str) -> f64 { trim_of(&r.ratio(w, CPU), 0.10) }
fn ratio_mem(r: &Run, w: &str) -> f64 { trim_of(&r.ratio(w, MEM), 0.10) }

/// Pearson correlation, used to ask which canary a workload moves with.
pub fn corr(a: &[f64], b: &[f64]) -> f64 {
    if a.len() < 2 || a.len() != b.len() { return 0.0; }
    let (ma, mb) = (mean_of(a), mean_of(b));
    let num: f64 = a.iter().zip(b).map(|(x, y)| (x - ma) * (y - mb)).sum();
    let da: f64 = a.iter().map(|x| (x - ma) * (x - ma)).sum::<f64>().sqrt();
    let db: f64 = b.iter().map(|y| (y - mb) * (y - mb)).sum::<f64>().sqrt();
    if da * db == 0.0 { 0.0 } else { num / (da * db) }
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
    if rel_spread(&c) <= rel_spread(&m) { trim_of(&c, 0.10) } else { trim_of(&m, 0.10) }
}

/// Pick the canary this workload actually moves *with*, by correlation.
///
/// A different within-run criterion, and the one worth testing: a payload
/// and the canary that shares its bottleneck should rise and fall together
/// round by round, whether or not their ratios happen to have similar
/// spreads.
fn ratio_corr(r: &Run, w: &str) -> f64 {
    let canary = pick_by_corr(r, w);
    trim_of(&r.ratio(w, canary), 0.10)
}

pub fn pick_by_corr(r: &Run, w: &str) -> &'static str {
    let v = r.get(w);
    if corr(v, r.get(CPU)).abs() >= corr(v, r.get(MEM)).abs() { CPU } else { MEM }
}

/// Which canary each selector picked, so a surprising row can be explained
/// rather than just noticed.
pub fn chosen_canary(r: &Run, w: &str) -> &'static str {
    let (c, m) = (r.ratio(w, CPU), r.ratio(w, MEM));
    if rel_spread(&c) <= rel_spread(&m) { "cpu" } else { "mem" }
}

pub fn chosen_by_corr(r: &Run, w: &str) -> &'static str {
    if pick_by_corr(r, w) == CPU { "cpu" } else { "mem" }
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
        ("raw_mean", raw_mean as Estimator),
        ("raw_median", raw_median),
        ("raw_trim10", raw_trim),
        ("ratio_cpu", ratio_cpu),
        ("ratio_mem", ratio_mem),
        ("ratio_auto", ratio_auto),
        ("ratio_corr", ratio_corr),
    ]
}
