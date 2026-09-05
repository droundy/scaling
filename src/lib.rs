/*!
A lightweight micro-benchmarking library which:

* measures until it reaches an accuracy you ask for, and tells you the
  accuracy it achieved;
* handles benchmarks which mutate state;
* can measure how a benchmark scales, as a power of `N`
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
printed to exactly the precision that error justifies.  Quick statistics
note:  in general you should expect a measured value to be more than *two*
standard errors *away* from the true value about 5% of the time, and about a
third of the time you should expect the discrepancy to be more than one
standard error.  So do *not* take this `±` value as a bound on the error!

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
  for [`bench()`].

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

Only power laws are fitted. A cost that is not one - `O(N log N)`, or
`O(2ᴺ)` - is reported as the integer power it most behaves like over the
range measured, with `goodness_of_fit` zeroed and the `(limit)` mark to say
that nothing described it exactly. Naming those shapes needs a different
kind of fit and would be a different feature; measuring a power well is the
thing this does.

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
mod bench;
mod scaling;

// `self::` because the crate is called `scaling` too, and rustdoc builds
// doctests with `--extern scaling` pointing at this very crate - which
// leaves a bare `scaling::` ambiguous between the module below and the
// whole crate. Rust 1.66 calls that ambiguity an error; later compilers
// quietly pick one, so this only ever failed on the oldest supported
// toolchain, and only when building doctests rather than the library.
pub use self::bench::{bench, bench_env, bench_gen_env, Stats};
pub use self::scaling::{bench_scaling, bench_scaling_gen, Scaling, ScalingStats};

use std::f64;
use std::time::*;

/// Roughly the longest a single benchmark should take.
///
/// A backstop rather than a target: both kinds of benchmark stop as soon as
/// they have the accuracy asked for, and neither sizes any of its work
/// against the time available.
const BENCH_TIME_MAX: Duration = Duration::from_secs(10);
/// How hard a benchmark works to pin down `ns_per_iter`, and when it gives
/// up.
///
/// [`bench()`], [`bench_env`] and [`bench_gen_env`] use [`Config::default`];
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
    // The floor is zero, not one: an error of 25 in its own unit wants no
    // decimals at all, and forcing one on it prints `25.0`, which is three
    // significant digits claiming to be two.
    (1 - x.log10().floor() as i64).clamp(0, 9) as usize
}

/// A value and its error, formatted to the precision the error justifies:
/// digits of the value beyond where the uncertainty starts are noise
/// dressed up as signal, so `71.9858 ± 0.17` is really only known to
/// `71.99 ± 0.17`, and printing the extra two digits would invite a reader
/// to believe them.
///
/// The error switches to scientific notation below `1e-4` rather than
/// spelling out a run of leading zeroes - an optimised-away benchmark can
/// reach `0.000000021` - but the value keeps plain digits at the same
/// decimal count, which is what the scientific notation stands in for.
///
/// Callers own the unit: this only picks how many digits to show, in
/// whatever unit `value` and `error` already share.
fn value_and_error(value: f64, error: f64) -> (String, String) {
    let decimals = error_decimals(error);
    let error_str = if error > 0.0 && error < 1e-4 {
        format!("{error:.1e}")
    } else {
        format!("{error:.decimals$}")
    };
    (format!("{value:.decimals$}"), error_str)
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
        // the mean. `m2` is a sum of squared deviations and so is only
        // non-negative in exact arithmetic; on a run with almost no real
        // spread, floating-point cancellation can push it fractionally
        // below zero. That is the same fact a genuine `m2 == 0.0` reports -
        // no detectable spread - so it is clamped there rather than let
        // through to `sqrt` as a `NaN` that would misrepresent a clean
        // measurement as a failed one.
        let var = (self.m2 / (self.count - 1) as f64).max(0.0);
        (self.mean, (var / self.count as f64).sqrt())
    }
}



/// Helpers shared by both modules' tests.
#[cfg(test)]
pub(crate) mod testutil {
    pub struct XorShift(pub u64);
    impl XorShift {
        pub fn next(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            x
        }

        /// Uniform on `[0, 1)`, from the top 53 bits - as many as an `f64`
        /// mantissa holds.
        fn uniform01(&mut self) -> f64 {
            (self.next() >> 11) as f64 / (1u64 << 53) as f64
        }

        /// A relative jitter drawn from `[-rel, rel)`, roughly triangular
        /// rather than flat: the sum of two independent uniforms concentrates
        /// near zero the way real measurement noise does, instead of every
        /// value in range being equally likely.
        pub fn jitter(&mut self, rel: f64) -> f64 {
            (self.uniform01() + self.uniform01() - 1.0) * rel
        }
    }

    /// Is the machine quiet enough for a timing assertion to mean anything?
    pub fn quiesced() -> bool {
        matches!(crate::quiet::status(), crate::quiet::Status::Pinned { .. })
    }

    pub fn mean_and_spread(xs: &[f64]) -> (f64, f64) {
        let n = xs.len() as f64;
        let mean = xs.iter().sum::<f64>() / n;
        let sd = (xs.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / n).sqrt();
        (mean, sd / mean)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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


}





