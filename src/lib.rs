#![doc = include_str!("../README.md")]
/*!

## Attribute inputs and setup

A flat benchmark may take no argument or one input. Use `input = value` when
each iteration should receive a clone of the same initial value, or
`make_input = || value` when each iteration needs a newly built value. The
function may take `&I`, `&mut I`, or `I` by value; by-value inputs are useful
when the benchmark consumes its input:

```rust
#[scaling::bench(input = vec![0_u8; 1024])]
fn count(bytes: &[u8]) -> usize { bytes.len() }

#[scaling::bench(make_input = || vec![0_u8; 1024])]
fn consume(bytes: Vec<u8>) -> usize { bytes.len() }
```

For comparisons, candidates use the group's shared `#[scaling::input]`
instead of `input =` or `make_input =` on the candidate. Ordinarily every
timed iteration is given an input of its own (a function that cannot change
its input may share a few instead: see [`bench`](macro@bench)). The shared
input is generated for each iteration, a batch of them to a round, and cloned
for each candidate, so every candidate in a round is measured on the same
values. That is why the input
type must be `Clone` when a group has more than one candidate, and the
generating and cloning are paid for out of [`max_time`](crate::Config::with_max_time), though they
are not timed. A setup-once candidate uses the shared input only
when its returned closure is first built; later inputs are still generated,
but that cached closure is called without them. Comparison candidates must
take `&I` or `&mut I`; owned inputs are currently supported only by
standalone benchmarks.

A function returning `impl Fn() -> O` or `impl FnMut() -> O` is a
setup-once benchmark: the function runs once, then the returned closure is
called for every timed iteration. This is useful when mutable state must
persist between calls, such as an advancing random-number generator. With
`input = value`, the value is passed to setup once, not cloned for every
call to the returned closure. This form cannot be combined with
`make_input =`, and a returned closure that itself takes an argument is not
supported.

For a scaling benchmark, `nmin` is required and `input =` is unavailable
because the input must vary with `n`. Use `make_input = |n| ...` to build
size-dependent input; it runs before timing, once per sample. A scaling
benchmark can also return `impl Fn() -> O` or `impl FnMut() -> O`: setup is
then cached separately for each size, and the returned closure is timed at
that size. It cannot be combined with `make_input =`.

An input registration can be expanded across types with `types(A, B, ...)`
or across sizes with `sizes(1, 2, ...)`. A type-expanded input is generic
and produces one registration per listed type. A size-expanded input takes
the size as its argument and produces one input per listed size. These
options are alternatives, not combinable. Candidates can likewise use
`types(A, B, ...)` to register a generic candidate once per listed input
type. For example, this registers two inputs for the same comparison:

```rust
#[scaling::input(group = "sorting", sizes(8, 32))]
fn values(size: usize) -> Vec<u8> { (0..size as u8).collect() }

#[scaling::bench(group = "sorting")]
fn sort(values: &mut Vec<u8>) { values.sort_unstable() }
```


## Reading the output

A flat benchmark prints as `name  value ± error`. The `±` is the standard
error of the value, in the same unit, and the value is printed to the
precision that error justifies. Two marks may follow:

* `(limit)`: the time budget ran out before the accuracy target was met, so
  the `±` is wider than you asked for ([`Timing::hit_limit`]).
* `(untrusted)`: too few samples were taken for the `±` itself to mean
  anything ([`Timing::untrustworthy`]).

Both can appear together as `(limit, untrusted)`.

A scaling benchmark prints `(constant ± error)ns/N (R²=…)`. `R²=0.000` with
`(limit)` means no power of `N` described the cost. A longer `max_time` will
not change that, since it is the model, not the sampling, that failed.

In a comparison, the baseline's row is an absolute time and every other
candidate's is its difference from the baseline: `-7.0% ± 0.4%` is seven
percent faster, give or take the error. `(< 1.0%)` means no difference was
found, and the figure is the smallest change this run could have detected at
its accuracy goal.

A group with several inputs prints as a grid, candidates down and inputs
across, with the baseline named in its title. The layout is chosen for you: a
group that would be wider than 100 columns is turned on its side, and if that
is still too wide it is printed with a line to each cell. A group whose inputs
differ in type is split into a table to each type when one table will not hold
it. Because `(limit)` and `(untrusted)` marks widen cells, two groups of the
same shape can lay out differently. Nothing about it is configurable yet; a
[`Group`] prints itself, so a caller can choose which groups to print and in
what order (see [`Report::groups`]).

A run prints nothing but how many benchmarks it is about to measure (on
stderr) until all of them have finished. The suite is measured interleaved, so
there are no partial results, and there is no way to filter a run: leave out
benchmarks you do not want measured by not registering them, which is what a
feature gate is for (see below). Results are not saved: [`Report`], [`Timing`]
and [`Difference`] deliberately have no serialization, because two runs a day
apart were not measured on the same machine. To compare with the past, measure
the past in the same run, as below.

## Keeping a `src/`-resident benchmark out of ordinary builds

Nothing here wraps the function in a `#[cfg]` for you: a `#[cfg]` written
above the attribute strips the whole item, macro included, before it ever
expands. A benchmark placed in `benches/` is already fine as it is -
`cargo` only builds that directory for `cargo bench` - but one placed in
`src/`, to sit next to the code it measures, compiles into *every* build by
default: an ordinary `cargo build`, and every crate depending on yours.
Gate it yourself, one of two ways.

**A feature of your own**, if you are also going to publish this crate and
want `cargo bench --features my-benchmarks` to keep working the ordinary
way. Make `scaling` an *optional* dependency tied to that feature, and add
it a second time, plainly, as a dev-dependency - so `benches/bench.rs`
itself always has it, feature or not:

```toml
[dependencies]
scaling = { version = "...", optional = true }
[dev-dependencies]
scaling = "..."
[features]
my-benchmarks = ["dep:scaling"]
```

then write every `src/`-resident benchmark under that same `#[cfg]`:

```
# fn fib(_: usize) -> usize { 0 }
#[cfg(feature = "my-benchmarks")]
#[scaling::bench]
fn fib_200() -> usize { fib(200) }
```

and run with `cargo bench --features my-benchmarks`. `scaling` ships no
feature of its own for this: a fixed name baked into the macro, say
`#[cfg(feature = "scaling-bench")]`, would be a convention nobody asked
for, and would compile silently to nothing for a crate that had never
defined that exact feature - worse than an explicit gate you chose
yourself. The cost of this route is a `#[cfg]` to remember on every
benchmark you write this way.

This route also buys something the other one cannot: benchmarks that
cross crate boundaries. `#[scaling::bench]` in `src/` becomes part of the
crate's own compiled output, the same as any other item behind a feature -
and registration is collected from *everything linked into one binary*,
regardless of which crate contributed it. So a family of related crates -
`rand_core`, `rand_chacha`, `rand_pcg`, and the like - can each register
their own benchmarks behind their own feature, and a single downstream
binary that depends on several of them with those features enabled gets
one combined, interleaved run spanning the whole family, with no crate
having to know about any of the others' benchmarks in advance. This is
exactly why every registration carries `crate_name`/`crate_version`: two
crates - or two versions of one - registering into the same binary is an
intended scenario, not an edge case, and a name they happen to share is
resolved by keeping every version, told apart by where it came from.
`#[cfg(test)]` code never leaves the crate that defines it, so the
dev-only route below cannot do this at all.

**Dev-only, no feature at all**, if you would rather not annotate every
benchmark individually. Put `scaling` in `[dev-dependencies]` only -
nothing under `[dependencies]` - and wrap the whole module in
`#[cfg(test)]` instead of one attribute per function:

```no_run
#[cfg(test)]
mod benches {
    #[scaling::bench]
    fn fib_200() -> usize { super::fib(200) }

    #[test]
    #[ignore] // a real run needs --release; plain `cargo test` should not pay for it
    fn run() {
        scaling::Config::default()
            .run_and_print()
            .expect("the benchmarks are registered consistently");
    }
}
```

and run with `cargo test --release -- --ignored run`. An ordinary `cargo
build` never sees `#[cfg(test)]` code at all, so it never touches
`scaling`, `inventory`, or anything either depends on - the same zero cost
to a normal build the feature route buys, bought instead by `#[cfg(test)]`,
something every Rust crate already uses, at the cost of the ordinary `cargo
bench` CLI: `#[test]` functions are not run by `cargo bench` on stable Rust,
so this route goes through `cargo test` instead (hence `--release`, since
`cargo test`'s own default profile is unoptimised) rather than through a
`main` of your own.

## Comparing against your own history

The same `group`/`baseline` machinery any other comparison in this crate
uses also answers "did this get slower since the last release" - and more
accurately than storing a number from a past run and diffing today's
against it later, because two runs a day apart do not share a machine
state: a warmer package, a different mix of interrupts, a CPU that has
since settled into a lower clock step, all shift a *stored* number without
shifting today's result to match. Comparing against your own history this
way never stores a number at all - it measures the *old* code itself,
fresh, in the very same interleaved round as the new code, so whatever the
machine happens to be doing shifts both equally and cancels out of the
difference, exactly as it does for any other pairing here.

Pin the old release as a dev-dependency, under a name of your own that is
not the crate's own:

```toml
[dev-dependencies]
my_crate_previous = { package = "my-crate", version = "=1.2.0" }
```

or, to compare against the tip of your default branch rather than a
tagged release, a git dependency naming no `branch`/`tag`/`rev` tracks
`origin`'s `HEAD`:

```toml
[dev-dependencies]
my_crate_previous = { package = "my-crate", git = "https://github.com/you/my-crate" }
```

or a particular commit, by `rev`:

```toml
[dev-dependencies]
my_crate_previous = { package = "my-crate", git = "https://github.com/you/my-crate", rev = "a1b2c3d" }
```

The `package = ".."` rename is what lets a crate depend on an older copy of
itself: the old copy gets a name that the crate's own code does not already
use.

Either way, what you get is the *actual function definitions* of that
version, not a cached timing - so write the comparison the ordinary way,
the released crate's public API on one side and your own on the other:

```ignore
#[scaling::bench(group = "sort", baseline)]
fn released(v: &mut Vec<u64>) { my_crate_previous::sort(v) }

#[scaling::bench(group = "sort")]
fn current(v: &mut Vec<u64>) { my_crate::sort(v) }
```

`baseline` names the released version, so the report reads the way a
regression check should: the code being written is measured *against* what
already shipped, not the other way around. There is one sharp edge: it stops
working if two different
versions of `scaling` itself ever end up anywhere in the dependency graph,
since registration is keyed on the literal monomorphized type `inventory`
collects, and two `scaling` versions split the registry silently rather
than erroring.

## Benchmarking algorithm

### Flat benchmarks: `#[bench]`

An *iteration* is a single execution of your code. A *sample* is a
measurement, during which your code may be run many times.

A sample is timed in *laps* of calls, a millisecond or so each: a warm-up lap
whose time is thrown away, then a short lap and a long one nine times as
long. The warm-up absorbs whatever the sample before it left behind - a cold
cache, a vector unit that had gone to sleep - so that what is measured is
your code running steadily, whatever ran before it. Reading the clock costs
something, and may set off something else; each lap carries it once, so the
long lap less the short one, over the difference in their calls, has none of
it. A first pass finds how many calls make a lap.

We then take one sample per round, and stop once the *standard error* of
their trimmed mean (a quarter cut from each end, which sheds the occasional
interruption) is small enough, and never on fewer than eight. This directly
answers "how precisely do I know `ns_per_iter`", which is what you actually
want.

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

[`Timing::std_error`] and [`Timing::rel_std_error`] give the error absolutely
and relatively, [`Timing::iterations`] and [`Timing::samples`] say how much
work it took, [`Timing::hit_limit`] tells you if the budget ran out before
the target accuracy was met, and [`Timing::untrustworthy`] tells you if too
few samples were collected for the error bar itself to mean anything.
Those are marked `(limit)` and `(untrusted)` in the output.

If a benchmark requires some state to run, one copy of the initial state is
prepared per iteration, unless the benchmark cannot change it and shares a few
instead (see [`bench`](macro@bench)).

### Scaling benchmarks: `#[bench_scaling]`

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
  for a flat one.

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
thing this does. For the same reason, a longer `max_time` does not rescue a
benchmark reported with `(limit)` and `R²=0.000`: the sampling was not what
failed.


## Caveats

### Caveat 1: Harness overhead

**TL;DR: Compile with `--release`; the overhead is likely to be within the
**noise of your
benchmark.**

Work which `scaling` does once-per-sample is kept negligible: the flat
benchmarks size each batch so that a sample takes far longer than the two
`Instant::now()` calls bracketing it, and the scaling benchmarks choose
sizes large enough that a single call dwarfs them. However, work which is
done once-per-iteration *will* be counted in the final times.

* For a benchmark taking no input this amounts to incrementing the loop
  counter and passing the return value through `std::hint::black_box`.
* For one taking an input, we also do a lookup into a big vector in order to
  get the input for that iteration.
* If you compile your program unoptimised, there may be additional overhead.
* If you install the counting [`Allocator`] to report allocations, it counts
  every allocation, including those made inside timed code, which costs a few
  instructions each. The baseline pays it too, so a comparison stays fair, but
  an allocation-heavy benchmark reads a little slower than it would without it.

The cost of the above operations depend on the details of your benchmark;
namely: (1) how large is the return value? and (2) does the benchmark evict
the input vector from the CPU cache? In practice, these criteria are only
satisfied by longer-running benchmarks, making these effects hard to measure.

### Caveat 2: Pure functions

**TL;DR: Return enough information to prevent the optimiser from eliminating
code from your benchmark.**

Benchmarking pure functions involves a nasty gotcha which users should be
aware of. Consider the following benchmarks:

```
# fn fib(_: usize) -> usize { 0 }
#[scaling::bench]
fn fib_1() -> usize { fib(500) }                      // fine

#[scaling::bench]
fn fib_2() { fib(500); }                              // spoiler: NOT fine

#[scaling::bench(make_input = || 0usize)]
fn fib_3(x: &mut usize) { *x = fib(500); }            // also fine, but ugly
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

This applies equally to [`bench_scaling`] and to a comparison's
alternatives, not just to a plain `#[bench]`: every timed call, whichever
of the three it is, is protected by [`std::hint::black_box`] the same way.
(A plain `#[bench]` runs as a comparison of one alternative, so those two
share their timing loop.) A scaling benchmark has one further wrinkle: the size `N` itself is also passed
through `black_box` before the call, not just the result afterward -
without that, the optimiser can see `N` as a literal within one round and
hoist the call out on that basis alone, the same elimination this caveat
is about, one step earlier.

### Caveat 3: A busy machine

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

#### CI

`quiet-bench reserve` wants root and exclusive cores, which an ordinary CI
runner - shared, often virtualized, rarely handing out either - usually
cannot give it. [`quiet::status`] still tells you outright rather than
letting a run silently assume it is quiesced: check it in CI the same way
you would locally, and expect [`quiet::Status::NotQuiesced`] there.

Be honest with yourself about what a tight error bar is worth on a busy
machine: the statistics can catch too *few* samples, a fact about the
budget they can see. They cannot catch a busy machine, which is exactly
the failure this caveat opened with - the whole run shifted together, so
the error bar stays tight and looks fully earned. There is no flag that
turns that into a caught case; the only fix is a quieter machine, or
judging results from CI with that firmly in mind rather than trusting
them the way a quiesced run's would be trusted.
[`#[scaling::metrics]`]: macro@metrics
*/

mod alloc;
mod assemble;
mod bench;
mod cpus;
mod difference;
mod estimate;
mod formatting;
mod input_group;
mod laps;
mod metrics;
mod names;
pub mod quiet;
/// Benchmarks registered from anywhere in a crate.
///
/// Public but hidden: the types here are named by generated registration
/// code rather than written by hand, and their shapes are not yet stable.
#[doc(hidden)]
pub mod registry;
mod run;
mod scaling;
mod suite;
pub(crate) use bench::{time_laps, time_loop};
pub(crate) use suite::Clock;
#[cfg(test)]
pub(crate) use suite::{block_on, Machine};
pub(crate) mod significant;

// Use `self::` because `scaling` is also the crate name; without it, doctests
// can end up with an ambiguous `scaling::` path when built against the crate
// itself.
pub use self::bench::Timing;

pub use self::alloc::{Allocations, Allocator};
pub use self::difference::Difference;
pub use self::input_group::Timings;
pub(crate) use self::metrics::MetricColumn;
pub use self::metrics::{MetricValue, Metrics};
pub use self::names::NameError;
pub use self::run::RegistrationError;
pub use self::scaling::{Scaling, ScalingStats};
pub use self::suite::{Group, Report};
pub(crate) use self::suite::{Measurement, TypedInput};

pub(crate) use self::input_group::InputGroup;
pub(crate) use self::suite::Suite;

/// Re-exported so that registration code written by a macro has a single
/// path to name, and callers need not depend on `inventory` themselves.
#[doc(hidden)]
pub use inventory;

// The attribute macros register a benchmark where it is written, rather than
// requiring it be added to a suite by hand. They are documented here, on the
// re-exports, because that is where rustdoc shows the documentation of a
// proc macro, and it lets the examples use this crate.

/// Registers a function as a benchmark, or as a candidate in a comparison.
///
/// ```
/// #[scaling::bench]
/// fn sum() -> u64 { (0..1000u64).sum() }
///
/// // One input, built fresh for each iteration.
/// #[scaling::bench(make_input = || vec![3, 1, 2])]
/// fn sort(v: &mut Vec<i32>) { v.sort() }
///
/// // A candidate in a comparison; see `input` for its input.
/// #[scaling::bench(group = "sorting", baseline)]
/// fn stable(v: &mut Vec<u64>) { v.sort() }
/// ```
///
/// A standalone benchmark is reported as `module::function`; a candidate by its
/// bare function name. The function takes no argument, or one input, and should
/// return something that depends on its work, which `scaling` passes through
/// [`std::hint::black_box`] so that the optimiser cannot delete the work (see
/// caveat 2 in the crate docs).
///
/// # Options
///
/// | option | meaning |
/// |---|---|
/// | `name = "text"` | What the report calls it, in place of the function's name. |
/// | `input = <expr>` | Each iteration is given a clone of this value, so its type must be `Clone`. Not with `make_input`, and not with `group`. |
/// | `make_input = <closure>` | Each iteration is given a value this builds. Not with `input`, and not with `group`. |
/// | `group = "name"` or `group("a", "b")` | Makes it one candidate of a comparison, in one group or several at once. It is measured on every [`input`](macro@input) of its group that has its input type. |
/// | `baseline` | Needs `group`. The candidate the others are reported against; with none marked, the first by name is used. |
/// | `uninteresting` | Needs `group`. Nobody asked whether this candidate differs from the baseline, only roughly by how much. It is shown as a size with no verdict, measured only to [`Config::with_rough_error`], and left out of the multiple-comparison count, so the comparisons that are of interest are judged less strictly for it. Not the baseline. |
/// | `types(A, B)` | Needs `group`. A candidate generic in its input is registered once for each listed type. |
///
/// The input can be taken as `&I` or `&mut I`, or by value (`I`) in a
/// standalone benchmark, where the function consumes it.
///
/// # Reusing the input
///
/// Every call is ordinarily handed an input of its own, made and cloned
/// before the timing starts. For a quick function on an input that is slow
/// to make, that is most of the work and most of the memory: a lap of a
/// millisecond may need hundreds of thousands of inputs held at once. So a
/// benchmark that takes `&I`, which cannot change its input, is given a small
/// pool of inputs and its calls go round the pool. So is every candidate of
/// an input declared `reuse_input` (see [`input`](macro@input)), which lets
/// candidates that take `&mut I` join in by promising to leave the input as
/// they found it: a benchmark that reverses a vector twice, or inserts a key
/// and removes it again. Within a comparison it is all or none: a candidate
/// timed on a pool is timed on warm inputs, and compared with one timed on
/// new ones it would measure that difference, not the functions'. So a group
/// in which any candidate takes `&mut I` from an input not declared
/// `reuse_input` is timed on new inputs throughout.
///
/// What is measured then is a function on inputs that stay in cache, and a
/// pool of a few thousand inputs, whose pattern a processor can start to
/// learn. For a function whose speed depends on the input being new that is
/// not what you want to know, and taking `&mut I` from an input that is not
/// declared `reuse_input` measures it the other way.
///
/// A function returning `impl Fn() -> O` or `impl FnMut() -> O` is a
/// setup-once benchmark: the function runs once and the closure it returns is
/// what is timed.
///
/// The output of a candidate can also be given to a [`metrics`](macro@metrics)
/// function, to report more than a time.
pub use scaling_macros::bench;

/// Registers a function as a scaling benchmark, measured at several sizes to
/// find how its cost grows as a power of `N`.
///
/// ```
/// #[scaling::bench_scaling(nmin = 0)]
/// fn sum_to(n: usize) -> u64 { (0..n as u64).sum() }
/// ```
///
/// The function takes the size `n`, or, with `make_input`, an input that was
/// built for that size.
///
/// | option | meaning |
/// |---|---|
/// | `nmin = N` | Required. The size to start climbing from. |
/// | `make_input = \|n\| ...` | Builds an input for size `n`, before the timing and once for each sample. The function then takes `&mut I`. |
/// | `name = "text"` | What the report calls it, in place of the function's name. |
///
/// `input` and `group` do not apply: the input has to vary with `n`, and what
/// a scaling benchmark measures is not something a comparison compares. A
/// function returning `impl Fn() -> O` is set up once for each size.
pub use scaling_macros::bench_scaling;

/// Registers an input, shared by every candidate of its group that takes its
/// type.
///
/// ```
/// #[scaling::input(group = "sorting", name = "reversed")]
/// fn reversed() -> Vec<u64> { (0..400u64).rev().collect() }
///
/// // One input for each size, named `ramp@64` and `ramp@256`.
/// #[scaling::input(group = "sorting", sizes(64, 256))]
/// fn ramp(n: usize) -> Vec<u64> { (0..n as u64).collect() }
///
/// #[scaling::bench(group = "sorting", baseline)]
/// fn stable(v: &mut Vec<u64>) { v.sort() }
/// ```
///
/// Candidates and inputs name the group and a type, never each other: every
/// candidate is measured on every input of its group and type, so adding an
/// input extends the comparison without touching the candidates.
///
/// | option | meaning |
/// |---|---|
/// | `group = "name"` or `group("a", "b")` | Required. The group or groups it feeds, without those groups being compared with each other. |
/// | `name = "text"` | What the input is called in the report, in place of the function's name. |
/// | `types(A, B)` | A function generic in the type it makes is registered once for each listed type. Not with `sizes`. |
/// | `sizes(1, 2)` | The function takes the size and is registered once for each, named with `@size` and shown in order of size. Not with `types`. |
/// | `reuse_input` | Every candidate measured on this input is given one input for many calls, not a new one for each. A candidate that takes `&I` cannot tell; one that takes `&mut I` promises to put the input back as it found it. If it does not, the calls after the first are not measured on the input they were meant to be, and neither are the other candidates, which see the same inputs. See [`bench`](macro@bench). |
///
/// The function takes no argument (or the size) and returns the input, whose
/// type must be `Clone`: it is generated for each iteration and cloned for each
/// candidate. A function returning `impl Fn() -> T` is set up once.
pub use scaling_macros::input;

/// Computes extra numbers from what the candidates of a group returned, so
/// that they are shown beside the times: how many bytes a serializer wrote, how
/// well its output compresses, how much memory it held.
///
/// ```
/// use scaling::Metrics;
///
/// #[scaling::input(group = "encode")]
/// fn text() -> String { "abcd".repeat(100) }
///
/// #[scaling::bench(group = "encode", baseline)]
/// fn plain(s: &mut String) -> Vec<u8> { s.clone().into_bytes() }
///
/// #[scaling::bench(group = "encode")]
/// fn doubled(s: &mut String) -> Vec<u8> {
///     let mut bytes = s.clone().into_bytes();
///     bytes.extend_from_slice(s.as_bytes());
///     bytes
/// }
///
/// // Applies to `plain` and `doubled`, and to any candidate of "encode"
/// // that returns a `Vec<u8>`, now or added later.
/// #[scaling::metrics(group = "encode")]
/// fn sizes(out: Vec<u8>) -> Metrics {
///     Metrics::new().bytes("size", out.len())
/// }
/// ```
///
/// which prints the size beside each time:
///
/// ```none
/// encode@text (String)  baseline: plain
/// candidate               time          size
/// plain       51.50ns ± 0.03ns          400B
/// doubled       +205.6% ± 0.4%  800B (+100%)
/// ```
///
/// # Options
///
/// | option | meaning |
/// |---|---|
/// | `group = "name"` or `group("a", "b")` | Required. The group or groups whose candidates it computes metrics for. |
/// | `allocation` | Counts the allocations of each candidate's run, which the function can then ask to show. See [Counting allocations](#counting-allocations). |
/// | `name = "text"` | What the function is called in diagnostics, in place of its own name. |
///
/// # The function
///
/// It takes the output by value, so it needs no `Clone` and may reuse or check
/// it, and returns a [`Metrics`]. There are two forms:
///
/// | signature | gets |
/// |---|---|
/// | `fn(out: O) -> Metrics` | what the candidate returned |
/// | `fn(input: &I, out: O) -> Metrics` | also the input, as it was before the candidate ran, which the candidate was free to change |
///
/// The second form needs an input that can be cloned, which a group's inputs
/// always are. `O` must be a type that can be named in a registration: not a
/// reference, nor anything with a lifetime, `impl Trait` or `dyn Trait` in it,
/// nor spelled with a generic parameter of the candidate. A candidate whose
/// output cannot be named is measured as usual and has no metrics. `()` is
/// fine, and with `allocation` is how a candidate that returns nothing gets its
/// counts.
///
/// # Which candidates get it
///
/// A function applies to every candidate in one of its groups that returns `O`
/// (and, for the second form, that is measured on an input of type `I`).
/// Candidates and functions name the group and a type and never each other, so
/// a candidate added later is picked up.
///
/// Each candidate has at most one: a second function for the same group and
/// type is reported as a contradiction before anything runs, and a function
/// that no candidate in any of its groups returns the type of is reported as a
/// warning, since it computed nothing. A function that several versions of one
/// crate have registered counts once, as the newest. To compute several numbers
/// about one type, return them all from one function.
///
/// # When it runs
///
/// Once for each cell, after its timing is finished, so it cannot disturb the
/// timing. One value is made by the group's input generator and cloned for each
/// candidate, and each candidate is called once on its copy. That run is not
/// timed and does not count against [`max_time`](crate::Config::with_max_time); it costs one call of
/// the candidate and one of your function, for each candidate and input. There
/// is no way to skip it yet.
///
/// Because it is one run, a metric is a single value, not a mean with an error
/// bar: it suits quantities that do not vary from run to run, like a size. If
/// the input is random, seed the generator, or the number will differ from one
/// run of the benchmark to the next.
///
/// # Counting allocations
///
/// How much memory a candidate used is not in what it returned. Install the
/// counting [`Allocator`], mark the function `allocation`, and ask for the
/// numbers to show:
///
/// ```
/// #[global_allocator]
/// static ALLOC: scaling::Allocator = scaling::Allocator::new();
///
/// #[scaling::bench(group = "build")]
/// fn grow() -> Vec<u8> { vec![0u8; 10_000] }
///
/// #[scaling::metrics(group = "build", allocation)]
/// fn memory(out: Vec<u8>) -> scaling::Metrics {
///     scaling::Metrics::new()
///         .bytes("size", out.len())
///         .peak_allocated_bytes()  // the most it held at once: `alloc peak`
///         .allocation_count()      // how many times it asked for memory: `alloc count`
///         .total_allocated_bytes() // how much it asked for in all: `alloc total`
///         .net_allocated_bytes()   // held at the end, net of what it freed: `alloc net`
/// }
/// ```
///
/// Only the candidate's own call, on its own thread, is counted: not its input,
/// which it was handed, and not what your function does with the output. A
/// function can also read the numbers, to build a metric of its own, with
/// [`Metrics::allocations`]. See [`Allocator`] for what is and is not seen, and
/// what installing it costs: it counts whenever it is installed, not only on
/// this run, so it slows allocation-heavy code a little, the baseline included.
///
/// A program that asks for counts without installing the allocator is refused
/// before anything runs, rather than shown zeros, and a function that asks for
/// them without `allocation` fails with a message saying so.
///
/// # How the numbers are shown
///
/// As columns beside the time, named as you named them. The baseline's value is
/// absolute and every other candidate's is `value (Δ%)` against it; there is no
/// `±`, since a metric is not sampled, and no notion of which direction is
/// better, so nothing is marked best. With several inputs and one or two
/// metrics the metrics are lines under each time in one grid; otherwise each
/// input gets a table. See "Reading the output" in the crate docs for how a
/// group that is too wide is laid out.
///
/// A timing's error says how many of its digits mean anything; nothing says so
/// for a metric, so each is shown to three significant figures. A precision on
/// the thing being printed changes that for all of them: `println!("{report:.5}")`
/// shows every metric to five, and so does `{group:.5}` for one [`Group`].
/// Timings are not affected.
///
/// # Reading them back
///
/// A script that measured with [`Config::run`] asks the [`Report`] for a
/// candidate's record by its name: `report.metrics("encode:json@text")?` is a
/// [`Metrics`], whose [`get`](Metrics::get) gives a value by the name you put it
/// under, and a shorter name such as `"json"` does as well when only one
/// candidate has it. A candidate with no metrics has an empty record. See
/// [Names](Report#names).
///
/// To have all of a group's candidates on one input together, ask for the
/// [`Report::comparison`]: [`Timings::metrics`] gives the records in the order
/// of [`Timings::names`].
pub use scaling_macros::metrics;

#[cfg(test)]
use std::sync::atomic::AtomicU64;
#[cfg(test)]
use std::sync::atomic::Ordering::Relaxed;
use std::time::*;

/// Spend at least this long *running a scaling benchmark* before believing
/// any accuracy target.
///
/// Measured time, not wall-clock time: an input that is slow to build would
/// otherwise satisfy the floor by being built, and construction is not
/// evidence about the function. [`max_time`](crate::Config::with_max_time) is the opposite - a
/// wall-clock cap, because that is a promise about how long the caller waits -
/// so the two clocks are deliberately different.
///
/// A time floor is scale-free where a sample-count floor is not: it costs a
/// slow function nothing while a fast one still gets enough time to reduce
/// variance before the target is treated as met. (Flat benchmarks and
/// comparisons have a floor in rounds instead: see
/// [`MIN_SAMPLES`](crate::input_group::MIN_SAMPLES).)
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
/// [`target_rel_error`](crate::Config::with_relative_error).
const MIN_SAMPLE_TIME: Duration = Duration::from_millis(3);

/// Roughly the longest a single benchmark should take.
///
/// A backstop rather than a target: both kinds of benchmark stop as soon as
/// they have the accuracy asked for, and neither sizes any of its work
/// against the time available.
const MAX_BENCH_TIME: Duration = Duration::from_secs(10);
/// How hard a benchmark works to pin down `ns_per_iter`, and when it gives
/// up.
///
/// A benchmark uses [`Config::default`] unless a caller supplies a different
/// `Config`; the methods below build one by hand.
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
/// # let _ = (tight, quick);
/// ```
#[derive(Debug, Clone)]
pub struct Config {
    // Set by `with_relative_error`, `with_absolute_error` and `with_max_time`,
    // which document them.
    pub(crate) target_rel_error: f64,
    pub(crate) target_abs_error: Duration,
    pub(crate) target_rough_error: f64,
    pub(crate) max_time: Duration,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            target_rel_error: 0.01,
            target_abs_error: Duration::ZERO,
            target_rough_error: 0.1,
            max_time: MAX_BENCH_TIME,
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

    /// Stop once the standard error falls below `fraction` of the measurement
    /// (`0.01` = 1%), keeping every other setting.
    ///
    /// A comparison reads this as a *sensitivity* rather than a precision: the
    /// smallest difference worth detecting, as a fraction of the baseline. See
    /// [`Difference::min_detectable_difference`], which is what that floor came to
    /// on a result that reported no change.
    ///
    /// The default is `0.01`, 1%.
    ///
    /// `0.0` disables the relative goal, leaving the absolute one
    /// ([`with_absolute_error`](Config::with_absolute_error)) alone in charge.
    /// These take `self` by value and hand it back, so they chain:
    /// `Config::default().with_relative_error(0.0).with_absolute_error(e)`.
    pub fn with_relative_error(mut self, fraction: f64) -> Self {
        self.target_rel_error = fraction;
        self
    }

    /// Stop once the standard error falls below `error`, keeping every other
    /// setting.
    ///
    /// Sampling stops as soon as *either* goal is met, so whichever is coarser
    /// for the function at hand is the one that ends up governing. That is the
    /// point of having both: a 1% relative goal on a 1 ns function asks for a
    /// precision finer than the clock can resolve, and would otherwise spend
    /// the whole budget failing to reach it. An absolute floor puts a bound on
    /// how much precision is worth chasing.
    ///
    /// The default is `Duration::ZERO`, which disables it, leaving the relative
    /// goal ([`with_relative_error`](Config::with_relative_error)) alone in
    /// charge.
    ///
    /// As with the relative goal, a comparison reads this as the smallest
    /// difference worth detecting rather than as a precision.
    pub fn with_absolute_error(mut self, error: Duration) -> Self {
        self.target_abs_error = error;
        self
    }

    /// How well to measure a candidate marked `uninteresting`: stop once the
    /// ratio of its time to the baseline's is known to within `fraction`
    /// (`0.1` = 10%), one standard error either way.
    ///
    /// Such a candidate is not tested for a change, and so is not part of the
    /// family the multiple-comparison correction counts; all it is asked for
    /// is how big the difference is, roughly. A looser goal costs less, and
    /// a measurement is only as quick as its strictest comparison.
    ///
    /// The default is `0.1`, 10%.
    ///
    /// `fraction` must be above zero. Unlike the relative goal, this one has
    /// no `0.0` that switches it off: `0.0` asks for a ratio known exactly,
    /// which is never met, so a comparison with an `uninteresting` candidate
    /// then runs until [`max_time`](Config::with_max_time) and is marked
    /// `(limit)`.
    pub fn with_rough_error(mut self, fraction: f64) -> Self {
        self.target_rough_error = fraction;
        self
    }

    /// Give up after roughly `max_time` of wall-clock time even if neither
    /// accuracy goal was reached, setting [`Timing::hit_limit`], keeping every
    /// other setting.
    ///
    /// Wall clock rather than measured time, because this is a promise about
    /// how long the caller waits - a benchmark whose input is slow to build has
    /// still taken that long. A comparison allows this much per alternative,
    /// since each produces its own [`Timing`] and would otherwise get a
    /// fraction of the budget one benchmark gets for the same target.
    ///
    /// So building inputs counts against it. That means every call of
    /// `make_input` or of an `#[input]` function, every clone handed to a
    /// comparison's candidates, and dropping them afterwards, though none of it
    /// counts towards the measurement. Two things do not count. One is work
    /// done once before the benchmark starts, such as evaluating
    /// `input = <value>` (each clone of that value still does). The other, in a
    /// suite, is the time other benchmarks spend on their turns.
    ///
    /// The consequence is that an expensive input eats into the budget. The run
    /// still ends on time, but with fewer samples, so a wider `±` and likely
    /// [`Timing::hit_limit`]. When an input costs much more to build than the
    /// function costs to run, raise `max_time` to match. The deadline is
    /// checked between samples, so a run can overshoot it by one sample's worth
    /// of input building and calls.
    ///
    /// The default is 10 seconds.
    pub fn with_max_time(mut self, max_time: Duration) -> Self {
        self.max_time = max_time;
        self
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
    /// This is the same predicate [`Timing::is_changed`] applies, asked
    /// of a hypothetical difference rather than the observed one, so a
    /// comparison stops exactly when the test it is about to run would fire
    /// at the goal. Deliberately independent of the difference actually
    /// measured: stopping as soon as a result *became* significant would be
    /// optional stopping, and would put back the false positives the
    /// Bonferroni correction exists to remove.
    /// `z_alpha` is passed in rather than read from a field: it belongs to
    /// the *family* of comparisons being run, which is a property of the call
    /// that started them and not of the `Config`. See `Config::z_alpha_for`.
    fn comparison_accuracy_met(&self, baseline_ns: f64, std_error: f64, z_alpha: f64) -> bool {
        // Every sample agreed to the limit of the timer's resolution; no
        // further sampling can improve on that. Also keeps the zero-mean
        // case out of the `0 / 0` that would follow.
        if std_error == 0.0 {
            return true;
        }
        significant::is_significant(self.comparison_goal_ns(baseline_ns), std_error, z_alpha)
    }

    /// The Bonferroni limit for a family of `comparisons` comparisons.
    ///
    /// Each entry point works this out for the family it can see:
    /// `InputGroup::run` for its own `k - 1`, and a [`Suite`] for its
    /// total, which it knows once its last entry is added and before it runs
    /// anything. Nothing is promised in advance, so there is nothing to
    /// verify afterwards.
    ///
    /// # Only a suite sees a whole family
    ///
    /// A caller who runs several standalone comparisons and reads them
    /// together has a family larger than any one call knows about, and none
    /// of them corrects for it - so the chance of some false positive among
    /// them grows with how many were run. `Config` used to carry a promised
    /// count and a `Drop` that checked it, which made the caller declare that
    /// total; removing that machinery removed the guarantee with it. A
    /// [`Suite`] is the path that still has it, by collecting everything
    /// before measuring anything.
    #[cfg(test)]
    pub(crate) fn z_alpha_for(comparisons: u64) -> f64 {
        significant::bonferroni_z_limit(comparisons, significant::FWER)
    }

    /// A seed distinguishing one standalone comparison's random stream from
    /// the next one's.
    ///
    /// Only the *order* alternatives are timed in depends on this, never a
    /// reported number, so this is entropy rather than state: it exists so
    /// that two comparisons run back to back do not draw the same sequence of
    /// orders and correlate with each other.
    ///
    /// A process-global counter rather than a field, because it replaced one
    /// on `Config` that existed for the plan and had to go with it.
    /// Consecutive comparisons differing matters more here than a
    /// comparison's order being reproducible across runs - a [`Suite`] seeds
    /// its entries from their position instead, and so stays reproducible.
    #[cfg(test)]
    pub(crate) fn next_comparison_seed() -> u64 {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        NEXT.fetch_add(1, Relaxed)
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
        std_error < self.comparison_goal_ns(ns_per_iter)
    }
}

/// Pick a human-readable unit from a magnitude in nanoseconds, returning
/// the divisor and its suffix.
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

/// How many decimal places `error` needs to show one significant digit.
fn error_decimals(error: f64) -> usize {
    if !error.is_finite() || error <= 0.0 {
        return 4;
    }
    // The 0.5 below causes us to print `1.1` or `0.6` instead of `1`.
    (-(0.5 * error).log10().floor() as i64).clamp(0, 9) as usize
}

/// A value and its error, formatted to the precision the error justifies, with extra digits as
/// requested by the formatter.
fn value_and_error(value: f64, error: f64, precision: Option<usize>) -> (String, String) {
    let decimals = error_decimals(error) + precision.unwrap_or(0);
    let error_str = if error > 0.0 && error < 1e-4 {
        format!("{error:.1e}")
    } else {
        format!("{error:.decimals$}")
    };
    (format!("{value:.decimals$}"), error_str)
}

/// Running mean and variance of the per-iteration times.
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

    /// Mean, and the standard error *of that mean*, in nanoseconds.
    ///
    /// Batching does not bias this. Because each sample already averages
    /// `unit` iterations, `sd(x) = sigma_iter / sqrt(unit)`, so the standard
    /// error of the mean over `k` samples equals `sigma_iter / sqrt(k * unit)`:
    /// the standard error over all `k * unit` raw iterations. The stopping
    /// rule is therefore correct regardless of what `unit` calibration picked,
    /// and needs no assumption about the shape of the noise: a
    /// randomized-input benchmark has `var(batch) ∝ unit` while a
    /// deterministic one has roughly constant per-sample jitter, and this
    /// estimator is right for both.
    ///
    /// The error is absolute rather than relative because that is the
    /// primitive quantity: it needs nothing but the samples, whereas
    /// dividing by the mean is undefined when the mean is zero.
    /// [`Timing::rel_std_error`] is derived from it for reporting.
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

    pub fn mean_and_spread(xs: &[f64]) -> (f64, f64) {
        let n = xs.len() as f64;
        let mean = xs.iter().sum::<f64>() / n;
        let sd = (xs.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / n).sqrt();
        (mean, sd / mean)
    }
}
