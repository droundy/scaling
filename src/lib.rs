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
use scaling::{bench,bench_input,bench_scaling};

# fn fib(_: usize) -> usize { 0 }
#
// Simple benchmarks are performed with `bench` or `bench_scaling`.
println!("fib 200: {}", bench(|| fib(200) ));
println!("fib 500: {}", bench(|| fib(500) ));
println!("fib scaling: {}", bench_scaling(|n| fib(n), 0));

// If a function needs to mutate some state, use `bench_input`.
println!("reverse: {}", bench_input(vec![0;100], |xs| xs.reverse() ));
println!("sort:    {}", bench_input(vec![0;100], |xs| xs.sort()    ));
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

## Flat benchmarks: `bench`, `bench_input`, `bench_gen_input`

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
  for [`bench`](fn@bench).

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

# Suites: measuring many benchmarks together

Benchmarks run one after another are measured in different machines. The
first runs on a cold package and the fiftieth on a warm one, so their
numbers are not comparable with each other, and neither is either of them
with the same suite run tomorrow.

[`Config::suite`] measures them interleaved instead, one sample each in
rotation, so every benchmark's samples spread across the whole session and
all of them average the same drift. Each `add` returns a token to read that
benchmark's answer from once [`Suite::run`] has finished:

```
let cfg = scaling::Config::default();
let mut suite = cfg.suite();
let sort = suite.add_input("sort", vec![5, 3, 1, 4, 2], |v: &mut Vec<i32>| v.sort());
let sum = suite.add("sum", || (0..100u64).sum::<u64>());
println!("{}", suite.run());
# let _ = (sort.get().unwrap(), sum.get().unwrap());
```

A suite is not restricted to one kind or one input type: [`Suite::add_scaling`]
takes a scaling benchmark and [`Suite::add_comparison`] takes a whole
[`ComparisonSet`], and the token remembers which, so each answer keeps its
own type. A comparison counts as *one* participant in the rotation, because
its round must stay whole for the paired error bar to mean anything - which
is also fair, since one of its turns runs `k` batches and produces `k`
[`Stats`].

What this buys is a *bound*, not an improvement. Reversing the declaration
order of eight identical workloads moves an interleaved benchmark by
0.15-0.45%, whatever the session; measured one after another the same
workloads move by anywhere from 0.10% to 1.19%, depending on nothing but how
much the machine happened to be drifting. The medians are near enough equal
(0.28% against 0.26%); the worst case is four times better. Interleaving
pays a floor it never gets back - every sample starts on a cache the rest of
the suite has been using - in exchange for a ceiling on drift.

So it does *not* make any single benchmark more precise - it averages drift
in rather than out - and it does not make a suite's numbers comparable with
a lone [`bench`](fn@bench) call. What it gives you is that the numbers within one
suite, and across runs of it, were measured in the same machine.

Each benchmark still gets [`Config::max_time`] of its own running time, so a
suite of `n` may take `n` times as long as one - the same arithmetic
[`Config::compare`] uses for two.

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
* In the case of [`bench_input`] and [`bench_gen_input`], we also do a lookup into a big vector in
  order to get the input for that iteration.
* If you compile your program unoptimised, there may be additional overhead.

The cost of the above operations depend on the details of your benchmark;
namely: (1) how large is the return value? and (2) does the benchmark evict
the input vector from the CPU cache? In practice, these criteria are only
satisfied by longer-running benchmarks, making these effects hard to measure.

## Caveat 2: Pure functions

**TL;DR: Return enough information to prevent the optimiser from eliminating
code from your benchmark.**

Benchmarking pure functions involves a nasty gotcha which users should be
aware of. Consider the following benchmarks:

```
# use scaling::{bench,bench_input};
#
# fn fib(_: usize) -> usize { 0 }
#
let fib_1 = bench(|| fib(500) );                     // fine
let fib_2 = bench(|| { fib(500); } );                // spoiler: NOT fine
let fib_3 = bench_input(0, |x| { *x = fib(500); } );   // also fine, but ugly
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
own input. This has the desired effect, but looks a bit weird.

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

/// Assembling registered benchmarks into a suite. See `REGISTRATION.md`.
#[cfg(feature = "registry")]
#[doc(hidden)]
pub mod assemble;
mod bench;
mod compare;
mod kway;
pub mod quiet;
/// Benchmarks registered from anywhere in a crate. See `REGISTRATION.md`.
///
/// Public but hidden: the types here are named by generated registration
/// code rather than written by hand, and their shapes are not yet stable.
#[doc(hidden)]
pub mod registry;
mod scaling;
mod suite;
pub(crate) use bench::{time_batch, time_loop};
pub(crate) use suite::{block_on, Clock, Machine};
pub(crate) mod significant;

// `self::` because the crate is called `scaling` too, and rustdoc builds
// doctests with `--extern scaling` pointing at this very crate - which
// leaves a bare `scaling::` ambiguous between the module below and the
// whole crate. Rust 1.66 calls that ambiguity an error; later compilers
// quietly pick one, so this only ever failed on the oldest supported
// toolchain, and only when building doctests rather than the library.
pub use self::bench::{bench, bench_gen_input, bench_input, Stats};
pub use self::compare::Comparison;
pub use self::kway::{ComparisonSet, Comparisons};
pub use self::scaling::{bench_scaling, bench_scaling_gen, Scaling, ScalingStats};
#[cfg(feature = "registry")]
pub use self::suite::RegisteredTokens;
pub use self::suite::{Report, Suite, Token};

/// Re-exported so that registration code written by a macro has a single
/// path to name, and callers need not depend on `inventory` themselves.
#[cfg(feature = "registry")]
#[doc(hidden)]
pub use inventory;

/// Attribute macros that register a benchmark where it is written, rather
/// than requiring it be added to a suite by hand. See `REGISTRATION.md`.
#[cfg(feature = "registry")]
pub use scaling_macros::{bench, bench_scaling, candidate, gen_input, input};

use std::f64;
use std::sync::atomic::Ordering::{Acquire, Release};
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::Arc;
use std::time::*;

/// Spend at least this long *running the benchmark* before believing any
/// accuracy target.
///
/// Measured time, not wall-clock time: an input that is slow to build would
/// otherwise satisfy the floor by being built, and construction is not
/// evidence about the function. [`Config::max_time`] is the opposite - a
/// wall-clock cap, because that is a promise about how long the caller
/// waits - so the two clocks are deliberately different.
///
/// A comparison gets twice this, since a round there buys evidence about
/// two functions and is only as good as its weaker half.
///
/// All three sampling loops used to stop on a count - six samples in [`bench`],
/// six rounds in [`bench_scaling`] - and a count is the wrong unit. Six
/// samples of a nanosecond-scale function is barely a millisecond of
/// evidence, and the accuracy target is then met by whichever six happened
/// to agree. Measured across seven workloads, 95% of `bench` runs stopped
/// there; the scaling sweep's opening rounds come to under two milliseconds
/// on a benchmark sitting at its measurable floor, which is the same
/// regime.
///
/// A floor in time is scale-free where a count floor is not: it costs a
/// slow function nothing, since one sample already exceeds it, while making
/// a fast one watch the machine for a while rather than for an instant.
/// Raising the counts instead would make a benchmark that sleeps 400ms per
/// iteration take ten seconds.
///
/// Three milliseconds is where it stops paying. Sweeping both floors
/// together over seven workloads - integer, transcendental, division and
/// branchy, from 20ns to 2.8us - round-robin so every cell met the same
/// drift:
///
/// ```none
///   time floor   spread   worst error bar   cost
///   none         0.316%        1.01x        1.3ms
///   1ms          0.244%        0.93x        1.4ms
///   3ms          0.143%        1.45x        3.4ms
///   10ms         0.144%        3.10x       10.4ms
/// ```
///
/// "worst error bar" is how far the reported `±` understates the spread
/// actually seen, for whichever workload it understated most. Ten
/// milliseconds buys no further reproducibility and costs a great deal of
/// honesty: past a few milliseconds the `±` shrinks faster than the answer
/// settles, so sampling harder yields a tighter number that is less true.
/// Reproducibility beyond this is the caller's to ask for, with
/// [`Config::target_rel_error`].
const MIN_SAMPLE_TIME: Duration = Duration::from_millis(3);

/// Roughly the longest a single benchmark should take.
///
/// A backstop rather than a target: both kinds of benchmark stop as soon as
/// they have the accuracy asked for, and neither sizes any of its work
/// against the time available.
const BENCH_TIME_MAX: Duration = Duration::from_secs(10);
/// How hard a benchmark works to pin down `ns_per_iter`, and when it gives
/// up.
///
/// [`bench`](fn@bench), [`bench_input`] and [`bench_gen_input`] use [`Config::default`];
/// call the same-named methods on a `Config` to choose your own.
///
/// ```
/// use scaling::Config;
/// use std::time::Duration;
///
/// // "to within a tenth of a percent"
/// let tight = Config::relative(0.001);
/// // "to within 50 nanoseconds, and do not spend more than a second"
/// let quick = Config::absolute(Duration::from_nanos(50))
///     .with_max_time(Duration::from_secs(1));
/// # let _ = (tight.target_rel_error, quick.target_abs_error);
/// ```
#[derive(Debug, Clone)]
pub struct Config {
    /// Stop once the standard error falls below this fraction of the
    /// measurement (`0.01` = 1%).
    ///
    /// The `compare_*` functions read this as a *sensitivity* rather than a
    /// precision: the smallest difference worth detecting, as a fraction of
    /// the baseline. See [`Config::compare_gen_input`], which spells out what
    /// that floor is and is not worth.
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
    ///
    /// As with [`Config::target_rel_error`], the `compare_*` functions read
    /// this as the smallest difference worth detecting rather than as a
    /// precision.
    pub target_abs_error: Duration,
    /// Give up after roughly this much wall-clock time even if neither goal
    /// was reached, setting [`Stats::hit_limit`].
    ///
    /// Wall clock rather than measured time, because this is a promise about
    /// how long the caller waits - a benchmark whose input is slow to build
    /// has still taken that long. The `compare_*` functions allow twice
    /// this, since they produce two [`Stats`] and would otherwise give each
    /// side half the budget a lone [`bench`](fn@bench) gets for the same target.
    pub max_time: Duration,
    /// The multiple-comparison plan, shared by every clone of this `Config`
    /// *until* one of them plans, which detaches it.
    ///
    /// See [`Config::with_comparisons_planned`] for what it is for.
    plan: Arc<Plan>,
}

/// How many comparisons were promised, how many have happened, and the
/// significance threshold the promise implies.
///
/// These live behind one `Arc` because they are one fact and because
/// [`Config::suite`] hands out only a `&Config` - a [`Suite`] must be able to
/// record a plan through a shared reference, since it knows how many
/// comparisons it holds only once the last one is added.
///
/// Holding them separately was wrong: `planned` used to be a plain field
/// while `made` was already shared, so two clones could disagree about the
/// plan and whichever dropped last decided whether the assertion in `Drop`
/// fired. Sharing both makes them agree - and
/// [`Config::with_comparisons_planned`] detaches, so a `Config` kept as a
/// template can still be cloned and planned several different ways.
#[derive(Debug)]
struct Plan {
    /// Claimed by each comparison as it starts.
    made: AtomicU64,
    /// What was promised.
    ///
    /// Read first and written last, which is what makes `z_alpha` safe to
    /// read without a lock: see [`Config::set_comparisons_planned`].
    planned: AtomicU64,
    /// The Bonferroni limit `planned` implies, as `f64::to_bits`. Cached
    /// because the sampling loop consults it after every round; an atomic
    /// load is nothing against a batch, where recomputing the inverse normal
    /// would not be.
    ///
    /// `NaN` until a plan is set, which makes every significance test false.
    z_alpha: AtomicU64,
    /// Whether the *caller* set this plan, as opposed to a [`Suite`]
    /// counting its own comparisons.
    ///
    /// A caller who says how many comparisons they will make may be planning
    /// some outside any suite, so their number is taken as final; suites
    /// otherwise add their own comparisons to it as they run.
    by_caller: AtomicBool,
}

impl Default for Plan {
    fn default() -> Self {
        Plan {
            made: AtomicU64::new(0),
            planned: AtomicU64::new(0),
            // Not `AtomicU64::new(0)`: an unset plan must read as `NaN`, and
            // zero bits are the float `0.0`, which would call everything
            // significant rather than nothing.
            z_alpha: AtomicU64::new(
                significant::bonferroni_z_limit(0, significant::FWER).to_bits(),
            ),
            by_caller: AtomicBool::new(false),
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Config {
            target_rel_error: 0.01,
            target_abs_error: Duration::ZERO,
            max_time: BENCH_TIME_MAX,
            plan: Default::default(),
        }
    }
}

impl Config {
    /// Ask for a standard error below `fraction` of the measurement
    /// (`0.001` = 0.1%).
    pub fn relative(fraction: f64) -> Self {
        Config::default().with_relative_error(fraction)
    }

    /// Ask for a standard error below `error` in absolute terms.
    ///
    /// This sets only the absolute goal, leaving the relative one at its
    /// default, so sampling stops at whichever of the two is reached first.
    pub fn absolute(error: Duration) -> Self {
        Config::default().with_absolute_error(error)
    }

    /// Set [`Config::target_rel_error`], keeping every other setting.
    ///
    /// `0.0` disables the relative goal, leaving `target_abs_error` alone in
    /// charge. These take `self` by value and hand it back, so they chain:
    /// `Config::default().with_relative_error(0.0).with_absolute_error(e)`.
    pub fn with_relative_error(mut self, fraction: f64) -> Self {
        self.target_rel_error = fraction;
        self
    }

    /// Set [`Config::target_abs_error`], keeping every other setting.
    ///
    /// `Duration::ZERO` disables the absolute goal.
    pub fn with_absolute_error(mut self, error: Duration) -> Self {
        self.target_abs_error = error;
        self
    }

    /// Set [`Config::max_time`], keeping every other setting.
    pub fn with_max_time(mut self, max_time: Duration) -> Self {
        self.max_time = max_time;
        self
    }

    /// Set the number of comparison benchmarks that will be taken.
    ///
    /// This is used to reduce the probability of false positives.  Otherwise
    /// when you are evaluating *many* benchmarks you'd be almost certain to
    /// see spurious "changes".
    ///
    /// This method can only be called once on any one `Config`.
    ///
    /// Planning *detaches* this `Config` from any clones it was sharing a
    /// plan with, so a `Config` kept as a template can be cloned and planned
    /// several different ways:
    ///
    /// ```
    /// let base = scaling::Config::relative(0.02);
    /// let two = base.clone().with_comparisons_planned(2);
    /// let three = base.clone().with_comparisons_planned(3);
    /// # std::mem::forget(base); std::mem::forget(two); std::mem::forget(three);
    /// ```
    ///
    /// Clones made *after* planning do share it, and between them must make
    /// exactly the number promised.
    pub fn with_comparisons_planned(mut self, comparisons: u64) -> Self {
        assert!(
            !self.plan.by_caller.load(Acquire),
            "only call with_comparisons_planned once!"
        );
        // Detach before recording. Without this, planning one clone of a
        // template would be visible to the next, and the assertion above
        // would fire on a caller who had done nothing wrong.
        self.plan = Default::default();
        self.plan.by_caller.store(true, Release);
        self.set_comparisons_planned(comparisons);
        self
    }

    /// What was promised via [`Config::with_comparisons_planned`].
    pub(crate) fn num_comparisons_planned(&self) -> u64 {
        self.plan.planned.load(Acquire)
    }

    /// The Bonferroni limit the plan implies; `NaN` when no plan is set.
    pub(crate) fn z_alpha(&self) -> f64 {
        f64::from_bits(self.plan.z_alpha.load(Acquire))
    }

    /// Record the plan through a shared reference.
    ///
    /// Separate from [`Config::with_comparisons_planned`] because a
    /// [`Suite`] records its plan *after* the caller's `Config` exists, and
    /// so cannot go through a builder that consumes `self`.
    ///
    /// `z_alpha` is stored **before** `planned`, and that order is what makes
    /// the pair safe to read without a lock. Every reader tests `planned`
    /// first and only consults `z_alpha` when it is non-zero
    /// ([`Config::comparison_accuracy_met`]), so an acquire-load that sees
    /// the new `planned` is guaranteed by the release-store to see the
    /// matching `z_alpha`. Written the other way round, a reader could see a
    /// plan with the `NaN` threshold that means "no plan", and would then
    /// find nothing significant however long it sampled.
    pub(crate) fn set_comparisons_planned(&self, comparisons: u64) {
        self.plan.z_alpha.store(
            significant::bonferroni_z_limit(comparisons, significant::FWER).to_bits(),
            Release,
        );
        self.plan.planned.store(comparisons, Release);
    }

    /// Add `comparisons` to the plan, and say what the total became.
    ///
    /// For [`Suite`], which counts its own comparisons rather than making
    /// the caller do it. Adding rather than setting is what lets a second
    /// suite built from the same `Config` account for itself: setting would
    /// leave the first suite's number in place and `Drop` would then
    /// complain about a count the caller never chose.
    ///
    /// The read-modify-write is a single `fetch_add`, so two suites run
    /// concurrently from one `Config` both count rather than one overwriting
    /// the other.
    pub(crate) fn add_comparisons_planned(&self, comparisons: u64) -> u64 {
        let total = self.plan.planned.fetch_add(comparisons, Release) + comparisons;
        // Same publish order as above, except that `planned` is already
        // visible - so a reader racing this sees either threshold, both of
        // which are real Bonferroni limits, rather than the `NaN` that means
        // "unplanned".
        self.plan.z_alpha.store(
            significant::bonferroni_z_limit(total, significant::FWER).to_bits(),
            Release,
        );
        total
    }

    /// Did the caller set this plan themselves, rather than a [`Suite`]?
    pub(crate) fn plan_set_by_caller(&self) -> bool {
        self.plan.by_caller.load(Acquire)
    }

    /// Claim `n` comparisons against the plan, and say how many had been
    /// claimed already.
    ///
    /// The prior count seeds each comparison's random stream, so consecutive
    /// comparisons do not choose the same order.
    pub(crate) fn claim_comparisons(&self, n: u64) -> u64 {
        self.plan.made.fetch_add(n, Release)
    }

    /// The smallest difference worth detecting, in nanoseconds, for a
    /// baseline of `baseline_ns`.
    ///
    /// The coarser of the two goals wins, matching how [`Config::accuracy_met`]
    /// stops at whichever is reached first.
    fn comparison_goal_ns(&self, baseline_ns: f64) -> f64 {
        (self.target_rel_error * baseline_ns).max(self.target_abs_error.as_secs_f64() * 1e9)
    }

    /// Is `std_error` small enough that a difference the size of the goal
    /// would be *detected*?
    ///
    /// This is the same predicate [`Comparison::is_changed`] applies, asked
    /// of a hypothetical difference rather than the observed one, so a
    /// comparison stops exactly when the test it is about to run would fire
    /// at the goal. Deliberately independent of the difference actually
    /// measured: stopping as soon as a result *became* significant would be
    /// optional stopping, and would put back the false positives the
    /// Bonferroni correction exists to remove.
    fn comparison_accuracy_met(&self, baseline_ns: f64, std_error: f64) -> bool {
        // Every sample agreed to the limit of the timer's resolution; no
        // further sampling can improve on that. Also keeps the zero-mean
        // case out of the `0 / 0` that would follow.
        if std_error == 0.0 {
            return true;
        }
        if self.num_comparisons_planned() == 0 {
            // `z_alpha` is `NaN`, so the real rule would never be satisfied
            // and every comparison would spend the whole budget. The results
            // are unusable regardless and `Drop` is about to name the number
            // that should have been planned - just don't take ten seconds
            // per comparison to get there.
            return self.accuracy_met(baseline_ns, std_error);
        }
        significant::is_significant(
            self.comparison_goal_ns(baseline_ns),
            std_error,
            self.z_alpha(),
        )
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
    /// [`Config::bench_gen_input`] for why batching does not bias this.
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
    ///
    /// Pins first. Every benchmark pins its own thread, but this gate is
    /// consulted *before* the first benchmark runs, so without pinning here
    /// the answer would be "not pinned" every time and these tests would
    /// skip themselves even under `quiet-bench run`.
    pub fn quiesced() -> bool {
        crate::quiet::pin_if_reserved();
        matches!(crate::quiet::status(), crate::quiet::Status::Pinned { .. })
    }

    /// A cost with a heavy right tail: nine calls in ten are trivial and the
    /// tenth is ten thousand times longer.
    ///
    /// This is the shape the selection effect feeds on. A handful of samples
    /// that happen to miss the tail have both a low mean and a small standard
    /// deviation - so the run stops, and stops low.
    pub fn bimodal_cost(seed: u64) -> impl FnMut() -> u64 {
        let mut rng = XorShift(seed | 1);
        move || {
            let n = if rng.next() % 10 == 0 { 10_000 } else { 1 };
            let mut acc = 0u64;
            for i in 0..n {
                acc = acc.wrapping_mul(31).wrapping_add(i as u64);
            }
            acc
        }
    }

    /// Near enough the same mean as [`bimodal_cost`], with no spread of its
    /// own at all - so whatever varies when this is measured is the machine.
    pub fn fixed_cost(seed: u64) -> impl FnMut() -> u64 {
        let mut rng = XorShift(seed | 1);
        move || {
            std::hint::black_box(rng.next());
            let mut acc = 0u64;
            for i in 0..1001 {
                acc = acc.wrapping_mul(31).wrapping_add(i as u64);
            }
            acc
        }
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
        println!("reverse: {}", bench_input(vec![0; 100], |xs| xs.reverse()));
        println!("sort:    {}", bench_input(vec![0; 100], |xs| xs.sort()));

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
            bench_input(0, |x| {
                *x = fib(500);
            })
        );
    }
}
