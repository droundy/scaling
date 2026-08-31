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
fib 200:    71.716ns ± 0.057ns
fib 500:    262.75ns ± 0.14ns
fib scaling:  (0.5567 ± 0.0036)ns/N (R²=0.999)
reverse:     51.80ns ± 0.62ns
sort:        111.3ns ± 1.1ns
```

Easy! However, please read the [caveats](#caveats) below before using.

# Benchmarking algorithm

An *iteration* is a single execution of your code. A *sample* is a measurement,
during which your code may be run many times. In other words: taking a sample
means performing some number of iterations and measuring the total time.

## Flat benchmarks: `bench`, `bench_env`, `bench_gen_env`

An *iteration* is a single execution of your code. A *sample* is a
measurement, during which your code may be run many times.

We first calibrate a batch size: the number of iterations per sample, chosen
so that the two clock reads bracketing a sample are a rounding error against
it. We then take equal-sized batches, and stop once the *standard error* of
their mean is small enough. This directly answers "how precisely do I know
`ns_per_iter`", which is what you actually want.

You may choose how accurate you want your benchmarks to be (see [`Config`])
or you may accept a reasonable default. Both a relative and an absolute goal
are available, and sampling stops as soon as either is met, so the coarser
one governs.

The output includes the standard error of the measurement, printed after
the `±` in the same unit as the measurement itself, and the measurement is
printed to exactly the precision that error justifies.

[`Stats::std_error`] and [`Stats::rel_std_error`] give the error absolutely
and relatively, [`Stats::iterations`] and [`Stats::samples`] say how much
work it took, [`Stats::hit_limit`] tells you if the budget ran out before
the target accuracy was met, and [`Stats::untrustworthy`] tells you if too
few samples were collected for the error bar itself to mean anything.
Those are marked `(limit)` and `(untrusted)` in the output.

If a benchmark requires some state to run, one copy of the initial state is
prepared per iteration.

## Scaling benchmarks: `bench_scaling`, `bench_scaling_gen`

These work in two stages, and the split is the point of the design.

**Stage one picks the sizes.** It climbs from `nmin`, timing a single call
at each candidate, until one call is long enough to time properly - below
about 20 microseconds you are measuring the clock, not the function. That
size becomes the bottom of the ladder, and the growth rate measured along
the way says how much a larger size will cost, which is what lets stage two
choose a top it can actually afford. These measurements steer and nothing
else; none of them ends up in the answer.

**Stage two measures.** It lays out six log-spaced sizes and times a single
call at each, repeating - six times to begin with, more until the answer is
precise enough. Repeating is what makes the difference: each size ends up
with an error bar that was *measured* rather than assumed, and that changes
what can be asked of the fit.

The range is chosen in *time*, not in size: far enough up that the largest
size takes four times as long as the smallest, which stage one's growth rate
converts into a size range. A fixed size range would mean a different time
range for every benchmark, since four times the size is four times the work
for a linear cost and sixty-four times for a cubic one. No time budget is
consulted - what makes a range good is that it separates the powers while
costing little, and stage one already measured both of those. The opening
rounds come to about eighty times the cheapest call, so a benchmark sitting
on the 20-microsecond floor is measured in under two milliseconds.

Nothing is batched. A benchmark worth asking about the scaling of is one
that gets slow as `N` grows, so where a batch would have been needed to
out-measure the clock, a larger `N` does the same job and tells you
something you wanted to know anyway. It also keeps the model honest: timing
a batch and dividing by its length turns the fixed per-batch overhead into a
`c/N` term, which no polynomial in `N` can represent, so it comes out
smeared across every coefficient. One call per sample leaves that overhead
as a plain constant, which the fit represents exactly.

Two separate things then have to be settled, and the output reports them
separately because they fail independently:

* **Which law?** With measured error bars this becomes a real
  goodness-of-fit test rather than a heuristic: chi-squared asks whether
  what the model failed to explain is as small as the error bars say it
  should be. A cost that no polynomial describes is rejected outright, and
  reported with `goodness_of_fit` zeroed and the `(limit)` mark, alongside
  the integer power it most behaves like over the range measured.
* **How big is its constant?** [`ScalingStats::rel_std_error`] answers this,
  and it is what the `±` in the output shows. Measuring continues until it
  meets the same accuracy target the flat benchmarks use, so
  `(43.1 ± 1.2)ns/N` means the same kind of thing as `43.1ns ± 1.2ns` does
  for [`bench`].

The two are deliberately not merged into one number, and measured error bars
are what keeps them apart. Where errors are only assumed, the usual move is
to widen them by however badly the fit turned out - which quietly converts
"wrong shape" into "imprecise constant", and hides exactly the failure worth
knowing about. Here the coefficient errors come from the sizes and their
error bars alone and never see the timings, so a bad shape has nowhere to
hide but chi-squared. Read the two together, and be suspicious of a small
`±` sitting next to `R²=0.000`.

Measuring stops once the model is accepted *and* its constant meets the
accuracy target, or when the time budget runs out - 10 seconds by default,
which is a backstop rather than something to be spent. Both conditions are
needed, because a wrong model does not present as an imprecise one: fit a
constant to a cost that grows and its prefactor is near enough the mean of
every measurement, precise immediately and quite wrong.

Only polynomial costs are fitted at present. An `O(2ᴺ)` cost is not
identified as such; it is rejected, and reported as the integer power that
best approximates it over the range measured.

# Caveats

## Caveat 1: Harness overhead

**TL;DR: Compile with `--release`; the overhead is likely to be within the
**noise of your
benchmark.**

Work which `scaling` does once-per-sample is kept negligible: the flat
benchmarks size each batch so that a sample takes far longer than the two
`Instant::now()` calls bracketing it, and the scaling benchmarks choose
sizes large enough that a single call dwarfs them. However, work which is
done once-per-iteration *will* be counted in the final times.

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
fib_1:   262.759ns ± 0.079ns
fib_2:   0.59300ns ± 0.00075ns
fib_3:   262.805ns ± 0.025ns
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

**TL;DR: on Linux, ``sudo `which quiet-bench` reserve 2`` then
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
/// on the order of 100 ns together - are a rounding error against it, and
/// short enough that a 10 second budget still fits thousands of samples.
const SAMPLE_TIME: Duration = Duration::from_millis(1);

/// A backstop on the number of samples, so the vector of them cannot grow
/// without bound.
///
/// This is about memory, not about the measurement: `max_time` is the real
/// budget, and at [`SAMPLE_TIME`] it allows ~10_000 samples, a hundred
/// times below this.
const MAX_SAMPLES: usize = 1_000_000;

/// How hard a benchmark works to pin down `ns_per_iter`, and when it gives
/// up.
///
/// [`bench`], [`bench_env`] and [`bench_gen_env`] use [`Config::default`];
/// call the same-named methods on a `Config` to choose your own.
///
/// ```
/// use scaling::Config;
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
/// # let _ = (tight.target_rel_error, quick.target_abs_error);
/// ```
#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    /// Stop once the standard error falls below this fraction of the
    /// measurement (`0.01` = 1%).
    pub target_rel_error: f64,
    /// Stop once the standard error falls below this duration.
    ///
    /// Sampling stops as soon as *either* goal is met, so whichever is
    /// coarser for the function at hand is the one that ends up governing.
    /// That is the point of having both: a 1% relative goal on a 1 ns
    /// function asks for a precision finer than the clock can resolve, and
    /// would otherwise spend the whole budget failing to reach it. An
    /// absolute floor puts a bound on how much precision is worth chasing.
    ///
    /// `Duration::ZERO` disables it, leaving `target_rel_error` alone in
    /// charge.
    pub target_abs_error: Duration,
    /// Give up after roughly this much wall-clock time even if neither goal
    /// was reached, setting [`Stats::hit_limit`].
    pub max_time: Duration,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            target_rel_error: 0.01,
            target_abs_error: Duration::ZERO,
            max_time: BENCH_TIME_MAX,
        }
    }
}

impl Config {
    /// Ask for a standard error below `fraction` of the measurement
    /// (`0.001` = 0.1%).
    pub fn relative(fraction: f64) -> Self {
        Config {
            target_rel_error: fraction,
            ..Config::default()
        }
    }

    /// Ask for a standard error below `error` in absolute terms.
    ///
    /// This sets only the absolute goal, leaving the relative one at its
    /// default, so sampling stops at whichever of the two is reached first.
    pub fn absolute(error: Duration) -> Self {
        Config {
            target_abs_error: error,
            ..Config::default()
        }
    }

    /// Is a measurement of `ns_per_iter` with standard error `std_error`
    /// (both in nanoseconds) precise enough to stop?
    fn accuracy_met(&self, ns_per_iter: f64, std_error: f64) -> bool {
        // A standard error of exactly zero means every sample agreed to the
        // limit of the timer's resolution, and no further sampling can
        // improve on that.
        if std_error == 0.0 {
            return true;
        }
        std_error < self.target_rel_error * ns_per_iter
            || std_error < self.target_abs_error.as_secs_f64() * 1e9
    }
}

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
            // NaN has two distinct causes: too few samples to estimate a
            // standard error from (only possible via the single-sample
            // "blew the whole time budget already" path), or - rarer, but
            // possible for a near-free benchmark on a coarse timer - a
            // measured time of exactly zero for every sample, which makes
            // the relative error a literal 0/0 regardless of sample count.
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
            let scaled = self.std_error / div;
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

    /// Benchmark the power-law scaling of a function.
    ///
    /// See [`bench_scaling`] for the default-accuracy version.
    ///
    /// The accuracy applies to [`Scaling::ns_per_scale`], the constant in
    /// front of the fitted law, and only once the law itself has been
    /// identified - see [`ScalingStats::rel_std_error`] for why those are
    /// two different questions.
    ///
    /// `target_rel_error` is the one that makes sense here.
    /// `target_abs_error` is accepted and well defined, but its units are
    /// nanoseconds per `Nᴾ Eᴺ`, which makes it confusing.
    pub fn bench_scaling<F, O>(&self, f: F, nmin: usize) -> ScalingStats
    where
        F: Fn(usize) -> O,
    {
        bench_scaling_with(self, f, nmin)
    }

    /// Benchmark the power-law scaling of a function with a generated input.
    ///
    /// See [`bench_scaling_gen`] for the default-accuracy version, and
    /// [`Config::bench_scaling`] for what the accuracy applies to.
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
    // entirely by the timed portion approaching `SAMPLE_TIME`. But if
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
#[derive(Default, Clone)]
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
        // `abs` because a fit is free to land on a negative coefficient -
        // a term the data does not really support can come out either side
        // of zero - and an error bar is a width, which has no sign.
        self.scaling.ns_per_scale.abs() * self.rel_std_error
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
        if self.std_error().is_nan() {
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

/// Benchmark the power-law scaling of the function.
///
/// Uses the default accuracy (see [`Config`]).
///
/// Reports the integer power ᴾ in 𝑶(𝑁ᴾ) and the constant in front of it,
/// with a standard error. Sizes are chosen by a first stage that climbs
/// until a single call is long enough to time; each is then measured
/// repeatedly, so the fit is judged against error bars that were measured
/// rather than assumed. A cost that no polynomial describes is rejected -
/// `goodness_of_fit` zeroed and `hit_limit` set - rather than fitted anyway.
/// Takes around 10s by default; see [`Config::max_time`].
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
    scaling_sweep(cfg, nmin, |n| {
        // `black_box` on the size as well as the result: without it the
        // optimiser can see a literal `n` and lift the whole call out, the
        // job the old code did by running over a `vec![n; iters]`.
        let n = black_box(n);
        let start = Instant::now();
        black_box(f(n));
        start.elapsed().as_secs_f64() * 1e9
    })
}

/// Run both stages and turn the result into a [`ScalingStats`].
///
/// `measure(n)` performs one call at size `n` and returns its cost in
/// nanoseconds; the two entry points differ only in how they build it.
///
/// The budget splits between the stages rather than being shared: stage one
/// gets [`DISCOVERY_SHARE`] to find the sizes and stage two gets what is
/// left to measure them properly. Keeping them separate means a slow
/// discovery cannot starve the measurement it exists to set up.
fn scaling_sweep(
    cfg: &Config,
    nmin: usize,
    mut measure: impl FnMut(usize) -> f64,
) -> ScalingStats {
    let mut calls = 0u64;
    let mut counted = |n: usize| {
        calls += 1;
        measure(n)
    };

    let range = discover_sizes(nmin, cfg.max_time.mul_f64(DISCOVERY_SHARE), &mut counted);
    let sizes = choose_sizes(range, nmin);

    // Each degree costs a size, and a fit needs more sizes than terms.
    let max_degree = MAX_DEGREE.min(sizes.len().saturating_sub(2));
    let measured = measure_scaling(
        &sizes,
        cfg,
        cfg.max_time.mul_f64(1.0 - DISCOVERY_SHARE),
        max_degree,
        &mut counted,
    );

    // One call per sample, so these two counts are the same number. Both
    // are kept because they answer different questions for a caller, and
    // because the plain-`bench` side of the crate still distinguishes them.
    let samples = calls as usize;
    let Some(fit) = measured.fit else {
        // No degree cleared its own error bar - the cost did not measurably
        // grow, and did not measurably do anything else either.
        return ScalingStats {
            scaling: Scaling {
                power: 0,
                exponential: 1,
                ns_per_scale: 0.0,
            },
            rel_std_error: f64::NAN,
            goodness_of_fit: 0.0,
            iterations: calls,
            samples,
            hit_limit: true,
        };
    };
    // A polynomial that misses by far more than the error bars allow has
    // not identified anything, whatever its coefficients came out as, so it
    // says so in the way callers already understand: a zeroed
    // `goodness_of_fit`, and the limit flag set.
    // No special case for a rejected fit: the power came from the log-log
    // slope either way, which is the exponent the cost behaves like over
    // the range measured whether or not a polynomial describes it exactly.
    // Rejection changes what we claim, not what we measured.
    let rejected = !(fit.chi2_per_dof <= CHI2_REJECT);
    ScalingStats {
        scaling: Scaling {
            power: fit.power,
            exponential: 1,
            ns_per_scale: fit.ns_per_scale,
        },
        rel_std_error: fit.std_error / fit.ns_per_scale.abs(),
        // R² is the share of the spread a model accounts for, and a
        // constant model accounts for none of it by construction - the fit
        // *is* the mean, so R² is identically zero however well it
        // describes the data. Reporting that zero would claim the shape was
        // unidentified when it was in fact settled, so a constant that
        // survived the chi-squared test is reported as the perfect fit it is.
        goodness_of_fit: if rejected {
            0.0
        } else if fit.power == 0 {
            1.0
        } else {
            fit.r2.max(0.0)
        },
        iterations: calls,
        samples,
        hit_limit: measured.hit_limit || rejected,
    }
}


/// Benchmark the power-law scaling of the function with generated input
///
/// This function is like [`bench_scaling`], but uses a generating function
/// to construct the input to your benchmarked function.
///
/// Reports the integer power ᴾ in 𝑶(𝑁ᴾ) and the constant in front of it,
/// with a standard error. Sizes are chosen by a first stage that climbs
/// until a single call is long enough to time; each is then measured
/// repeatedly, so the fit is judged against error bars that were measured
/// rather than assumed. A cost that no polynomial describes is rejected -
/// `goodness_of_fit` zeroed and `hit_limit` set - rather than fitted anyway.
/// Takes around 10s by default; see [`Config::max_time`].
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
    scaling_sweep(cfg, nmin, |n| {
        // Build the environment before the clock starts and drop it after
        // the clock stops, so neither generation nor drop lands in the
        // measurement.
        let mut x = gen_env(n);
        let start = Instant::now();
        black_box(f(&mut x));
        let elapsed = start.elapsed();
        drop(x);
        elapsed.as_secs_f64() * 1e9
    })
}


// The polynomial fit below is validated against synthetic data in
// `tests::fitting`, but does not yet drive `compute_scaling_gen`: wiring it
// in changes what gets reported for real workloads (notably `N log N`, which
// is not a polynomial at all), so it lands separately from the machinery.

// Not yet wired into the two-stage measurement, which is deliberately
// limited to polynomials. This is the half that names `N log N` and
// exponential costs, and it is what `scales_o_2_to_the_n` is ignored
// pending; it is kept, and kept tested, rather than deleted and rewritten.
#[allow(dead_code)]
/// Fit `y = c0 + c1 x + ... + c_degree x^degree` by weighted least
/// squares, returning each coefficient with its standard error.
///
/// `x` is rescaled to `(0, 1]` before fitting and the coefficients scaled
/// back afterwards. A raw Vandermonde matrix over `N` up to a few thousand
/// is catastrophically ill-conditioned; over `(0, 1]` it is merely awkward,
/// and comfortably within `f64` for the handful of degrees considered here.
///
/// The standard errors come from the diagonal of `(XᵀWX)⁻¹`, so they
/// account for the correlation between powers - which is the whole
/// difficulty with polynomial fits, since `N` and `N²` are far from
/// independent over a bounded range.
fn poly_fit(xs: &[f64], ys: &[f64], ws: &[f64], degree: usize) -> Option<(Vec<f64>, Vec<f64>)> {
    let terms = degree + 1;
    let n = xs.len();
    if n <= terms {
        return None;
    }
    let scale = xs.iter().cloned().fold(0.0, f64::max);
    // `is_finite` first so the comparison never has to reason about NaN.
    if !scale.is_finite() || scale <= 0.0 {
        return None;
    }
    // Design matrix rows, in the rescaled variable.
    let rows: Vec<Vec<f64>> = xs
        .iter()
        .map(|&x| {
            let u = x / scale;
            (0..terms).map(|j| u.powi(j as i32)).collect()
        })
        .collect();

    // Normal equations: (XᵀWX) c = XᵀWy.
    let mut a = vec![vec![0.0; terms]; terms];
    let mut b = vec![0.0; terms];
    for i in 0..n {
        let w = ws[i];
        for j in 0..terms {
            b[j] += w * rows[i][j] * ys[i];
            for k in 0..terms {
                a[j][k] += w * rows[i][j] * rows[i][k];
            }
        }
    }
    let inv = invert(&a)?;
    let coef: Vec<f64> = (0..terms)
        .map(|j| (0..terms).map(|k| inv[j][k] * b[k]).sum())
        .collect();

    // Weighted residual variance, then scale the inverse by it.
    let mut chi2 = 0.0;
    for i in 0..n {
        let pred: f64 = (0..terms).map(|j| coef[j] * rows[i][j]).sum();
        chi2 += ws[i] * (ys[i] - pred).powi(2);
    }
    let s2 = chi2 / (n - terms) as f64;
    let se: Vec<f64> = (0..terms).map(|j| (s2 * inv[j][j]).sqrt()).collect();

    // Undo the rescaling: a coefficient on u^j is one on x^j / scale^j.
    let unscale = |v: &Vec<f64>| -> Vec<f64> {
        (0..terms).map(|j| v[j] / scale.powi(j as i32)).collect()
    };
    Some((unscale(&coef), unscale(&se)))
}

/// A polynomial fit against sizes whose error bars were *measured* rather
/// than inferred.
#[derive(Debug, Clone, PartialEq)]
struct PolyFit {
    /// Coefficient of each power, `coefficients[k]` multiplying `Nᵏ`.
    coefficients: Vec<f64>,
    /// Standard error of each coefficient.
    ses: Vec<f64>,
    /// Chi-squared per degree of freedom: near 1 when this polynomial
    /// describes the measurements within their own error bars, large when
    /// it does not.
    chi2_per_dof: f64,
    /// Weighted R²: the share of the spread in the measurements that this
    /// polynomial accounts for.
    r2: f64,
}

/// Fit `t = c₀ + c₁N + ... + c_degree·N^degree` to sizes measured with
/// known standard errors.
///
/// Differs from a plain weighted least squares in where the coefficient
/// errors come from. When the point errors are only *assumed*, the usual
/// move is to scale the covariance by the residual variance - the fit
/// grades its own uncertainty from how well it happened to fit. Here each
/// `se` was measured by repeating the benchmark at that size, so the
/// covariance is `(XᵀWX)⁻¹` outright with `W = 1/se²`, and no rescaling is
/// wanted.
///
/// That separation is the point: the coefficient errors then say how well
/// the coefficients are known, and `chi2_per_dof` independently says
/// whether the model fits at all. Conflated, a bad model quietly inflates
/// the error bars instead of admitting it is the wrong shape.
fn weighted_poly_fit(ns: &[f64], means: &[f64], ses: &[f64], degree: usize) -> Option<PolyFit> {
    let terms = degree + 1;
    let n = ns.len();
    if n <= terms {
        return None;
    }
    let scale = ns.iter().cloned().fold(0.0, f64::max);
    if !scale.is_finite() || scale <= 0.0 {
        return None;
    }
    let mut ws = Vec::with_capacity(n);
    for &se in ses {
        if !se.is_finite() || se <= 0.0 {
            return None;
        }
        ws.push(1.0 / (se * se));
    }
    // Rescale sizes into (0, 1] before building the design: a raw
    // Vandermonde over sizes in the thousands is hopeless in f64.
    let rows: Vec<Vec<f64>> = ns
        .iter()
        .map(|&x| {
            let u = x / scale;
            (0..terms).map(|j| u.powi(j as i32)).collect()
        })
        .collect();
    let mut a = vec![vec![0.0; terms]; terms];
    let mut b = vec![0.0; terms];
    for i in 0..n {
        for j in 0..terms {
            b[j] += ws[i] * rows[i][j] * means[i];
            for k in 0..terms {
                a[j][k] += ws[i] * rows[i][j] * rows[i][k];
            }
        }
    }
    let inv = invert(&a)?;
    let coef: Vec<f64> = (0..terms)
        .map(|j| (0..terms).map(|k| inv[j][k] * b[k]).sum())
        .collect();
    let chi2: f64 = (0..n)
        .map(|i| {
            let pred: f64 = (0..terms).map(|j| coef[j] * rows[i][j]).sum();
            ws[i] * (means[i] - pred).powi(2)
        })
        .sum();
    // No residual-variance factor here - see the note above.
    let se: Vec<f64> = (0..terms).map(|j| inv[j][j].max(0.0).sqrt()).collect();
    let unscale = |v: &[f64]| -> Vec<f64> {
        (0..terms).map(|j| v[j] / scale.powi(j as i32)).collect()
    };
    // Weighted R², about the weighted mean. Reported alongside chi-squared
    // rather than instead of it: R² says how much of the spread the fit
    // accounts for, which is flattering whenever the spread is large, while
    // chi-squared asks the sharper question of whether what is left over is
    // as small as the error bars say it should be.
    let wsum: f64 = ws.iter().sum();
    let wmean: f64 = (0..n).map(|i| ws[i] * means[i]).sum::<f64>() / wsum;
    let sstot: f64 = (0..n).map(|i| ws[i] * (means[i] - wmean).powi(2)).sum();
    Some(PolyFit {
        coefficients: unscale(&coef),
        ses: unscale(&se),
        chi2_per_dof: chi2 / (n - terms) as f64,
        r2: if sstot > 0.0 { 1.0 - chi2 / sstot } else { 0.0 },
    })
}

/// The highest power stage two will consider.
///
/// Cubic is already past what a benchmark can usually distinguish, and each
/// extra degree costs a size: [`weighted_poly_fit`] needs more sizes than
/// terms, so degree three needs five.
const MAX_DEGREE: usize = 3;

/// How much of the budget stage one may spend finding sizes.
///
/// Discovery is reconnaissance - every measurement it takes is a single
/// unreplicated call, useful for steering and not for the answer - so it
/// gets a minority share and stage two keeps the rest.
const DISCOVERY_SHARE: f64 = 0.25;

/// The most sizes stage one's climb will try before giving up on going
/// higher.
const MAX_CLIMB_STEPS: usize = 8;

/// How many sizes stage two measures.
///
/// More than two, because two points can be joined by a line of any shape
/// and say nothing about which; and enough that a cubic still has degrees
/// of freedom left over, so chi-squared has something to test.
const NUM_SIZES: usize = 6;

/// The least a single call may cost and still be worth measuring.
///
/// This is the floor stage one climbs to. `Instant` resolves to some tens
/// of nanoseconds, so at 20 microseconds the clock contributes on the order
/// of 0.1% - below the noise in anything worth benchmarking, and far below
/// the accuracy anyone asks for. Sizes cheaper than this do not measure the
/// function, they measure the clock.
const MIN_MEASURABLE: Duration = Duration::from_micros(20);

/// How much slower the largest size should be than the smallest.
///
/// The ladder is placed in *time*, not in size, and this is the whole of
/// the choice. Time is where the cost lives and where the accuracy comes
/// from, so it is the quantity worth being deliberate about; the size range
/// is then whatever delivers it, `TIME_SPAN^(1/p)`, which is four for a
/// linear cost, two for a quadratic one and 1.59 for a cubic. Choosing a
/// size span directly would mean choosing a different time span for every
/// benchmark without meaning to.
///
/// Four rather than two because it doubles the range for about half as much
/// again in cost: six rungs log-spaced over a time ratio `T` cost
/// `(T^(6/5) - 1)/(T^(1/5) - 1)` times the cheapest one, which is 8.7 at
/// `T = 2` and 13.4 at `T = 4`.
const TIME_SPAN: f64 = 4.0;

/// The most the sizes may span however slowly the cost grows.
///
/// A nearly flat cost would otherwise ask for an unbounded size range to
/// reach [`TIME_SPAN`], and there is a point past which a wider range
/// measures a different machine - one whose working set still fits in
/// cache - rather than a smaller version of the same one.
const MAX_SIZE_SPAN: f64 = 64.0;

/// Above this chi-squared per degree of freedom, the polynomial is not the
/// right shape and we decline to call it the scaling.
///
/// Chi-squared per degree of freedom sits near 1 when a model accounts for
/// the data to within its error bars, so the threshold belongs near 1 too:
/// with five degrees of freedom a correct model gives 1 ± 0.63, and three
/// is a comfortable three sigma above that.
///
/// It sat at ten for a while, on the reasoning that erring loose was cheap.
/// It is not cheap. Ten accepts a model wrong by an order of magnitude, and
/// what it let through was a *constant* accepted as the shape of a linear
/// cost whenever the measurements were noisy enough. That failure then
/// hides itself: the prefactor of a degree-zero fit is near enough the mean
/// of every measurement, so it looks precise immediately and the refinement
/// loop stops before collecting the data that would have shown the trend.
/// A wrong model never announces itself as imprecise, so the accuracy
/// target cannot catch it - it has to be rejected here or not at all.
///
/// Not 1 exactly, because these error bars are themselves measured from six
/// replicates and uncertain by about a third, and because timing noise is
/// not Gaussian. Three leaves room for that without leaving room for a
/// model that is simply wrong. Erring tight is also the safer direction:
/// rejecting a good fit costs a fallback to the log-log slope, which for a
/// true power law returns the same integer anyway.
const CHI2_REJECT: f64 = 3.0;

/// What stage one hands to stage two: the smallest size worth measuring,
/// what one call there costs, and how fast cost grows with size.
///
/// Deliberately a *lower* bound and a rate rather than a list of sizes.
/// Stage one takes single unreplicated measurements, which are fine for
/// steering and not fit to draw conclusions from, so it should decide as
/// little as possible - and the largest size is not its decision to make,
/// because it depends on a budget only stage two knows how it will spend.
#[derive(Debug, Clone, Copy, PartialEq)]
struct SizeRange {
    /// The floor of stage two's ladder: the *largest* size stage one tried.
    ///
    /// Largest, despite being a lower bound, because stage one climbs from
    /// below and stops at the first size worth measuring. Every size under
    /// it has been tried and found too cheap to time, so this is the only
    /// one stage one has evidence for - and being the last rung of a
    /// cautious climb, it is affordable by construction.
    lo: usize,
    /// Nanoseconds for one call at `lo`.
    ///
    /// The unit stage two budgets in: knowing what the cheapest rung costs,
    /// and how fast cost grows from it, is what turns a budget into a
    /// ladder. Stage one keeps it under a ceiling set from that budget, so
    /// a full opening round is affordable however the ladder comes out.
    lo_time: f64,
    /// Continuous exponent: cost grows about as `N^exponent`.
    ///
    /// Continuous, not rounded. Rounding here would throw away exactly the
    /// information [`choose_sizes`] needs, which is not what the answer is
    /// but how expensive a larger size will turn out to be.
    exponent: f64,
}

/// Stage one: find the smallest size worth measuring, and how fast cost
/// grows from there.
///
/// Climbs from `nmin` until one call clears [`MIN_MEASURABLE`], and stops
/// there. Every measurement here is a single call.
///
/// `budget` is a backstop against a pathological climb, not a target: the
/// climb stops at the floor, and what it costs is whatever getting there
/// cost. Nothing here is sized against the time available.
fn discover_sizes(
    nmin: usize,
    budget: Duration,
    mut measure: impl FnMut(usize) -> f64,
) -> SizeRange {
    let step = nmin.max(1);
    let budget_ns = budget.as_secs_f64() * 1e9;
    let floor_ns = MIN_MEASURABLE.as_secs_f64() * 1e9;

    let mut last_n = step;
    let mut last_t = measure(step);
    let mut spent = last_t;
    // The rung we will hand over: the largest size tried, which is the
    // first one to clear the floor. Until something clears it, the largest
    // we have is the best we can offer, however untrustworthy its timing.
    let mut lo = (last_n, last_t);
    let mut prev: Option<(f64, f64)> = None;
    let mut exponent = 1.0;
    // See `measure_scaling`: the calls are not the only thing that costs
    // time here either.
    let started = Instant::now();

    for _ in 0..MAX_CLIMB_STEPS {
        if started.elapsed().as_secs_f64() * 1e9 >= budget_ns {
            break;
        }
        if let Some((pn, pt)) = prev {
            if let Some(p) = two_point_exponent(pn, pt, last_n as f64, last_t) {
                exponent = p;
            }
        }
        // Measurable, and affordable: done climbing.
        if last_t >= floor_ns {
            lo = (last_n, last_t);
            break;
        }
        let left = Duration::from_secs_f64((budget_ns - spent).max(0.0) / 1e9);
        // Aim each step at the floor we are trying to clear; `next_size`
        // caps the growth and keeps the step affordable.
        let Some(next) = next_size(last_n as f64, last_t, exponent, MIN_MEASURABLE, left)
        else {
            break;
        };
        // Sizes are multiples of `nmin`, which is the caller's unit.
        let next = (next / step).max(1) * step;
        if next <= last_n {
            break;
        }
        let t = measure(next);
        spent += t;
        prev = Some((last_n as f64, last_t));
        last_n = next;
        last_t = t;
        lo = (last_n, last_t);
    }

    // A climb that never took a step has a floor but no growth rate, and
    // `exponent` is still the assumed 1.0. That assumption is not harmless:
    // `choose_sizes` budgets with it, so believing an N-cubed cost to be
    // linear plans a ladder whose top rung costs hundreds of times what was
    // predicted. One probe upward buys the real rate. It aims at the
    // ceiling rather than the floor, which is already behind us, and `lo`
    // does not move - the probe is reconnaissance, not a rung, and may well
    // land somewhere too expensive to be one.
    if prev.is_none() {
        let left = Duration::from_secs_f64((budget_ns - spent).max(0.0) / 1e9);
        // Aim at where the ladder's top will be - `TIME_SPAN` times the
        // cost we are at. The floor is behind us, so aiming there would ask
        // for a step backwards and get none; aiming at the top measures the
        // growth rate across exactly the range it will be used to describe.
        let aim = Duration::from_secs_f64(last_t * TIME_SPAN / 1e9);
        if let Some(next) = next_size(last_n as f64, last_t, exponent, aim, left) {
            let next = (next / step).max(1) * step;
            if next > last_n {
                let t = measure(next);
                if let Some(p) = two_point_exponent(last_n as f64, last_t, next as f64, t) {
                    exponent = p;
                }
            }
        }
    }

    SizeRange {
        lo: lo.0,
        lo_time: lo.1,
        exponent,
    }
}

/// Choose the sizes stage two will measure.
///
/// The bottom is stage one's: below [`SizeRange::lo`] we would be timing
/// the clock. The top is the only choice, and it is made in time - far
/// enough up that the largest size takes [`TIME_SPAN`] times as long as the
/// smallest, which stage one's growth rate converts into a size.
///
/// No budget is consulted. What makes a range good is that it separates the
/// powers while costing little, and both of those are settled by the
/// numbers stage one already measured; checking a budget as well would only
/// let a large one talk us into spending more than the answer needs. The
/// opening rounds come to about 80 times the cheapest call, so a benchmark
/// at the 20-microsecond floor is measured in under two milliseconds. A
/// noisy one costs more, but it costs more by *needing* more - the
/// refinement loop buys precision it has found it lacks, which is the only
/// good reason to spend longer.
///
/// Sizes come out log-spaced, the spacing that divides a range of powers
/// evenly, since equal ratios rather than equal differences are what make
/// each rung contribute comparably.
fn choose_sizes(range: SizeRange, nmin: usize) -> Vec<usize> {
    let step = nmin.max(1);
    // A cost that barely grows would need an unbounded size range to reach
    // `TIME_SPAN`; `MAX_SIZE_SPAN` is where we stop asking.
    let span = if range.exponent > 0.0 {
        TIME_SPAN.powf(1.0 / range.exponent).min(MAX_SIZE_SPAN)
    } else {
        MAX_SIZE_SPAN
    };
    let ratio = span.powf(1.0 / (NUM_SIZES - 1) as f64);

    let mut sizes: Vec<usize> = Vec::with_capacity(NUM_SIZES);
    for i in 0..NUM_SIZES {
        let x = range.lo as f64 * ratio.powi(i as i32);
        let n = ((x / step as f64).round().max(1.0) as usize).saturating_mul(step);
        // Rounding onto a multiple of `nmin` can land two rungs together;
        // keep the ladder strictly increasing rather than measuring a size
        // twice and calling the two measurements independent.
        if sizes.last() != Some(&n) {
            sizes.push(n);
        }
    }

    // Log spacing assumes there are enough distinct integers in the range
    // to land on, and near the bottom there are not: a cubic cost wants a
    // size span of only 1.59, which from `lo` of 1 rounds every rung onto
    // 1 or 2. Too few sizes to fit is the one failure with no answer at
    // all, so fall back to the densest packing there is - consecutive steps
    // up from `lo`, every one the smallest size still available.
    if sizes.len() < NUM_SIZES {
        sizes = (0..NUM_SIZES)
            .map(|i| range.lo.saturating_add(i.saturating_mul(step)))
            .collect();
    }
    sizes
}

/// How many times each size is measured before the first fit is attempted.
///
/// Every measurement here is a *single call* of the benchmarked function.
/// Nothing is batched: a benchmark worth asking about the scaling of is one
/// that gets slow as `N` grows, so where a batch would have been needed to
/// out-measure the clock, a larger `N` does the same job and tells us
/// something we wanted to know anyway. Stage one already picks sizes whose
/// single call is long enough to time, which is what makes this safe.
///
/// Not batching is also what keeps the fitted model honest. Timing a batch
/// and dividing by its length folds the fixed per-batch overhead into a
/// `c/N` term, which no polynomial in `N` can represent, so it comes out
/// smeared across every coefficient. One call per sample leaves that
/// overhead where it belongs: a constant, which the degree-zero term of
/// [`weighted_poly_fit`] represents exactly.
///
/// Six rather than three because these standard errors become the fit's
/// weights. An SE from three samples is itself uncertain by around half its
/// own size, and a badly mis-estimated weight does more damage than a
/// merely wide one; six brings that to about a third, and the loop below
/// improves it where it matters.
const INITIAL_REPEATS: usize = 6;

/// Measure the scaling of `measure` across `sizes`, refining until the
/// dominant coefficient is known to the configured relative accuracy.
///
/// `measure(n)` performs one call at size `n` and returns its cost in
/// nanoseconds. Each size is measured [`INITIAL_REPEATS`] times, giving a
/// mean and a standard error over those repeats; those feed
/// [`scaling_fit`], and if its answer is not precise enough every size is
/// measured once more. So the loop spends time only to buy precision it has
/// already found it lacks.
///
/// Only the *relative* accuracy target applies. The absolute one is a
/// `Duration`, and what stage two reports is not a duration - it is
/// nanoseconds per `N^power`, whose units change with the power that was
/// found. Comparing it against a time would be a units error that happens
/// to typecheck.
///
/// Returns the fit and whether the budget ran out first; a fit that ran out
/// of budget is still the best available answer, just not a precise one.
struct Measured {
    ns: Vec<f64>,
    means: Vec<f64>,
    ses: Vec<f64>,
    fit: Option<ScalingFit>,
    /// The budget ran out before the accuracy target was reached.
    hit_limit: bool,
}

fn measure_scaling(
    sizes: &[usize],
    cfg: &Config,
    budget: Duration,
    max_degree: usize,
    mut measure: impl FnMut(usize) -> f64,
) -> Measured {
    let ns: Vec<f64> = sizes.iter().map(|&n| n as f64).collect();
    let mut acc = vec![Running::default(); sizes.len()];
    let budget_ns = budget.as_secs_f64() * 1e9;
    let mut spent = 0.0;
    // Two clocks, because they measure different things and either can be
    // the binding one. `spent` adds up what the calls themselves cost,
    // which is what the accuracy is bought with; `started` is real time,
    // which also covers what `measure` does around the call - building and
    // dropping an environment, most of all, which for something like a
    // sort costs as much again as the sort does. Budgeting on `spent`
    // alone would overrun by whatever that setup costs, and would never
    // terminate at all for a benchmark whose calls measure as zero.
    let started = Instant::now();
    let over_budget =
        |spent: f64| spent >= budget_ns || started.elapsed().as_secs_f64() * 1e9 >= budget_ns;

    let mut round = |acc: &mut Vec<Running>, spent: &mut f64| {
        for (i, &n) in sizes.iter().enumerate() {
            let t = measure(n);
            acc[i].push(t);
            *spent += t;
        }
    };
    // Two rounds unconditionally, because a standard error needs two
    // points to exist at all and without one there is nothing to fit. After
    // that the budget has a say: a ladder that turned out more expensive
    // than predicted should return a poor answer on time rather than a good
    // one long after the caller stopped waiting.
    for i in 0..INITIAL_REPEATS {
        if i >= 2 && over_budget(spent) {
            break;
        }
        round(&mut acc, &mut spent);
    }

    loop {
        let mut means = Vec::with_capacity(acc.len());
        let mut ses = Vec::with_capacity(acc.len());
        for a in &acc {
            let (m, se) = a.mean_and_stderr();
            means.push(m);
            ses.push(se);
        }
        let fit = scaling_fit(&ns, &means, &ses, max_degree);
        let precise = fit
            .as_ref()
            .is_some_and(|f| f.std_error < cfg.target_rel_error * f.ns_per_scale.abs());
        // Check the budget only after a fit that was not good enough, so a
        // benchmark that is already precise enough never reports having hit
        // a limit it did not need.
        if precise || over_budget(spent) {
            return Measured {
                ns,
                means,
                ses,
                fit,
                hit_limit: !precise,
            };
        }
        round(&mut acc, &mut spent);
    }
}

/// The answer stage two is working towards: which integer power dominates,
/// how big its coefficient is, and how well either is known.
#[derive(Debug, Clone, Copy, PartialEq)]
struct ScalingFit {
    /// The dominant integer power: 1 for `O(N)`, 2 for `O(N²)`.
    power: usize,
    /// Nanoseconds per `N^power`.
    ns_per_scale: f64,
    /// Standard error of `ns_per_scale`.
    std_error: f64,
    /// Chi-squared per degree of freedom for the chosen polynomial.
    chi2_per_dof: f64,
    /// Weighted R² for the chosen polynomial.
    r2: f64,
}

/// Choose the integer power and report its coefficient.
///
/// The power comes from the log-log slope over the measured sizes: if cost
/// goes as `Nᵖ` then `log t` is linear in `log N` with gradient `p`, and
/// rounding that gradient gives the integer power. The coefficient then
/// comes from a polynomial refit at that degree, so it means what its units
/// say - `ns` per `N^power`, with the lower-order terms carried alongside
/// rather than folded in.
///
/// Chi-squared reports on the fit; it does not choose it. That separation
/// is the point. It used to choose - walk the degrees, keep the first the
/// threshold accepted - and the answer then swung with the threshold in
/// both directions. Loose, and a *constant* was accepted as the shape of a
/// linear cost whenever the measurements were noisy. Tight, and summing
/// integers came out `O(N²)`: it is not exactly linear, because per-element
/// cost changes as the vector outgrows cache, so at high precision a
/// straight line is genuinely rejected and a parabola genuinely fits
/// better. Both answers were defensible from the residuals and both were
/// wrong, because "which model survives a threshold" is not the question
/// anyone asked.
///
/// The slope answers the question that was asked - how fast does this grow
/// - and it does not care that a parabola could be drawn through the
/// points. It also needs no separate rule for a term too small to matter: a
/// cubic contributing a billionth of the runtime moves the slope by a
/// billionth, so it is ignored by arithmetic rather than by a threshold.
///
/// What chi-squared still does, and only it can do, is say whether *any*
/// polynomial describes the data. That is reported, not acted on.
fn scaling_fit(
    ns: &[f64],
    means: &[f64],
    ses: &[f64],
    max_degree: usize,
) -> Option<ScalingFit> {
    let slope = power_fit(ns, means, ses)?.exponent;
    if !slope.is_finite() {
        return None;
    }
    let power = slope.round().clamp(0.0, max_degree as f64) as usize;
    let fit = weighted_poly_fit(ns, means, ses, power)?;
    Some(ScalingFit {
        power,
        ns_per_scale: fit.coefficients[power],
        std_error: fit.ses[power],
        chi2_per_dof: fit.chi2_per_dof,
        r2: fit.r2,
    })
}

/// Invert a small symmetric positive-definite matrix by Gauss-Jordan with
/// partial pivoting. `None` if it is singular to working precision, which
/// is how an unidentifiable fit reports itself.
fn invert(a: &[Vec<f64>]) -> Option<Vec<Vec<f64>>> {
    let n = a.len();
    let mut m: Vec<Vec<f64>> = a
        .iter()
        .enumerate()
        .map(|(i, row)| {
            let mut r = row.clone();
            r.extend((0..n).map(|j| if i == j { 1.0 } else { 0.0 }));
            r
        })
        .collect();
    for col in 0..n {
        let pivot = (col..n).max_by(|&i, &j| m[i][col].abs().total_cmp(&m[j][col].abs()))?;
        if m[pivot][col].abs() < 1e-300 {
            return None;
        }
        m.swap(col, pivot);
        let d = m[col][col];
        for v in m[col].iter_mut() {
            *v /= d;
        }
        for row in 0..n {
            if row != col {
                let f = m[row][col];
                if f != 0.0 {
                    let (pivot_row, target) = if row < col {
                        let (a, b) = m.split_at_mut(col);
                        (&b[0], &mut a[row])
                    } else {
                        let (a, b) = m.split_at_mut(row);
                        (&a[col], &mut b[0])
                    };
                    for (t, p) in target.iter_mut().zip(pivot_row.iter()) {
                        *t -= f * p;
                    }
                }
            }
        }
    }
    Some(m.into_iter().map(|r| r[n..].to_vec()).collect())
}

// Not yet wired into the two-stage measurement, which is deliberately
// limited to polynomials. This is the half that names `N log N` and
// exponential costs, and it is what `scales_o_2_to_the_n` is ignored
// pending; it is kept, and kept tested, rather than deleted and rewritten.
#[allow(dead_code)]
/// A coefficient must exceed its own standard error by this factor to count
/// as real.
///
/// The knob that decides how much non-polynomial growth gets absorbed into
/// spurious high-order terms. Measured on synthetic `N log N` - which is
/// genuinely faster than `N` but is not a polynomial at all, so *some*
/// answer has to be wrong - this reports `N⁴` at 3, `N³` at 5, `N²` at 8
/// and `N` at 12. Set here so that a real but weak quadratic
/// (`5N + 0.001N²`, which shows up at 7.2) is still caught, accepting that
/// `N log N` reads as a little worse than linear rather than exactly
/// linear, which it genuinely is.
/// A candidate growth rate, ordered slowest to fastest.
///
/// Deliberately not just polynomial degrees: `N log N` and `2ᴺ` are not
/// polynomials, so a basis of powers alone has to approximate them with
/// high-order terms and reports the wrong answer - synthetic `N log N`
/// comes back as `N⁴`. Listing them as terms of their own costs nothing,
/// because least squares is linear in the *coefficients*, not in the
/// predictor, and a `N log N` column is as ordinary as an `N²` one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Growth {
    /// Cost that does not grow with `N`.
    Constant,
    /// `N`.
    Linear,
    /// `N log N`.
    Linearithmic,
    /// `Nᵖ` for `p >= 2`.
    Power(usize),
    /// `2ᴺ`.
    Exponential,
}

#[allow(dead_code)]
impl Growth {
    /// The terms this crate will consider, slowest-growing first.
    ///
    /// Order matters: the basis is orthogonalised in this sequence, so each
    /// term is judged on what it explains *beyond everything that grows
    /// more slowly*. That is precisely the question "what is the
    /// asymptotically dominant term", and it is why the answer is the
    /// highest significant entry rather than the largest one.
        fn candidates(max_power: usize) -> Vec<Growth> {
        let mut v = vec![Growth::Constant, Growth::Linear, Growth::Linearithmic];
        v.extend((2..=max_power).map(Growth::Power));
        v.push(Growth::Exponential);
        v
    }

    /// This term's value at `n`, given the largest `n` in the sample.
    ///
    /// `2ᴺ` is normalised against `largest` because a sweep reaching
    /// `N = 470` would otherwise overflow to infinity; scaling a basis
    /// column changes only its coefficient, never the fit.
        fn value(self, n: f64, largest: f64) -> f64 {
        match self {
            Growth::Constant => 1.0,
            Growth::Linear => n,
            Growth::Linearithmic => n * n.max(2.0).ln(),
            Growth::Power(p) => n.powi(p as i32),
            Growth::Exponential => ((n - largest) * std::f64::consts::LN_2).exp(),
        }
    }
}

/// A power-law fit of measured times against problem size: `t ≈ c·Nᵖ`.
///
/// Every field is a statement about the *measurements*, not about the
/// algorithm. `exponent` is continuous on purpose - `N log N` has no
/// integer exponent, and a cost that is linear over part of the range and
/// quadratic over the rest has no single one either. Rounding to an integer
/// is a presentation decision, taken later and only once `chi2_per_dof`
/// says a power law is a fair description at all.
#[derive(Debug, Clone, Copy, PartialEq)]
struct PowerFit {
    /// The exponent `p`.
    exponent: f64,
    /// Standard error of `exponent`.
    exponent_se: f64,
    /// The prefactor `c`, in nanoseconds, so that `t ≈ c·Nᵖ`.
    prefactor: f64,
    /// Chi-squared per degree of freedom.
    ///
    /// Near 1 when a single power law describes the data within the
    /// measured error bars; large when it does not. This is a real
    /// goodness-of-fit test rather than a heuristic, and it is only
    /// available because each point carries an error bar that was
    /// *measured* rather than assumed - see [`power_fit`].
    ///
    /// Measured on synthetic sweeps: 0.8 for `O(N)`, 1.4 for `O(N²)`, 1.6
    /// for `O(N³)`, against 32 for `N log N`, 758 for `5N + 0.05N²` and
    /// 6924 for `2ᴺ`. The separation is not subtle.
    chi2_per_dof: f64,
}

/// Weighted least squares of `log t` against `log N`.
///
/// Taking logs turns `t = c·Nᵖ` into a straight line whose slope is the
/// exponent, so one ordinary linear fit answers the whole question - no
/// candidate basis, no model selection, and nothing that has to be
/// orthogonalised.
///
/// Each point must arrive with its own standard error, from having been
/// measured independently rather than inferred from the spread about the
/// line. That is what makes this the textbook weighted case with *known*
/// variances: `se(log t) = se(t)/t`, so the weight is `(t/se)²`, and
/// `chi2_per_dof` becomes a genuine goodness-of-fit rather than a
/// restatement of the residuals. Every earlier attempt here assumed a noise
/// shape instead of measuring one, and was optimistic by about twofold for
/// its trouble.
fn power_fit(ns: &[f64], ts: &[f64], ses: &[f64]) -> Option<PowerFit> {
    if ns.len() < 3 {
        return None;
    }
    let mut x = Vec::with_capacity(ns.len());
    let mut y = Vec::with_capacity(ns.len());
    let mut w = Vec::with_capacity(ns.len());
    for ((&n, &t), &se) in ns.iter().zip(ts).zip(ses) {
        if !(n > 0.0 && t > 0.0 && se > 0.0) || !se.is_finite() {
            return None;
        }
        x.push(n.ln());
        y.push(t.ln());
        w.push((t / se).powi(2));
    }
    let sw: f64 = w.iter().sum();
    if !sw.is_finite() || sw <= 0.0 {
        return None;
    }
    let xbar: f64 = x.iter().zip(&w).map(|(xi, wi)| wi * xi).sum::<f64>() / sw;
    let ybar: f64 = y.iter().zip(&w).map(|(yi, wi)| wi * yi).sum::<f64>() / sw;
    let sxx: f64 = x
        .iter()
        .zip(&w)
        .map(|(xi, wi)| wi * (xi - xbar).powi(2))
        .sum();
    if !sxx.is_finite() || sxx <= 0.0 {
        // Every N identical: no leverage on the exponent at all.
        return None;
    }
    let sxy: f64 = x
        .iter()
        .zip(&y)
        .zip(&w)
        .map(|((xi, yi), wi)| wi * (xi - xbar) * (yi - ybar))
        .sum();
    let exponent = sxy / sxx;
    let intercept = ybar - exponent * xbar;
    let chi2: f64 = x
        .iter()
        .zip(&y)
        .zip(&w)
        .map(|((xi, yi), wi)| wi * (yi - (intercept + exponent * xi)).powi(2))
        .sum();
    Some(PowerFit {
        exponent,
        // With known variances the exponent's variance is 1/Sxx outright -
        // no residual scale enters, because the weights are already in the
        // right units.
        exponent_se: (1.0 / sxx).sqrt(),
        prefactor: intercept.exp(),
        chi2_per_dof: chi2 / (ns.len() - 2) as f64,
    })
}

/// The exponent implied by two measurements alone: `log(t₂/t₁) / log(N₂/N₁)`.
///
/// Cheap enough to drive size selection, where the question is only "how
/// fast is this growing, roughly" - enough to predict what the next size
/// will cost and so to choose one that is large enough to be informative
/// without being too slow to afford.
///
/// Read across adjacent pairs it also says something the global fit cannot:
/// a *drifting* local exponent means no single power law holds, and which
/// way it drifts says why. Rising towards a limit is a mixed cost
/// approaching its asymptotic term; rising without bound is faster than any
/// polynomial.
fn two_point_exponent(n1: f64, t1: f64, n2: f64, t2: f64) -> Option<f64> {
    if !(n1 > 0.0 && n2 > 0.0 && t1 > 0.0 && t2 > 0.0) || n1 == n2 {
        return None;
    }
    Some((t2 / t1).ln() / (n2 / n1).ln())
}

/// Never let one step multiply the problem size by more than this.
///
/// The extrapolation below is only as good as an exponent estimated from
/// two noisy points, and its error enters as an exponent - so a modest
/// mistake in `p` is a large mistake in predicted cost. Capping the step
/// bounds what a wrong guess can cost: worst case we take an extra
/// measurement or two, rather than launching a single evaluation that eats
/// the entire budget.
const MAX_SIZE_GROWTH: f64 = 8.0;

/// The most of the remaining budget one measurement may be predicted to
/// consume.
///
/// Overshooting is far worse than undershooting. A size that turns out too
/// small costs one cheap measurement and is immediately corrected; a size
/// that turns out too large can spend the whole budget on a single point
/// and leave nothing to fit. So the prediction has to fit several times
/// over into what is left.
const BUDGET_SHARE_PER_SIZE: f64 = 0.25;

/// Choose the next problem size to measure during size discovery.
///
/// Extrapolates from `exponent`: if cost grows as `Nᵖ`, then reaching
/// `target` from `(last_n, last_t)` wants `last_n · (target/last_t)^(1/p)`.
///
/// Deliberately timid, in three separate ways, because the prediction is
/// built on an exponent estimated from two noisy measurements and enters
/// the answer as a reciprocal exponent:
///
/// * `p` is floored at 1. A sublinear estimate - which noise alone can
///   produce at small sizes, where fixed overheads still dominate and cost
///   barely moves - would give `1/p > 1` and demand an enormous jump.
/// * The step is capped at [`MAX_SIZE_GROWTH`] regardless.
/// * The predicted cost must fit [`BUDGET_SHARE_PER_SIZE`] of what remains,
///   so no single measurement can consume the budget even if the estimate
///   is badly wrong.
///
/// Returns `None` when even the smallest useful step - one more than the
/// last size - is predicted to overrun the budget, which is how discovery
/// learns it has reached the largest size it can afford.
fn next_size(
    last_n: f64,
    last_t: f64,
    exponent: f64,
    target: Duration,
    budget_left: Duration,
) -> Option<usize> {
    if !(last_n >= 1.0 && last_t > 0.0) || !exponent.is_finite() {
        return None;
    }
    let target_ns = target.as_secs_f64() * 1e9;
    let affordable_ns = budget_left.as_secs_f64() * 1e9 * BUDGET_SHARE_PER_SIZE;
    if affordable_ns <= 0.0 {
        return None;
    }
    // Never plan a measurement we cannot pay for, whatever the target says.
    let aim_ns = target_ns.min(affordable_ns);
    if aim_ns <= last_t {
        // The size we have already measured costs as much as we can afford
        // to spend on the next one; there is no room to grow.
        return None;
    }
    let p = exponent.max(1.0);
    // Three ceilings, whichever is lowest:
    //
    // * the step that reaches the target under the measured exponent;
    // * the step that stays affordable under a *pessimistic* exponent, one
    //   whole power above the measured one;
    // * the flat cap.
    //
    // The pessimistic term deserves its place because the estimate comes
    // from two noisy points and enters the cost as an exponent, so being
    // wrong about it is expensive in one direction only: too small a size
    // wastes one cheap measurement and corrects itself next step, while too
    // large a size can spend the entire budget on a single point and leave
    // nothing to fit. Budgeting as though the cost climbs a power faster
    // than measured makes the step affordable *by construction* rather than
    // proposing one and hoping.
    let to_target = (aim_ns / last_t).powf(1.0 / p);
    let affordable_growth = (affordable_ns / last_t).powf(1.0 / (p + 1.0));
    let growth = to_target.min(affordable_growth).min(MAX_SIZE_GROWTH);
    // `is_finite` first so the comparison never has to reason about NaN.
    if !growth.is_finite() || growth <= 1.0 {
        return None;
    }
    // Round down, not up: rounding up can push a step that was affordable
    // by a hair over the line, and undershooting is the cheap mistake.
    let next = (last_n * growth).floor().max(last_n + 1.0);
    if !next.is_finite() || next > usize::MAX as f64 {
        return None;
    }
    // A last check in the currency the budget is paid in, which also covers
    // the rounding-up above.
    let predicted = last_t * (next / last_n).powf(p + 1.0);
    (predicted <= affordable_ns).then_some(next as usize)
}

const SIGNIFICANT: f64 = 6.0;


// Not yet wired into the two-stage measurement, which is deliberately
// limited to polynomials. This is the half that names `N log N` and
// exponential costs, and it is what `scales_o_2_to_the_n` is ignored
// pending; it is kept, and kept tested, rather than deleted and rewritten.
#[allow(dead_code)]
/// Orthonormalise the basis columns *in the weighted inner product over
/// the sampled points*, by modified Gram-Schmidt.
///
/// Emphatically not orthogonal polynomials on a continuous interval:
/// Legendre or Chebyshev families are orthogonal with respect to an
/// integral over a range, and a benchmark's sizes are neither uniformly
/// spaced nor equally weighted - they grow geometrically and are weighted
/// by `1/y²`. A basis orthogonal for that integral is not orthogonal for
/// *this sample*, which is the only thing that makes the fitted
/// coefficients uncorrelated and their significance separately readable.
///
/// A column that is entirely explained by earlier ones - which is how an
/// unidentifiable term announces itself - comes back as zeros, and so
/// scores no significance at all.
fn orthonormalise(cols: &[Vec<f64>], ws: &[f64]) -> Vec<Vec<f64>> {
    let dot = |a: &[f64], b: &[f64]| -> f64 {
        a.iter().zip(b).zip(ws).map(|((x, y), w)| w * x * y).sum()
    };
    let mut q: Vec<Vec<f64>> = Vec::with_capacity(cols.len());
    for col in cols {
        let mut v = col.clone();
        for prev in &q {
            let d = dot(&v, prev);
            for (vi, pi) in v.iter_mut().zip(prev) {
                *vi -= d * pi;
            }
        }
        let norm = dot(&v, &v).max(0.0).sqrt();
        if norm.is_finite() && norm > 1e-12 {
            for vi in v.iter_mut() {
                *vi /= norm;
            }
            q.push(v);
        } else {
            q.push(vec![0.0; col.len()]);
        }
    }
    q
}

// Not yet wired into the two-stage measurement, which is deliberately
// limited to polynomials. This is the half that names `N log N` and
// exponential costs, and it is what `scales_o_2_to_the_n` is ignored
// pending; it is kept, and kept tested, rather than deleted and rewritten.
#[allow(dead_code)]
/// The fastest-growing term the data actually supports.
///
/// Because the basis is orthonormalised in growth order, the coefficient on
/// each term measures what that term adds beyond every slower one, and
/// every coefficient shares the same standard error - so comparing them is
/// a single division. The answer is the *highest* significant term, not the
/// largest: a quadratic cost makes the `N log N` term look significant too,
/// since it is partly absorbing the curvature, but nothing above `N²`
/// survives and that is what settles it.
fn dominant_growth(xs: &[f64], ys: &[f64], ws: &[f64], max_power: usize) -> Option<Growth> {
    let terms = Growth::candidates(max_power);
    if xs.len() <= terms.len() {
        return None;
    }
    let largest = xs.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    if !largest.is_finite() {
        return None;
    }
    let cols: Vec<Vec<f64>> = terms
        .iter()
        .map(|t| xs.iter().map(|&x| t.value(x, largest)).collect())
        .collect();
    let q = orthonormalise(&cols, ws);
    let dot = |a: &[f64], b: &[f64]| -> f64 {
        a.iter().zip(b).zip(ws).map(|((x, y), w)| w * x * y).sum()
    };
    let coef: Vec<f64> = q.iter().map(|qi| dot(qi, ys)).collect();
    let fitted: Vec<f64> = (0..ys.len())
        .map(|i| coef.iter().zip(&q).map(|(c, qi)| c * qi[i]).sum())
        .collect();
    let dof = xs.len() - terms.len();
    let chi2: f64 = ys
        .iter()
        .zip(&fitted)
        .zip(ws)
        .map(|((y, f), w)| w * (y - f).powi(2))
        .sum();
    let se = (chi2 / dof as f64).sqrt();
    if !se.is_finite() || se <= 0.0 {
        return None;
    }
    terms
        .iter()
        .zip(&coef)
        .filter(|(_, c)| c.abs() / se > SIGNIFICANT)
        .map(|(t, _)| *t)
        .next_back()
}

// Not yet wired into the two-stage measurement, which is deliberately
// limited to polynomials. This is the half that names `N log N` and
// exponential costs, and it is what `scales_o_2_to_the_n` is ignored
// pending; it is kept, and kept tested, rather than deleted and rewritten.
#[allow(dead_code)]
/// The highest polynomial degree whose coefficient the data actually
/// supports - the asymptotically dominant term.
///
/// This is what makes a mixed cost like `aN + bN²` come out as `N²`: every
/// term is fitted at once, so the quadratic is found on top of the linear
/// rather than having to beat it outright, which is what a search over
/// single-term models can never do.
///
/// Degrees the sampled range cannot support disqualify themselves: their
/// coefficients come out with standard errors as large as themselves. So a
/// short sweep under-reports rather than inventing structure, and no
/// separate "maximum degree for this range" rule is needed.
fn dominant_degree(xs: &[f64], ys: &[f64], ws: &[f64], max_degree: usize) -> Option<usize> {
    let mut best = None;
    for degree in 0..=max_degree {
        let Some((coef, se)) = poly_fit(xs, ys, ws, degree) else {
            break;
        };
        // Only the leading term is asked about here; the lower ones are
        // present so that it is judged on what it adds, not on what the
        // whole curve looks like.
        if se[degree] > 0.0 && coef[degree].abs() / se[degree] > SIGNIFICANT {
            best = Some(degree);
        }
    }
    best
}

// Not yet wired into the two-stage measurement, which is deliberately
// limited to polynomials. This is the half that names `N log N` and
// exponential costs, and it is what `scales_o_2_to_the_n` is ignored
// pending; it is kept, and kept tested, rather than deleted and rewritten.
#[allow(dead_code)]
/// [`dominant_degree`], but only trusted if it survives dropping the
/// largest sample.
///
/// A conclusion that rests on the single biggest `N` is not a conclusion
/// yet: that point has the most leverage in the fit, so one unlucky
/// measurement - or one cache threshold crossed right at the end of the
/// sweep - can set the answer by itself. Refitting without it costs one
/// extra fit and asks whether the shape is a property of the data or of
/// that point.
///
/// `None` means "not confirmed": the caller keeps sampling, which widens
/// the range and either corroborates the term or drops it. That is the
/// right response either way, because both readings are still live.
///
/// This is deliberately not the same question as significance. A weak but
/// real term - `5N + 0.001N²` - is significant on the full sweep and
/// vanishes without the largest point, and the honest answer there is
/// neither "quadratic" nor "linear" but "keep going".
fn confirmed_dominant_degree(
    xs: &[f64],
    ys: &[f64],
    ws: &[f64],
    max_degree: usize,
) -> Option<usize> {
    let full = dominant_degree(xs, ys, ws, max_degree)?;
    let largest = xs
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.total_cmp(b.1))
        .map(|(i, _)| i)?;
    let keep = |v: &[f64]| -> Vec<f64> {
        v.iter()
            .enumerate()
            .filter(|(i, _)| *i != largest)
            .map(|(_, &x)| x)
            .collect()
    };
    let trimmed = dominant_degree(&keep(xs), &keep(ys), &keep(ws), max_degree)?;
    (full == trimmed).then_some(full)
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
        // A constant used to be the case the fit could say least about:
        // with only an R² to go on, nothing distinguished "flat" from
        // "could not tell", so it reported itself clueless. Measured error
        // bars settle it - a degree-zero fit that lands inside them has
        // identified the shape as surely as any other - so a constant is
        // now a proper answer, reached and stood behind.
        assert_eq!(1.0, stats.goodness_of_fit);
        assert!(!stats.hit_limit);
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

    // Exponential costs are not fitted at present: the two-stage
    // measurement is deliberately limited to polynomials, and an `O(2ᴺ)`
    // cost comes back rejected - `goodness_of_fit` zeroed and the limit
    // flag set - with whichever integer power best approximates it over the
    // range measured. That is honest but weaker than what this test asks
    // for, and restoring it needs a fit in log space that this stage does
    // not yet do. Kept, un-deleted, as the record of what is owed.
    #[test]
    #[ignore = "exponential fitting not yet ported to the two-stage measurement"]
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
    fn quiesced() -> bool {
        matches!(quiet::status(), quiet::Status::Pinned { .. })
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

    /// Synthetic data with a known answer, which is the only way to check
    /// that the fit reports the *asymptotically dominant* term rather than
    /// whichever single power happens to fit best.
    mod fitting {
        use super::*;

        /// Geometrically spaced sizes, as a real sweep produces.
        fn sizes(count: usize) -> Vec<f64> {
            let mut v: Vec<f64> = (2..)
                .map(|k| (1.35f64).powi(k).round())
                .take_while(|_| true)
                .take(count * 3)
                .collect();
            v.dedup();
            v.truncate(count);
            v
        }

        /// Deterministic multiplicative noise, so a failure reproduces.
        fn noisy(f: impl Fn(f64) -> f64, xs: &[f64], rel: f64, seed: u64) -> Vec<f64> {
            let mut rng = XorShift(seed | 1);
            xs.iter()
                .map(|&x| {
                    // Two uniforms averaged: crude, but symmetric about 0
                    // and bounded, which is all this needs.
                    let u = |r: &mut XorShift| (r.next() >> 11) as f64 / (1u64 << 53) as f64;
                    let jitter = (u(&mut rng) + u(&mut rng) - 1.0) * rel;
                    f(x) * (1.0 + jitter)
                })
                .collect()
        }

        /// Timing noise is multiplicative, so weight by 1/y²: that makes
        /// every point contribute its *relative* error, instead of letting
        /// the largest sizes dominate simply by being largest.
        fn weights(ys: &[f64]) -> Vec<f64> {
            ys.iter().map(|&y| 1.0 / (y * y)).collect()
        }

        fn degree_of(f: impl Fn(f64) -> f64, count: usize, seed: u64) -> Option<usize> {
            let xs = sizes(count);
            let ys = noisy(f, &xs, 0.05, seed);
            dominant_degree(&xs, &ys, &weights(&ys), 4)
        }

        fn growth_of(f: impl Fn(f64) -> f64, count: usize, seed: u64) -> Option<Growth> {
            let xs = sizes(count);
            let ys = noisy(f, &xs, 0.05, seed);
            dominant_growth(&xs, &ys, &weights(&ys), 3)
        }

        /// Geometrically spaced sizes spanning a wide range, as size
        /// selection is meant to produce.
        fn wide_sizes() -> Vec<f64> {
            let mut v: Vec<f64> = (6..20).map(|k| (1.6f64).powi(k).round()).collect();
            v.dedup();
            v
        }

        /// What stage two hands the fit: each size measured independently,
        /// so every point arrives with its own error bar rather than one
        /// inferred from the spread about the line.
        fn measured(
            f: impl Fn(f64) -> f64,
            ns: &[f64],
            rel: f64,
            seed: u64,
        ) -> (Vec<f64>, Vec<f64>) {
            let mut rng = XorShift(seed | 1);
            let mut ts = Vec::new();
            let mut ses = Vec::new();
            for &n in ns {
                let truth = f(n);
                let u = |r: &mut XorShift| (r.next() >> 11) as f64 / (1u64 << 53) as f64;
                let jitter = (u(&mut rng) + u(&mut rng) - 1.0) * rel;
                ts.push(truth * (1.0 + jitter));
                ses.push(truth * rel);
            }
            (ts, ses)
        }

        const HUGE: Duration = Duration::from_secs(3600);

        #[test]
        fn the_next_size_aims_at_the_target_using_the_measured_power() {
            // Quadratic: to go from 1ms to 16ms, a 16x in time, wants a 4x
            // in size - inside the cap, so the power is what decides.
            let n = next_size(100.0, 1e6, 2.0, Duration::from_millis(16), HUGE).unwrap();
            assert!((390..=410).contains(&n), "quadratic step gave {n}");
            // Cubic: the same 16x in time is only a 2.5x in size, because
            // the cost climbs faster with each unit of N.
            let n = next_size(100.0, 1e6, 3.0, Duration::from_millis(16), HUGE).unwrap();
            assert!((245..=260).contains(&n), "cubic step gave {n}");
            // Linear: a 100x in time would want a 100x in size, so here the
            // cap is what decides instead.
            let n = next_size(100.0, 1e6, 1.0, Duration::from_millis(100), HUGE).unwrap();
            assert_eq!(800, n, "linear step should be capped at 8x");
        }

        #[test]
        fn a_wild_exponent_cannot_provoke_a_wild_step() {
            // A sublinear estimate is what noise produces at small sizes,
            // where fixed overheads still dominate and cost barely moves.
            // Taken literally it asks for an enormous jump; the floor at
            // p = 1 and the growth cap between them refuse.
            for exponent in [0.01, 0.2, 0.5, 1.0] {
                let n = next_size(100.0, 1e3, exponent, HUGE, HUGE).unwrap();
                assert!(n <= 800, "exponent {exponent} gave {n}");
            }
            // Nonsense in, nothing out - rather than a nonsensical size.
            assert_eq!(None, next_size(100.0, 1e6, f64::NAN, HUGE, HUGE));
            assert_eq!(None, next_size(100.0, 0.0, 2.0, HUGE, HUGE));

            // With the budget rather than the cap deciding, the floor at
            // p = 1 is what keeps the step honest. A measured exponent of
            // 0.01 says "cost barely moves with N", so budgeting on it
            // would license nearly doubling the size; treating the cost as
            // at least linear - and charging a power above that - keeps the
            // step to something a wrong guess can survive.
            let budget = Duration::from_nanos(8_000_000);
            let n = next_size(100.0, 1e6, 0.01, HUGE, budget).unwrap();
            assert!(
                n <= 150,
                "a near-zero exponent should not license a {n}-sized step"
            );
        }

        #[test]
        fn one_measurement_cannot_eat_the_whole_budget() {
            // The last measurement took 1s and only 2s remain. A quarter of
            // what is left is 500ms, less than the point we already have,
            // so there is no affordable step and discovery stops.
            assert_eq!(
                None,
                next_size(100.0, 1e9, 2.0, HUGE, Duration::from_secs(2))
            );
            // With plenty of budget the same call proceeds, and what it
            // picks is predicted to cost well under the whole of it.
            let budget = Duration::from_secs(600);
            let n = next_size(100.0, 1e9, 2.0, HUGE, budget).unwrap();
            let predicted = 1e9 * ((n as f64) / 100.0).powi(3);
            assert!(
                predicted <= budget.as_secs_f64() * 1e9 * 0.25 + 1.0,
                "picked {n}, predicted {predicted}ns of a {budget:?} budget"
            );
        }

        #[test]
        fn size_discovery_always_makes_progress_or_stops() {
            // Never returns the size it was given: either a strictly larger
            // one, or `None`. A loop built on this cannot spin.
            let mut n = 1.0f64;
            let mut steps = 0;
            while let Some(next) = next_size(n, 1e3 * n * n, 2.0, HUGE, HUGE) {
                assert!(next as f64 > n, "{next} did not advance past {n}");
                n = next as f64;
                steps += 1;
                if steps > 200 {
                    break;
                }
            }
            assert!(steps > 0, "should have taken at least one step");
        }

        /// What stage two collects: a handful of repeats at each size,
        /// summarised as a mean and a standard error over those repeats.
        fn replicated(
            f: impl Fn(f64) -> f64,
            ns: &[f64],
            rel: f64,
            repeats: usize,
            seed: u64,
        ) -> (Vec<f64>, Vec<f64>) {
            let mut rng = XorShift(seed | 1);
            let mut means = Vec::new();
            let mut ses = Vec::new();
            for &n in ns {
                let truth = f(n);
                let mut acc = Running::default();
                for _ in 0..repeats {
                    let u = |r: &mut XorShift| (r.next() >> 11) as f64 / (1u64 << 53) as f64;
                    let jitter = (u(&mut rng) + u(&mut rng) - 1.0) * rel;
                    acc.push(truth * (1.0 + jitter));
                }
                let (mean, se) = acc.mean_and_stderr();
                means.push(mean);
                ses.push(se);
            }
            (means, ses)
        }

        #[test]
        fn integer_powers_come_from_measured_error_bars() {
            let ns = wide_sizes();
            for (name, power, f) in [
                ("N", 1, Box::new(|n: f64| 3.0 * n) as Box<dyn Fn(f64) -> f64>),
                ("N^2", 2, Box::new(|n: f64| 0.02 * n * n)),
                ("N^3", 3, Box::new(|n: f64| 1e-4 * n * n * n)),
            ] {
                for seed in [1u64, 3, 5, 7] {
                    let (means, ses) = replicated(&f, &ns, 0.02, 6, seed);
                    let fit = scaling_fit(&ns, &means, &ses, 3).unwrap();
                    assert_eq!(power, fit.power, "{name} seed {seed}");
                    assert!(
                        fit.chi2_per_dof < 5.0,
                        "{name} seed {seed}: chi2/dof {} should be near 1",
                        fit.chi2_per_dof
                    );
                }
            }
        }

        #[test]
        fn a_mixed_cost_reports_the_power_it_ends_up_at() {
            // The case a single-term search cannot get right: linear
            // dominates over most of the range, but the cost is quadratic.
            let ns = wide_sizes();
            for seed in [1u64, 3, 5, 7] {
                let (means, ses) =
                    replicated(|n| 5.0 * n + 0.05 * n * n, &ns, 0.02, 6, seed);
                let fit = scaling_fit(&ns, &means, &ses, 3).unwrap();
                assert_eq!(2, fit.power, "seed {seed}");
                assert!(
                    (fit.ns_per_scale - 0.05).abs() < 5.0 * fit.std_error,
                    "seed {seed}: {} +- {} should bracket 0.05",
                    fit.ns_per_scale,
                    fit.std_error
                );
            }
        }

        #[test]
        fn more_repeats_tighten_the_answer() {
            // The lever stage two pulls when the fit is not precise enough:
            // measure each size more times. Nothing else changes.
            let ns = wide_sizes();
            let few = replicated(|n| 0.02 * n * n, &ns, 0.05, 3, 1);
            let many = replicated(|n| 0.02 * n * n, &ns, 0.05, 48, 1);
            let a = scaling_fit(&ns, &few.0, &few.1, 3).unwrap();
            let b = scaling_fit(&ns, &many.0, &many.1, 3).unwrap();
            assert!(
                b.std_error < a.std_error,
                "48 repeats ({}) should beat 3 ({})",
                b.std_error,
                a.std_error
            );
        }

        /// A synthetic single call: the true cost at `n`, jittered.
        fn one_call(f: impl Fn(f64) -> f64, rel: f64, seed: u64) -> impl FnMut(usize) -> f64 {
            let mut rng = XorShift(seed | 1);
            move |n| {
                let u = |r: &mut XorShift| (r.next() >> 11) as f64 / (1u64 << 53) as f64;
                let jitter = (u(&mut rng) + u(&mut rng) - 1.0) * rel;
                f(n as f64) * (1.0 + jitter)
            }
        }

        fn counted<'a>(
            calls: &'a std::cell::Cell<usize>,
            mut inner: impl FnMut(usize) -> f64 + 'a,
        ) -> impl FnMut(usize) -> f64 + 'a {
            move |n| {
                calls.set(calls.get() + 1);
                inner(n)
            }
        }

        fn precise() -> Config {
            Config {
                target_rel_error: 0.01,
                target_abs_error: Duration::ZERO,
                max_time: Duration::from_secs(1),
            }
        }

        #[test]
        fn measuring_refines_until_the_answer_is_precise_enough() {
            let sizes = [64usize, 128, 256, 512, 1024];
            for seed in [1u64, 3, 5, 7] {
                let cfg = precise();
                let m = measure_scaling(
                    &sizes,
                    &cfg,
                    Duration::from_secs(3600),
                    3,
                    one_call(|n| 0.02 * n * n, 0.10, seed),
                );
                let (fit, limited) = (m.fit.unwrap(), m.hit_limit);
                assert!(!limited, "seed {seed}: should not have hit the budget");
                assert_eq!(2, fit.power, "seed {seed}");
                assert!(
                    fit.std_error < cfg.target_rel_error * fit.ns_per_scale,
                    "seed {seed}: {} +- {} misses the 1% target",
                    fit.ns_per_scale,
                    fit.std_error
                );
                assert!(
                    (fit.ns_per_scale - 0.02).abs() < 5.0 * fit.std_error,
                    "seed {seed}: {} +- {} should bracket 0.02",
                    fit.ns_per_scale,
                    fit.std_error
                );
            }
        }

        /// Replicates that straddle the truth exactly, so the mean of a
        /// full round-set lands on it and the fit has nothing to explain.
        ///
        /// Random jitter cannot be used here: chi-squared per degree of
        /// freedom is scale-free, near 1 whatever the noise level, so with
        /// six replicates it wanders either side of the threshold according
        /// to the seed. This test is about what the loop does once the
        /// model *is* accepted, so the data has to make that certain.
        fn balanced(f: impl Fn(f64) -> f64, rel: f64) -> impl FnMut(usize) -> f64 {
            let mut seen: std::collections::HashMap<usize, usize> = Default::default();
            move |n| {
                let k = seen.entry(n).or_insert(0);
                let sign = if *k % 2 == 0 { 1.0 } else { -1.0 };
                *k += 1;
                f(n as f64) * (1.0 + sign * rel)
            }
        }

        #[test]
        fn a_target_already_met_costs_nothing_extra() {
            // The loop buys precision it has found it lacks, and not
            // otherwise: a model that fits and a target already met must be
            // paid for exactly once.
            let sizes = [64usize, 128, 256, 512, 1024];
            let calls = std::cell::Cell::new(0);
            let cfg = Config {
                target_rel_error: 0.5,
                ..precise()
            };
            let m = measure_scaling(
                &sizes,
                &cfg,
                Duration::from_secs(3600),
                3,
                counted(&calls, balanced(|n| 0.02 * n * n, 0.02)),
            );
            let fit = m.fit.expect("exact quadratic data must fit");
            assert!(!m.hit_limit);
            assert_eq!(2, fit.power);
            assert!(
                fit.chi2_per_dof <= CHI2_REJECT,
                "premise: the model must be accepted, chi2/dof was {}",
                fit.chi2_per_dof
            );
            assert_eq!(INITIAL_REPEATS * sizes.len(), calls.get());
        }

        #[test]
        fn a_tighter_target_is_paid_for_with_more_calls() {
            let sizes = [64usize, 128, 256, 512, 1024];
            let mut counts = Vec::new();
            for target in [0.05, 0.005] {
                let calls = std::cell::Cell::new(0);
                let cfg = Config {
                    target_rel_error: target,
                    ..precise()
                };
                measure_scaling(
                    &sizes,
                    &cfg,
                    Duration::from_secs(3600),
                    3,
                    counted(&calls, one_call(|n| 0.02 * n * n, 0.10, 1)),
                );
                counts.push(calls.get());
            }
            assert!(
                counts[1] > counts[0],
                "0.5% took {} calls, 5% took {}",
                counts[1],
                counts[0]
            );
        }

        #[test]
        fn running_out_of_budget_still_answers_and_says_so() {
            // Better a wide answer flagged as wide than no answer: the
            // caller can tell the difference, which is the whole point of
            // returning the flag alongside the fit.
            let sizes = [64usize, 128, 256, 512, 1024];
            let cfg = Config {
                target_rel_error: 1e-9,
                ..precise()
            };
            let m = measure_scaling(
                &sizes,
                &cfg,
                Duration::from_millis(50),
                3,
                one_call(|n| 0.02 * n * n, 0.10, 1),
            );
            assert!(m.hit_limit, "an unreachable target must report the limit");
            assert_eq!(2, m.fit.unwrap().power);
        }

        #[test]
        fn a_benchmark_that_measures_as_free_still_terminates() {
            // A call the optimiser removed entirely can time as zero, and
            // a budget spent in units of measured cost would then never be
            // spent at all. The wall clock is what stops it.
            let sizes = [64usize, 128, 256, 512, 1024];
            let cfg = Config {
                target_rel_error: 1e-12,
                ..precise()
            };
            let m = measure_scaling(&sizes, &cfg, Duration::from_millis(20), 3, |_| 0.0);
            assert!(m.hit_limit);
            assert!(m.fit.is_none(), "nothing measurable, so nothing to report");
        }

        #[test]
        fn setup_around_the_call_counts_against_the_budget() {
            // `bench_scaling_gen` builds and drops an environment outside
            // the timed region, so real time can run out long before the
            // measured cost does. Simulated here by a `measure` that
            // sleeps far longer than the time it reports.
            let sizes = [64usize, 128];
            let cfg = Config {
                target_rel_error: 1e-12,
                ..precise()
            };
            let started = Instant::now();
            let m = measure_scaling(&sizes, &cfg, Duration::from_millis(100), 0, |_| {
                thread::sleep(Duration::from_millis(10));
                1.0
            });
            let elapsed = started.elapsed();
            assert!(m.hit_limit);
            assert!(
                elapsed < Duration::from_millis(600),
                "budget of 100ms overran to {elapsed:?}"
            );
        }

        #[test]
        fn an_exhausted_budget_still_buys_an_error_bar() {
            // The budget is already gone after the first round here. Two
            // rounds happen anyway, because a standard error needs two
            // points to exist and a fit needs standard errors: stopping on
            // the budget alone would return nothing at all rather than
            // something wide, which is the worse of the two failures.
            let sizes = [64usize, 128, 256];
            let cfg = Config {
                target_rel_error: 1e-12,
                ..precise()
            };
            let m = measure_scaling(
                &sizes,
                &cfg,
                Duration::from_micros(1),
                1,
                one_call(|n| 3.0 * n, 0.05, 1),
            );
            assert!(m.hit_limit);
            let fit = m.fit.expect("two rounds are enough to fit");
            assert!(fit.std_error.is_finite() && fit.std_error > 0.0);
        }

        /// Records what sizes a stage-one climb asked for.
        fn climb(
            nmin: usize,
            cost: impl Fn(f64) -> f64,
            budget: Duration,
        ) -> (SizeRange, Vec<usize>) {
            let tried = std::cell::RefCell::new(Vec::new());
            let range = discover_sizes(nmin, budget, |n| {
                tried.borrow_mut().push(n);
                cost(n as f64)
            });
            let tried = tried.into_inner();
            (range, tried)
        }

        #[test]
        fn a_measurable_first_size_still_yields_a_growth_rate() {
            // A cost that is already worth timing at `nmin` gives the climb
            // nothing to do, and it would hand over the assumed exponent of
            // 1 - which `choose_sizes` would then use to budget, planning a
            // ladder whose top rung costs a hundredfold what it predicted.
            for (name, p, cost) in [
                ("N", 1.0, Box::new(|n: f64| 1e7 * n) as Box<dyn Fn(f64) -> f64>),
                ("N^2", 2.0, Box::new(|n: f64| 1e7 * n * n)),
                ("N^3", 3.0, Box::new(|n: f64| 1e7 * n * n * n)),
            ] {
                let (range, tried) = climb(1, &cost, Duration::from_secs(3600));
                assert_eq!(1, range.lo, "{name}: the floor should not move");
                assert!(
                    tried.len() > 1,
                    "{name}: must probe upward to learn the growth rate"
                );
                assert!(
                    (range.exponent - p).abs() < 0.2,
                    "{name}: exponent came out {}",
                    range.exponent
                );
            }
        }

        #[test]
        fn the_ladder_always_has_enough_rungs_to_fit() {
            // Log spacing assumes enough distinct integers to land on, and
            // near the bottom there are not: from `lo` of 1 across a span
            // of 2, six log-spaced rungs round onto two. A fit needs more
            // sizes than terms, so a collapsed ladder cannot find the
            // scaling the sweep was set up to look for - the cubic case
            // was reduced to fitting a constant.
            for lo in [1usize, 2, 7, 100, 20_000] {
                for p in [0.5, 1.0, 2.0, 3.0] {
                    for secs in [1u64, 8, 60] {
                        let range = SizeRange {
                            lo,
                            lo_time: 1e7,
                            exponent: p,
                        };
                        let sizes = choose_sizes(range, 1);
                        assert_eq!(
                            NUM_SIZES,
                            sizes.len(),
                            "lo={lo} p={p} {secs}s gave {sizes:?}"
                        );
                        assert!(
                            sizes.windows(2).all(|w| w[0] < w[1]),
                            "lo={lo} p={p} {secs}s not increasing: {sizes:?}"
                        );
                        assert!(sizes[0] >= lo, "lo={lo}: {sizes:?} starts below the floor");
                    }
                }
            }
        }

        #[test]
        fn a_ladder_keeps_the_caller_unit() {
            // `nmin` is the caller's unit - sizes are counts of it - so
            // every rung has to be a multiple of it, densest packing
            // included.
            for nmin in [3usize, 64, 1000] {
                for p in [1.0, 3.0] {
                    let range = SizeRange {
                        lo: nmin,
                        lo_time: 1e7,
                        exponent: p,
                    };
                    let sizes = choose_sizes(range, nmin);
                    assert!(
                        sizes.iter().all(|n| n % nmin == 0),
                        "nmin={nmin} p={p}: {sizes:?}"
                    );
                }
            }
        }

        #[test]
        fn the_ladder_spans_a_fixed_amount_of_time_not_of_size() {
            // The whole of the choice. A fixed size span would mean a
            // different time span for every benchmark - four times the size
            // is four times the work for a linear cost and sixty-four times
            // for a cubic one - so the span is set in time and the sizes
            // follow from the growth rate stage one measured.
            //
            // Placed high enough up that log spacing has integers to land
            // on; down at the bottom the densest packing takes over and the
            // time span is whatever consecutive sizes happen to give.
            let mut tops = Vec::new();
            for p in [1.0, 2.0, 3.0] {
                let range = SizeRange {
                    lo: 20_000,
                    lo_time: 1e7,
                    exponent: p,
                };
                let sizes = choose_sizes(range, 1);
                assert_eq!(NUM_SIZES, sizes.len(), "p={p}: {sizes:?} collapsed");
                let time_span = (*sizes.last().unwrap() as f64 / 20_000.0).powf(p);
                assert!(
                    (time_span - TIME_SPAN).abs() < 0.1 * TIME_SPAN,
                    "p={p}: {sizes:?} spans {time_span}x in time, wanted {TIME_SPAN}x"
                );
                tops.push(*sizes.last().unwrap());
            }
            assert!(
                tops[0] > tops[1] && tops[1] > tops[2],
                "faster growth must reach the same time in fewer sizes, got {tops:?}"
            );
        }

        #[test]
        fn the_opening_rounds_cost_what_the_arithmetic_says() {
            // Six rungs log-spaced over a time ratio T cost
            // (T^(6/5) - 1)/(T^(1/5) - 1) times the cheapest one, which is
            // 13.4 at T = 4; six opening rounds make it about 80. At the
            // 20-microsecond floor that is under two milliseconds, and it
            // is the figure that says this does not need a time budget to
            // keep it honest.
            let lo_time = MIN_MEASURABLE.as_secs_f64() * 1e9;
            let range = SizeRange {
                lo: 1000,
                lo_time,
                exponent: 1.0,
            };
            let sizes = choose_sizes(range, 1);
            let round: f64 = sizes
                .iter()
                .map(|&n| lo_time * n as f64 / 1000.0)
                .sum();
            let opening = round * INITIAL_REPEATS as f64;
            assert!(
                opening < 2e6,
                "opening rounds came to {opening}ns, expected under 2ms"
            );
        }

        #[test]
        fn a_forced_ladder_is_the_cheapest_one_that_fits() {
            // Where affordability has to give way, it gives way as little
            // as possible: every rung is the smallest size still available,
            // so no ladder with this many rungs costs less. What the budget
            // then buys is fewer rounds, which `measure_scaling` handles by
            // stopping early and saying so - a wide answer on time, rather
            // than no answer at all.
            let range = SizeRange {
                lo: 1,
                lo_time: 1e7,
                exponent: 3.0,
            };
            let sizes = choose_sizes(range, 1);
            assert_eq!(vec![1, 2, 3, 4, 5, 6], sizes);
        }

        #[test]
        fn a_resolved_but_negligible_term_is_not_the_scaling() {
            // Measured on real hardware: summing integers produced a cubic
            // coefficient of about -5e-10 against a standard error of
            // 3e-16. Distinguishable from zero by seven orders of
            // magnitude, and worth about a billionth of the runtime. Being
            // sure a term is not zero says nothing about it being the
            // scaling, and only the second question has a useful answer.
            //
            // The error bars are given directly rather than simulated,
            // because that is the situation: a benchmark repeatable enough
            // to resolve a term far below the one that carries the cost.
            let ns = wide_sizes();
            let top = ns.iter().cloned().fold(0.0, f64::max);
            // Sized so the cubic is a billionth of the runtime at the top.
            let tiny = 1e-9 * 3.0 / top.powi(2);
            let means: Vec<f64> = ns.iter().map(|&n| 3.0 * n + tiny * n * n * n).collect();
            let ses: Vec<f64> = means.iter().map(|&m| 1e-12 * m).collect();

            // The term really is resolved - this is not a test of a term
            // that failed the significance rule for other reasons.
            let cubic = weighted_poly_fit(&ns, &means, &ses, 3).unwrap();
            assert!(
                cubic.coefficients[3].abs() / cubic.ses[3] > SIGNIFICANT,
                "the cubic should clear significance easily, at {}",
                cubic.coefficients[3].abs() / cubic.ses[3]
            );

            let fit = scaling_fit(&ns, &means, &ses, 3).unwrap();
            assert_eq!(1, fit.power, "a billionth of the runtime is not the scaling");
        }

        #[test]
        fn a_term_that_carries_the_cost_is_still_found() {
            // The other side of the same rule: a leading term that really
            // does account for the runtime must survive it, including when
            // a large lower-order term keeps its share well under half.
            let ns = wide_sizes();
            for (name, power, f) in [
                ("N^2", 2, Box::new(|n: f64| 0.02 * n * n) as Box<dyn Fn(f64) -> f64>),
                ("5N + 0.05N^2", 2, Box::new(|n: f64| 5.0 * n + 0.05 * n * n)),
                ("N^3", 3, Box::new(|n: f64| 1e-4 * n * n * n)),
            ] {
                let (means, ses) = replicated(&f, &ns, 0.002, 12, 1);
                let fit = scaling_fit(&ns, &means, &ses, 3).unwrap();
                assert_eq!(power, fit.power, "{name}");
            }
        }

        #[test]
        fn a_wide_error_bar_does_not_make_a_constant_the_shape_of_a_trend() {
            // The failure the rejection threshold exists to stop, taken
            // from the ladder `choose_sizes` actually builds: a linear cost
            // spanning four times in time, measured to 20%. A constant
            // misses that by chi-squared per degree of freedom of about 5 -
            // plainly the wrong shape, and yet under a threshold of ten it
            // was accepted, because ten tolerates a model wrong by an order
            // of magnitude.
            //
            // The damage is not just the wrong power. A degree-zero
            // prefactor is near enough the mean of every measurement, so it
            // looks precise at once and the refinement loop stops - the
            // accuracy target cannot save us, because a wrong model does
            // not present as an imprecise one.
            let ns = [1000.0, 1320.0, 1741.0, 2297.0, 3031.0, 4000.0];
            let means: Vec<f64> = ns.iter().map(|&n| 10.0 * n).collect();
            let ses: Vec<f64> = means.iter().map(|&m| 0.20 * m).collect();

            let flat = weighted_poly_fit(&ns, &means, &ses, 0).unwrap();
            assert!(
                flat.chi2_per_dof > CHI2_REJECT,
                "a constant must be rejected here, chi2/dof was {}",
                flat.chi2_per_dof
            );
            // And it is the threshold doing the work, not a hopeless fit:
            // this is the size of miss a loose threshold waves through.
            assert!(
                flat.chi2_per_dof < 10.0,
                "premise: the miss is moderate, chi2/dof was {}",
                flat.chi2_per_dof
            );

            let fit = scaling_fit(&ns, &means, &ses, 3).unwrap();
            assert_eq!(1, fit.power);
            assert!((fit.ns_per_scale - 10.0).abs() < 1e-6, "{}", fit.ns_per_scale);
        }

        #[test]
        fn a_whole_sweep_says_whether_it_believes_itself() {
            // Both stages end to end, on costs of known shape, driven by a
            // synthetic clock. Chi-squared no longer picks the model, so
            // reporting is the whole of its remaining job: saying whether
            // any polynomial actually described what was measured.
            let cfg = Config {
                target_rel_error: 0.02,
                target_abs_error: Duration::ZERO,
                max_time: Duration::from_secs(60),
            };

            // A real power law: identified, believed, and not flagged.
            let mut clock = one_call(|n| 50.0 * n * n, 0.02, 1);
            let stats = scaling_sweep(&cfg, 1, |n| clock(n));
            assert_eq!(2, stats.scaling.power);
            assert!(stats.goodness_of_fit > 0.9, "{}", stats.goodness_of_fit);
            assert!(!stats.hit_limit);
            assert!(
                (stats.scaling.ns_per_scale - 50.0).abs() < 0.1 * 50.0,
                "{}",
                stats.scaling.ns_per_scale
            );

            // A cost no polynomial describes. It still reports the power it
            // behaves like - the slope does not need a polynomial to exist
            // - but says it could not vouch for the shape.
            let mut clock = one_call(|n| 40.0 * n * (n + 1.0).ln(), 0.0005, 1);
            let stats = scaling_sweep(&cfg, 1, |n| clock(n));
            assert_eq!(
                0.0, stats.goodness_of_fit,
                "an N log N cost is not a polynomial and should say so"
            );
            assert!(stats.hit_limit);
        }

        #[test]
        fn a_bad_fit_does_not_widen_the_coefficient_error_bars() {
            // The whole reason for measuring the error bars rather than
            // assuming them: how precisely a coefficient is known depends on
            // the sizes and how well each was measured, and on nothing else.
            // `(XtWX)^-1` never sees the timings. So two datasets that share
            // sizes and error bars get identical coefficient errors even when
            // one is the right shape and the other is nowhere close - the
            // misfit shows up in chi-squared, where it belongs, instead of
            // being laundered into error bars wide enough to cover it.
            let ns = wide_sizes();
            let (fits, ses) = replicated(|n| 0.02 * n * n, &ns, 0.01, 8, 1);
            let misfits: Vec<f64> = ns.iter().map(|&n| 2.0 * n * n.ln()).collect();

            let good = weighted_poly_fit(&ns, &fits, &ses, 2).unwrap();
            let bad = weighted_poly_fit(&ns, &misfits, &ses, 2).unwrap();

            for (j, (g, b)) in good.ses.iter().zip(bad.ses.iter()).enumerate() {
                assert!(
                    (g - b).abs() <= 1e-9 * g.abs(),
                    "term {j}: error bars {g} and {b} should not depend on the timings"
                );
            }
            assert!(good.chi2_per_dof < 5.0, "{}", good.chi2_per_dof);
            assert!(bad.chi2_per_dof > 100.0, "{}", bad.chi2_per_dof);
        }

        #[test]
        fn the_reported_coefficient_is_refit_at_the_chosen_degree() {
            // Our terms are not orthogonal, so a coefficient read off a wider
            // fit is a partial thing: it describes what N^p contributes once
            // higher terms have taken their share, which is not the question
            // asked. Refitting at the chosen degree both re-centres it and
            // stops it paying for degrees of freedom the data did not need.
            let ns = wide_sizes();
            for seed in [1u64, 3, 5, 7] {
                let (means, ses) = replicated(|n| 3.0 * n, &ns, 0.02, 6, seed);
                let fit = scaling_fit(&ns, &means, &ses, 3).unwrap();
                assert_eq!(1, fit.power, "seed {seed}");

                let refit = weighted_poly_fit(&ns, &means, &ses, fit.power).unwrap();
                assert_eq!(refit.coefficients[fit.power], fit.ns_per_scale, "seed {seed}");
                assert_eq!(refit.ses[fit.power], fit.std_error, "seed {seed}");

                let wide = weighted_poly_fit(&ns, &means, &ses, 3).unwrap();
                assert!(
                    fit.std_error < 0.8 * wide.ses[fit.power],
                    "seed {seed}: refitting should tighten {} below {}",
                    fit.std_error,
                    wide.ses[fit.power]
                );
            }
        }

        #[test]
        fn a_shape_that_does_not_fit_is_visible_in_chi_squared() {
            // Coefficient errors say how well the coefficients are known;
            // chi-squared says whether the model is the right shape. Keeping
            // them apart is the point of using measured error bars, and it
            // is what lets a bad shape be caught rather than absorbed into
            // wider error bars.
            let ns = wide_sizes();
            let (means, ses) = replicated(|n| 2.0 * n * n.ln(), &ns, 0.005, 12, 1);
            let fit = scaling_fit(&ns, &means, &ses, 3).unwrap();
            assert!(
                fit.chi2_per_dof > 10.0,
                "N log N is not a polynomial here; chi2/dof was {}",
                fit.chi2_per_dof
            );
        }

        #[test]
        fn a_power_law_gives_back_its_own_exponent() {
            let ns = wide_sizes();
            for (name, p, f) in [
                ("N", 1.0, Box::new(|n: f64| 3.0 * n) as Box<dyn Fn(f64) -> f64>),
                ("N^2", 2.0, Box::new(|n: f64| 0.02 * n * n)),
                ("N^3", 3.0, Box::new(|n: f64| 1e-4 * n * n * n)),
            ] {
                for seed in 1..5 {
                    let (ts, ses) = measured(&f, &ns, 0.01, seed);
                    let fit = power_fit(&ns, &ts, &ses).unwrap();
                    assert!(
                        (fit.exponent - p).abs() < 0.02,
                        "{name} seed {seed}: exponent {} should be about {p}",
                        fit.exponent
                    );
                    // And it says so confidently: a single power law really
                    // does describe this, so chi-squared per degree of
                    // freedom sits near one.
                    assert!(
                        fit.chi2_per_dof < 5.0,
                        "{name} seed {seed}: chi2/dof {} should be near 1",
                        fit.chi2_per_dof
                    );
                }
            }
        }

        #[test]
        fn the_prefactor_comes_back_too() {
            let ns = wide_sizes();
            let (ts, ses) = measured(|n| 0.02 * n * n, &ns, 0.01, 1);
            let fit = power_fit(&ns, &ts, &ses).unwrap();
            assert!(
                (fit.prefactor - 0.02).abs() < 0.02 * 0.1,
                "prefactor {} should be about 0.02",
                fit.prefactor
            );
        }

        #[test]
        fn a_cost_that_is_not_one_power_law_says_so() {
            // Chi-squared per degree of freedom is the signal, and it is
            // not a close call: pure powers sit near 1 (asserted above),
            // while these are tens to hundreds.
            let ns = wide_sizes();
            for (name, f) in [
                (
                    "N log N",
                    Box::new(|n: f64| 2.0 * n * n.ln()) as Box<dyn Fn(f64) -> f64>,
                ),
                ("5N + 0.05N^2", Box::new(|n: f64| 5.0 * n + 0.05 * n * n)),
            ] {
                let (ts, ses) = measured(&f, &ns, 0.01, 1);
                let fit = power_fit(&ns, &ts, &ses).unwrap();
                assert!(
                    fit.chi2_per_dof > 10.0,
                    "{name}: chi2/dof {} should be large",
                    fit.chi2_per_dof
                );
            }
        }

        #[test]
        fn a_drifting_local_exponent_shows_a_mixed_cost_approaching_its_limit() {
            // 5N + 0.05N^2 is asymptotically quadratic, but only becomes so
            // as N grows. The local exponent climbs towards 2, and *that*
            // is the asymptotic answer - a single global fit averages the
            // whole transition and lands uselessly in between.
            let ns = wide_sizes();
            let (ts, _) = measured(|n| 5.0 * n + 0.05 * n * n, &ns, 0.001, 1);
            let local: Vec<f64> = ns
                .windows(2)
                .zip(ts.windows(2))
                .filter_map(|(n, t)| two_point_exponent(n[0], t[0], n[1], t[1]))
                .collect();
            let first = local.first().copied().unwrap();
            let last = local.last().copied().unwrap();
            assert!(first < 1.4, "starts near linear, got {first}");
            assert!(last > 1.8, "ends near quadratic, got {last}");

            // A genuine power law does not drift.
            let (ts, _) = measured(|n| 0.02 * n * n, &ns, 0.001, 1);
            let local: Vec<f64> = ns
                .windows(2)
                .zip(ts.windows(2))
                .filter_map(|(n, t)| two_point_exponent(n[0], t[0], n[1], t[1]))
                .collect();
            for p in &local {
                assert!((p - 2.0).abs() < 0.15, "steady quadratic, got {p}");
            }
        }

        #[test]
        fn two_points_are_enough_for_a_rough_exponent() {
            // Which is all size selection needs: enough to predict what the
            // next size will cost.
            assert!((two_point_exponent(10.0, 100.0, 100.0, 10000.0).unwrap() - 2.0).abs() < 1e-9);
            assert!((two_point_exponent(10.0, 10.0, 100.0, 100.0).unwrap() - 1.0).abs() < 1e-9);
            assert_eq!(None, two_point_exponent(10.0, 1.0, 10.0, 2.0));
            assert_eq!(None, two_point_exponent(10.0, 0.0, 20.0, 1.0));
        }

        #[test]
        fn names_the_fastest_growing_term_the_data_supports() {
            for seed in 1..5 {
                assert_eq!(Some(Growth::Constant), growth_of(|_| 42.0, 20, seed), "constant");
                assert_eq!(Some(Growth::Linear), growth_of(|n| 3.0 * n, 20, seed), "linear");
                assert_eq!(
                    Some(Growth::Power(2)),
                    growth_of(|n| 0.02 * n * n, 20, seed),
                    "quadratic"
                );
                assert_eq!(
                    Some(Growth::Power(3)),
                    growth_of(|n| 1e-4 * n * n * n, 20, seed),
                    "cubic"
                );
            }
        }

        #[test]
        fn n_log_n_is_named_rather_than_approximated() {
            // A basis of powers alone cannot represent this, and reports it
            // as N^4 - faster-growing than the truth, which is the failure
            // that motivated giving it a term of its own.
            for seed in 1..5 {
                assert_eq!(
                    Some(Growth::Linearithmic),
                    growth_of(|n| 2.0 * n * n.max(2.0).ln(), 20, seed),
                    "seed {seed}"
                );
            }
        }

        #[test]
        fn an_exponential_is_named_rather_than_approximated() {
            // 2^N needs a small range of N or it overflows every f64 in
            // sight; the basis normalises the column against the largest N
            // for exactly that reason.
            let xs: Vec<f64> = (1..26).map(|k| k as f64).collect();
            for seed in 1..5 {
                let ys = noisy(|n| 1e3 * (n * std::f64::consts::LN_2).exp(), &xs, 0.03, seed);
                assert_eq!(
                    Some(Growth::Exponential),
                    dominant_growth(&xs, &ys, &weights(&ys), 3),
                    "seed {seed}"
                );
            }
            // ...and a merely-quadratic cost over that same range must not
            // be mistaken for one.
            for seed in 1..5 {
                let ys = noisy(|n| 0.02 * n * n, &xs, 0.03, seed);
                assert_eq!(
                    Some(Growth::Power(2)),
                    dominant_growth(&xs, &ys, &weights(&ys), 3),
                    "seed {seed}"
                );
            }
        }

        #[test]
        fn a_mixed_cost_is_named_by_its_fastest_term() {
            for seed in 1..5 {
                assert_eq!(
                    Some(Growth::Power(2)),
                    growth_of(|n| 5.0 * n + 0.05 * n * n, 20, seed),
                    "5N + 0.05N^2, seed {seed}"
                );
                // Linear plus linearithmic is linearithmic, which a
                // power-only basis could not say at all.
                assert_eq!(
                    Some(Growth::Linearithmic),
                    growth_of(|n| 20.0 * n + 2.0 * n * n.max(2.0).ln(), 20, seed),
                    "20N + 2N logN, seed {seed}"
                );
            }
        }

        #[test]
        fn finds_the_degree_of_a_pure_power_law() {
            for seed in 1..6 {
                assert_eq!(Some(1), degree_of(|n| 3.0 * n, 20, seed), "linear, seed {seed}");
                assert_eq!(Some(2), degree_of(|n| 0.02 * n * n, 20, seed), "quadratic, seed {seed}");
            }
        }

        #[test]
        fn a_mixed_cost_reports_its_dominant_term() {
            // The case a single-term search cannot get right: the linear
            // part is larger over most of the range, but the quadratic is
            // what the cost is asymptotically.
            for seed in 1..6 {
                assert_eq!(
                    Some(2),
                    degree_of(|n| 5.0 * n + 0.05 * n * n, 20, seed),
                    "5N + 0.05N^2, seed {seed}"
                );
            }
        }

        #[test]
        fn a_range_too_short_to_see_a_term_does_not_invent_one() {
            // A genuine cubic, but sampled over too small a span of N to be
            // distinguishable. Under-reporting is the safe direction, and
            // it should never come back as *more* than cubic.
            let short = degree_of(|n| 1e-4 * n * n * n + 2.0 * n, 8, 1);
            assert!(
                matches!(short, Some(0..=3)),
                "short range should not overreach, got {short:?}"
            );
            // Given enough range the same cost is identified.
            assert_eq!(Some(3), degree_of(|n| 1e-4 * n * n * n + 2.0 * n, 20, 1));
        }

        #[test]
        fn a_constant_cost_has_no_growing_term() {
            assert_eq!(Some(0), degree_of(|_| 42.0, 20, 1));
        }

        #[test]
        fn coefficients_come_back_with_believable_error_bars() {
            let xs = sizes(20);
            let ys = noisy(|n| 0.02 * n * n, &xs, 0.05, 3);
            let (coef, se) = poly_fit(&xs, &ys, &weights(&ys), 2).unwrap();
            // The quadratic coefficient is recovered to within a few of its
            // own standard errors - the error bar means what it says.
            assert!(
                (coef[2] - 0.02).abs() < 4.0 * se[2],
                "coef {} +- {} should bracket 0.02",
                coef[2],
                se[2]
            );
            assert!(se[2] > 0.0 && se[2] < 0.02, "se {} implausible", se[2]);
        }

        fn confirmed(f: impl Fn(f64) -> f64, count: usize, seed: u64) -> Option<usize> {
            let xs = sizes(count);
            let ys = noisy(f, &xs, 0.05, seed);
            confirmed_dominant_degree(&xs, &ys, &weights(&ys), 4)
        }

        /// Mostly small jitter, occasionally a sample that ran much
        /// slower - which is what real timing noise looks like, and unlike
        /// bounded jitter it can put a genuine outlier at the largest size.
        fn heavy_tailed(f: impl Fn(f64) -> f64, xs: &[f64], seed: u64) -> Vec<f64> {
            let mut rng = XorShift(seed | 1);
            xs.iter()
                .map(|&x| {
                    let u = |r: &mut XorShift| (r.next() >> 11) as f64 / (1u64 << 53) as f64;
                    let base = (u(&mut rng) + u(&mut rng) - 1.0) * 0.03;
                    let spike = if u(&mut rng) < 0.10 { u(&mut rng) * 0.8 } else { 0.0 };
                    f(x) * (1.0 + base + spike)
                })
                .collect()
        }

        #[test]
        fn a_degree_resting_on_the_largest_sample_is_not_confirmed() {
            let xs = sizes(24);
            // A quadratic term barely above the noise. On this draw the
            // full sweep sees it, but it evaporates without the largest
            // size - so the honest answer is neither "quadratic" nor
            // "linear" but "keep sampling", which is what `None` asks for.
            let ys = heavy_tailed(|n| 5.0 * n + 0.002 * n * n, &xs, 12);
            let ws = weights(&ys);
            assert_eq!(Some(2), dominant_degree(&xs, &ys, &ws, 4));
            assert_eq!(None, confirmed_dominant_degree(&xs, &ys, &ws, 4));

            // The check must not cry wolf: a purely linear cost is stable
            // under the same noise for every seed tried, so a solid answer
            // is never withheld.
            for seed in 1..40 {
                let ys = heavy_tailed(|n| 5.0 * n, &xs, seed);
                let ws = weights(&ys);
                assert_eq!(
                    Some(1),
                    confirmed_dominant_degree(&xs, &ys, &ws, 4),
                    "linear cost should be confirmed, seed {seed}"
                );
            }

            // A term with room to spare survives losing its largest point.
            for seed in 1..5 {
                assert_eq!(Some(2), confirmed(|n| 5.0 * n + 0.05 * n * n, 20, seed));
                assert_eq!(Some(1), confirmed(|n| 3.0 * n, 20, seed));
            }
        }

        #[test]
        fn a_singular_fit_reports_failure_rather_than_nonsense() {
            // Every x identical: no range at all, so nothing is
            // identifiable and the normal equations are singular.
            let xs = vec![5.0; 12];
            let ys = vec![1.0; 12];
            assert!(poly_fit(&xs, &ys, &[1.0; 12], 2).is_none());
            // Fewer points than coefficients.
            assert!(poly_fit(&[1.0, 2.0], &[1.0, 2.0], &[1.0, 1.0], 3).is_none());
        }
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
        let stats = only_absolute(5).bench(variable_cost(7));
        println!("absolute 5ns: {stats}");
        assert!(!stats.hit_limit, "should have reached +-5ns in the budget");
        assert!(
            stats.std_error < 5.0,
            "asked for +-5ns, got +-{:.2}ns",
            stats.std_error
        );

        // A looser absolute ask must be cheaper - the target is doing the
        // work, not some fixed amount of sampling.
        let cheap = only_absolute(100).bench(variable_cost(7));
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

    /// The scaling error bar has to describe the spread that actually
    /// occurs, not merely be a number that shrinks when asked. This is the
    /// property that cannot be checked by reading the code: the estimator
    /// is HC3 precisely because the textbook OLS standard error is roughly
    /// 2x optimistic once the samples are heteroscedastic, which they are
    /// here, and only a repeated-measurement check can tell the two apart.
    #[test]
    fn scaling_error_bar_is_honest() {
        println!();
        if !quiesced() {
            println!("SKIPPED: machine is not quiesced (see `quiet-bench reserve`)");
            return;
        }
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
        // Only a third, not a majority: the single-term search misreads
        // this linear workload as quadratic about 17% of the time - see
        // `dominant_degree`, which exists to replace it - so demanding a
        // majority of twelve runs is itself a coin-flip. This test is about
        // whether the error bar is honest, not about model selection, so it
        // asks only for enough agreeing runs to measure a spread from.
        assert!(
            linear.len() * 3 > REPEATS,
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





