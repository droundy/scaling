/*!
A lightweight micro-benchmarking library which:

* measures until it reaches an accuracy you ask for, and tells you the
  accuracy it achieved;
* handles benchmarks which mutate state;
* can measure simple polynomial or exponential scaling behavior
* is very easy to use!

`scaling` is designed to work with either slow or fast functions.
It's forked from [easybench], which is itself inspired by [criterion],
but doesn't do as much sophisticated
analysis (no outlier detection, no HTML output).

[easybench]: https://crates.io/crates/easybench
[criterion]: https://crates.io/crates/criterion

```
use scaling::{bench,bench_env,bench_scaling};

# fn fib(_: usize) -> usize { 0 }
#
// Simple benchmarks are performed with `bench` or `bench_scaling`.
println!("fib 200: {}", bench(|| fib(200) ));
println!("fib 500: {}", bench(|| fib(500) ));
println!("fib scaling: {}", bench_scaling(|n| fib(n), 0));

// If a function needs to mutate some state, use `bench_env`.
println!("reverse: {}", bench_env(vec![0;100], |xs| xs.reverse() ));
println!("sort:    {}", bench_env(vec![0;100], |xs| xs.sort()    ));
```

Running the above yields the following results:

```none
fib 200:    71.723ns ± 0.057ns
fib 500:    257.05ns ± 0.87ns
fib scaling:  (0.5507 ± 0.0055)ns/N (R²=0.999)
reverse:     75.02ns ± 0.75ns
sort:        97.59ns ± 0.97ns
```

Easy! However, please read the [caveats](#caveats) below before using.

# Benchmarking algorithm

An *iteration* is a single execution of your code. A *sample* is a measurement,
during which your code may be run many times. In other words: taking a sample
means performing some number of iterations and measuring the total time.

## Flat benchmarks: `bench`, `bench_env`, `bench_gen_env`

These first calibrate a batch size (the number of iterations per sample)
large enough that per-sample overhead is negligible, then take equal-sized
batches and stop once the *standard error* of their mean is small enough.
This directly answers "how precisely do I know `ns_per_iter`", which is
what you actually want, as opposed to the older R²-based rule this
replaced: R² asks "is time linear in iteration count", which a handful of
points satisfy almost regardless of how noisy each one is, and says nothing
about how well the slope is known.

"Small enough" is yours to define, either way round (see [`Accuracy`]):
[`Accuracy::Relative`] for a fraction of the measurement (1% by default),
or [`Accuracy::Absolute`] for a plain `Duration`, which is often the more
natural request when you already know the difference you are trying to
resolve.

The reported figure is the standard error of the measurement, printed after
the `±` in the same unit as the measurement itself so that two results can
be compared by eye: they differ by more than their noise when the gap
between them is several times the larger `±`. The measurement is printed to
exactly the precision that error justifies and no further, so every digit
you see is one the benchmark actually stands behind.

[`Stats::std_error`] and [`Stats::rel_std_error`] give the error absolutely
and relatively, [`Stats::iterations`] and [`Stats::samples`] say how much
work it took, and [`Stats::hit_limit`] tells you if the budget ran out
before the target accuracy was met - in which case the line is marked
`(limit)`. That budget is 10 seconds by default (see [`Config`]).

If a benchmark requires some state to run, one copy of the initial state is
prepared per iteration.

## Scaling benchmarks: `bench_scaling`, `bench_scaling_gen`

These fit several candidate power/exponential laws by OLS linear regression
and pick the best fit by R², since here we care about the *shape* of the
scaling relationship rather than just one number, and R² is a reasonable way
to compare candidate models against each other. The first sample taken
performs only 1 iteration, but as we continue taking samples we increase the
number of iterations with increasing rapidity.

Two separate things have to be settled before there is an answer, and the
output reports them separately:

* **Which law?** R², shown in the output, is the signal for this. When the
  data cannot tell the candidates apart it is set to zero outright, rather
  than a law being picked on a coin-toss.
* **How big is its constant?** [`ScalingStats::rel_std_error`] answers this,
  and it is what the `±` in the output shows. Sampling continues until it
  meets the same [`Accuracy`] target the flat benchmarks use, so
  `(43.1 ± 1.2)ns/N` means the same kind of thing as `43.1ns ± 1.2ns` does
  for [`bench`].

The two are deliberately not merged into one number, because an error bar
cannot speak for the choice of law that it was computed *after*: a run that
picks the wrong shape can report a very tight `±` on a constant that means
nothing. Read them together, and be suspicious of a small `±` sitting next
to `R²=0.000`.

Sampling stops once the law is identified *and* its constant meets the
accuracy target, or when the time budget runs out - 10 seconds by default,
extended twelvefold for a sweep that has not managed to identify any law at
all, since without a shape there is no answer to report.

# Caveats

## Caveat 1: Harness overhead

**TL;DR: Compile with `--release`; the overhead is likely to be within the
**noise of your
benchmark.**

Work which `scaling` does once-per-sample is kept negligible: the flat
benchmarks size each batch so that a sample takes far longer than the two
`Instant::now()` calls bracketing it, and the scaling benchmarks subtract it
via the regression's intercept. However, work which is done once-per-iteration
*will* be counted in the final times.

* In the case of [`bench()`] this amounts to incrementing the loop counter and
  passing the return value through `std::hint::black_box`.
* In the case of [`bench_env`] and [`bench_gen_env`], we also do a lookup into a big vector in
  order to get the environment for that iteration.
* If you compile your program unoptimised, there may be additional overhead.

The cost of the above operations depend on the details of your benchmark;
namely: (1) how large is the return value? and (2) does the benchmark evict
the environment vector from the CPU cache? In practice, these criteria are only
satisfied by longer-running benchmarks, making these effects hard to measure.

## Caveat 2: Pure functions

**TL;DR: Return enough information to prevent the optimiser from eliminating
code from your benchmark.**

Benchmarking pure functions involves a nasty gotcha which users should be
aware of. Consider the following benchmarks:

```
# use scaling::{bench,bench_env};
#
# fn fib(_: usize) -> usize { 0 }
#
let fib_1 = bench(|| fib(500) );                     // fine
let fib_2 = bench(|| { fib(500); } );                // spoiler: NOT fine
let fib_3 = bench_env(0, |x| { *x = fib(500); } );   // also fine, but ugly
# let _ = (fib_1, fib_2, fib_3);
```

The results are a little surprising:

```none
fib_1:    261.77ns ± 0.92ns
fib_2:   0.59161ns ± 0.00033ns
fib_3:   258.509ns ± 0.093ns
```

Oh, `fib_2`, why do you lie? The answer is: `fib(500)` is pure, and its
return value is immediately thrown away, so the optimiser deletes the call
entirely. What is left to measure is an empty loop, which clocks in at a
fraction of a nanosecond - not the 258 ns the work would have cost.

What about the other two? `fib_1` looks very similar, with one exception:
the closure which we're benchmarking returns the result of the `fib(500)`
call. When it runs your code, `scaling` passes that return value through
[`std::hint::black_box`], which the optimiser must treat as though it were
used, before throwing it away. This is why `fib_1` is safe from having code
accidentally eliminated.

In the case of `fib_3`, we actually *do* use the return value: each
iteration we take the result of `fib(500)` and store it in the iteration's
environment. This has the desired effect, but looks a bit weird.

## Caveat 3: A busy machine

**TL;DR: on Linux, `sudo quiet-bench reserve 2` then
`quiet-bench run <your benchmark>`.**

The accuracy `scaling` reports covers noise it can *see* while sampling. It
cannot see the machine around it: another process on the same core, a CPU
dropping out of turbo as it heats up, or an interrupt landing mid-sample all
shift the answer without widening the error bar.

The `quiet-bench` binary shipped with this crate reserves one or more CPUs
for benchmarking and moves everything else - processes, interrupts - off
them, and pins the clock frequency. Benchmarks then pin themselves to the
reserved CPUs automatically, with no code change. See the [`quiet`] module
for the details, and [`quiet::status`] to check at runtime whether it took
effect.
*/

pub mod quiet;

use std::f64;
use std::fmt::{self, Display, Formatter};
use std::hint::black_box;
use std::time::*;

// We try to spend at most this many seconds (roughly) in total on
// each benchmark. A scaling sweep that has not yet identified a law will
// keep going to a multiple of this - see `scaling_verdict`.
const BENCH_TIME_MAX: Duration = Duration::from_secs(10);
// We try to spend at least this many seconds in total on each
// benchmark.
const BENCH_TIME_MIN: Duration = Duration::from_millis(1);

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

/// A backstop on the number of samples, so a pathologically small
/// [`Config::sample_time`] cannot grow the sample vector without bound.
///
/// This is about memory, not about the measurement: `max_time` is the real
/// budget, and at the default `sample_time` it allows ~10_000 samples, a
/// hundred times below this.
const MAX_SAMPLES: usize = 1_000_000;

/// How good an answer a benchmark should work for, expressed either way
/// round.
///
/// Both forms describe the same quantity - the standard error of
/// `ns_per_iter` - so exactly one of them applies to any given run. They
/// are an enum rather than two fields for that reason: with independent
/// fields, "stop when either is satisfied" quietly lets the looser one win
/// (defeating the point of setting a tight one), and "stop when both are"
/// makes a purely absolute target impossible to ask for without also
/// disabling the relative one.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Accuracy {
    /// Stop once the standard error falls below this fraction of the
    /// measurement (`0.01` = 1%). Good when you care about proportional
    /// precision regardless of scale.
    Relative(f64),
    /// Stop once the standard error falls below this absolute duration.
    ///
    /// Often the more natural request when you already know the difference
    /// you are trying to resolve: to tell apart two implementations you
    /// believe differ by ~1 µs, ask for an error well under that and stop
    /// paying for precision you will not use.
    Absolute(Duration),
}

impl Accuracy {
    /// Is a measurement of `ns_per_iter` with standard error `std_error`
    /// (both in nanoseconds) good enough?
    ///
    /// Both variants compare absolute quantities, so neither has to divide
    /// by `ns_per_iter` - which matters, because that division is what made
    /// a zero-cost benchmark impossible to satisfy.
    fn is_met(&self, ns_per_iter: f64, std_error: f64) -> bool {
        // A standard error of exactly zero means every sample agreed to the
        // limit of the timer's resolution. No amount of further sampling can
        // improve on that, so any target is as met as it is ever going to
        // be. Without this, a benchmark whose cost is optimised away
        // entirely - `bench(|| {})`, the module docs' "Pure functions"
        // caveat - would never stop voluntarily under *either* variant and
        // would silently burn its whole time budget on every run.
        if std_error == 0.0 {
            return true;
        }
        match *self {
            Accuracy::Relative(target) => std_error < target * ns_per_iter,
            Accuracy::Absolute(target) => std_error < target.as_secs_f64() * 1e9,
        }
    }
}

/// How hard a benchmark works to pin down `ns_per_iter`, and when it gives
/// up.
///
/// [`bench`], [`bench_env`] and [`bench_gen_env`] use [`Config::default`];
/// call the same-named methods on a `Config` to choose your own. The two
/// fields you are likely to want are `accuracy` (how good an answer do I
/// want) and `max_time` (how long am I willing to wait); everything else
/// the benchmark works out for itself.
///
/// ```
/// use scaling::{Accuracy, Config};
/// use std::time::Duration;
///
/// # fn fib(_: usize) -> usize { 0 }
/// // "to within a tenth of a percent"
/// let tight = Config::relative(0.001);
/// // "to within 50 nanoseconds, and do not spend more than a second"
/// let quick = Config {
///     max_time: Duration::from_secs(1),
///     ..Config::absolute(Duration::from_nanos(50))
/// };
/// # let _ = (tight.accuracy, quick.accuracy, Accuracy::Relative(0.01));
/// ```
#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    /// How good an answer to work for. See [`Accuracy`].
    pub accuracy: Accuracy,
    /// Give up after roughly this much wall-clock time even if `accuracy`
    /// was never reached, setting [`Stats::hit_limit`].
    pub max_time: Duration,
    /// Calibrate the batch size so that one sample takes about this long.
    ///
    /// You should rarely need to touch this. The default is chosen so that
    /// per-sample overhead - two `Instant::now()` calls, on the order of
    /// 100 ns - is around 0.01% of a sample, comfortably below any accuracy
    /// you are likely to ask for.
    ///
    /// It is exposed because batch size is not purely an implementation
    /// detail: a batch is `unit` back-to-back iterations sharing a cache,
    /// so for a memory-sensitive benchmark this changes *what you are
    /// measuring*, not just how precisely. Shrink it if you want each
    /// iteration to see a colder cache.
    pub sample_time: Duration,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            accuracy: Accuracy::Relative(0.01),
            max_time: BENCH_TIME_MAX,
            sample_time: Duration::from_millis(1),
        }
    }
}

impl Config {
    /// A [`Config::default`] asking for a standard error below `fraction`
    /// of the measurement (`0.001` = 0.1%).
    pub fn relative(fraction: f64) -> Self {
        Config {
            accuracy: Accuracy::Relative(fraction),
            ..Config::default()
        }
    }

    /// A [`Config::default`] asking for a standard error below `error` in
    /// absolute terms.
    pub fn absolute(error: Duration) -> Self {
        Config {
            accuracy: Accuracy::Absolute(error),
            ..Config::default()
        }
    }
}

/// Statistics for a benchmark run.
#[derive(Debug, PartialEq, Clone)]
pub struct Stats {
    /// The time, in nanoseconds, per iteration.
    pub ns_per_iter: f64,
    /// Relative standard error of `ns_per_iter`, as a fraction (0.01 = 1%).
    ///
    /// This is the standard error *of sampling* seen within this call: the
    /// benchmark's own run-to-run variability plus timer noise. It is *not*
    /// a bound on systematic differences between separate process runs —
    /// CPU frequency state, code layout, and cache/allocator state shift
    /// between runs, and no statistic computed inside one call can see
    /// them. A very small `rel_std_error` on a deterministic benchmark
    /// means sampling is no longer the limit, not that the number is
    /// accurate to that many digits. `NaN` when it cannot be computed at
    /// all: either fewer than 2 samples were collected (see `hit_limit`),
    /// or every sample measured as exactly zero.
    pub rel_std_error: f64,
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
    /// `true` if the benchmark gave up on its time budget before reaching
    /// its `accuracy` target - the accuracy you asked for was not achieved.
    ///
    /// Also `true` in the conservative case where the budget ran out before
    /// enough samples had accumulated to trust the standard error, even if
    /// `rel_std_error` happens to look good: that is precisely the reading
    /// not to believe. `rel_std_error` is still reported whenever it can be
    /// computed at all, so a stopped-short run gives you a wide error bar
    /// rather than none.
    pub hit_limit: bool,
}

impl Stats {
    /// The standard error of [`Stats::ns_per_iter`] in nanoseconds - the
    /// absolute counterpart of [`Stats::rel_std_error`], and what `Display`
    /// shows after the `±`.
    ///
    /// Use this to compare two measurements: they differ by more than their
    /// noise when the gap between their `ns_per_iter` is several times the
    /// larger of their standard errors. `NaN` exactly when `rel_std_error`
    /// is.
    pub fn std_error(&self) -> f64 {
        self.ns_per_iter * self.rel_std_error
    }
}

/// Pick a human-readable unit from a magnitude in nanoseconds, returning
/// the divisor and its suffix.
///
/// We choose units ourselves rather than deferring to `Duration`'s `Debug`,
/// which cannot help here: `Duration` has nanosecond resolution, so the
/// error bar on a fast benchmark - 0.12 ns on a 71 ns function is entirely
/// typical - would round to a useless `0ns`.
fn unit_for(ns: f64) -> (f64, &'static str) {
    let magnitude = ns.abs();
    if magnitude < 1e3 {
        (1.0, "ns")
    } else if magnitude < 1e6 {
        (1e3, "µs")
    } else if magnitude < 1e9 {
        (1e6, "ms")
    } else {
        (1e9, "s")
    }
}

/// How many decimal places `x` needs to show two significant digits, which
/// is all the precision an error bar ever deserves.
fn error_decimals(x: f64) -> usize {
    // The `is_finite` test comes first so that the comparison below never
    // has to reason about NaN.
    if !x.is_finite() || x <= 0.0 {
        return 4;
    }
    (1 - x.log10().floor() as i64).clamp(1, 9) as usize
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
        let limit = if self.hit_limit { " (limit)" } else { "" };
        if self.rel_std_error.is_nan() {
            // NaN has two distinct causes: too few samples to estimate a
            // standard error from (only possible via the single-sample
            // "blew the whole time budget already" path), or - rarer, but
            // possible for a near-free benchmark on a coarse timer - a
            // measured time of exactly zero for every sample, which makes
            // `rel_std_error` a literal 0/0 regardless of sample count.
            //
            // With no error bar there is nothing to set the precision, so
            // fall back to a fixed four decimals.
            let why = if self.samples < 2 {
                format!(
                    "only {} sample{}",
                    self.samples,
                    if self.samples == 1 { "" } else { "s" }
                )
            } else {
                "measured time was exactly zero".to_string()
            };
            let value = format!("{:.4}{}", self.ns_per_iter / div, unit);
            write!(f, "{value:>11} (± unknown, {why}){limit}")
        } else {
            let scaled = self.std_error() / div;
            // The error's own precision sets the measurement's: digits of
            // the value beyond where the uncertainty starts are noise
            // dressed up as signal. So `71.9858ns ± 0.17ns` is really only
            // known to `71.99ns ± 0.17ns`, and printing the extra two
            // digits invites a reader to believe them.
            let decimals = error_decimals(scaled);
            // Below about four decimal places, spelling the error out costs
            // a run of leading zeroes that conveys nothing (an
            // optimised-away benchmark can reach `0.000000021`). Scientific
            // notation stays short and says the same thing - but the value
            // still wants plain digits, so it keeps the decimal count that
            // matches.
            let error = if scaled > 0.0 && scaled < 1e-4 {
                format!("{scaled:.1e}{unit}")
            } else {
                format!("{:.*}{}", decimals, scaled, unit)
            };
            let value = format!("{:.*}{}", decimals, self.ns_per_iter / div, unit);
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

/// Run a benchmark, targeting the default accuracy (see [`Config`]).
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

/// Run a benchmark with an environment, targeting the default accuracy (see
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

/// Run a benchmark with a generated environment, targeting the default
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
    /// Run a benchmark, targeting `self`'s accuracy. See [`bench`] for the
    /// default-accuracy convenience function, and [`Config::bench_gen_env`]
    /// for the algorithm.
    pub fn bench<F, O>(&self, mut f: F) -> Stats
    where
        F: FnMut() -> O,
    {
        self.bench_env((), |_| f())
    }

    /// Run a benchmark with an environment, targeting `self`'s accuracy.
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

    /// Run a benchmark with a generated environment, targeting `self`'s
    /// accuracy.
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
                rel_std_error: f64::NAN,
                iterations: probed,
                samples: 1,
                hit_limit: true,
            };
        }
        // Otherwise the probe that finished calibration serves as the
        // warmup sample and is discarded, same as the regression-based
        // algorithm discarded its first sample as a cache-warming
        // exercise.

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
                samples.count >= MIN_SAMPLES && self.accuracy.is_met(mean, std_error);
            if precise_enough || out_of_budget {
                return Stats {
                    ns_per_iter: mean,
                    // Derived, not primitive: `NaN` both when there were too
                    // few samples for a standard error to exist and when the
                    // mean is zero, which is exactly what `rel_std_error`
                    // documents.
                    rel_std_error: std_error / mean,
                    // Derived rather than accumulated, which keeps the
                    // arithmetic in u64 and out of the loop: `usize` would
                    // overflow on a 32-bit target, where a fast benchmark
                    // can legitimately run past 4.3 billion iterations.
                    iterations: probed + samples.count as u64 * unit as u64,
                    samples: samples.count,
                    // Stopping short of `MIN_SAMPLES` counts as hitting the
                    // limit even if the error happens to look good, because
                    // that is exactly the reading we do not yet trust.
                    hit_limit: !precise_enough,
                };
            }
        }
    }

    /// Benchmark the power-law scaling of a function, targeting `self`'s
    /// accuracy. See [`bench_scaling`] for the default-accuracy version.
    ///
    /// The accuracy applies to [`Scaling::ns_per_scale`], the constant in
    /// front of the fitted law, and only once the law itself has been
    /// identified - see [`ScalingStats::rel_std_error`] for why those are
    /// two different questions.
    ///
    /// [`Accuracy::Relative`] is the one that makes sense here.
    /// [`Accuracy::Absolute`] is accepted and well defined, but its units
    /// are nanoseconds per `Nᴾ Eᴺ`, which change meaning with the law that
    /// happens to be selected - so a figure chosen for one shape is rarely
    /// the figure you want for another.
    pub fn bench_scaling<F, O>(&self, f: F, nmin: usize) -> ScalingStats
    where
        F: Fn(usize) -> O,
    {
        bench_scaling_with(self, f, nmin)
    }

    /// Benchmark the power-law scaling of a function with a generated
    /// input, targeting `self`'s accuracy. See [`bench_scaling_gen`] for
    /// the default-accuracy version, and [`Config::bench_scaling`] for what
    /// the accuracy applies to.
    pub fn bench_scaling_gen<G, F, I, O>(&self, gen_env: G, f: F, nmin: usize) -> ScalingStats
    where
        G: FnMut(usize) -> I,
        F: Fn(&mut I) -> O,
    {
        bench_scaling_gen_with(self, gen_env, f, nmin)
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
    // Cap how much wall-clock time a single calibration probe - setup plus
    // timing - may cost. Ordinarily the extrapolation below is driven
    // entirely by the timed portion approaching `cfg.sample_time`. But if
    // `f`'s cost is optimised away (see the module docs' "Pure functions"
    // caveat, e.g. `bench_env(v, |_| {})`), the timed portion never grows
    // no matter how large `unit` gets, while untimed environment
    // construction - which very much does cost real time and memory for a
    // non-trivial `I` - would otherwise grow without bound before the
    // `start.elapsed() > cfg.max_time` check below ever gets a chance to
    // run, since that allocation itself can take unbounded time. This
    // ceiling bounds the *total* cost of a probe, not just the part we
    // intend to measure, so it catches that case regardless of what `I` is.
    // Kept well under `max_time` (rather than some large fraction of it) to
    // limit *memory*, not just time: on fast hardware a looser ceiling
    // would let calibration allocate proportionally more before it fires.
    let probe_ceiling_ns = (cfg.max_time / 100)
        .max(Duration::from_millis(5))
        .as_secs_f64()
        * 1e9;
    // Two independent, timing-blind ceilings on `unit` itself, combined by
    // taking whichever is smaller. Neither is a hard memory guarantee on its
    // own - see below - but together they cover far more real cases than
    // either alone:
    //
    // `MAX_CALIBRATION_UNIT` catches the case the time-based ceiling above
    // cannot: when *both* `f` and the environment are trivial enough (e.g.
    // `bench(|| {})`, where `I` is `()`), the optimiser can eliminate the
    // entire batch - construction and loop alike - up to an enormous `unit`,
    // so `setup_ns` and `t` can both keep reading as ~0 indefinitely and no
    // timing-based check can detect that in advance.
    //
    // `MAX_CALIBRATION_BYTES / size_of::<I>()` catches the complementary
    // case: a non-trivial, non-heap-indirect `I` (an array, a plain struct)
    // whose per-clone cost is real but small, where `probe_ceiling_ns`
    // alone would still permit millions of copies before firing. For a
    // heap-indirect `I` (`Vec<T>`, `Box<T>`, `String`) `size_of::<I>()` only
    // sees the inline handle, not what it points to, so this cap cannot see
    // that memory either - the time-based ceiling above is what bounds that
    // case, imperfectly, by keeping the *wall-clock cost* of construction
    // bounded even when its *size* is invisible to us. Between the three,
    // every case has at least one real backstop, but none of them alone is
    // a hard guarantee for every `I` - as always, keep your environment
    // small (see the module docs).
    const MAX_CALIBRATION_UNIT: usize = 2_000_000;
    const MAX_CALIBRATION_BYTES: usize = 64 * 1024 * 1024;
    let unit_cap = MAX_CALIBRATION_UNIT.min(MAX_CALIBRATION_BYTES / std::mem::size_of::<I>().max(1));
    let target = cfg.sample_time.as_secs_f64() * 1e9;
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

/// Running mean and variance of the per-iteration times, updated in O(1)
/// per sample.
///
/// The sampling loop asks whether it can stop after *every* sample, so
/// recomputing from a stored vector would make the loop O(k²) in the number
/// of samples - fine at the default `sample_time`, but `sample_time` is a
/// public knob and shrinking it puts the loop in a regime where it spends
/// more of the budget on arithmetic than on measuring. Keeping the running
/// figures also means the samples themselves never need storing.
///
/// This is Welford's algorithm rather than accumulating `sum` and
/// `sum_of_squares`, because the variance we want is a minute difference
/// between two large numbers in that formulation - a 260 ns benchmark
/// measured to 0.1% - and would lose most of its significant digits to
/// cancellation. Welford never forms that difference.
#[derive(Default)]
struct Running {
    count: usize,
    mean: f64,
    m2: f64,
}

impl Running {
    fn push(&mut self, x: f64) {
        self.count += 1;
        let delta = x - self.mean;
        self.mean += delta / self.count as f64;
        self.m2 += delta * (x - self.mean);
    }

    /// Mean, and the standard error *of that mean*, in nanoseconds. See
    /// [`Config::bench_gen_env`] for why batching does not bias this.
    ///
    /// The error is absolute rather than relative because that is the
    /// primitive quantity: it needs nothing but the samples, whereas
    /// dividing by the mean is undefined when the mean is zero.
    /// [`Stats::rel_std_error`] is derived from it for reporting.
    fn mean_and_stderr(&self) -> (f64, f64) {
        if self.count < 2 {
            // A standard error needs at least two points to exist at all.
            return (self.mean, f64::NAN);
        }
        // Sample variance (Bessel-corrected), then the standard error of
        // the mean.
        let var = self.m2 / (self.count - 1) as f64;
        (self.mean, (var / self.count as f64).sqrt())
    }
}

/// Statistics for a benchmark run determining the scaling of a function.
#[derive(Debug, PartialEq, Clone)]
pub struct ScalingStats {
    pub scaling: Scaling,
    /// Relative standard error of [`Scaling::ns_per_scale`], as a fraction
    /// (0.01 = 1%).
    ///
    /// **This is conditional on the reported scaling law being the right
    /// one.** It says how well the constant is known *given* that the
    /// function really is `O(Nᴾ Eᴺ)` for the reported `P` and `E`; it says
    /// nothing about whether that law was chosen correctly, because it is
    /// computed after the choice and cannot see the alternatives that were
    /// rejected. `goodness_of_fit` is the signal for that half - it is set
    /// to zero when the fit could not distinguish between candidate laws -
    /// so read the two together, and treat a tight error bar next to a zero
    /// `goodness_of_fit` as "precise about a shape I could not pin down".
    ///
    /// `NaN` before three samples, where a standard error cannot exist.
    pub rel_std_error: f64,
    pub goodness_of_fit: f64,
    /// How many times the benchmarked code was actually run.
    pub iterations: u64,
    /// How many samples were taken (ie. how many times we allocated the
    /// environment and measured the time).
    pub samples: usize,
    /// `true` if the benchmark ran out of time before reaching its
    /// `accuracy` target, or gave up without identifying a scaling law.
    pub hit_limit: bool,
}

impl ScalingStats {
    /// The standard error of [`Scaling::ns_per_scale`], in the same units
    /// as it - the absolute counterpart of
    /// [`ScalingStats::rel_std_error`], and what `Display` shows after the
    /// `±`. See that field for what the figure does and does not cover.
    pub fn std_error(&self) -> f64 {
        self.scaling.ns_per_scale * self.rel_std_error
    }
}
/// The timing and scaling results (without statistics) for a benchmark.
#[derive(Debug, PartialEq, Clone)]
pub struct Scaling {
    /// The scaling power
    /// If this is 2, for instance, you have an O(N²) algorithm.
    pub power: usize,
    /// An exponetial behavior, i.e. 2ᴺ
    pub exponential: usize,
    /// The time, in nanoseconds, per scaled size of the problem. If
    /// the problem scales as O(N²) for instance, this is the number
    /// of nanoseconds per N².
    pub ns_per_scale: f64,
}

impl Display for ScalingStats {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        // Same rules as `Stats`: value and error in one unit, the error to
        // two significant figures, and the value to exactly the precision
        // the error justifies. The unit is written once, outside the
        // parentheses, since it applies to both.
        let suffix = self.scaling.scale_suffix();
        let (div, unit) = unit_for(self.scaling.ns_per_scale);
        let value = self.scaling.ns_per_scale / div;
        let limit = if self.hit_limit { " (limit)" } else { "" };
        // R² stays, unlike on `Stats`, because here it is not a stand-in
        // for precision - it is the only signal about whether the right
        // *shape* was found, which no error bar on the constant can give.
        // Zero means the fit could not tell the candidate laws apart.
        if self.rel_std_error.is_nan() {
            write!(
                f,
                "{value:>8.2}{unit}{suffix} (± unknown, only {} samples){limit} (R²={:.3})",
                self.samples, self.goodness_of_fit
            )
        } else {
            let scaled = self.std_error() / div;
            let decimals = error_decimals(scaled);
            let error = if scaled > 0.0 && scaled < 1e-4 {
                format!("{scaled:.1e}")
            } else {
                format!("{scaled:.*}", decimals)
            };
            let shown = format!("({:.*} ± {}){}{}", decimals, value, error, unit, suffix);
            write!(f, "{shown:>22}{limit} (R²={:.3})", self.goodness_of_fit)
        }
    }
}
impl Scaling {
    /// The `/N²`-style suffix naming what `ns_per_scale` is measured per,
    /// without the number in front of it. Split out so that
    /// [`ScalingStats`] can print `(0.51 ± 0.02)ns/N` - the error belongs
    /// inside the parentheses with the value it qualifies, and the unit
    /// only wants saying once.
    fn scale_suffix(&self) -> String {
        let n_power = match self.power {
            0 => String::new(),
            1 => "N".to_string(),
            2 => "N²".to_string(),
            3 => "N³".to_string(),
            4 => "N⁴".to_string(),
            5 => "N⁵".to_string(),
            6 => "N⁶".to_string(),
            7 => "N⁷".to_string(),
            8 => "N⁸".to_string(),
            9 => "N⁹".to_string(),
            p => format!("N^{p}"),
        };
        match (self.exponential, self.power) {
            (1, 0) => "/iter".to_string(),
            (1, _) => format!("/{n_power}"),
            (e, 0) => format!("/{e}ᴺ"),
            (e, _) => format!("/({n_power}{e}ᴺ)"),
        }
    }
}

impl Display for Scaling {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        let per_iter = Duration::from_nanos(self.ns_per_scale as u64);
        let per_iter = if self.ns_per_scale < 1.0 {
            format!("{:.2}ns", self.ns_per_scale)
        } else if self.ns_per_scale < 10.0 {
            format!("{:.1}ns", self.ns_per_scale)
        } else {
            format!("{:?}", per_iter)
        };
        if self.exponential == 1 {
            match self.power {
                0 => write!(f, "{:>8}/iter", per_iter),
                1 => write!(f, "{:>8}/N   ", per_iter),
                2 => write!(f, "{:>8}/N²  ", per_iter),
                3 => write!(f, "{:>8}/N³  ", per_iter),
                4 => write!(f, "{:>8}/N⁴  ", per_iter),
                5 => write!(f, "{:>8}/N⁵  ", per_iter),
                6 => write!(f, "{:>8}/N⁶  ", per_iter),
                7 => write!(f, "{:>8}/N⁷  ", per_iter),
                8 => write!(f, "{:>8}/N⁸  ", per_iter),
                9 => write!(f, "{:>8}/N⁹  ", per_iter),
                _ => write!(f, "{:>8}/N^{}", per_iter, self.power),
            }
        } else {
            match self.power {
                0 => write!(f, "{:>8}/{}ᴺ", per_iter, self.exponential),
                1 => write!(f, "{:>8}/(N{}ᴺ)   ", per_iter, self.exponential),
                2 => write!(f, "{:>8}/(N²{}ᴺ)  ", per_iter, self.exponential),
                3 => write!(f, "{:>8}/(N³{}ᴺ)  ", per_iter, self.exponential),
                4 => write!(f, "{:>8}/(N⁴{}ᴺ)  ", per_iter, self.exponential),
                5 => write!(f, "{:>8}/(N⁵{}ᴺ)  ", per_iter, self.exponential),
                6 => write!(f, "{:>8}/(N⁶{}ᴺ)  ", per_iter, self.exponential),
                7 => write!(f, "{:>8}/(N⁷{}ᴺ)  ", per_iter, self.exponential),
                8 => write!(f, "{:>8}/(N⁸{}ᴺ)  ", per_iter, self.exponential),
                9 => write!(f, "{:>8}/(N⁹{}ᴺ)  ", per_iter, self.exponential),
                _ => write!(f, "{:>8}/(N^{}{}ᴺ)", per_iter, self.power, self.exponential),
            }
        }
    }
}

/// Benchmark the power-law scaling of the function, targeting the default
/// accuracy (see [`Config`]).
///
/// This function assumes that the function scales as 𝑶(𝑁ᴾ𝐸ᴺ).
/// It conisders higher powers for faster functions, and tries to
/// keep the measuring time around 10s.  It measures the power ᴾ and exponential base 𝐸
/// based on n R² goodness of fit parameter.
///
/// See [`Config::bench_scaling`] to choose your own accuracy.
pub fn bench_scaling<F, O>(f: F, nmin: usize) -> ScalingStats
where
    F: Fn(usize) -> O,
{
    Config::default().bench_scaling(f, nmin)
}

fn bench_scaling_with<F, O>(cfg: &Config, f: F, nmin: usize) -> ScalingStats
where
    F: Fn(usize) -> O,
{
    quiet::pin_if_requested();
    let mut data = Vec::new();
    // The time we started the benchmark (not used in results)
    let bench_start = Instant::now();

    // Collect data until BENCH_TIME_MAX is reached.
    for iters in slow_fib(BENCH_SCALE_TIME) {
        // Prepare the environments - nmin per iteration
        let n = if nmin > 0 { iters * nmin } else { iters };
        // Generate a Vec holding n's to hopefully keep the optimizer
        // from lifting the function out of the loop, as it could if
        // we had `f(n)` in there, and `f` were inlined or `const`.
        let xs = vec![n; iters];
        // Start the clock
        let iter_start = Instant::now();
        for x in xs.into_iter() {
            // Run the code and pretend to use the output
            black_box(f(x));
        }
        let time = iter_start.elapsed();
        data.push((n, iters, time));

        let elapsed = bench_start.elapsed();
        if elapsed > BENCH_TIME_MIN {
            if let Some(stats) = scaling_verdict(compute_scaling_gen(&data), cfg, elapsed) {
                return stats;
            }
        }
    }
    unreachable!()
}

/// Should a scaling sweep stop now?
///
/// Two independent questions have to come out right, and they are not
/// interchangeable. `goodness_of_fit` is zeroed by `compute_scaling_gen`
/// when the data could not tell the candidate laws apart, so it answers
/// *do I know the shape*; the accuracy target answers *do I know the
/// constant*. Identification gates the accuracy check rather than sitting
/// beside it, because a tightly-known constant attached to the wrong
/// scaling law is worse than useless - it is confidently wrong.
///
/// This replaces the old `goodness_of_fit > 0.99` rule, which had the same
/// flaw here that it had for flat benchmarks: R² asks whether the points
/// lie on *a* line of the assumed shape, which they can do beautifully
/// while the gradient itself is barely pinned down. Measured on this
/// crate's own scaling benchmarks, the R² rule stopped an O(2ᴺ) sweep with
/// a 7.4% error on the constant, and an O(N log N) sweep at 1.7%.
fn scaling_verdict(
    mut stats: ScalingStats,
    cfg: &Config,
    elapsed: Duration,
) -> Option<ScalingStats> {
    let identified = stats.goodness_of_fit > 0.0;
    let precise =
        identified && cfg.accuracy.is_met(stats.scaling.ns_per_scale, stats.std_error());
    // Keep going well past `max_time` for a sweep that has not even
    // identified a law yet, exactly as before: without a shape there is no
    // answer at all, whereas an imprecise constant is at least a number.
    // 12x reproduces the previous 10s/120s pair at the default budget.
    if precise || elapsed > cfg.max_time * 12 || (elapsed > cfg.max_time && identified) {
        stats.hit_limit = !precise;
        Some(stats)
    } else {
        None
    }
}

/// Benchmark the power-law scaling of the function with generated input
///
/// This function is like [`bench_scaling`], but uses a generating function
/// to construct the input to your benchmarked function.
///
/// This function assumes that the function scales as 𝑶(𝑁ᴾ𝐸ᴺ).
/// It conisders higher powers for faster functions, and tries to
/// keep the measuring time around 10s.  It measures the power ᴾ and exponential base 𝐸
/// based on n R² goodness of fit parameter.
///
/// # Example
/// ```
/// use scaling::bench_scaling_gen;
///
/// let summation = bench_scaling_gen(|n| vec![3.0; n], |v| v.iter().cloned().sum::<f64>(),0);
/// println!("summation: {}", summation);
/// assert_eq!(1, summation.scaling.power); // summation must run in linear time.
/// ```
/// which gives output
/// ```none
/// summation:    (1.206 ± 0.011)ns/N (R²=0.999)
/// ```
///
/// See [`Config::bench_scaling_gen`] to choose your own accuracy.
pub fn bench_scaling_gen<G, F, I, O>(gen_env: G, f: F, nmin: usize) -> ScalingStats
where
    G: FnMut(usize) -> I,
    F: Fn(&mut I) -> O,
{
    Config::default().bench_scaling_gen(gen_env, f, nmin)
}

fn bench_scaling_gen_with<G, F, I, O>(
    cfg: &Config,
    mut gen_env: G,
    f: F,
    nmin: usize,
) -> ScalingStats
where
    G: FnMut(usize) -> I,
    F: Fn(&mut I) -> O,
{
    quiet::pin_if_requested();
    let mut data = Vec::new();
    // The time we started the benchmark (not used in results)
    let bench_start = Instant::now();

    let mut am_slow = false;
    // Collect data until BENCH_TIME_MAX is reached.
    for iters in slow_fib(BENCH_SCALE_TIME) {
        // Prepare the environments - nmin per iteration
        let n = if nmin > 0 { iters * nmin } else { iters };
        let iters = if am_slow { 1 + (iters & 1) } else { iters };
        let mut xs = std::iter::repeat_with(|| gen_env(n))
            .take(iters)
            .collect::<Vec<I>>();
        // Start the clock
        let iter_start = Instant::now();
        // We iterate over `&mut xs` rather than draining it, because we
        // don't want to drop the env values until after the clock has stopped.
        for x in &mut xs {
            // Run the code and pretend to use the output
            black_box(f(x));
        }
        let time = iter_start.elapsed();
        if !am_slow && iters == 1 && time > Duration::from_micros(1) {
            am_slow = true;
        }
        data.push((n, iters, time));

        let elapsed = bench_start.elapsed();
        if elapsed > BENCH_TIME_MIN {
            if let Some(stats) = scaling_verdict(compute_scaling_gen(&data), cfg, elapsed) {
                return stats;
            }
        }
    }
    unreachable!()
}

/// This function assumes that the function scales as 𝑶(𝑁ᴾ𝐸ᴺ).  It measures the scaling
/// based on n R² goodness of fit parameter, and returns the best fit.
/// If it believes itself clueless, the goodness_of_fit is set to zero.
fn compute_scaling_gen(data: &[(usize, usize, Duration)]) -> ScalingStats {
    let num_n = {
        let mut ns = data.iter().map(|(n, _, _)| *n).collect::<Vec<_>>();
        ns.dedup();
        ns.len()
    };

    // If the first iter in a sample is consistently slow, that's fine -
    // that's why we do the linear regression. If the first sample is slower
    // than the rest, however, that's not fine.  Therefore, we discard the
    // first sample as a cache-warming exercise.

    // Compute some stats for each of several different
    // powers, to see which seems most accurate.
    let mut stats = Vec::new();
    let mut best = 0;
    let mut second_best = 0;
    for i in 1..num_n / 2 + 2 {
        for power in 0..i {
            let exponential = i - power;
            let pdata: Vec<_> = data[1..]
                .iter()
                .map(|&(n, i, t)| {
                    (
                        (exponential as f64).powi(n as i32)
                            * (n as f64).powi(power as i32)
                            * (i as f64),
                        t,
                    )
                })
                .collect();
            let (grad, r2, se) = fregression(&pdata);
            stats.push(ScalingStats {
                scaling: Scaling {
                    power,
                    exponential,
                    ns_per_scale: grad,
                },
                rel_std_error: se / grad,
                goodness_of_fit: r2,
                iterations: data[1..].iter().map(|&(x, _, _)| x as u64).sum(),
                samples: data[1..].len(),
                // Set by the caller, which is what knows about the budget.
                hit_limit: false,
            });
            if r2 > stats[best].goodness_of_fit || stats[best].goodness_of_fit.is_nan() {
                second_best = best;
                best = stats.len() - 1;
            }
        }
    }

    if num_n < 10 || stats[second_best].goodness_of_fit == stats[best].goodness_of_fit {
        stats[best].goodness_of_fit = 0.0;
    } else {
        // println!("finished...");
        // for s in stats.iter() {
        //     println!("  {}", s);
        // }
        // for d in data[data.len()-4..].iter() {
        //     println!("    {}, {} -> {} ns", d.0, d.1, d.2.as_nanos());
        // }
        // println!("best is {}", stats[best]);
    }
    stats[best].clone()
}

/// Compute the OLS linear regression line for the given data set, returning
/// the line's gradient and R². Requires at least 2 samples.
//
// Overflows:
//
// * sum(x * x): num_samples <= 0.5 * log_k (1 + 2 ^ 64 (FACTOR - 1))
fn fregression(data: &[(f64, Duration)]) -> (f64, f64, f64) {
    if data.len() < 2 {
        return (f64::NAN, f64::NAN, f64::NAN);
    }
    // Do all the arithmetic using f64, because it can happen that the
    // squared numbers to overflow using integer arithmetic if the
    // tests are too fast (so we run too many iterations).
    let data: Vec<_> = data
        .iter()
        .map(|&(x, y)| (x, y.as_nanos() as f64))
        .collect();
    let n = data.len() as f64;
    let xbar = data.iter().map(|&(x, _)| x).sum::<f64>() / n;
    let ybar = data.iter().map(|&(_, y)| y).sum::<f64>() / n;
    let ssxx = data.iter().map(|&(x, _)| (x - xbar).powi(2)).sum::<f64>();
    let ssyy = data.iter().map(|&(_, y)| (y - ybar).powi(2)).sum::<f64>();
    let ssxy = data
        .iter()
        .map(|&(x, y)| (x - xbar) * (y - ybar))
        .sum::<f64>();
    let gradient = ssxy / ssxx;
    let r2 = ssxy * ssxy / (ssxx * ssyy);
    assert!(r2.is_nan() || r2 <= 1.0);

    // Standard error of the gradient, via White's HC3 estimator:
    //
    //     Var(b) = Σ (xᵢ-x̄)² (eᵢ/(1-hᵢ))² / SSxx²,   hᵢ = 1/n + (xᵢ-x̄)²/SSxx
    //
    // rather than the textbook `sqrt(SSE/(n-2)/SSxx)`. The textbook form
    // assumes every point scatters equally about the line, and a scaling
    // sweep breaks that assumption on purpose: later samples do more work,
    // so they are noisier in absolute terms, and they are also the
    // high-leverage points because the sizes grow geometrically. Those two
    // facts compound.
    //
    // Measured over synthetic fits at this crate's own sample spacing,
    // comparing each estimator against the actual run-to-run spread of the
    // fitted gradient:
    //
    //     samples   noise model        textbook   HC3
    //        40     constant             1.00x    0.94x
    //        40     var ∝ x              1.67x    0.97x
    //        40     sd ∝ x               2.27x    0.99x
    //        60     sd ∝ x               2.24x    1.01x
    //
    // where >1 means the estimator claims a tighter error than really
    // occurs. The textbook form gets worse as samples accumulate, which is
    // the opposite of what a reader expects; HC3 stays honest, erring
    // slightly wide - the safe direction for an error bar.
    //
    // HC3 divides by `1 - hᵢ`, so it needs enough points for every leverage
    // to stay below 1; with two points the fit is exact, every residual is
    // zero, and there is no information about scatter at all.
    let std_error = if data.len() < 3 {
        f64::NAN
    } else {
        let intercept = ybar - gradient * xbar;
        let acc: f64 = data
            .iter()
            .map(|&(x, y)| {
                let dx = x - xbar;
                let leverage = 1.0 / n + dx * dx / ssxx;
                let adjusted = (y - (intercept + gradient * x)) / (1.0 - leverage);
                dx * dx * adjusted * adjusted
            })
            .sum();
        acc.sqrt() / ssxx
    };
    (gradient, r2, std_error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Duration;

    fn fib(n: usize) -> usize {
        let mut i = 0;
        let mut sum = 0;
        let mut last = 0;
        let mut curr = 1usize;
        while i < n - 1 {
            sum = curr.wrapping_add(last);
            last = curr;
            curr = sum;
            i += 1;
        }
        sum
    }

    // This is only here because doctests don't work with `--nocapture`.
    #[test]
    #[ignore]
    fn doctests_again() {
        println!();
        println!("fib 200: {}", bench(|| fib(200)));
        println!("fib 500: {}", bench(|| fib(500)));
        println!("fib scaling: {}", bench_scaling(|n| fib(n), 0));
        println!("reverse: {}", bench_env(vec![0; 100], |xs| xs.reverse()));
        println!("sort:    {}", bench_env(vec![0; 100], |xs| xs.sort()));

        // This is fine:
        println!("fib 1:   {}", bench(|| fib(500)));
        // This is NOT fine:
        println!(
            "fib 2:   {}",
            bench(|| {
                fib(500);
            })
        );
        // This is also fine, but a bit weird:
        println!(
            "fib 3:   {}",
            bench_env(0, |x| {
                *x = fib(500);
            })
        );
    }

    #[test]
    fn scales_o_one() {
        println!();
        let stats = bench_scaling(|_| thread::sleep(Duration::from_millis(10)), 1);
        println!("O(N): {}", stats);
        assert_eq!(stats.scaling.power, 0);
        println!("   error: {:e}", stats.scaling.ns_per_scale - 1e7);
        assert!((stats.scaling.ns_per_scale - 1e7).abs() < 1e6);
        // A constant function gives the fit nothing to distinguish the
        // candidate laws with, so it should say so rather than pick one:
        // `goodness_of_fit` zeroed, and `hit_limit` set because it never
        // reached an answer it was willing to stand behind.
        assert_eq!(0.0, stats.goodness_of_fit);
        assert!(stats.hit_limit);
        let shown = format!("{stats}");
        assert!(shown.contains('±'), "{shown}");
        assert!(shown.contains("R²"), "{shown}");
    }

    #[test]
    fn scales_o_n() {
        println!();
        let stats = bench_scaling(|n| thread::sleep(Duration::from_millis(10 * n as u64)), 1);
        println!("O(N): {}", stats);
        assert_eq!(stats.scaling.power, 1);
        println!("   error: {:e}", stats.scaling.ns_per_scale - 1e7);
        assert!((stats.scaling.ns_per_scale - 1e7).abs() < 1e5);

        println!("Summing integers");
        let stats = bench_scaling_gen(
            |n| (0..n as u64).collect::<Vec<_>>(),
            |v| v.iter().cloned().sum::<u64>(),
            1,
        );
        println!("O(N): {}", stats);
        println!("   error: {:e}", stats.scaling.ns_per_scale - 1e7);
        assert_eq!(stats.scaling.power, 1);
    }

    #[test]
    fn scales_o_n_log_n_looks_like_n() {
        println!("Sorting integers");
        let stats = bench_scaling_gen(
            |n| {
                (0..n as u64)
                    .map(|i| (i * 13 + 5) % 137)
                    .collect::<Vec<_>>()
            },
            |v| v.sort(),
            1,
        );
        println!("O(N log N): {}", stats);
        println!("   error: {:e}", stats.scaling.ns_per_scale - 1e7);
        assert_eq!(stats.scaling.power, 1);
    }

    #[test]
    fn scales_o_2_to_the_n() {
        println!();
        let stats = bench_scaling(|n| thread::sleep(Duration::from_nanos((1 << n) as u64)), 1);
        println!("O(2ᴺ): {}", stats);
        assert_eq!(stats.scaling.power, 0);
        assert_eq!(stats.scaling.exponential, 2);
        println!("   error: {:e}", stats.scaling.ns_per_scale - 1.0);
        assert!((stats.scaling.ns_per_scale - 1.0).abs() < 0.2);
    }

    #[test]
    fn scales_o_n_square() {
        println!();
        let stats = bench_scaling(
            |n| thread::sleep(Duration::from_millis(10 * (n * n) as u64)),
            1,
        );
        println!("O(N): {}", stats);
        assert_eq!(stats.scaling.power, 2);
        println!("   error: {:e}", stats.scaling.ns_per_scale - 1e7);
        assert!((stats.scaling.ns_per_scale - 1e7).abs() < 1e5);
    }

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
        assert!(stats.rel_std_error.is_nan());
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
    struct XorShift(u64);
    impl XorShift {
        fn next(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            x
        }
    }

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

    fn mean_and_spread(xs: &[f64]) -> (f64, f64) {
        let n = xs.len() as f64;
        let mean = xs.iter().sum::<f64>() / n;
        let sd = (xs.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / n).sqrt();
        (mean, sd / mean)
    }

    #[test]
    fn accuracy_matches_the_request() {
        println!();
        const REPEATS: usize = 15;
        for &target in &[0.05, 0.01] {
            let cfg = Config {
                accuracy: Accuracy::Relative(target),
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
        // Ground truth: a long, tight-target run.
        let truth = Config {
            accuracy: Accuracy::Relative(0.002),
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
        const REPEATS: usize = 10;
        // Wide gap between targets, and the tight one well below the noise
        // a single calibrated batch already has on its own: with only a
        // small gap, both targets get satisfied as soon as `MIN_SAMPLES` is
        // reached and the test can't distinguish "tighter target costs
        // more" from "both stopped at the same floor".
        let loose: Vec<Stats> = (0..REPEATS)
            .map(|r| {
                Config {
                    accuracy: Accuracy::Relative(0.05),
                    ..Config::default()
                }
                .bench(variable_cost(seed_for(r)))
            })
            .collect();
        let tight: Vec<Stats> = (0..REPEATS)
            .map(|r| {
                Config {
                    accuracy: Accuracy::Relative(0.003),
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
        assert!(Accuracy::Relative(0.01).is_met(0.0, 0.0));
        assert!(Accuracy::Absolute(Duration::from_nanos(50)).is_met(0.0, 0.0));

        // A real error still has to clear the bar, either way round.
        assert!(!Accuracy::Relative(0.01).is_met(100.0, 5.0));
        assert!(Accuracy::Relative(0.01).is_met(100.0, 0.5));
        assert!(!Accuracy::Absolute(Duration::from_nanos(1)).is_met(100.0, 5.0));
        assert!(Accuracy::Absolute(Duration::from_nanos(10)).is_met(100.0, 5.0));
    }

    #[test]
    fn display_reports_an_absolute_error_in_the_value_s_own_unit() {
        let shown = |ns: f64, rel: f64| {
            format!(
                "{}",
                Stats {
                    ns_per_iter: ns,
                    rel_std_error: rel,
                    iterations: 10,
                    samples: 6,
                    hit_limit: false,
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
    }

    #[test]
    fn an_absolute_accuracy_target_is_honoured() {
        println!();
        let tight = Config::absolute(Duration::from_nanos(5));
        let stats = tight.bench(variable_cost(7));
        println!("absolute 5ns: {stats}");
        assert!(!stats.hit_limit, "should have reached +-5ns in the budget");
        assert!(
            stats.std_error() < 5.0,
            "asked for +-5ns, got +-{:.2}ns",
            stats.std_error()
        );

        // A looser absolute ask must be cheaper - the target is doing the
        // work, not some fixed amount of sampling.
        let loose = Config::absolute(Duration::from_nanos(100));
        let cheap = loose.bench(variable_cost(7));
        println!("absolute 100ns: {cheap}");
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
            !stats.rel_std_error.is_nan(),
            "a standard error exists from {} samples and should be reported",
            stats.samples
        );
        // Stopped short, so we say so regardless of how tight it looks.
        assert!(stats.hit_limit);
        assert!(stats.ns_per_iter > 99.0e6);
    }

    #[test]
    fn unreachable_target_is_flagged() {
        println!();
        // An accuracy no amount of sampling will reach, and a budget far too
        // short to try: the benchmark must say it fell short rather than
        // return a confident-looking number.
        let cfg = Config {
            accuracy: Accuracy::Relative(1e-9),
            max_time: Duration::from_millis(50),
            ..Config::default()
        };
        let stats = cfg.bench(variable_cost(1));
        println!("{stats}");
        assert!(stats.hit_limit);
        assert!(!cfg.accuracy.is_met(stats.ns_per_iter, stats.rel_std_error));
    }

    /// The scaling error bar has to describe the spread that actually
    /// occurs, not merely be a number that shrinks when asked. This is the
    /// property that cannot be checked by reading the code: the estimator
    /// is HC3 precisely because the textbook OLS standard error is roughly
    /// 2x optimistic once the samples are heteroscedastic, which they are
    /// here, and only a repeated-measurement check can tell the two apart.
    #[test]
    fn scaling_error_bar_is_honest() {
        println!();
        const REPEATS: usize = 12;
        // A genuinely linear workload with real per-sample noise, so the
        // spread being measured is the estimator's, not the machine's.
        let runs: Vec<ScalingStats> = (0..REPEATS)
            .map(|_| {
                bench_scaling_gen(
                    |n| (0..n as u64).collect::<Vec<_>>(),
                    |v| v.iter().cloned().sum::<u64>(),
                    1,
                )
            })
            .collect();

        // Only compare runs that agreed on the law. `ns_per_scale` is
        // measured per `Nᴾ Eᴺ`, so a run that picked a different P is
        // reporting a different quantity in different units, and pooling
        // them would be comparing nanoseconds-per-N with
        // nanoseconds-per-N². Model selection here really is occasionally
        // wrong - roughly one run in twelve picks N² for this linear
        // workload - and when it is, its error bar is tight and
        // confidently wrong, which is the whole reason
        // `ScalingStats::rel_std_error` documents itself as conditional on
        // the law being right.
        let linear: Vec<&ScalingStats> = runs.iter().filter(|s| s.scaling.power == 1).collect();
        assert!(
            linear.len() * 2 > REPEATS,
            "only {} of {REPEATS} runs identified the linear law",
            linear.len()
        );

        let (_, observed) = mean_and_spread(
            &linear.iter().map(|s| s.scaling.ns_per_scale).collect::<Vec<_>>(),
        );
        let claimed = linear.iter().map(|s| s.rel_std_error).sum::<f64>() / linear.len() as f64;
        let ratio = observed / claimed;
        println!("claimed {:.3}%, observed {:.3}%, ratio {ratio:.2}x", 100.0 * claimed, 100.0 * observed);
        // Generous, like its flat-benchmark counterpart: a spread estimated
        // from a dozen runs is itself noisy, and run-to-run drift the
        // estimator cannot see (cache state, frequency) inflates the
        // observed side. The point is to catch an error bar that has gone
        // decorative, which is where the textbook OLS form was heading.
        assert!(
            ratio < 4.0,
            "claimed {:.3}% but observed spread was {:.3}% ({ratio:.1}x overconfident)",
            100.0 * claimed,
            100.0 * observed
        );
    }

    #[test]
    fn reported_error_is_honest() {
        println!();
        const REPEATS: usize = 40;
        for &target in &[0.05, 0.02, 0.01] {
            let cfg = Config {
                accuracy: Accuracy::Relative(target),
                ..Config::default()
            };
            let stats: Vec<Stats> = (0..REPEATS)
                .map(|r| cfg.bench(variable_cost(seed_for(r))))
                .collect();
            let claimed = stats.iter().map(|s| s.rel_std_error).sum::<f64>() / REPEATS as f64;
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

// Each time we take a sample we increase the number of iterations
// using a slow version of the Fibonacci sequence, which
// asymptotically grows exponentially, but also gives us a different
// value each time (except for repeating 1 twice, once for warmup).

// For our standard `bench_*` we use slow_fib(25), which was chosen to
// asymptotically match the prior behavior of the library, which grew
// by an exponential of 1.1.
const BENCH_SCALE_TIME: usize = 25;

fn slow_fib(scale_time: usize) -> impl Iterator<Item = usize> {
    #[derive(Debug)]
    struct SlowFib {
        which: usize,
        buffer: Vec<usize>,
    }
    impl Iterator for SlowFib {
        type Item = usize;
        fn next(&mut self) -> Option<usize> {
            // println!("!!! {:?}", self);
            let oldwhich = self.which;
            self.which = (self.which + 1) % self.buffer.len();
            self.buffer[self.which] = self.buffer[oldwhich] + self.buffer[self.which];
            Some(self.buffer[self.which])
        }
    }
    assert!(scale_time > 3);
    let mut buffer = vec![1; scale_time];
    // buffer needs just the two zeros to make it start with two 1
    // values.  The rest should be 1s.
    buffer[1] = 0;
    buffer[2] = 0;
    SlowFib { which: 0, buffer }
}

#[test]
fn test_fib() {
    // The following code was used to demonstrate that asymptotically
    // the SlowFib grows as the 1.1 power, just as the old code.  It
    // differs in that it increases linearly at the beginning, which
    // leads to larger numbers earlier in the sequence.  It also
    // differs in that it does not repeat any numbers in the sequence,
    // which hopefully leads to better linear regression, particularly
    // if we can only run a few iterations.
    let mut prev = 1;
    for x in slow_fib(25).take(200) {
        let rat = x as f64 / prev as f64;
        println!("ratio: {}/{} = {}", prev, x, rat);
        prev = x;
    }
    let five: Vec<_> = slow_fib(25).take(5).collect();
    assert_eq!(&five, &[1, 1, 2, 3, 4]);
    let more: Vec<_> = slow_fib(25).take(32).collect();
    assert_eq!(
        &more,
        &[
            1, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23,
            24, 25, 26, 28, 31, 35, 40, 46,
        ]
    );
    let previous_sequence: Vec<_> = (0..32).map(|n| (1.1f64).powi(n).round() as usize).collect();
    assert_eq!(
        &previous_sequence,
        &[
            1, 1, 1, 1, 1, 2, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 9, 10, 11, 12, 13,
            14, 16, 17, 19,
        ]
    );
    let previous_sequence: Vec<_> = (20..40)
        .map(|n| (1.1f64).powi(n).round() as usize)
        .collect();
    assert_eq!(
        &previous_sequence,
        &[7, 7, 8, 9, 10, 11, 12, 13, 14, 16, 17, 19, 21, 23, 26, 28, 31, 34, 37, 41,]
    );
}

