//! Timing a benchmark that does not take a size: [`bench`] and its
//! variants, and the sampling loop behind them.
//!
//! The loop keeps taking samples until the standard error of the mean is
//! small enough to meet the caller's accuracy target, or until the time
//! budget runs out; see [`Config`] for the target and [`Stats`] for what
//! comes back.

use super::*;
use std::fmt::{self, Display, Formatter};
use std::hint::black_box;
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
pub struct Stats {
    /// The time, in nanoseconds, per iteration.
    pub ns_per_iter: f64,
    /// Standard error of `ns_per_iter`, in nanoseconds - the figure shown
    /// after the `±`.
    ///
    /// This is the standard error *of sampling* seen within this call: the
    /// benchmark's own run-to-run variability plus timer noise. It is *not*
    /// a bound on systematic differences between separate process runs —
    /// CPU frequency state, code layout, and cache/allocator state shift
    /// between runs, and no statistic computed inside one call can see
    /// them. A very small `std_error` on a deterministic benchmark means
    /// sampling is no longer the limit, not that the number is accurate to
    /// that many digits. `NaN` when fewer than 2 samples were collected
    /// (see `hit_limit`), where a standard error cannot exist.
    ///
    /// [`Stats::rel_std_error`] gives the same figure as a fraction.
    pub std_error: f64,
    /// How many times the benchmarked code was actually run, including the
    /// calibration probes whose timings were discarded.
    ///
    /// `u64` rather than `usize` because this is a count rather than
    /// anything memory-sized: a nanosecond-scale benchmark can legitimately
    /// run past 4.3 billion iterations within its time budget, which a
    /// 32-bit `usize` could not hold.
    pub iterations: u64,
    /// How many samples were taken (ie. how many times we allocated the
    /// environment and measured the time).
    pub samples: usize,
    /// `true` if the benchmark ran out of time before reaching its accuracy
    /// target: the answer is real, just less precise than you asked for.
    ///
    /// Look at `std_error` to see how much less. Distinct from
    /// [`Stats::untrustworthy`], which is about whether to believe
    /// `std_error` in the first place.
    pub hit_limit: bool,
    /// `true` if too few samples were collected for the standard error
    /// itself to be worth believing.
    ///
    /// A standard error estimated from two or three samples is so noisy
    /// that it may look small purely by luck, so this says "the `±` on this
    /// line is not evidence of anything", regardless of how tight it
    /// appears. It is a different question from `hit_limit`: a slow
    /// function on a short budget sets both, but a fast noisy one that
    /// simply needed longer sets only `hit_limit`, and its error bar is
    /// perfectly believable - just wider than requested.
    pub untrustworthy: bool,
}

impl Stats {
    /// Standard error as a fraction of the measurement (0.01 = 1%).
    ///
    /// `NaN` when [`Stats::std_error`] is, and also when `ns_per_iter` is
    /// zero, where a relative error is undefined.
    pub fn rel_std_error(&self) -> f64 {
        self.std_error / self.ns_per_iter
    }
}


impl Display for Stats {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        // Report the error bar in the *same* unit as the measurement, even
        // when that means leading zeroes. The point of an error bar is to
        // let a reader tell at a glance whether two results differ by more
        // than their uncertainty, and that is a direct digit-for-digit
        // comparison when the units match - whereas "100.2673ms ± 20.05µs"
        // makes them do a unit conversion in their head first, and
        // "± 0.02%" makes them do arithmetic.
        let (div, unit) = unit_for(self.ns_per_iter);
        // Two separate things can be wrong with a line, so they get two
        // separate marks: `(limit)` means the answer is less precise than
        // requested, `(untrusted)` means the `±` itself is not worth
        // reading. A slow function on a short budget earns both.
        let limit = match (self.hit_limit, self.untrustworthy) {
            (true, true) => " (limit, untrusted)",
            (true, false) => " (limit)",
            (false, true) => " (untrusted)",
            (false, false) => "",
        };
        if self.std_error.is_nan() {
            // `Running::mean_and_stderr` gives NaN for exactly one reason:
            // fewer than two samples to estimate a standard error from,
            // only possible via the single-sample "blew the whole time
            // budget already" path.
            //
            // With no error bar there is nothing to set the precision, so
            // fall back to a fixed four decimals.
            let value = format!("{:.4}{}", self.ns_per_iter / div, unit);
            write!(
                f,
                "{value:>11} (± unknown, only {} sample{}){limit}",
                self.samples,
                if self.samples == 1 { "" } else { "s" }
            )
        } else {
            let (value, error) = value_and_error(self.ns_per_iter / div, self.std_error / div);
            let value = format!("{value}{unit}");
            let error = format!("{error}{unit}");
            // Deliberately no iteration or sample count. Those were worth
            // showing when the only quality signal was an R², which says
            // nothing about how well the answer is known; now that the `±`
            // states the precision outright they are just noise on a line
            // meant to be scanned in a column. Both remain on [`Stats`] for
            // anyone who wants them.
            write!(f, "{value:>11} ± {error}{limit}")
        }
    }
}


/// Run a benchmark, with default accuracy (see [`Config`]).
///
/// The return value of `f` is not used, but we trick the optimiser into
/// thinking we're going to use it. Make sure to return enough information
/// to prevent the optimiser from eliminating code from your benchmark! (See
/// the module docs for more.)
pub fn bench<F, O>(f: F) -> Stats
where
    F: FnMut() -> O,
{
    Config::default().bench(f)
}

/// Run a benchmark with an environment, with default accuracy (see
/// [`Config`]).
///
/// See [`Config::bench_env`] for the full documentation.
pub fn bench_env<F, I, O>(env: I, f: F) -> Stats
where
    F: FnMut(&mut I) -> O,
    I: Clone,
{
    Config::default().bench_env(env, f)
}

/// Run a benchmark with a generated environment, with default
/// accuracy (see [`Config`]).
///
/// See [`Config::bench_gen_env`] for the full documentation.
pub fn bench_gen_env<G, F, I, O>(gen_env: G, f: F) -> Stats
where
    G: FnMut() -> I,
    F: FnMut(&mut I) -> O,
{
    Config::default().bench_gen_env(gen_env, f)
}


impl Config {
    /// Run a benchmark.
    ///
    /// See [`bench`] for the default-accuracy version, and
    /// [`Config::bench_gen_env`] for the algorithm.
    pub fn bench<F, O>(&self, mut f: F) -> Stats
    where
        F: FnMut() -> O,
    {
        self.bench_env((), |_| f())
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
    pub fn bench_env<F, I, O>(&self, env: I, f: F) -> Stats
    where
        F: FnMut(&mut I) -> O,
        I: Clone,
    {
        self.bench_gen_env(move || env.clone(), f)
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
    pub fn bench_gen_env<G, F, I, O>(&self, mut gen_env: G, mut f: F) -> Stats
    where
        G: FnMut() -> I,
        F: FnMut(&mut I) -> O,
    {
        quiet::pin_if_requested();
        let start = Instant::now();
        let mut xs: Vec<I> = Vec::new();
        let (unit, first_ns, probed) = calibrate(&mut gen_env, &mut f, &mut xs, self, start);
        if start.elapsed() > self.max_time {
            // Even the single calibration probe blew the whole time budget
            // (an extremely slow benchmark): report it directly rather
            // than paying for a second full-length call just to "warm up".
            return Stats {
                ns_per_iter: first_ns / unit as f64,
                std_error: f64::NAN,
                iterations: probed,
                samples: 1,
                hit_limit: true,
                untrustworthy: true,
            };
        }
        // Otherwise the probe that finished calibration serves as the
        // warmup sample and is discarded.

        let mut samples = Running::default();
        loop {
            let (_, t) = time_batch(&mut gen_env, &mut f, &mut xs, unit);
            samples.push(t / unit as f64);
            let (mean, std_error) = samples.mean_and_stderr();

            let out_of_budget = samples.count >= MAX_SAMPLES || start.elapsed() > self.max_time;
            // `MIN_SAMPLES` gates only the *voluntary* stop. Its job is to
            // stop us concluding from a standard error so noisy it might
            // have dipped below the target by luck - a hazard that exists
            // only when the standard error is what makes us stop. When the
            // budget is what makes us stop, that selection effect is absent,
            // so we report the error bar we have (wide, and honestly so)
            // rather than discarding it. A slow function with a short
            // `max_time` may only fit three or four samples, and three
            // samples' worth of error bar beats none.
            let precise_enough =
                samples.count >= MIN_SAMPLES && self.accuracy_met(mean, std_error);
            if precise_enough || out_of_budget {
                return Stats {
                    ns_per_iter: mean,
                    std_error,
                    // Derived rather than accumulated, which keeps the
                    // arithmetic in u64 and out of the loop: `usize` would
                    // overflow on a 32-bit target, where a fast benchmark
                    // can legitimately run past 4.3 billion iterations.
                    iterations: probed + samples.count as u64 * unit as u64,
                    samples: samples.count,
                    hit_limit: !precise_enough,
                    // Two different complaints. Running out of clock leaves
                    // a wider error bar than asked for, but one that still
                    // means what it says; stopping below `MIN_SAMPLES`
                    // leaves an error bar too noisy to read at all.
                    untrustworthy: samples.count < MIN_SAMPLES,
                };
            }
        }
    }

}

/// Time `iters` back-to-back calls of `f`, each on its own freshly
/// generated environment. Returns `(setup_ns, timed_ns)`: the time spent
/// generating and collecting the `iters` environments (untimed, but still
/// real wall-clock cost that [`calibrate`] must account for so it cannot be
/// tricked by a benchmark whose timed cost is optimised away), and the time
/// spent actually running `f` over them. Environments are all created
/// before the clock for `timed_ns` starts and all dropped after it stops,
/// so neither generation nor drop pollutes `timed_ns` itself.
///
/// `xs` is a caller-owned scratch buffer, cleared and refilled here rather
/// than allocated fresh each call. When the same buffer is reused across
/// many same-sized calls (as the main sampling loop does once `calibrate`
/// has fixed `unit`), this turns what would otherwise be a repeated
/// allocate-then-free of a batch-sized buffer - for a large `unit`,
/// hundreds of megabytes, over and over - into a reused allocation that's
/// merely cleared and refilled. That matters beyond just being faster: this
/// crate's own test suite once demonstrated that heavy allocator churn from
/// one benchmark call can leave enough of a mark on process-wide allocator
/// state to detectably perturb the *timing* of an unrelated benchmark run
/// immediately afterward in the same process.
fn time_batch<G, F, I, O>(gen_env: &mut G, f: &mut F, xs: &mut Vec<I>, iters: usize) -> (f64, f64)
where
    G: FnMut() -> I,
    F: FnMut(&mut I) -> O,
{
    let setup_start = Instant::now();
    xs.clear();
    xs.extend(std::iter::repeat_with(&mut *gen_env).take(iters));
    let setup_ns = setup_start.elapsed().as_secs_f64() * 1e9;
    let start = Instant::now();
    // We iterate over `&mut *xs` rather than draining it, because we don't
    // want to drop the env values until after the clock has stopped.
    for x in &mut *xs {
        black_box(f(x));
    }
    let timed_ns = start.elapsed().as_secs_f64() * 1e9;
    (setup_ns, timed_ns)
}

/// Find a batch size whose measured duration reaches `cfg.sample_time`.
/// Returns the batch size and the duration (in nanoseconds) of the probe
/// that reached it, so that probe can be reused as the warmup sample
/// instead of being measured a second time. `xs` is the same reusable
/// scratch buffer described on [`time_batch`].
fn calibrate<G, F, I, O>(
    gen_env: &mut G,
    f: &mut F,
    xs: &mut Vec<I>,
    cfg: &Config,
    start: Instant,
) -> (usize, f64, u64)
where
    G: FnMut() -> I,
    F: FnMut(&mut I) -> O,
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
        let (setup_ns, t) = time_batch(gen_env, f, xs, unit);
        probed += unit as u64;
        let total_ns = setup_ns + t;
        // Accept immediately, without ever retrying at this size, as soon
        // as *any* ceiling is reached: `t >= target` is the ordinary case,
        // `total_ns >= probe_ceiling_ns` is what saves us when construction
        // dominates, and `unit >= unit_cap` is the timing-blind backstop
        // above. Retrying here (rather than accepting) would just re-pay
        // the same large cost for no benefit.
        if t >= target
            || total_ns >= probe_ceiling_ns
            || unit >= unit_cap
            || start.elapsed() > cfg.max_time
        {
            return (unit, t, probed);
        }
        // Extrapolate from whichever cost is closer to its own ceiling: the
        // timed portion approaching `target`, or the *total* probe cost
        // approaching `probe_ceiling_ns`. Both factors are ceilings on how
        // much bigger the *next* probe should be, so growth decelerates
        // smoothly as either limit is approached instead of overshooting
        // it by up to 100x.
        let factor_time = (target / t.max(1.0)).clamp(2.0, 100.0);
        let factor_safety = (probe_ceiling_ns / total_ns.max(1.0)).max(1.0);
        let factor = factor_time.min(factor_safety);
        unit = ((unit as f64 * factor).ceil() as usize)
            .max(unit + 1)
            .min(unit_cap);
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn very_quick() {
        println!();
        println!("very quick: {}", bench(|| {}));
    }

    #[test]
    fn very_slow() {
        println!();
        let stats = bench(|| thread::sleep(Duration::from_millis(400)));
        println!("very slow: {}", stats);
        assert!(stats.ns_per_iter > 399.0e6);
        // Deterministic, so we expect to stop as soon as `MIN_SAMPLES` is
        // reached rather than being forced further by the accuracy target.
        assert!(stats.samples >= 6);
    }

    #[test]
    fn painfully_slow() {
        println!();
        let stats = bench(|| thread::sleep(Duration::from_secs(11)));
        println!("painfully slow: {}", stats);
        println!("ns {}", stats.ns_per_iter);
        assert!(stats.ns_per_iter > 11.0e9);
        // A single call already blows the entire time budget, so we report
        // it directly rather than pay for a second 11-second call.
        assert_eq!(1, stats.iterations);
        assert_eq!(1, stats.samples);
        assert!(stats.hit_limit);
        // One sample is far too few to trust an error bar, and there is not
        // even one to report.
        assert!(stats.untrustworthy);
        assert!(stats.std_error.is_nan());
    }

    #[test]
    fn sadly_slow() {
        println!();
        let stats = bench(|| thread::sleep(Duration::from_secs(6)));
        println!("sadly slow: {}", stats);
        println!("ns {}", stats.ns_per_iter);
        assert!(stats.ns_per_iter > 6.0e9);
        // The calibration probe (whose timing is discarded as warmup) plus
        // one real sample already exceed the 10-second budget - and both of
        // them really did run the benchmark, so both are counted.
        assert_eq!(2, stats.iterations);
        assert_eq!(1, stats.samples);
        assert!(stats.hit_limit);
    }

    #[test]
    fn test_sleep() {
        println!();
        println!(
            "sleep 1 ms: {}",
            bench(|| thread::sleep(Duration::from_millis(1)))
        );
    }

    #[test]
    fn noop() {
        println!();
        println!("noop base: {}", bench(|| {}));
        println!("noop 0:    {}", bench_env(vec![0u64; 0], |_| {}));
        println!("noop 16:   {}", bench_env(vec![0u64; 16], |_| {}));
        println!("noop 64:   {}", bench_env(vec![0u64; 64], |_| {}));
        println!("noop 256:  {}", bench_env(vec![0u64; 256], |_| {}));
        println!("noop 512:  {}", bench_env(vec![0u64; 512], |_| {}));
    }

    #[test]
    fn ret_value() {
        println!();
        println!(
            "no ret 32:    {}",
            bench_env(vec![0u64; 32], |x| { x.clone() })
        );
        println!("return 32:    {}", bench_env(vec![0u64; 32], |x| x.clone()));
        println!(
            "no ret 256:   {}",
            bench_env(vec![0u64; 256], |x| { x.clone() })
        );
        println!(
            "return 256:   {}",
            bench_env(vec![0u64; 256], |x| x.clone())
        );
        println!(
            "no ret 1024:  {}",
            bench_env(vec![0u64; 1024], |x| { x.clone() })
        );
        println!(
            "return 1024:  {}",
            bench_env(vec![0u64; 1024], |x| x.clone())
        );
        println!(
            "no ret 4096:  {}",
            bench_env(vec![0u64; 4096], |x| { x.clone() })
        );
        println!(
            "return 4096:  {}",
            bench_env(vec![0u64; 4096], |x| x.clone())
        );
        println!(
            "no ret 50000: {}",
            bench_env(vec![0u64; 50000], |x| { x.clone() })
        );
        println!(
            "return 50000: {}",
            bench_env(vec![0u64; 50000], |x| x.clone())
        );
    }

    // Cheap deterministic PRNG so a failure reproduces from its seed.

    /// A benchmark with real, injected variance (coefficient of variation
    /// around 50%): a randomized amount of trivial work. Deterministic
    /// functions aren't a good fit for testing `rel_std_error`'s honesty -
    /// their spread is machine noise, not something the estimator controls -
    /// so these accuracy tests need randomized cost instead.
    fn variable_cost(seed: u64) -> impl FnMut() -> u64 {
        let mut rng = XorShift(seed | 1);
        move || {
            let n = 1 + (rng.next() % 2000) as usize;
            let mut acc = 0u64;
            for i in 0..n {
                acc = acc.wrapping_mul(31).wrapping_add(i as u64);
            }
            acc
        }
    }

    /// Distinct, well-spread seeds so repeats are independent. Reusing one
    /// seed across repeats would replay the same random sequence and
    /// understate the very spread these tests measure.
    fn seed_for(repeat: usize) -> u64 {
        0x9e37_79b9_7f4a_7c15u64.wrapping_mul(repeat as u64 + 1) | 1
    }

    /// Whether this machine is quiesced enough for a calibration check to
    /// mean anything.
    ///
    /// The tests that compare a claimed error bar against the spread
    /// actually observed across repeated runs are measuring *between-run*
    /// variation, which is precisely what no within-run statistic can see -
    /// it is the documented limit of `std_error`. On a machine that is
    /// merely idle rather than reserved, that variation swamps what is
    /// being tested: measured 12.2x on the flat check and 5.2x on the
    /// scaling one, against 1.25x when pinned. So these skip rather than
    /// fail, and are exercised by running the suite under
    /// `quiet-bench run` - which is what `quiet-bench` is for.


    #[test]
    fn accuracy_matches_the_request() {
        println!();
        if !quiesced() {
            println!("SKIPPED: machine is not quiesced (see `quiet-bench reserve`)");
            return;
        }
        const REPEATS: usize = 15;
        for &target in &[0.05, 0.01] {
            let cfg = Config {
                target_rel_error: target,
                ..Config::default()
            };
            let estimates: Vec<f64> = (0..REPEATS)
                .map(|r| cfg.bench(variable_cost(seed_for(r))).ns_per_iter)
                .collect();
            let (_, observed) = mean_and_spread(&estimates);
            println!(
                "target {:.1}% -> observed run-to-run spread {:.2}%",
                100.0 * target,
                100.0 * observed
            );
            // A standard deviation estimated from only REPEATS runs is
            // itself noisy, so this leaves generous room - the point is to
            // catch a stopping rule that has become decorative (as the old
            // R²-based one was), not to pin down the constant precisely.
            assert!(
                observed < target * 4.0,
                "asked for {:.2}% accuracy but observed spread was {:.2}%",
                100.0 * target,
                100.0 * observed
            );
        }
    }

    #[test]
    fn estimates_the_mean_not_the_minimum() {
        println!();
        // Comparing a long run against short ones only isolates the
        // estimator when the machine holds still. Contention does not
        // affect the two equally - the long run averages over more of it -
        // so on a busy machine this measures the neighbours rather than
        // the estimator, which is the same reason its siblings above skip.
        if !quiesced() {
            println!("SKIPPED: machine is not quiesced (see `quiet-bench reserve`)");
            return;
        }
        // Ground truth: a long, tight-target run.
        let truth = Config {
            target_rel_error: 0.002,
            max_time: Duration::from_secs(20),
            ..Config::default()
        }
        .bench(variable_cost(0xabcd_ef01))
        .ns_per_iter;

        const REPEATS: usize = 15;
        let estimates: Vec<f64> = (0..REPEATS)
            .map(|r| Config::default().bench(variable_cost(seed_for(r))).ns_per_iter)
            .collect();
        let (mean, _) = mean_and_spread(&estimates);
        let bias = (mean - truth) / truth;
        println!(
            "truth {truth:.1} ns/iter, estimated {mean:.1} ns/iter, bias {:+.2}%",
            100.0 * bias
        );
        // An estimator that reported (say) the minimum of each batch rather
        // than the mean would be biased sharply negative on a workload with
        // this much variance - well outside this bound.
        assert!(bias.abs() < 0.1, "bias {:+.2}%", 100.0 * bias);
    }

    #[test]
    fn tighter_target_costs_more_and_is_more_precise() {
        println!();
        if !quiesced() {
            println!("SKIPPED: machine is not quiesced (see `quiet-bench reserve`)");
            return;
        }
        const REPEATS: usize = 10;
        // Wide gap between targets, and the tight one well below the noise
        // a single calibrated batch already has on its own: with only a
        // small gap, both targets get satisfied as soon as `MIN_SAMPLES` is
        // reached and the test can't distinguish "tighter target costs
        // more" from "both stopped at the same floor".
        let loose: Vec<Stats> = (0..REPEATS)
            .map(|r| {
                Config {
                    target_rel_error: 0.05,
                    ..Config::default()
                }
                .bench(variable_cost(seed_for(r)))
            })
            .collect();
        let tight: Vec<Stats> = (0..REPEATS)
            .map(|r| {
                Config {
                    target_rel_error: 0.003,
                    ..Config::default()
                }
                .bench(variable_cost(seed_for(r)))
            })
            .collect();
        let iters = |v: &[Stats]| v.iter().map(|s| s.iterations).sum::<u64>();
        let (loose_iters, tight_iters) = (iters(&loose), iters(&tight));
        println!("loose iterations {loose_iters}, tight iterations {tight_iters}");
        assert!(tight_iters > 2 * loose_iters);

        let spread = |v: &[Stats]| mean_and_spread(&v.iter().map(|s| s.ns_per_iter).collect::<Vec<_>>()).1;
        let (loose_spread, tight_spread) = (spread(&loose), spread(&tight));
        println!(
            "loose spread {:.2}%, tight spread {:.2}%",
            100.0 * loose_spread,
            100.0 * tight_spread
        );
        assert!(tight_spread < loose_spread);
    }


    #[test]
    fn a_zero_standard_error_meets_any_target() {
        // A benchmark optimised away entirely measures identically every
        // time, so its standard error is exactly zero and no further
        // sampling can improve it. Both variants have to accept that.
        // Deciding this by dividing the error by the (also zero) mean gave
        // NaN, which compares false against everything, so such a benchmark
        // could never stop voluntarily and burned its whole budget on every
        // run - under `Relative` just as much as `Absolute`.
        assert!(Config::relative(0.01).accuracy_met(0.0, 0.0));
        assert!(Config::absolute(Duration::from_nanos(50)).accuracy_met(0.0, 0.0));

        // A real error still has to clear the bar, either way round.
        assert!(!Config::relative(0.01).accuracy_met(100.0, 5.0));
        assert!(Config::relative(0.01).accuracy_met(100.0, 0.5));
        assert!(!Config {
            target_rel_error: 0.0,
            target_abs_error: Duration::from_nanos(1),
            ..Config::default()
        }
        .accuracy_met(100.0, 5.0));
        assert!(Config {
            target_rel_error: 0.0,
            target_abs_error: Duration::from_nanos(10),
            ..Config::default()
        }
        .accuracy_met(100.0, 5.0));

        // The two goals are independent, and the coarser one wins: a 1%
        // goal on a 100ns measurement wants the error under 1ns, but a
        // 5ns absolute floor says 5ns is close enough, so it stops.
        assert!(Config {
            target_rel_error: 0.01,
            target_abs_error: Duration::from_nanos(5),
            ..Config::default()
        }
        .accuracy_met(100.0, 4.0));
    }

    #[test]
    fn display_reports_an_absolute_error_in_the_value_s_own_unit() {
        let shown = |ns: f64, rel: f64| {
            format!(
                "{}",
                Stats {
                    ns_per_iter: ns,
                    std_error: ns * rel,
                    iterations: 10,
                    samples: 6,
                    hit_limit: false,
                    untrustworthy: false,
                }
            )
            .trim_start()
            .to_string()
        };

        // A sub-nanosecond error bar has to survive: it is the ordinary case
        // for a fast function, and formatting via `Duration` (which has
        // nanosecond resolution) would round it away to `0ns`. The value is
        // shown to the same two decimals as the error, not to more: `71.0000`
        // would be claiming four digits the measurement does not support.
        assert_eq!(shown(71.0, 0.0017), "71.00ns ± 0.12ns");

        // Both sides in the same unit, so two results can be compared digit
        // for digit without a unit conversion in the reader's head - and to
        // the same precision, so every digit printed is one the measurement
        // actually justifies.
        assert_eq!(shown(100_267_300.0, 0.0002), "100.267ms ± 0.020ms");

        // Two significant digits is all an error bar deserves, whatever its
        // magnitude relative to the value.
        assert_eq!(shown(2_500.0, 0.032), "2.500µs ± 0.080µs");

        // Even an error far below the value's own unit keeps two digits
        // rather than collapsing to zero - and here that does mean four
        // decimals on the value, because the error genuinely reaches them.
        assert_eq!(shown(0.4523, 0.02), "0.4523ns ± 0.0090ns");

        // Two digits is also all it gets when the error is large enough to
        // need none: a noisy benchmark whose error reaches the tens of its
        // own unit says `± 25ns`, not `± 25.0ns`, which would be a third
        // digit the measurement cannot support.
        assert_eq!(shown(500.0, 0.05), "500ns ± 25ns");
    }

    #[test]
    fn an_absolute_accuracy_target_is_honoured() {
        println!();
        // Only the absolute goal: the relative one is disabled, since
        // sampling stops at whichever goal is coarser and the 1% default
        // would otherwise govern for a workload of this size.
        let only_absolute = |ns| Config {
            target_rel_error: 0.0,
            target_abs_error: Duration::from_nanos(ns),
            ..Config::default()
        };
        // 25ns, not the 5ns this used to ask for. `variable_cost` has a
        // coefficient of variation around 50%, so the standard error falls
        // as the square root of the sample count and the last factor of two
        // costs four times what the one before it did. Swept on one machine
        // against the default budget:
        //
        //   target      se reached   iterations
        //        5ns      6.975ns      912848 (limit)
        //       10ns     10.000ns      419017
        //       25ns     24.995ns       62929
        //      100ns     99.886ns        4873
        //      500ns    499.284ns         169
        //
        // 5ns was not reachable there at all, and was only ever reached on
        // a machine fast enough to buy it - so the test passed or failed on
        // the hardware rather than on the library, which is what it went on
        // doing, about half the time, on CI. 25ns costs a fourteenth of the
        // iterations the budget demonstrably supports, so what is being
        // tested is that the target governs sampling, not that this
        // particular machine is quick.
        let stats = only_absolute(25).bench(variable_cost(7));
        println!("absolute 25ns: {stats}");
        assert!(!stats.hit_limit, "should have reached +-25ns in the budget");
        assert!(
            stats.std_error < 25.0,
            "asked for +-25ns, got +-{:.2}ns",
            stats.std_error
        );

        // A looser absolute ask must be cheaper - the target is doing the
        // work, not some fixed amount of sampling. 500ns is met by the
        // minimum sampling every run takes, so this comparison cannot come
        // down to noise either.
        let cheap = only_absolute(500).bench(variable_cost(7));
        println!("absolute 500ns: {cheap}");
        assert!(
            cheap.iterations < stats.iterations,
            "loose target used {} iterations, tight used {}",
            cheap.iterations,
            stats.iterations
        );
    }

    #[test]
    fn a_slow_function_on_a_short_budget_still_gets_an_error_bar() {
        println!();
        // ~100ms per iteration against a 350ms budget: calibration takes one
        // iteration and each sample takes another, so only a handful fit -
        // fewer than MIN_SAMPLES. We should still get a real error bar out
        // of the samples we managed, rather than NaN.
        let cfg = Config {
            max_time: Duration::from_millis(350),
            ..Config::default()
        };
        let stats = cfg.bench(|| thread::sleep(Duration::from_millis(100)));
        println!("{stats}");
        assert!(
            stats.samples >= 2 && stats.samples < MIN_SAMPLES,
            "expected to stop short of MIN_SAMPLES, got {} samples",
            stats.samples
        );
        assert!(
            !stats.std_error.is_nan(),
            "a standard error exists from {} samples and should be reported",
            stats.samples
        );
        // Both marks: the clock ran out (hit_limit) *and* there were too
        // few samples to believe the error bar (untrustworthy). The error
        // bar is still reported - a wide honest one beats none.
        assert!(stats.hit_limit);
        assert!(stats.untrustworthy);
        assert!(stats.ns_per_iter > 99.0e6);
    }

    #[test]
    fn unreachable_target_is_flagged() {
        println!();
        // An accuracy no amount of sampling will reach, and a budget far too
        // short to try: the benchmark must say it fell short rather than
        // return a confident-looking number.
        let cfg = Config {
            target_rel_error: 1e-9,
            max_time: Duration::from_millis(50),
            ..Config::default()
        };
        let stats = cfg.bench(variable_cost(1));
        println!("{stats}");
        assert!(stats.hit_limit);
        assert!(!cfg.accuracy_met(stats.ns_per_iter, stats.std_error));
        // This one collected plenty of samples, it just needed longer than
        // the budget allowed: the error bar is wider than asked for but
        // perfectly believable, which is exactly the case `hit_limit`
        // covers and `untrustworthy` does not.
        assert!(!stats.untrustworthy);
    }

    #[test]
    fn reported_error_is_honest() {
        println!();
        if !quiesced() {
            println!("SKIPPED: machine is not quiesced (see `quiet-bench reserve`)");
            return;
        }
        const REPEATS: usize = 40;
        for &target in &[0.05, 0.02, 0.01] {
            let cfg = Config {
                target_rel_error: target,
                ..Config::default()
            };
            let stats: Vec<Stats> = (0..REPEATS)
                .map(|r| cfg.bench(variable_cost(seed_for(r))))
                .collect();
            let claimed = stats.iter().map(|s| s.rel_std_error()).sum::<f64>() / REPEATS as f64;
            let (_, observed) =
                mean_and_spread(&stats.iter().map(|s| s.ns_per_iter).collect::<Vec<_>>());
            let ratio = observed / claimed;
            println!(
                "target {:.1}%: claimed {:.2}%, observed {:.2}%, ratio {:.2}x",
                100.0 * target,
                100.0 * claimed,
                100.0 * observed,
                ratio
            );
            // `rel_std_error` should describe the spread that actually
            // occurs, not merely shrink on demand: an estimator that just
            // claimed whatever the caller asked for would pass every other
            // test in this module. On a quiesced machine this ratio runs
            // 0.8-1.0x; on a shared/noisy one, outlier samples inflate the
            // observed side, so this bound is generous rather than tight.
            assert!(
                ratio < 3.0,
                "claimed {:.2}% but observed spread was {:.2}% ({:.1}x overconfident)",
                100.0 * claimed,
                100.0 * observed,
                ratio
            );
        }
    }
}

