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
//! that also contained the other rungs. An algorithm that stood only on rung
//! k would establish a different regime - different cache occupancy,
//! different round period - and composition moves a cost by percent-scale
//! amounts, so a replayed timing inherits the recorded round rather than the
//! one that algorithm would have created.
//!
//! How much that matters depends on the algorithm. One that samples the
//! whole ladder is replayed almost exactly, because the recorder draws rungs
//! the same way; one that parks on a single rung is the case where the
//! recorded round and the real one diverge most. Checking it means measuring
//! an algorithm for real and comparing, which is worth doing once there is
//! an algorithm worth checking.

use crate::timing::{rung_name, Run};

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
    /// one number. For most workloads it is flat at ~370ns across the whole
    /// ladder - genuinely a per-measurement constant - but a workload that
    /// prepares an input per iteration pays that per iteration too, so for
    /// those it grows with the batch. `f64_sin` runs about 12.5ns an
    /// iteration of preparation. A hard-coded constant would be right for
    /// most and badly wrong at the top of such a ladder, which is more than
    /// enough to invert a ranking.
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
    let mut by_base: std::collections::BTreeMap<String, Vec<Rung>> = Default::default();
    for name in &r.names {
        let Some(meta) = r.rungs.get(name) else {
            continue;
        };
        let base = name.split('@').next().unwrap_or(name).to_string();
        let batch_ns: Vec<f64> = r.get(name).iter().map(|x| x * meta.n as f64).collect();
        if batch_ns.is_empty() {
            continue;
        }
        by_base.entry(base).or_default().push(Rung {
            n: meta.n,
            batch_ns,
            overhead_ns: meta.overhead_ns,
        });
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
        self.spent_ns += ns + r.overhead_ns;
        self.draws += 1;
        ns
    }

    pub fn n(&self, k: usize) -> f64 {
        self.tape.rungs[k].n as f64
    }

    pub fn rungs(&self) -> usize {
        self.tape.rungs.len()
    }
}

/// What an algorithm is, as far as replay is concerned.
pub struct Policy {
    pub name: &'static str,
    /// Rung durations as fractions of the usable ceiling, `MAX_RUNG_NS`.
    /// Empty means "whatever calibration lands on", the auto case.
    ///
    /// Fractions of the ceiling rather than multiples of `SAMPLE`, because
    /// `SAMPLE` is 100us and the ladder now ends at 20us: every target
    /// expressed against it clamped to the same top rung, and three
    /// distinct policies reported one identical row three times.
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
    /// Samples taken, calibration probes included.
    pub draws: usize,

}

/// Fewest samples before a standard error means anything.
const MIN_SAMPLES: usize = 5;

/// Fraction trimmed from *each* end of a rung's per-sample values, for the
/// single-rung and two-rung estimators. Overridable with `LAB_PAIR_TRIM` so
/// the level is chosen by measurement.
fn pair_trim() -> f64 {
    static T: std::sync::OnceLock<f64> = std::sync::OnceLock::new();
    *T.get_or_init(|| {
        std::env::var("LAB_PAIR_TRIM")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_TRIM)
    })
}

/// Chosen by sweeping 0, 10%, 25% and 40% on the quiet deep recording. Any
/// trim removes the +0.3-0.7% bias an untrimmed estimate carries from tick
/// excursions - the truth is a trimmed mean, so an untrimmed estimator was
/// measuring a slightly different quantity - and with the winsorised error
/// bar 10% and 25% agree closely. 25% matches the truth's own trim.
const DEFAULT_TRIM: f64 = 0.25;

/// Mean of the middle `1 - 2*trim` of `v`. Symmetric, so ordinary noise
/// leaves it centred while one-sided excursions are discarded.
fn trimmed_mean(v: &[f64], trim: f64) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    if trim <= 0.0 {
        return v.iter().sum::<f64>() / v.len() as f64;
    }
    let mut w = v.to_vec();
    w.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let cut = ((w.len() as f64) * trim).floor() as usize;
    let cut = cut.min((w.len() - 1) / 2);
    let m = &w[cut..w.len() - cut];
    m.iter().sum::<f64>() / m.len() as f64
}

/// Standard error of a trimmed mean, allowing for correlation.
///
/// Winsorise the whole series at the trim fraction - values beyond each
/// cutoff are replaced by the cutoff rather than dropped - take batch means
/// of that, and divide by `1 - 2*trim`. That is the textbook variance of a
/// trimmed mean (Tukey and McLaughlin), with batch means standing in for
/// the plain variance so that correlated samples are still allowed for.
///
/// A previous version trimmed *each block* instead, which is not the same
/// thing and fails quietly when blocks are small. At 10% trim and 50
/// samples a block held about seven values, 10% of seven rounds down to
/// nothing, and the blocks went untrimmed while the estimate was trimmed -
/// so the bar described a plain mean while the number was a trimmed one,
/// and came out nearly three times too wide. Winsorising the series once
/// makes the cutoffs independent of how the series is then cut into blocks.
fn batch_se_trimmed(v: &[f64], trim: f64) -> f64 {
    if trim <= 0.0 {
        return batch_se(v);
    }
    if v.len() < MIN_SAMPLES {
        return f64::INFINITY;
    }
    let mut sorted = v.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let cut = ((sorted.len() as f64) * trim).floor() as usize;
    let cut = cut.min((sorted.len() - 1) / 2);
    let lo = sorted[cut];
    let hi = sorted[sorted.len() - 1 - cut];
    let wins: Vec<f64> = v.iter().map(|&x| x.clamp(lo, hi)).collect();
    batch_se(&wins) / (1.0 - 2.0 * trim)
}

/// Fewest samples before any algorithm may stop, overridable with
/// `LAB_MIN_SAMPLES` so the effect can be measured rather than argued.
///
/// The error bar is estimated from blocks of the samples taken so far, and
/// from a handful of samples it is estimated from a handful of blocks - four
/// blocks of two, at the point the two-rung estimator was stopping. A bar
/// that uncertain is as likely to dip low by chance as to be right, and the
/// stopping rule fires precisely when it dips, so it selects the moments the
/// bar is most wrong. The same few samples also let one bad pair carry the
/// whole estimate, which is where the blowups come from.
fn min_samples() -> usize {
    static N: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *N.get_or_init(|| {
        std::env::var("LAB_MIN_SAMPLES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_MIN_SAMPLES)
    })
}

/// Chosen by sweeping 5, 20, 50, 100 and 200 on the quiet deep recording.
/// At 5 the two-rung estimator stopped after about eight pairs, blew up in
/// 2-8% of trials and claimed half the error it had. Blowups are gone by
/// 20; `bar/sd` keeps improving to 100 (f64_sin 0.64 -> 1.05) and not
/// beyond. It costs a fast workload a few milliseconds rather than a few
/// hundred microseconds.
const DEFAULT_MIN_SAMPLES: usize = 100;

/// Fewest whole sweeps of the ladder before a fitted slope has an error bar.
///
/// This is a floor on what a fit costs: it must pay for several visits to
/// the top rung before it can say anything about its own uncertainty. That
/// is the price of spanning the ladder, and it is why a fit cannot be as
/// cheap as standing on one cheap rung.
const MIN_SWEEPS: usize = 8;

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
    // Square-root rule, for the reason given in `slope_se`: blocks have to
    // lengthen as samples accumulate or they never outlast the correlation.
    let b = ((v.len() as f64).sqrt() as usize).clamp(4, 20);
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

/// What an algorithm does with the ladder.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Choice {
    /// Stand on one rung.
    One(usize),
    /// Subtract a low rung from a high one.
    Pair(usize, usize),
    /// Use every rung, drawn at random, and fit a line.
    ///
    /// This needs no rung *choice*, and so needs no calibration to make
    /// one. The ladder is discovered by walking up from n=1 until a batch
    /// overruns the ceiling, and every probe taken on the way is kept as a
    /// data point - so the growth loop stops being a tax paid before
    /// measuring and becomes part of the measurement. That matters because
    /// the calibrated policies spend ten to twenty times the measurement on
    /// probes for a fast workload.
    ///
    /// There was a variant that drew cheap rungs more often, to give each
    /// rung equal wall time rather than an equal count. It is gone: drawing
    /// unevenly destroys the balanced design that the block-slope error
    /// estimate depends on, and it showed - under additive noise it claimed
    /// a bar eighty times too large and covered 100% of the time. An
    /// estimator and its error bar are not separable choices.
    /// `trim` is the fraction discarded from each end of every rung before
    /// fitting, which keeps the estimate centred on symmetric noise while
    /// discarding the one-sided contamination that ticks add.
    All { trim: f64, floor: usize },
}

/// The longest batch in a recording, which is where a growth loop stops.
///
/// Taken from the recording rather than from a constant, because which
/// rungs exist is the recorder's decision - see `rungs_for` in `main.rs`.
/// An analysis reads the ladder it was given; it does not get to filter it,
/// and an algorithm that wants a rung outside it needs fresh data.
fn top_ns(tape: &Tape) -> f64 {
    tape.rungs
        .last()
        .map(|r| r.batch_ns.iter().sum::<f64>() / r.batch_ns.len() as f64)
        .unwrap_or(0.0)
}

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

/// Least-squares slope of batch time against batch size.
///
/// The slope is the per-iteration cost and the intercept is the fixed cost
/// per measurement, so a fit over the whole ladder yields both - where a
/// single rung yields neither separately, and a pair yields the slope only.
/// Fit through one robust point per rung, rather than through every sample.
///
/// A scheduler tick adds about 5us to whatever batch it lands in, and the
/// chance of landing grows with batch length: ~1% at a 10us batch, ~10% at
/// 100us, ~87% at 2ms. Where hits are rare they are outliers and can be
/// trimmed away; where they are ubiquitous there is no clean batch left to
/// find and the contamination is simply part of what the workload appears
/// to cost. Trimming the upper tail of each rung therefore removes the bias
/// at the short rungs and cannot remove it at the long ones - which is an
/// argument about where the ladder should end, not only about how to average.
///
/// One-sided: a tick only ever makes a batch slower.
fn fit_trimmed(pts: &[(f64, f64)], trim: f64) -> f64 {
    if trim <= 0.0 {
        return slope(pts);
    }
    let mut by: std::collections::BTreeMap<u64, Vec<f64>> = Default::default();
    for &(n, ns) in pts {
        by.entry(n as u64).or_default().push(ns);
    }
    let rows: Vec<(f64, f64)> = by
        .into_iter()
        .filter_map(|(n, mut v)| {
            if v.len() < MIN_FOR_TRIM {
                return None;
            }
            v.sort_by(|a, b| a.partial_cmp(b).unwrap());
            // Symmetric, not one-sided.
            //
            // Cutting only the upper tail looks right - a tick can only add
            // time - but the tail it cuts is mostly ordinary noise. On a 2%
            // symmetric distribution a 25% upper trim moves the mean down by
            // about 0.4 sigma, which measured as a -0.8% bias: the tick bias
            // traded for a trim bias of the same size. Trimming both ends
            // leaves the centre where it was for symmetric noise, while
            // still discarding one-sided contamination, so long as fewer
            // than `trim` of the samples are contaminated.
            let cut = ((v.len() as f64) * trim).floor() as usize;
            let lo = cut.min(v.len() / 2);
            let hi = v.len() - cut.min(v.len() / 2);
            let w = &v[lo..hi.max(lo + 1)];
            let m = w.iter().sum::<f64>() / w.len() as f64;
            Some((n as f64, m))
        })
        .collect();
    if rows.len() < 2 {
        return slope(pts);
    }
    slope(&rows)
}

/// Fewest samples at a rung before trimming its tail means anything.
const MIN_FOR_TRIM: usize = 4;

/// How much of each rung to discard from *each* end.
///
/// Half of it is doing the work - the upper half, where ticks land - and the
/// other half is there to keep the estimate centred on symmetric noise. The
/// fraction has to exceed the chance of a tick landing in the longest batch
/// on the ladder, which is the batch length over the tick period: ~2% at
/// 20us, 17% at 100us, 50% at 500us, and certainty beyond a millisecond.
/// Past 50% a median has nothing clean left to find, which is the real
/// argument for where a ladder should end.
const TRIM: f64 = 0.25;

fn slope(pts: &[(f64, f64)]) -> f64 {
    let k = pts.len() as f64;
    let sx: f64 = pts.iter().map(|p| p.0).sum();
    let sy: f64 = pts.iter().map(|p| p.1).sum();
    let sxx: f64 = pts.iter().map(|p| p.0 * p.0).sum();
    let sxy: f64 = pts.iter().map(|p| p.0 * p.1).sum();
    let den = k * sxx - sx * sx;
    if den.abs() < f64::EPSILON {
        return f64::NAN;
    }
    (k * sxy - sx * sy) / den
}

/// Standard error of the slope, from the spread of slopes fitted to
/// contiguous blocks.
///
/// The textbook standard error of a regression slope assumes independent
/// residuals, and consecutive timings are not independent: they share a
/// clock frequency, a cache state, a scheduler phase. Fitting each block
/// separately and taking the spread of those slopes divides out whatever is
/// correlated within a block - the batch-means argument, applied to a slope
/// instead of to a mean.
/// How many distinct batch sizes appear, which is one sweep of the ladder.
fn ladder_len(pts: &[(f64, f64)]) -> Option<usize> {
    let mut ns: Vec<u64> = pts.iter().map(|p| p.0 as u64).collect();
    ns.sort_unstable();
    ns.dedup();
    Some(ns.len())
}

fn slope_se(pts: &[(f64, f64)], trim: f64) -> f64 {
    if pts.len() < MIN_SAMPLES * 2 {
        return f64::INFINITY;
    }
    // Blocks must be whole sweeps of the ladder.
    //
    // A block cut mid-sweep is missing whichever rungs fell after the cut,
    // so its fitted slope reflects the rungs it happened to get rather than
    // the noise - which is the unbalanced-design problem the shuffled sweep
    // exists to remove, reintroduced at the block boundary. Trimming to a
    // multiple of the ladder length is not enough on its own: until there
    // are several whole sweeps there is no balanced block to be had, and
    // saying so is better than returning a number built from fragments.
    let l = ladder_len(pts).unwrap_or(1).max(1);
    let sweeps = pts.len() / l;
    if sweeps < MIN_SWEEPS {
        return f64::INFINITY;
    }
    // Block *length* has to grow with the data, not just block count.
    //
    // Batch means only divide out correlation when a block outlasts the
    // correlation time. Taking more blocks as data arrives keeps them one
    // sweep long forever, so adjacent blocks stay correlated and their
    // spread understates the error - by about fourfold at phi=0.8. The
    // square-root rule splits the difference: both the number of blocks and
    // their length grow, so the estimate decorrelates as samples accumulate.
    let b = (sweeps as f64).sqrt() as usize;
    let b = b.clamp(4, 20);
    let per = (sweeps / b) * l;
    if per < l {
        return f64::INFINITY;
    }
    let mut sl: Vec<f64> = Vec::with_capacity(b);
    for i in 0..b {
        let v = fit_trimmed(&pts[i * per..(i + 1) * per], trim);
        if !v.is_finite() {
            return f64::INFINITY;
        }
        sl.push(v);
    }
    let m = sl.iter().sum::<f64>() / b as f64;
    let var = sl.iter().map(|x| (x - m) * (x - m)).sum::<f64>() / (b as f64 - 1.0);
    (var / b as f64).sqrt()
}

fn run_choice(p: &mut Player, c: Choice, target: f64, budget_s: f64) -> Outcome {
    let budget_ns = budget_s * 1e9;
    let mut vals: Vec<f64> = Vec::new();
    // For `All`: (n, batch_ns) points to fit.
    let mut pts: Vec<(f64, f64)> = Vec::new();
    let mut ladder: Vec<usize> = Vec::new();
    let fitting = matches!(c, Choice::All { .. });
    let (trim, floor) = match c {
        Choice::All { trim, floor } => (trim, floor),
        _ => (0.0, 0),
    };
    if fitting {
        // Discover the ladder by growing from one iteration, keeping every
        // probe. This is the growth loop, except that nothing it measures
        // is thrown away.
        // Every rung in the recording. There is no ceiling to discover
        // here: the recorder already applied one, so what is on disk is
        // what an algorithm is allowed to stand on.
        for k in 0..p.rungs() {
            let ns = p.draw(k);
            pts.push((p.n(k), ns));
            ladder.push(k);
        }
    }
    let mut rng: u64 = 0x2545F4914F6CDD1D ^ (p.spent_ns.to_bits() | 1);
    // Remaining rungs in the current sweep; see `Choice::All` below.
    let mut queue: Vec<usize> = Vec::new();
    let mut est = f64::NAN;
    let mut se = f64::INFINITY;
    let mut capped = false;
    // Next sample count at which to test the stopping rule.
    //
    // Testing it on every draw makes the simulation quadratic: the error
    // estimate is linear in the samples so far, and a tight goal at a small
    // rung runs for tens of thousands of draws. A geometric schedule costs
    // a constant factor of the draws instead, and is what a real
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
            Choice::All { .. } => {
                // A shuffled sweep, not independent draws.
                //
                // Drawing each rung independently lets contiguous blocks end
                // up with different mixes of rungs, and a block's fitted
                // slope then depends on which rungs it happened to contain.
                // The spread of block slopes stops being an estimate of the
                // noise and becomes an estimate of how unevenly the rungs
                // were dealt - which made the claimed bar 200x too large
                // under additive noise and 2x too small under correlated
                // noise. Cycling through a fresh random order gives every
                // block the same design.
                if queue.is_empty() {
                    queue = ladder.clone();
                    for i in (1..queue.len()).rev() {
                        rng = crate::step(rng);
                        queue.swap(i, (rng >> 33) as usize % (i + 1));
                    }
                }
                let k = queue.pop().unwrap();
                let ns = p.draw(k);
                pts.push((p.n(k), ns));
            }
        }
        let count = if fitting { pts.len() } else { vals.len() };
        if count >= check {
            check = ((count as f64 * 1.3) as usize).max(count + 1);
            if fitting {
                est = fit_trimmed(&pts, trim);
                se = slope_se(&pts, trim);
            } else {
                est = trimmed_mean(&vals, pair_trim());
                se = batch_se_trimmed(&vals, pair_trim());
            }
            // The floor is on *sweeps*, not samples: batch means needs
            // blocks longer than the correlation time and enough of them to
            // take a spread over, and stopping at the first moment the bar
            // looks small enough is exactly how it fails to get either.
            let sweeps = if fitting && !ladder.is_empty() { pts.len() / ladder.len() } else { 0 };
            if est > 0.0
                && se.is_finite()
                && se / est <= target
                && sweeps >= floor
                && count >= min_samples()
            {
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
    Outcome { est, se, seconds: p.spent_ns * 1e-9, capped, wrapped: p.wrapped, draws: p.draws }
}

/// Replay a calibration, then let it choose its own rungs and measure.
///
/// The counterpart to [`measure`]: this one pays for calibration and has to
/// find the rung without being told, which is the situation any real
/// algorithm is in.
pub fn calibrated(tape: &Tape, pol: &Policy, start: f64) -> Outcome {
    let mut p = Player::new(tape, start);
    let top = top_ns(tape);
    let (cal_n, per_iter) = calibrate(&mut p, top);
    let pick = |want: f64| -> usize { tape.nearest(want) };
    let c = if pol.rungs.is_empty() {
        Choice::One(pick(cal_n))
    } else if pol.rungs.len() == 1 {
        Choice::One(pick(pol.rungs[0] * top / per_iter.max(1e-9)))
    } else {
        let a = pick(pol.rungs[0] * top / per_iter.max(1e-9));
        let b = pick(pol.rungs[1] * top / per_iter.max(1e-9));
        if a == b { Choice::One(a) } else { Choice::Pair(a.min(b), a.max(b)) }
    };
    run_choice(&mut p, c, pol.target, pol.budget_s)
}

/// The best estimate of a workload's true per-iteration cost, from the whole
/// recording at once: `(t(2N) - t(N)) / N` for the top two rungs.
///
/// This was the slope between the *widest* pair - n=1 and the top rung - on
/// the argument that more lever arm is more precision. It is, but the lever
/// ran straight through the region where batch time is not linear in n.
/// Local slopes along cpu_canary's ladder are 5.30 ns/iter from n=1 to 64
/// and 4.09 from 64 to 128, then flat at 2.36 +- 0.2% for five doublings. The
/// widest pair averaged that start-up excess in and reported 2.409, 2% high,
/// so estimators that were reading the flat part correctly were being
/// scored as biased low.
///
/// The top two rungs sit in the linear regime and still span N iterations,
/// which is as much lever as the top of the ladder offers. Where a workload
/// never becomes linear - btree_miss's local slope falls 28% across its
/// ladder as larger batches rewarm more of its working set - this is the
/// marginal cost at the largest batch measured, which is a definite quantity
/// even though it is not *the* cost; that workload does not have one.
///
/// Per-rung values are trimmed means rather than means; see [`typical`].
/// Pooled over every sample in both rungs, this uses orders of magnitude more
/// machine time than any algorithm under test is allowed, which is the only
/// sense in which one measurement can referee another.
pub fn truth(tape: &Tape) -> (f64, f64) {
    if tape.rungs.len() < 2 {
        let r = &tape.rungs[0];
        let per: Vec<f64> = r.batch_ns.iter().map(|x| x / r.n as f64).collect();
        let m = per.iter().sum::<f64>() / per.len() as f64;
        return (m, batch_se(&per));
    }
    let hi = tape.rungs.len() - 1;
    let a = &tape.rungs[hi - 1];
    let b = &tape.rungs[hi];
    let dn = (b.n - a.n) as f64;
    let est = (typical(&b.batch_ns) - typical(&a.batch_ns)) / dn;
    // The error is taken from the untrimmed series, which overstates it
    // slightly: the trimmed mean is the steadier of the two. For a reference
    // that is the safe direction to be wrong in.
    let pa = batch_se(&a.batch_ns);
    let pb = batch_se(&b.batch_ns);
    (est, (pa * pa + pb * pb).sqrt() / dn)
}

/// A rung's typical batch time: the mean of its middle half.
///
/// Symmetric, so ordinary noise leaves it centred, and trimming both ends
/// removes the one-sided excursions a scheduler tick adds. A tick lands in
/// roughly 2% of batches near the 20us ceiling and adds ~5us to each, which
/// pulls a plain mean up by a meaningful fraction of the tightest accuracy
/// goal - and pulls the top rung up more than the one below it, so the
/// excess does not cancel in the subtraction.
fn typical(v: &[f64]) -> f64 {
    let mut w = v.to_vec();
    w.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let cut = w.len() / 4;
    let m = &w[cut..w.len() - cut];
    m.iter().sum::<f64>() / m.len() as f64
}

/// Accuracy targets to report against, as relative standard error.
const TARGETS: [f64; 3] = [0.02, 0.01, 0.005];

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
    /// Median estimate against the truth, as a percentage. The half of a
    /// `cover` failure no error bar can fix: estimates that cluster tightly
    /// in the wrong place.
    bias: f64,
    /// Standard deviation of the estimates across trials, as a percentage -
    /// what the claimed bar ought to be.
    spread: f64,
    /// Median bar the algorithm claimed, as a percentage.
    bar: f64,
    /// Median number of samples a trial took before stopping.
    draws: f64,
}

fn score(label: String, outs: &[Outcome], truth_ns: f64, target: f64) -> Score {
    let n = outs.len() as f64;
    let good: Vec<&Outcome> = outs.iter().filter(|o| o.est.is_finite() && o.est > 0.0).collect();
    let thin = good.len() as f64 / n < 0.9
        || good.iter().filter(|o| o.wrapped).count() as f64 / n > WRAP_MAX;
    if good.is_empty() {
        return Score { label, time: f64::INFINITY, capped: false, within: 0.0, cover: 0.0,
                       blow: 1.0, thin: true, pass: false, bias: f64::NAN,
                       spread: f64::NAN, bar: f64::NAN, draws: f64::NAN };
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
    let med = |mut v: Vec<f64>| -> f64 {
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        v[v.len() / 2]
    };
    let ests: Vec<f64> = good.iter().map(|o| o.est).collect();
    let k = ests.len() as f64;
    let mean = ests.iter().sum::<f64>() / k;
    let spread = if k > 1.0 {
        100.0 * (ests.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / (k - 1.0)).sqrt() / truth_ns
    } else {
        f64::NAN
    };
    let bias = 100.0 * (med(ests) - truth_ns) / truth_ns;
    let bar = 100.0 * med(good.iter().map(|o| o.se).collect()) / truth_ns;
    let draws = med(good.iter().map(|o| o.draws as f64).collect());
    Score { label, time, capped, within, cover, blow, thin, pass, bias, spread, bar, draws }
}

fn line(s: &Score) -> String {
    let base = format!(
        "{:>22} {:>9} {:>8} {:>8} {:>8}",
        s.label,
        fmt_time(s.time, s.capped),
        pct(s.within),
        pct(s.cover),
        pct(s.blow)
    );
    // Why a cell failed, rather than just that it did. `cover` alone cannot
    // tell a bar that is too small from an estimate that is in the wrong
    // place, and those need opposite fixes: one is the variance estimate,
    // the other is the estimator. Only on request, because a table that
    // always carries its own diagnosis is a table nobody reads to the end.
    if std::env::var("LAB_VERBOSE").is_ok() {
        format!(
            "{base}   bias {:+6.2}%  spread {:5.2}%  bar {:5.2}%  bar/sd {:4.2}  draws {:>6.0}",
            s.bias,
            s.spread,
            s.bar,
            s.bar / s.spread,
            s.draws
        )
    } else {
        base
    }
}

/// The calibrated algorithms: these pay for calibration and have to find
/// their own rungs, which is the situation a real one is in.
fn cal_policies(target: f64) -> Vec<Policy> {
    vec![
        Policy { name: "cal one-rung", rungs: &[1.0], target, budget_s: 10.0 },
        // `(t(2N) - t(N)) / N`: both rungs in the linear regime. This was
        // an eighth of the top against the top, which put its low end three
        // doublings down, inside the start-up excess the subtraction is
        // there to remove.
        //
        // There was also a `cal auto` here, which stood on whatever rung
        // calibration landed on. With the ceiling at the top of the ladder
        // that was always the top rung, so it reported the same row as
        // `cal one-rung` in every cell of every run.
        Policy { name: "cal two-rung", rungs: &[0.5, 1.0], target, budget_s: 10.0 },
    ]
}

pub fn report(paths: &[String]) {

    println!(
        "Rung choice, calibration and stopping replayed from recordings.\n\
         Which rungs exist is the recorder's decision, not this one: an algorithm\n\
         wanting a rung outside the recorded ladder needs fresh data, not a new query.\n\n\
         all-rungs  = walk up from n=1 keeping every probe, then sample the ladder\n\
                      at random and fit; no rung choice, so no calibration to make one\n\
         cal *      = calibrate, pick a rung, measure there; charged for the probes\n\
         within     = landed inside the accuracy goal (want >={})\n\
         cover      = landed inside the bar the run itself claimed (want ~{})\n\
         blow       = off by more than {BLOWUP}x the goal (want <={})\n",
        pct(PASS_WITHIN), pct(EXPECT_COVERAGE), pct(BLOWUP_MAX),
    );

    // One file at a time, loaded and dropped.
    //
    // Loading them all first would hold every recording at once: a deep
    // cheap composition is 100MB on disk and some 600MB loaded, so the full
    // powerset would ask for tens of gigabytes. Nothing needs two
    // recordings in memory together - a tape is reported against its own
    // truth, and two files holding the same workload are two sections of
    // the report either way.
    let mut any = false;
    for path in paths {
        let Some(run) = Run::load(path) else {
            continue;
        };
        let tapes = self::tapes(&run);
        if tapes.is_empty() {
            continue;
        }
        any = true;
        if paths.len() > 1 {
            println!("--- {path} ---");
        }
    for tape in &tapes {
        let (truth_ns, truth_se) = truth(tape);
        if !(truth_ns.is_finite() && truth_ns > 0.0) {
            continue;
        }

        println!(
            "===== {} =====  truth {:.4} ns/iter +- {:.2}%",
            tape.workload, truth_ns, 100.0 * truth_se / truth_ns
        );
        // Did the machine hold still while this was recorded?
        //
        // Sample order survives even though timestamps do not, and samples
        // arrive at a steady rate, so position stands in for time. If the
        // cost at the end differs from the cost at the start, the pooled
        // truth is an average of two different machines and no short trial
        // can match it - which would look exactly like every estimator
        // failing at once.
        //
        // Per rung, so a change in which rungs were drawn cannot masquerade
        // as a change in speed, and the median across rungs, so one noisy
        // rung cannot carry it.
        // The shape of the ladder, only on request: per rung, the typical
        // batch time and the *local* slope from the rung below it.
        //
        // If batch time were exactly `a + b*n` every local slope would be
        // the same number. When they are not, "the per-iteration cost"
        // depends on which part of the ladder is doing the measuring, and
        // the truth used for scoring - the slope from n=1 to the top rung -
        // is one choice among several rather than the answer. An estimator
        // that reads a local slope near the top would then disagree with it
        // without being wrong, and would show up here as bias.
        if std::env::var("LAB_VERBOSE").is_ok() {
            let typical = |r: &Rung| -> f64 {
                let mut v = r.batch_ns.clone();
                v.sort_by(|a, b| a.partial_cmp(b).unwrap());
                let cut = v.len() / 4;
                let w = &v[cut..v.len() - cut];
                w.iter().sum::<f64>() / w.len() as f64
            };
            println!("  {:>8} {:>13} {:>13} {:>10}", "n", "batch ns", "local slope", "vs truth");
            let mut prev: Option<(f64, f64)> = None;
            for r in &tape.rungs {
                let t = typical(r);
                let n = r.n as f64;
                match prev {
                    Some((pn, pt)) => {
                        let sl = (t - pt) / (n - pn);
                        println!(
                            "  {:>8} {:>13.1} {:>13.4} {:>+9.2}%",
                            r.n, t, sl, 100.0 * (sl - truth_ns) / truth_ns
                        );
                    }
                    None => println!("  {:>8} {:>13.1} {:>13} {:>10}", r.n, t, "", ""),
                }
                prev = Some((n, t));
            }
        }
        let drift = drift_of(tape);
        if drift.is_finite() {
            println!(
                "  drift start to end: {:+.2}%{}",
                drift,
                if drift.abs() > 1.0 {
                    "   <- exceeds the tightest accuracy goal"
                } else {
                    ""
                }
            );
        }
        println!(
            "  recorded rungs: n={}..{} ({}), longest batch {:.1}us,              overhead {:.0}ns/sample",
            tape.rungs[0].n,
            tape.rungs[tape.rungs.len() - 1].n,
            tape.rungs.len(),
            top_ns(tape) / 1e3,
            tape.rungs[0].overhead_ns,
        );

        for &target in &TARGETS {
            println!("  goal {:.1}%", 100.0 * target);
            println!(
                "    {:>22} {:>9} {:>8} {:>8} {:>8}",
                "algorithm", "time", "within", "cover", "blow"
            );
            let mut rows: Vec<Score> = Vec::new();
            for (label, c) in [
                ("all-rungs", Choice::All { trim: 0.0, floor: 0 }),
                ("all-rungs/trim", Choice::All { trim: TRIM, floor: 0 }),
            ] {
                let outs: Vec<Outcome> = (0..CAL_TRIALS)
                    .map(|i| measure(tape, c, target, 10.0, i as f64 / CAL_TRIALS as f64))
                    .collect();
                rows.push(score(label.to_string(), &outs, truth_ns, target));
            }
            for pol in cal_policies(target) {
                let outs: Vec<Outcome> = (0..CAL_TRIALS)
                    .map(|i| calibrated(tape, &pol, i as f64 / CAL_TRIALS as f64))
                    .collect();
                rows.push(score(pol.name.to_string(), &outs, truth_ns, target));
            }
            // A cell whose trials ran off the end of the recording is
            // reporting the same noise repeatedly, so it is not reported at
            // all rather than reported with a caveat beside it.
            rows.retain(|r| !r.thin);
            if rows.is_empty() {
                println!("      recording too thin at this goal");
                continue;
            }
            rows.sort_by(|a, b| a.time.partial_cmp(&b.time).unwrap());
            for r in &rows {
                println!("    {}  {}", line(r), if r.pass { "pass" } else { "FAIL" });
            }
        }
        println!();
    }
    }
    if !any {
        eprintln!("no usable recordings");
    }
}

// ---------------------------------------------------------------------------
// Testing the harness, rather than the algorithms.
// ---------------------------------------------------------------------------

/// A tape whose answer is known by construction.
///
/// Every number the report prints about real data depends on `truth`, and
/// `truth` is itself an estimate - the widest rung pair, pooled. That is a
/// *slope*, which is also what the fitting algorithm computes, so a fit
/// agreeing with it may be agreeing about method rather than about the
/// workload. A single-rung algorithm is meanwhile scored against a quantity
/// it never set out to measure, and would look wrong even if it were
/// perfect. There is no way to separate those from inside the real data.
///
/// So: synthesise a ladder from `a + b*n` plus a chosen noise, where `b` is
/// known exactly. Any departure from `b` is the harness or the estimator,
/// not a disagreement about what the truth is.
fn synthetic(name: &str, a: f64, b: f64, noise: Noise, samples: usize, seed: u64, ceiling_ns: f64) -> Tape {
    let mut rng = seed | 1;
    let mut uni = {
        let mut r = seed.wrapping_mul(0x9E3779B97F4A7C15) | 1;
        move || {
            r = crate::step(r);
            (r >> 11) as f64 / (1u64 << 53) as f64
        }
    };
    let mut g = move || {
        // Box-Muller, which needs two uniforms and yields one normal here.
        rng = crate::step(rng);
        let u1 = ((rng >> 11) as f64 / (1u64 << 53) as f64).max(1e-12);
        rng = crate::step(rng);
        let u2 = (rng >> 11) as f64 / (1u64 << 53) as f64;
        (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
    };
    let mut rungs = Vec::new();
    let mut n = 1usize;
    loop {
        let clean = a + b * n as f64;
        let mut prev = 0.0;
        let batch_ns: Vec<f64> = (0..samples)
            .map(|_| {
                let z = g();
                let e = match noise {
                    // A fixed jitter per measurement, whatever the batch.
                    Noise::Additive(sd) => sd * z,
                    // Jitter proportional to the batch, as a clock that
                    // drifts in rate produces.
                    Noise::Multiplicative(rel) => rel * clean * z,
                    // Correlated in time, which is what breaks sd/sqrt(n)
                    // and what batch means exist to survive.
                    Noise::Ar1(rel, phi) => {
                        prev = phi * prev + (1.0f64 - phi * phi).sqrt() * z;
                        rel * clean * prev
                    }
                    // Rare, large, one-sided: a scheduler tick landing in
                    // the batch. Probability grows with batch length.
                    Noise::MulTick(rel, period_ns, hit_ns) => {
                        // Ticks are a periodic timer interrupt, not a
                        // Poisson process. With CONFIG_HZ=1000 they arrive
                        // every millisecond, so a batch of length T with a
                        // random phase contains floor(T/P + u) of them.
                        //
                        // The difference from Poisson is not cosmetic. The
                        // count here varies by at most one however long the
                        // batch, so the contamination's *variance* stays
                        // bounded while its mean grows with T. A short batch
                        // therefore gets a 0-or-1 outlier - visible, and
                        // trimmable - and a long batch gets a near-constant
                        // tax with no outlier to find. Poisson would have
                        // the spread grow as sqrt(T) and would make long
                        // batches look noisier and more detectable than
                        // they really are.
                        let hits = (clean / period_ns + uni()).floor();
                        rel * clean * z + hits * hit_ns
                    }
                };
                (clean + e).max(1.0)
            })
            .collect();
        rungs.push(Rung { n, batch_ns, overhead_ns: 370.0 });
        if clean > ceiling_ns || n > 1 << 30 {
            break;
        }
        n *= 2;
    }
    Tape { workload: name.to_string(), rungs }
}

/// Rounds written per synthetic recording.
///
/// Each workload visits one rung per round, so a rung collects roughly
/// `SELFTEST_ROUNDS / rungs` samples - about 2800 across a fourteen-rung
/// ladder. That has to be generous enough that the analyzer's trials do not
/// run off the end of the recording and start re-reading noise they have
/// already seen, since a cell that wraps reports nothing.
const SELFTEST_ROUNDS: usize = 40000;
/// Noise models for synthetic ladders, each isolating one thing that goes
/// wrong with real timings.
#[derive(Clone, Copy)]
enum Noise {
    /// Fixed jitter per measurement, whatever the batch size.
    Additive(f64),
    /// Jitter proportional to the batch, as a clock drifting in rate gives.
    Multiplicative(f64),
    /// Correlated in time: what breaks sd/sqrt(n), and what batch means
    /// exists to survive.
    Ar1(f64, f64),
    /// Baseline multiplicative noise *plus* periodic scheduler ticks. The
    /// baseline matters: without it a fit converges the moment its error
    /// estimate hits zero, takes a handful of samples, and never meets a
    /// tick - which looks like an unbiased estimator and is an untested one.
    MulTick(f64, f64, f64),
}

impl Noise {
    fn label(&self) -> String {
        match self {
            Noise::Additive(sd) => format!("additive {sd:.0}ns"),
            Noise::Multiplicative(r) => format!("multiplicative {:.1}%", 100.0 * r),
            Noise::Ar1(r, phi) => format!("ar1 {:.1}% phi={phi}", 100.0 * r),
            Noise::MulTick(rel, p, hit) => format!(
                "multiplicative {:.1}% + {:.0}ns ticks every {:.0}us",
                100.0 * rel, hit, p / 1e3
            ),
        }
    }
}

/// Write synthetic recordings whose true answer is stated in the name.
///
/// The self-test used to build ladders in memory, score them itself, and
/// print its own table - a second analyzer, with its own opinions about what
/// to report, kept alongside the real one. So a bug in `analyze` could not
/// be caught by `selftest`, and a bug in the recording format could not be
/// caught by either, because nothing was ever written down.
///
/// Now it only generates. Each fake workload is named for the per-iteration
/// cost it was built with - `2.5ns-mul2pct` really is 2.5 ns an iteration
/// with 2% multiplicative noise - so running `analyze` over the output puts
/// the claimed truth next to the right answer on the same line. The format
/// round-trip stops being a separate test and becomes a condition of the
/// whole thing working: if `Timing::write` or `Run::load` mangled anything,
/// every reported truth would disagree with its own label.
pub fn selftest(dir: &str) {
    if let Err(e) = std::fs::create_dir_all(dir) {
        eprintln!("could not create {dir}: {e}");
        std::process::exit(2);
    }
    // Fixed cost of 370ns per measurement, which is what the real harness
    // costs outside the timer; see `Rung::overhead_ns`.
    const A: f64 = 370.0;
    let cases: [(&str, f64, Noise); 7] = [
        // No noise at all: every estimator must return the name exactly,
        // and anything else is an arithmetic bug that no amount of real
        // machine time would expose.
        ("2.5ns-clean", 2.5, Noise::Additive(0.0)),
        ("2.5ns-mul2pct", 2.5, Noise::Multiplicative(0.02)),
        ("2.5ns-add150ns", 2.5, Noise::Additive(150.0)),
        ("2.5ns-ar1phi8", 2.5, Noise::Ar1(0.02, 0.8)),
        ("2.5ns-ticks", 2.5, Noise::MulTick(0.02, 1e6, 5000.0)),
        ("250ns-mul2pct", 250.0, Noise::Multiplicative(0.02)),
        ("250ns-ar1phi8", 250.0, Noise::Ar1(0.02, 0.8)),
    ];

    let mut seed = 0x243F6A8885A308D3u64;
    let tapes: Vec<Tape> = cases
        .iter()
        .map(|(name, b, noise)| {
            seed = crate::step(seed);
            // Samples per rung, not per recording: a workload visits one
            // rung per round.
            let per_rung = SELFTEST_ROUNDS / 8;
            synthetic(name, A, *b, *noise, per_rung, seed, crate::RUNG_MAX_NS)
        })
        .collect();

    let mut t = crate::timing::Timing::new();
    for tape in &tapes {
        for (k, r) in tape.rungs.iter().enumerate() {
            t.rungs.insert(
                rung_name(&tape.workload, k),
                crate::timing::RungMeta {
                    n: r.n,
                    overhead_ns: r.overhead_ns,
                },
            );
        }
    }

    // Emitted exactly as the runner emits: one sample per workload per
    // round, at a rung drawn from a shuffled sweep of that workload's own
    // ladder, in a fresh slot order each round. A recording that is laid
    // out differently from a real one would let the analyzer pass here and
    // fail on the machine.
    let path = format!("{dir}/synthetic.bin");
    let idx = t.open(&path);
    let mut cursor: Vec<Vec<usize>> = tapes.iter().map(|x| vec![0; x.rungs.len()]).collect();
    let mut queue: Vec<Vec<usize>> = vec![Vec::new(); tapes.len()];
    let mut rng = 0x9E3779B97F4A7C15u64;
    let mut short = 0usize;
    for _round in 0..SELFTEST_ROUNDS {
        let mut order: Vec<usize> = (0..tapes.len()).collect();
        for i in (1..order.len()).rev() {
            rng = crate::step(rng);
            order.swap(i, (rng >> 33) as usize % (i + 1));
        }
        for &w in order.iter() {
            if queue[w].is_empty() {
                queue[w] = (0..tapes[w].rungs.len()).collect();
                for i in (1..queue[w].len()).rev() {
                    rng = crate::step(rng);
                    let j = (rng >> 33) as usize % (i + 1);
                    queue[w].swap(i, j);
                }
            }
            let k = queue[w].pop().unwrap();
            let rung = &tapes[w].rungs[k];
            if cursor[w][k] >= rung.batch_ns.len() {
                short += 1;
                continue;
            }
            let ns = rung.batch_ns[cursor[w][k]];
            cursor[w][k] += 1;
            let Some(&i) = idx.get(&rung_name(&tapes[w].workload, k)) else {
                continue;
            };
            t.time(i, || ns);
        }
    }
    if short > 0 {
        eprintln!("note: ran out of generated samples {short} times; rungs are uneven");
    }

    t.finish();
    println!(
        "wrote {} samples to {path}\n\n\
         Each workload is named for the per-iteration cost it was built with, so\n\
         `lab analyze {path}` should report a truth matching every name:\n",
        t.written
    );
    for (name, b, noise) in cases {
        println!("  {name:>16}  true {b} ns/iter, {}", noise.label());
    }
    println!("\n  lab analyze {path}");
}

/// Does a workload's cost move with the company it keeps?
///
/// This is what the powerset is for, and nothing else answers it. Every
/// other question here - which rung, which estimator, when to stop - is
/// about measuring *a* number well. This one asks whether the number is a
/// property of the workload at all, or of the round it was measured in.
///
/// Each recording holds one fixed composition, so the comparison is between
/// files: the same workload, measured in different company, against the same
/// truth estimator. Where a workload was measured alone that is the control.
///
/// Repeated passes over the same composition give the other half of it. A
/// difference between compositions only means something if it is larger than
/// the difference between two measurements of the *same* composition, so
/// both are reported side by side.
pub fn compositions(paths: &[String]) {
    // workload -> composition -> truths, one per pass
    let mut by: std::collections::BTreeMap<String, std::collections::BTreeMap<String, Vec<f64>>> =
        Default::default();
    for path in paths {
        let Some(run) = Run::load(path) else {
            continue;
        };
        let tapes = self::tapes(&run);
        // The composition is who shared the round. cpu_canary is in every
        // round structurally, so naming it would say nothing.
        let mut present: Vec<String> = tapes
            .iter()
            .map(|t| t.workload.clone())
            .filter(|w| w != "cpu_canary")
            .collect();
        present.sort();
        let composition = present.join("+");
        for tape in &tapes {
            let (t, _) = truth(tape);
            if t.is_finite() && t > 0.0 {
                by.entry(tape.workload.clone())
                    .or_default()
                    .entry(composition.clone())
                    .or_default()
                    .push(t);
            }
        }
    }

    println!(
        "Does a workload's cost depend on what shares its round?\n\n\
         Each row is one composition. `vs alone` compares against the same workload\n\
         measured by itself; `repeat` is the spread between passes over that same\n\
         composition, which is how much disagreement means nothing.\n"
    );

    for (workload, comps) in &by {
        // The control is this workload with no company. `cpu_canary` never
        // has that - it is in every round structurally - so for it the
        // smallest composition stands in, which understates its
        // sensitivity rather than overstating it: the baseline already has
        // a neighbour in it.
        let (label, alone) = match comps.get(workload.as_str()) {
            Some(v) => ("alone", v),
            None => {
                let Some((c, v)) = comps
                    .iter()
                    .min_by_key(|(c, _)| c.matches('+').count())
                else {
                    continue;
                };
                (c.as_str(), v)
            }
        };
        let base = median_of(alone);
        println!(
            "===== {workload} =====  {label} {base:.4} ns/iter over {} pass(es)",
            alone.len()
        );
        println!(
            "  {:>52} {:>12} {:>9} {:>8}",
            "composition", "truth", "vs alone", "repeat"
        );
        let mut rows: Vec<(f64, String, f64, f64, usize)> = comps
            .iter()
            .map(|(c, v)| {
                let m = median_of(v);
                let rel = 100.0 * (m - base) / base;
                (rel.abs(), c.clone(), m, rel, v.len())
            })
            .collect();
        rows.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
        for (_, c, m, rel, n) in rows.iter().take(12) {
            let v = &comps[c];
            let repeat = if v.len() > 1 {
                let lo = v.iter().cloned().fold(f64::INFINITY, f64::min);
                let hi = v.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
                format!("{:.2}%", 100.0 * (hi - lo) / m)
            } else {
                "  -".to_string()
            };
            let _ = n;
            let short = if c.len() > 52 { &c[c.len() - 52..] } else { c };
            println!("  {short:>52} {m:>12.4} {rel:>+8.2}% {repeat:>8}");
        }
        println!();
    }
}

/// How much a workload's cost changed between the start and end of a
/// recording, as a percentage, median over its rungs.
fn drift_of(tape: &Tape) -> f64 {
    let mut per_rung: Vec<f64> = Vec::new();
    for r in &tape.rungs {
        let n = r.batch_ns.len();
        if n < 60 {
            continue;
        }
        let third = n / 3;
        let trimmed = |v: &[f64]| -> f64 {
            let mut w = v.to_vec();
            w.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let cut = w.len() / 4;
            let s = &w[cut..w.len() - cut];
            s.iter().sum::<f64>() / s.len() as f64
        };
        let first = trimmed(&r.batch_ns[..third]);
        let last = trimmed(&r.batch_ns[n - third..]);
        if first > 0.0 {
            per_rung.push(100.0 * (last - first) / first);
        }
    }
    median_of(&per_rung)
}

fn median_of(v: &[f64]) -> f64 {
    let mut w = v.to_vec();
    w.sort_by(|a, b| a.partial_cmp(b).unwrap());
    if w.is_empty() {
        f64::NAN
    } else {
        w[w.len() / 2]
    }
}
