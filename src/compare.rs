//! Timing a benchmark that does not take a size: [`bench`] and its
//! variants, and the sampling loop behind them.
//!
//! The loop keeps taking samples until the standard error of the mean is
//! small enough to meet the caller's accuracy target, or until the time
//! budget runs out; see [`Config`] for the target and [`Stats`] for what
//! comes back.

use super::*;
use std::fmt::{self, Display, Formatter};
use std::sync::atomic::Ordering::Relaxed;
use std::time::{Duration, Instant};

/// Never stop *voluntarily* on fewer samples than this.
///
/// A standard deviation estimated from `k` points is itself uncertain by
/// roughly `1/sqrt(2(k-1))` - about 32% at `k = 6`, and over 70% at
/// `k = 2`. Stopping the instant a noisy estimate happens to dip below the
/// target would systematically favour the runs that got lucky, so we
/// require a handful of samples before believing the standard error at all.
///
/// Note the emphasis: this is a floor on *concluding we are done*, not on
/// reporting. The selection effect it defends against exists only when the
/// standard error is the thing that stops us. If instead
/// [`Config::max_time`] runs out first - which is what happens to a slow
/// function on a short budget - nothing has been selected for, and the
/// error bar from the three or four samples we did manage is honest, wide,
/// and a good deal more use than none at all. So a budget-forced stop
/// reports whatever standard error it has (and sets [`Stats::hit_limit`]).
///
/// Not a knob: callers control accuracy with [`Config::accuracy`]
/// and cost with [`Config::max_time`], and no useful benchmark wants a
/// different answer here.
const MIN_SAMPLES: usize = 6;

/// How long one sample should take: calibration picks a batch size aiming
/// for this.
///
/// Long enough that the two `Instant::now()` calls bracketing a sample -
/// on the order of 100 ns together - stay a rounding error against it, at
/// about 0.1%, which is far below any accuracy worth asking for.
///
/// It was 1ms, and shortening it is nearly free. A benchmark stops when the
/// standard error of the mean is small enough, and that error is set by the
/// spread *between* samples, which for the benchmarks measured here is
/// dominated by drift the batch size does not affect - so the same number
/// of samples is needed either way, and each one costs a tenth as much:
///
/// ```none
///                    reported at 1ms / at 100us      wall at 1ms / at 100us
///   empty closure         0.5921 / 0.5933 ns             8.9 / 1.5 ms
///   ~3ns of arithmetic    2.6236 / 2.6594 ns            13.2 / 1.4 ms
///   ~2.9us of arithmetic  2880.7 / 2883.6 ns            10.7 / 1.4 ms
///   noisy 1.7us workload  1753.2 / 1766.1 ns            10.3 / 5.9 ms
/// ```
///
/// The answers are unchanged and the run-to-run spread is no worse; only
/// the cost moves. The exception is [`bench_env`] and [`bench_gen_env`],
/// which report about 20% lower, because a smaller batch means a smaller
/// environment vector to index into - that lookup is harness overhead
/// rather than the benchmark, so measuring less of it is a gain, but it is
/// a visible change in what those two report.
const SAMPLE_TIME: Duration = Duration::from_micros(100);

/// A backstop on the number of samples, so the vector of them cannot grow
/// without bound.
///
/// This is about memory, not about the measurement: `max_time` is the real
/// budget, and at [`SAMPLE_TIME`] it allows ~100_000 samples, ten times
/// below this.
const MAX_SAMPLES: usize = 1_000_000;

/// Statistics for a benchmark run.
#[derive(Debug, PartialEq, Clone)]
pub struct Comparison {
    /// The [`Stats`] for the old version.
    pub old: Stats,
    /// The [`Stats`] for the new version.
    pub new: Stats,
}

impl Comparison {
    pub fn difference_ns(&self) -> f64 {
        self.new.ns_per_iter - self.old.ns_per_iter
    }
    pub fn std_error(&self) -> f64 {
        (self.new.std_error.powi(2) - self.old.std_error.powi(2)).sqrt()
    }
    /// Determines whether this changed.
    ///
    /// There should be a 5% chance of false positives, if `num_comparisons_planned` is set correctly.
    pub fn is_changed(&self) -> bool {
        crate::significant::is_significant(self.difference_ns(), self.std_error(), 0.05)
    }
}

impl Display for Comparison {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        let is_changed = self.is_changed();
        if is_changed {
            write!(f, "(unchanged)")
        } else {
            let percent_change = self.difference_ns()/self.old.ns_per_iter*100.0;
            let rel_error = self.std_error()/self.old.ns_per_iter*100.0;
            write!(f, "{percent_change:+.1}% +/- {rel_error:.1}%")
        }
    }
}

impl Config {
    /// Run a benchmark.
    ///
    /// See [`bench`] for the default-accuracy version, and
    /// [`Config::bench_gen_env`] for the algorithm.
    pub fn compare<OLD, NEW, O>(&self, mut f_old: OLD, mut f_new: NEW) -> Comparison
    where
        OLD: FnMut() -> O,
        NEW: FnMut() -> O,
    {
        self.compare_env((), |_| f_old(), |_| f_new())
    }

    /// Run a benchmark with an environment.
    ///
    /// The value `env` is a clonable prototype for the "benchmark
    /// environment". Each iteration receives a freshly-cloned mutable copy
    /// of this environment. The time taken to clone the environment is not
    /// included in the results.
    ///
    /// Nb: it's very possible that we will end up allocating many (>10,000)
    /// copies of `env` at the same time. Probably best to keep it small.
    ///
    /// See [`Config::bench_gen_env`] and the module docs for more.
    ///
    /// ## Overhead
    ///
    /// Every iteration, `bench_env` performs a lookup into a big vector in
    /// order to get the environment for that iteration. If your benchmark
    /// is memory-intensive then this could, in the worst case, amount to a
    /// systematic cache-miss (ie. this vector would have to be fetched from
    /// DRAM at the start of every iteration). In this case the results could
    /// be affected by a hundred nanoseconds. This is a worst-case scenario
    /// however, and I haven't actually been able to trigger it in
    /// practice... but it's good to be aware of the possibility.
    pub fn compare_env<OLD, NEW, I, O>(&self, env: I, f_old: OLD, f_new: NEW) -> Comparison
    where
        OLD: FnMut(&mut I) -> O,
        NEW: FnMut(&mut I) -> O,
        I: Clone,
    {
        self.compare_gen_env(move || env.clone(), f_old, f_new)
    }

    /// Run a benchmark with a generated environment.
    ///
    /// The function `gen_env` creates the "benchmark environment" for the
    /// computation. Each iteration receives a freshly-created environment.
    /// The time taken to create the environment is not included in the
    /// results.
    ///
    /// Nb: it's very possible that we will end up generating many (>10,000)
    /// copies of `env` at the same time. Probably best to keep it small.
    ///
    /// See `bench` and the module docs for more.
    ///
    /// ## Overhead
    ///
    /// Every iteration, `bench_gen_env` performs a lookup into a big vector
    /// in order to get the environment for that iteration. If your
    /// benchmark is memory-intensive then this could, in the worst case,
    /// amount to a systematic cache-miss (ie. this vector would have to be
    /// fetched from DRAM at the start of every iteration). In this case the
    /// results could be affected by a hundred nanoseconds. This is a
    /// worst-case scenario however, and I haven't actually been able to
    /// trigger it in practice... but it's good to be aware of the
    /// possibility.
    ///
    /// # Algorithm
    ///
    /// 1. **Calibrate.** Find the smallest batch size `unit` whose measured
    ///    duration reaches `sample_time`, extrapolating multiplicatively
    ///    from the last probe (clamped to [2x, 100x] per step) so a
    ///    nanosecond-scale function reaches its batch size in a handful of
    ///    probes.
    /// 2. **Sample.** Repeatedly time a batch of exactly `unit` iterations,
    ///    recording the per-iteration time `x_j = t_j / unit`. The batch
    ///    from step 1 that first reached `sample_time` is reused as the
    ///    warmup sample and discarded, rather than measured again.
    /// 3. **Stop** once there are at least `MIN_SAMPLES` samples *and*
    ///    the `accuracy` target is met, where
    ///    `stderr(x) = sd(x) / sqrt(k)` with a Bessel-corrected `sd`. If
    ///    `max_time` runs out first, stop anyway, set `hit_limit`, and
    ///    report the standard error from however many samples were
    ///    collected - two is enough for one to exist, and a wide honest
    ///    error bar beats none.
    ///
    /// Because each `x_j` already averages `unit` iterations,
    /// `sd(x) = sigma_iter / sqrt(unit)`, so the standard error of the mean
    /// here equals `sigma_iter / sqrt(k * unit)` - the standard error over
    /// all `k * unit` raw iterations. The stopping rule is therefore
    /// correct regardless of what `unit` calibration picked, and needs no
    /// assumption about the shape of the noise: a randomized-input
    /// benchmark has `var(batch) ∝ unit` while a deterministic one has
    /// roughly constant per-sample jitter, and this estimator is right for
    /// both.
    pub fn compare_gen_env<G, OLD, NEW, I, O>(
        &self,
        mut gen_env: G,
        mut f_old: OLD,
        mut f_new: NEW,
    ) -> Comparison
    where
        G: FnMut() -> I,
        OLD: FnMut(&mut I) -> O,
        NEW: FnMut(&mut I) -> O,
    {
        crate::significant::NUM_MEASUREMENTS_PLANNED
            .fetch_max(self.num_comparisons_planned, Relaxed);
        crate::significant::NUM_MEASUREMENTS.fetch_add(1, Relaxed);
        quiet::pin_if_requested();
        let start = Instant::now();
        let mut xs: Vec<I> = Vec::new();
        let (unit, old_ns, new_ns, probed) =
            calibrate(&mut gen_env, &mut f_old, &mut f_new, &mut xs, self, start);
        if start.elapsed() > self.max_time {
            // Even the single calibration probe blew the whole time budget
            // (an extremely slow benchmark): report it directly rather
            // than paying for a second full-length call just to "warm up".
            return Comparison {
                old: Stats {
                    ns_per_iter: old_ns / unit as f64,
                    std_error: f64::NAN,
                    iterations: probed,
                    samples: 1,
                    hit_limit: true,
                    untrustworthy: true,
                },
                new: Stats {
                    ns_per_iter: new_ns / unit as f64,
                    std_error: f64::NAN,
                    iterations: probed,
                    samples: 1,
                    hit_limit: true,
                    untrustworthy: true,
                },
            };
        }
        // Otherwise the probe that finished calibration serves as the
        // warmup sample and is discarded.

        let mut old_samples = Running::default();
        let mut new_samples = Running::default();
        loop {
            let (_, old_t) = time_batch(&mut gen_env, &mut f_old, &mut xs, unit);
            let (_, new_t) = time_batch(&mut gen_env, &mut f_new, &mut xs, unit);
            old_samples.push(old_t / unit as f64);
            new_samples.push(new_t / unit as f64);

            let (old_mean, old_std_error) = old_samples.mean_and_stderr();
            let (new_mean, new_std_error) = new_samples.mean_and_stderr();

            let out_of_budget = old_samples.count >= MAX_SAMPLES || start.elapsed() > self.max_time;
            // `MIN_SAMPLES` gates only the *voluntary* stop. Its job is to
            // stop us concluding from a standard error so noisy it might
            // have dipped below the target by luck - a hazard that exists
            // only when the standard error is what makes us stop. When the
            // budget is what makes us stop, that selection effect is absent,
            // so we report the error bar we have (wide, and honestly so)
            // rather than discarding it. A slow function with a short
            // `max_time` may only fit three or four samples, and three
            // samples' worth of error bar beats none.
            let std_error = (old_std_error.powi(2) + new_std_error.powi(2)).sqrt();
            let precise_enough = old_samples.count >= MIN_SAMPLES
                && self.accuracy_met(old_mean.min(new_mean), std_error);
            if precise_enough || out_of_budget {
                return Comparison {
                    old: Stats {
                        ns_per_iter: old_mean,
                        std_error: old_std_error,
                        iterations: probed + old_samples.count as u64 * unit as u64,
                        samples: old_samples.count,
                        hit_limit: !precise_enough,
                        untrustworthy: old_samples.count < MIN_SAMPLES,
                    },
                    new: Stats {
                        ns_per_iter: new_mean,
                        std_error: new_std_error,
                        iterations: probed + new_samples.count as u64 * unit as u64,
                        samples: new_samples.count,
                        hit_limit: !precise_enough,
                        untrustworthy: new_samples.count < MIN_SAMPLES,
                    },
                };
            }
        }
    }
}

/// Find a batch size whose measured duration reaches `cfg.sample_time`.
/// Returns the batch size and the duration (in nanoseconds) of the probe
/// that reached it, so that probe can be reused as the warmup sample
/// instead of being measured a second time. `xs` is the same reusable
/// scratch buffer described on [`time_batch`].
fn calibrate<G, OLD, NEW, I, O>(
    gen_env: &mut G,
    f_old: &mut OLD,
    f_new: &mut NEW,
    xs: &mut Vec<I>,
    cfg: &Config,
    start: Instant,
) -> (usize, f64, f64, u64)
where
    G: FnMut() -> I,
    OLD: FnMut(&mut I) -> O,
    NEW: FnMut(&mut I) -> O,
{
    // A ceiling on the *total* cost of one probe, setup as well as timing.
    // Ordinarily the extrapolation below is driven by the timed portion
    // approaching `SAMPLE_TIME`, but when `f`'s cost is optimised away (see
    // the module docs' "Pure functions" caveat, e.g. `bench_env(v, |_| {})`)
    // that portion never grows however large `unit` gets - while untimed
    // environment construction does, unboundedly, and before the
    // `start.elapsed() > cfg.max_time` check below can ever run, since the
    // allocation is itself what takes the time. A hundredth of `max_time`
    // rather than some large fraction of it, to bound memory as well: on
    // fast hardware a looser ceiling buys proportionally more allocation
    // before it fires.
    let probe_ceiling_ns = (cfg.max_time / 100)
        .max(Duration::from_millis(5))
        .as_secs_f64()
        * 1e9;
    // Two more ceilings on `unit`, needing no timing at all, whichever is
    // smaller. `MAX_CALIBRATION_UNIT` covers what no clock can see: with
    // `f` *and* the environment both trivial (`bench(|| {})`, `I` of `()`)
    // the optimiser can delete the whole batch, so `setup_ns` and `t` read
    // as ~0 however large `unit` grows. `MAX_CALIBRATION_BYTES` covers an
    // `I` whose per-clone cost is real but too small for `probe_ceiling_ns`
    // to catch before millions of copies - an array, a plain struct - since
    // `size_of` sees a `Vec` or `String` as its inline handle only. That
    // last case is left to the wall-clock ceiling above, which bounds it
    // only indirectly: between the three every `I` has some backstop and
    // none has a hard guarantee, so keep environments small.
    const MAX_CALIBRATION_UNIT: usize = 2_000_000;
    const MAX_CALIBRATION_BYTES: usize = 64 * 1024 * 1024;
    let unit_cap =
        MAX_CALIBRATION_UNIT.min(MAX_CALIBRATION_BYTES / std::mem::size_of::<I>().max(1));
    let target = SAMPLE_TIME.as_secs_f64() * 1e9;
    let mut unit = 1usize;
    // Every probe really does run the benchmark, so they count towards
    // `Stats::iterations` even though their timings are discarded.
    let mut probed = 0u64;
    loop {
        let (old_setup_ns, old_t) = time_batch(gen_env, f_old, xs, unit);
        let (new_setup_ns, new_t) = time_batch(gen_env, f_new, xs, unit);
        probed += unit as u64;
        let total_ns = old_setup_ns + new_setup_ns + old_t + new_t;
        // Accept immediately, without ever retrying at this size, as soon
        // as *any* ceiling is reached: `t >= target` is the ordinary case,
        // `total_ns >= probe_ceiling_ns` is what saves us when construction
        // dominates, and `unit >= unit_cap` is the timing-blind backstop
        // above. Retrying here (rather than accepting) would just re-pay
        // the same large cost for no benefit.
        if old_t + new_t >= target
            || total_ns >= probe_ceiling_ns
            || unit >= unit_cap
            || start.elapsed() > cfg.max_time
        {
            return (unit, old_t, new_t, probed);
        }
        // Extrapolate from whichever cost is closer to its own ceiling: the
        // timed portion approaching `target`, or the *total* probe cost
        // approaching `probe_ceiling_ns`. Both factors are ceilings on how
        // much bigger the *next* probe should be, so growth decelerates
        // smoothly as either limit is approached instead of overshooting
        // it by up to 100x.
        let factor_time = (target / (old_t + new_t).max(1.0)).clamp(2.0, 100.0);
        let factor_safety = (probe_ceiling_ns / total_ns.max(1.0)).max(1.0);
        let factor = factor_time.min(factor_safety);
        unit = ((unit as f64 * factor).ceil() as usize)
            .max(unit + 1)
            .min(unit_cap);
    }
}
