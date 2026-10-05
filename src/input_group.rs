//! Measuring one or more alternatives over a shared input: [`InputGroup`].
//!
//! The reason to time all alternatives together rather than in pairs is the
//! same reason a comparison's alternatives beat two separate benchmarks run one
//! after the other.
//! Whatever the machine does slowly - a clock drifting, a package warming -
//! lands on every alternative within the same round and cancels out of the
//! differences between them. Measured one after another instead, each would
//! sample a different stretch of that drift, and the differences would carry
//! it.

use super::*;
use std::fmt::{self, Display, Formatter};
use std::time::{Duration, Instant};

/// Never stop *voluntarily* on fewer rounds than this.
///
/// Each round gives one value per alternative, so this is the fewest values
/// a standard error is ever judged from. The bar is itself an estimate, and
/// from very few rounds it comes out small by luck often enough that a rule
/// which keeps looking would stop on exactly those runs. Eight is where the
/// lab found that stopped happening, and Student's t, which the comparisons
/// use, charges for whatever uncertainty remains.
///
/// Note the emphasis: this is a floor on *concluding we are done*, not on
/// reporting. If [`max_time`](crate::Config::with_max_time) runs out first,
/// nothing has been selected for, and the error bar from the rounds we did
/// manage is reported (with [`Timing::hit_limit`] set).
pub(crate) const MIN_SAMPLES: usize = 8;

/// How much the round count grows between looks at the error bars.
///
/// Looking after every round would give a noisy bar more chances to dip
/// below the goal by luck; growing geometrically costs at most this much
/// overshoot.
const CHECK_GROWTH: f64 = 1.3;

/// The fraction trimmed from each end of the per-round values.
///
/// Something occasionally interrupts a sample - a scheduler tick, another
/// process, the thread moving core - and it always makes the sample slower.
/// A trimmed mean ignores those few without having to recognise them.
const TRIM: f64 = 0.25;

/// The base length of one lap, in nanoseconds.
///
/// Every sample is timed as laps: a warm-up lap that is thrown away, then a
/// short lap and a long one. The warm-up lap absorbs whatever the previous
/// sample (or anything else) left behind - a cold cache, a sleeping vector
/// unit, a clock that has not yet caught up - so the measured laps describe
/// the function running steadily, whatever ran before it. A millisecond was
/// enough, on the lab's machine, to make every function's result
/// independent of its neighbours.
///
/// `SCALING_LAP_UNIT_US` overrides it, for experiments with this prototype.
fn lap_unit_ns() -> f64 {
    static UNIT: std::sync::OnceLock<f64> = std::sync::OnceLock::new();
    *UNIT.get_or_init(|| {
        std::env::var("SCALING_LAP_UNIT_US")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .filter(|&us| us > 0.0)
            .map_or(1e6, |us| us * 1e3)
    })
}

/// A sample's laps, in units: warm-up, short, long.
///
/// The estimate is the long lap less the short one, divided by the
/// difference in their calls. Reading the clock between laps costs something
/// fixed, and may set off something else (waking a power-gated unit, say);
/// whatever it is, each lap carries it once, so the subtraction removes it
/// exactly. The long lap is nine times the short one because the subtraction
/// costs precision: against simply adding the laps, the variance grows by
/// (K+1)²/(K-1)², which is 9 at K=2 and 1.56 at K=9.
const SHAPE: [usize; 3] = [1, 1, 9];

/// A function whose single call lasts this many base units needs no warm-up
/// call.
///
/// The warm-up lap exists to absorb what lingers from whatever ran before:
/// something that settles within about a base unit. Against a call this long
/// that is under one part in a hundred, so the warm-up would only double the
/// cost of a function that is already slow.
const NO_WARMUP_UNITS: f64 = 100.0;

/// The most a sample's inputs may cost to prepare, in nanoseconds.
///
/// Preparing inputs is untimed, but not free: it comes out of the budget, and
/// every input prepared is held until the sample has run. A function that is
/// quick to call on an input that is slow to make - a cheap query on a
/// freshly built vector - would otherwise need a million inputs to fill one
/// lap. Bounding what a sample may spend making them bounds both the time
/// and, without having to see inside the inputs, the memory; the lap is
/// then as long as that allows, which for such a function is shorter than
/// the base unit.
const MAX_PREPARATION_NS: f64 = 5e6;

/// The most inputs in the pool an alternative that reuses its input goes
/// round.
///
/// Enough that the inputs a randomised benchmark draws are a fair sample of
/// what it would see, and that a long enough run of them is not one the
/// machine can learn the answers to.
const MAX_POOL_INPUTS: usize = 1 << 16;

/// The most memory such a pool may take: a few megabytes, whatever the
/// inputs are.
const MAX_POOL_BYTES: usize = 8 * 1024 * 1024;

/// How many inputs of `input_bytes` each such a pool may hold: the most that
/// [`MAX_POOL_INPUTS`] and [`MAX_POOL_BYTES`] allow, and always at least one.
fn pool_limit(input_bytes: usize) -> usize {
    match input_bytes {
        0 => MAX_POOL_INPUTS,
        bytes => MAX_POOL_INPUTS.min(MAX_POOL_BYTES / bytes),
    }
    .max(1)
}

/// The most such a pool may cost to make, in nanoseconds. It is made afresh
/// every round, and is meant to be a small part of one.
const MAX_POOL_PREPARATION_NS: f64 = 1e6;

/// Laps at least this many base units long need no subtraction.
///
/// A group lap this long exists only because some member's single call is
/// that long. Whatever reading the clock costs is then a negligible part of
/// a lap, so the group uses one warm-up lap and one measured lap, which saves
/// it nine calls of its slowest member every round.
const LONG_LAP_UNITS: f64 = 10.0;

/// A backstop on the round count, so the vector of samples cannot grow
/// without bound.
///
/// This is about memory, not about the measurement: `max_time` is the real
/// budget, and a round of a few milliseconds at the very least allows fewer
/// than a hundred thousand rounds in the default one.
const MAX_SAMPLES: usize = 1_000_000;

/// A generator of inputs, type-erased so that all the alternatives can share
/// one.
type GenInput<I> = dyn FnMut() -> I + 'static;

/// One alternative, type-erased so that alternatives may differ in what they
/// return.
///
/// The erasure is at the level of a whole batch rather than a single call:
/// what is behind the pointer is [`time_loop`] with its `F` already chosen,
/// so the loop it runs is a direct call, and the indirection is paid once
/// per batch rather than once per iteration. On a 9 ns function the
/// per-iteration form costs 14%; this costs nothing measurable.
///
/// [`Alternative::batch`] takes a batch of inputs already prepared rather
/// than making its own, so that every alternative in a round is handed the
/// *same* inputs. That is what makes the per-round differences genuinely
/// paired: if each drew its own inputs and the cost varied with the input,
/// the difference between two alternatives would carry the difference between
/// two draws as well, and no amount of averaging distinguishes the two.
trait Alternative<I> {
    /// Time `n` calls over `xs`, returning the total in nanoseconds. With
    /// fewer than `n` inputs the calls go round them again.
    fn batch(&mut self, xs: &mut [I], n: usize) -> f64;

    /// Time consecutive laps of `laps[j]` iterations over `xs`, returning
    /// each lap's time in nanoseconds. See [`time_laps`].
    fn laps(&mut self, xs: &mut [I], laps: [usize; 3]) -> [f64; 3];

    /// Whether [`Alternative::measure`] has anything to compute.
    fn has_metrics(&self) -> bool {
        false
    }

    /// Have [`Alternative::measure`] count the allocations of its run, so
    /// that metrics can ask for them. A no-op for an alternative with no
    /// metrics, which has no use for the count.
    fn count_allocations(&mut self) {}

    /// Run once on `input`, outside the timing, and compute the metrics from
    /// what it returned. `pristine` is the input as it was before the run,
    /// when there is one to give.
    fn measure(&mut self, _input: &mut I, _pristine: Option<&I>) -> Metrics {
        Metrics::new()
    }
}

/// An alternative that is only timed.
struct Timed<F>(F);

impl<I, F, O> Alternative<I> for Timed<F>
where
    F: FnMut(&mut I) -> O,
{
    fn batch(&mut self, xs: &mut [I], n: usize) -> f64 {
        time_loop(&mut self.0, xs, n)
    }

    fn laps(&mut self, xs: &mut [I], laps: [usize; 3]) -> [f64; 3] {
        time_laps(&mut self.0, xs, laps)
    }
}

/// An alternative that is timed, and then run once more for its metrics.
///
/// The function and the metrics live together because the function is
/// needed twice and cannot be shared between two closures: sharing it
/// through a cell would put a borrow check inside the timing loop.
struct Measured<F, M, O> {
    f: F,
    metrics: M,
    /// Whether the run is counted for its allocations.
    count: bool,
    _output: std::marker::PhantomData<fn() -> O>,
}

impl<I, F, M, O> Alternative<I> for Measured<F, M, O>
where
    F: FnMut(&mut I) -> O,
    M: FnMut(Option<&I>, O) -> Metrics,
{
    fn batch(&mut self, xs: &mut [I], n: usize) -> f64 {
        time_loop(&mut self.f, xs, n)
    }

    fn laps(&mut self, xs: &mut [I], laps: [usize; 3]) -> [f64; 3] {
        time_laps(&mut self.f, xs, laps)
    }

    fn has_metrics(&self) -> bool {
        true
    }

    fn count_allocations(&mut self) {
        self.count = true;
    }

    fn measure(&mut self, input: &mut I, pristine: Option<&I>) -> Metrics {
        // Only the alternative's own call is counted, not the metrics
        // function that follows: what it allocates to inspect the output is
        // not the alternative's doing.
        let (output, counted) = if self.count {
            let (output, stats) = crate::alloc::measure(|| (self.f)(input));
            (output, Some(stats))
        } else {
            ((self.f)(input), None)
        };
        // For the function to read, if it wants to build a metric from them.
        let provided = crate::alloc::provide(counted);
        let mut metrics = (self.metrics)(pristine, output);
        drop(provided);
        metrics.resolve_allocation(counted);
        metrics
    }
}

/// Benchmarks sharing an input, gathered before any of them runs.
#[expect(clippy::type_complexity)]
pub struct InputGroup<I> {
    cfg: Config,
    make_input: Box<GenInput<I>>,
    clone_input: Option<Box<dyn Fn(&I) -> I + 'static>>,
    entries: Vec<Entry<I>>,
}

struct Entry<I> {
    name: String,
    alt: Box<dyn Alternative<I>>,
    /// Whether the alternative leaves its input as it found it, so that one
    /// input can serve many calls. See [`InputGroup::reusing_input`].
    reuse: bool,
    /// Whether anyone wants to know if it differs from the baseline, or only
    /// roughly by how much. See [`InputGroup::uninteresting`].
    interesting: bool,
}

impl Config {
    /// Create an InputGroup for testing.
    #[cfg(test)]
    pub(crate) fn input_group(&self) -> InputGroup<()> {
        InputGroup {
            cfg: self.clone(),
            make_input: Box::new(|| ()),
            clone_input: Some(Box::new(Clone::clone)),
            entries: Vec::new(),
        }
    }

    /// Start gathering alternatives that each need freshly generated input.
    ///
    /// One batch of inputs is generated per round. With multiple alternatives
    /// it is cloned for each one, so they are measured on the same inputs and
    /// none can leave anything behind for the next. That is why `I` must be
    /// [`Clone`] here, and why the clone should be faithful: an alternative
    /// handed a shallow copy sharing a buffer with the original is not being
    /// measured on its own input. A singleton group uses the generated batch
    /// directly and does not need `I: Clone`.
    ///
    /// Neither the generating nor the cloning is timed, but both are paid
    /// out of [`max_time`](crate::Config::with_max_time).
    ///
    /// Like `Config::input_group`: this assembles a registered input group
    /// or matrix lane.
    pub(crate) fn input_group_make_input<G, I: Clone + 'static>(
        &self,
        make_input: G,
    ) -> InputGroup<I>
    where
        G: FnMut() -> I + 'static,
    {
        InputGroup {
            cfg: self.clone(),
            make_input: Box::new(make_input),
            clone_input: Some(Box::new(Clone::clone)),
            entries: Vec::new(),
        }
    }

    pub(crate) fn input_group_make_input_uncloned<G, I>(&self, make_input: G) -> InputGroup<I>
    where
        G: FnMut() -> I + 'static,
    {
        InputGroup {
            cfg: self.clone(),
            make_input: Box::new(make_input),
            clone_input: None,
            entries: Vec::new(),
        }
    }
}

impl InputGroup<()> {
    /// Add an alternative that takes no input. The first one added is the
    /// baseline.
    ///
    /// Only used by tests exercising [`Config::input_group`] directly - see
    /// its doc comment.
    #[cfg(test)]
    pub(crate) fn add<F, O>(self, name: &str, mut f: F) -> Self
    where
        F: FnMut() -> O + 'static,
    {
        self.add_input(name, move |_: &mut ()| f())
    }
}

impl<I: 'static> InputGroup<I> {
    /// Add an alternative that takes the generated input. The first one
    /// added is the baseline.
    ///
    /// The alternatives must agree on the input type, but not on what they
    /// return: each is timed by its own instantiation of the timing loop,
    /// and only that loop, not its `O`, is visible to the group's `run`.
    pub fn add_input<F, O>(mut self, name: &str, f: F) -> Self
    where
        F: FnMut(&mut I) -> O + 'static,
    {
        self.entries.push(Entry {
            name: name.to_string(),
            alt: Box::new(Timed(f)),
            reuse: false,
            interesting: true,
        });
        self
    }

    /// Like [`InputGroup::add_input`], and once the timing is done the
    /// alternative is run one more time and `metrics` computes extra numbers
    /// from what it returned.
    ///
    /// `metrics` takes the output by value, so it needs no `Clone`, and may
    /// reuse or check it. The run is outside the timing and happens once, so
    /// it suits quantities that do not vary from run to run.
    pub fn add_input_metrics<F, O, M>(mut self, name: &str, f: F, mut metrics: M) -> Self
    where
        F: FnMut(&mut I) -> O + 'static,
        O: 'static,
        M: FnMut(O) -> Metrics + 'static,
    {
        self.entries.push(Entry {
            name: name.to_string(),
            alt: Box::new(Measured {
                f,
                metrics: move |_: Option<&I>, output| metrics(output),
                count: false,
                _output: std::marker::PhantomData,
            }),
            reuse: false,
            interesting: true,
        });
        self
    }

    /// Like [`InputGroup::add_input_metrics`], and `metrics` is also given
    /// the input as it was before the run, which the alternative is free to
    /// have changed.
    ///
    /// # Panics
    ///
    /// If the group has no way to clone its input, since the pristine copy
    /// has to come from somewhere.
    pub fn add_input_metrics_with_input<F, O, M>(mut self, name: &str, f: F, mut metrics: M) -> Self
    where
        F: FnMut(&mut I) -> O + 'static,
        O: 'static,
        M: FnMut(&I, O) -> Metrics + 'static,
    {
        assert!(
            self.clone_input.is_some(),
            "metrics that read the input need an input that can be cloned"
        );
        self.entries.push(Entry {
            name: name.to_string(),
            alt: Box::new(Measured {
                f,
                metrics: move |pristine: Option<&I>, output| {
                    metrics(pristine.expect("a clone of the input was kept"), output)
                },
                count: false,
                _output: std::marker::PhantomData,
            }),
            reuse: false,
            interesting: true,
        });
        self
    }

    /// Count the allocations of the run that the alternative added last is
    /// given for its metrics, so that they can ask for them.
    ///
    /// Does nothing for an alternative without metrics.
    pub fn counting_allocations(mut self) -> Self {
        if let Some(last) = self.entries.last_mut() {
            last.alt.count_allocations();
        }
        self
    }

    /// Say that the alternative added last leaves its input as it found it,
    /// so that the same input can serve many of its calls.
    ///
    /// Without this every call is handed an input of its own, made and
    /// cloned for it, and a lap of a quick function on an input that is slow
    /// to make needs a great many of them. With it, the alternative is run on
    /// a small pool of inputs and its calls go round the pool again and
    /// again, so a lap costs a pool's worth of inputs however long it is.
    ///
    /// A function that takes its input by `&I` cannot change it, and so
    /// always qualifies. One that takes `&mut I` is promising to put the
    /// input back as it found it: if it does not, the later calls, and the
    /// other alternatives which are handed copies of the same inputs, are
    /// not measured on the input they were meant to be.
    ///
    /// What is measured then is the function on inputs that stay in cache,
    /// with a pattern of inputs short enough for the machine to learn;
    /// where that matters, leave it off.
    pub fn reusing_input(mut self) -> Self {
        if let Some(last) = self.entries.last_mut() {
            last.reuse = true;
        }
        self
    }

    /// Say that nobody asked whether the alternative added last differs from
    /// the baseline, only roughly by how much.
    ///
    /// It is then measured only until that ratio is known to within
    /// [`Config::with_rough_error`], is never called a change, and is not
    /// one of the comparisons the multiple-comparison correction counts. The
    /// baseline is the one thing that cannot be uninteresting, and for it
    /// this does nothing.
    pub fn uninteresting(mut self) -> Self {
        if let Some(last) = self.entries.last_mut() {
            last.interesting = false;
        }
        self
    }

    /// Say that every alternative added so far leaves its input as it found
    /// it, as [`InputGroup::reusing_input`] says of one. For a group whose
    /// input is declared to be reused, which is a promise every candidate
    /// sharing it makes.
    pub fn all_reusing_input(mut self) -> Self {
        for e in &mut self.entries {
            e.reuse = true;
        }
        self
    }

    /// How many comparisons with the baseline are tested for a change: the
    /// alternatives after it that are of interest. This is what the
    /// multiple-comparison correction counts.
    pub(crate) fn comparisons(&self) -> u64 {
        self.entries
            .iter()
            .skip(1)
            .filter(|e| e.interesting)
            .count() as u64
    }

    /// How many alternatives have been added.
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    /// The `Config` this set was built from, which governs both its accuracy
    /// goal and its budget.
    ///
    /// For [`Suite::add_input_group`], which sizes the group's clock and
    /// so needs the same `Config` the sampling loop will consult - the set
    /// carries its own, and it is not necessarily the suite's.
    pub(crate) fn cfg(&self) -> &Config {
        &self.cfg
    }

    /// Time them all, interleaved, and report each against the baseline.
    ///
    /// # Algorithm
    ///
    /// 1. **Calibrate.** For each alternative, time batches of growing size
    ///    until one lasts a lap's base unit, and note what one call costs.
    ///    The group's lap length is the base unit, or the longest single call
    ///    among its members if that is longer, so that every member's laps
    ///    last about the same time.
    /// 2. **Sample.** Each round times every alternative once, in a fresh
    ///    random order, as laps over the same inputs: a warm-up lap, then a
    ///    short and a long lap (see [`SHAPE`]). Each sample gives one
    ///    per-iteration time, the long lap less the short one.
    /// 3. **Compare** within each round: the log of each candidate's time
    ///    over the baseline's in that round. A trimmed mean over rounds is
    ///    the answer, and its standard error over rounds the bar.
    /// 4. **Stop** at [`MIN_SAMPLES`] rounds or later, looking again each
    ///    time the round count has grown by [`CHECK_GROWTH`], once *every*
    ///    comparison would detect a change the size of the goal at the
    ///    family's Bonferroni level, judged by Student's t. A lone
    ///    alternative stops when its own time is known to the goal. Running
    ///    out of [`max_time`](crate::Config::with_max_time) stops it too, and
    ///    marks the results.
    ///
    /// # Panics
    ///
    /// If no alternatives were added, or multiple alternatives were added
    /// without a way to clone their shared inputs.
    ///
    /// A `Suite` never calls this directly: it interleaves the group's
    /// samples with the rest of the suite via its own scheduler. Only tests
    /// call it directly, to check the sampling algorithm independently of a
    /// suite.
    #[cfg(test)]
    pub(crate) fn run(self) -> Timings {
        // Before pinning and before the machine lock, both of which have
        // effects that outlive a panic and the second of which blocks.
        assert!(
            !self.entries.is_empty(),
            "an input group needs at least one alternative, got {}",
            self.entries.len()
        );
        let _machine = Machine::claim();
        // `k` times the budget, because `k` timings come out of this: at the
        // single budget each alternative would get a `k`th of the wall clock
        // a lone benchmark is allowed, for the same target.
        let clock = Clock::new(self.cfg.max_time * self.entries.len().max(1) as u32);
        // A family of `k - 1`: one comparison is reported per alternative
        // beyond the baseline, and nothing outside this set shares the
        // threshold.
        let family = self.comparisons();
        block_on(
            &clock,
            self.run_async(&clock, family, Config::next_comparison_seed()),
        )
    }

    /// The input group sampling loop, which yields to the scheduler between rounds.
    ///
    /// A round - every alternative once - is atomic for the same reason a
    /// two-way comparison's is: the ratios this reports cancel the machine's
    /// movement only because every alternative met it within the same short
    /// round.
    ///
    /// `clock` must be built with `k` times [`max_time`](crate::Config::with_max_time), as the
    /// caller above does. `family` is how many comparisons share the
    /// Bonferroni correction - this set's own `k - 1` when run alone, or the
    /// whole suite's total when run in one - and `seed` distinguishes its
    /// random stream from its siblings'.
    ///
    /// # Panics
    ///
    /// If no alternatives were added, or multiple alternatives were added
    /// without a way to clone their shared inputs.
    pub(crate) async fn run_async(self, clock: &Clock, family: u64, seed: u64) -> Timings {
        let InputGroup {
            cfg,
            mut make_input,
            clone_input,
            mut entries,
        } = self;
        let k = entries.len();
        assert!(
            k > 0,
            "an input group needs at least one alternative, got {k}"
        );
        assert!(
            k == 1 || clone_input.is_some(),
            "multiple alternatives need clonable shared inputs"
        );
        let clone_input = clone_input.as_deref();
        // `master` holds the round's inputs; `xs` is the copy an alternative
        // is actually handed, and may be left in any state.
        let mut master: Vec<I> = Vec::new();
        let mut xs: Vec<I> = Vec::new();
        let cal = calibrate(
            &mut make_input,
            &mut entries,
            &mut master,
            &mut xs,
            clone_input,
            clock,
        )
        .await;
        release(&mut master);
        release(&mut xs);
        let mut iterations = cal.probed.clone();

        // Laps matched in time across the group: a member whose single call
        // outlasts the base unit sets the lap length for everyone, so that
        // every member's laps meet the same stretch of the machine's
        // behaviour. But not without limit: a quick function grouped with a
        // slow one is not made to run for as long as the slow one's call, and
        // beyond ten units the machine's moods are no longer something that
        // matching could do anything about.
        let base = lap_unit_ns();
        let longest = cal.per_call.iter().copied().fold(base, f64::max);
        let lap_ns = longest.min(LONG_LAP_UNITS * base);
        let reuse: Vec<bool> = entries.iter().map(|e| e.reuse).collect();
        let rough: Vec<bool> = entries.iter().map(|e| !e.interesting).collect();
        let shape = if lap_ns >= LONG_LAP_UNITS * base || !cal.affords(SHAPE, &reuse) {
            [1, 1, 0]
        } else {
            SHAPE
        };
        let plans = plan_samples(&cal, &reuse, lap_ns, shape, base);
        let most_inputs = plans.iter().map(|p| p.inputs).max().unwrap_or(1);

        let mut own: Vec<Vec<f64>> = vec![Vec::new(); k];
        let mut order: Vec<usize> = (0..k).collect();
        let mut rng: u64 = (0x9E37_79B9_7F4A_7C15 ^ seed.wrapping_mul(0x2545_F491_4F6C_DD1D)) | 1;
        let mut rounds = 0usize;
        let mut next_look = MIN_SAMPLES;

        let precise_enough = loop {
            refill(&mut make_input, &mut master, most_inputs);
            shuffle(&mut order, &mut rng);
            for &i in &order {
                let plan = &plans[i];
                // An alternative that leaves its input as it found it is run
                // on the round's own inputs, and the others, which are handed
                // a copy to do as they like with, still find them untouched.
                let timed = if k == 1 || reuse[i] {
                    entries[i].alt.laps(&mut master[..plan.inputs], plan.laps)
                } else {
                    clone_into(
                        &master[..plan.inputs],
                        &mut xs,
                        clone_input.expect("multiple alternatives need clonable inputs"),
                    );
                    entries[i].alt.laps(&mut xs, plan.laps)
                };
                own[i].push(per_iteration(timed, plan.laps));
                iterations[i] += plan.calls() as u64;
            }
            rounds += 1;

            let out_of_budget = rounds >= MAX_SAMPLES || clock.exhausted();
            if rounds >= next_look || out_of_budget {
                next_look = ((next_look as f64 * CHECK_GROWTH).ceil() as usize).max(next_look + 1);
                let precise = rounds >= MIN_SAMPLES && all_precise(&cfg, &own, family, &rough);
                if precise || out_of_budget {
                    break precise;
                }
            }
            // One whole round per poll, never part of one. And nothing held
            // across the yield: every group in a suite waits here between
            // its rounds, so inputs kept would add up over all of them
            // instead of being only the one group's that is running.
            release(&mut master);
            release(&mut xs);
            clock.yield_now().await;
        };

        // Nothing more is run on them, and what follows (the metrics) needs
        // the memory more than they do.
        release(&mut master);
        release(&mut xs);

        let mut timings: Vec<Timing> = (0..k)
            .map(|i| {
                let (ns_per_iter, std_error, _) = trimmed(&own[i]);
                Timing {
                    ns_per_iter,
                    std_error,
                    iterations: iterations[i],
                    samples: rounds,
                    hit_limit: !precise_enough,
                    untrustworthy: rounds < MIN_SAMPLES,
                    difference: None,
                }
            })
            .collect();
        let baseline = timings[0];
        for i in 1..k {
            timings[i].difference = Some(match paired(&own[i], &own[0]) {
                Paired::Log(ln, se, df) => {
                    let limit = if rough[i] {
                        f64::NAN
                    } else {
                        limit(family, df)
                    };
                    Difference::from_log_ratio(&baseline, ln, se, limit, rough[i])
                }
                Paired::Linear(se, df) => {
                    Difference::from_parts(&baseline, &timings[i], limit(family, df), se)
                }
            });
        }
        let metrics = measure_metrics(&mut entries, &mut make_input, clone_input);
        Timings {
            names: entries.into_iter().map(|e| e.name).collect(),
            timings,
            metrics,
        }
    }
}

/// Run every alternative that has metrics once, each on its own copy of one
/// fresh input, and gather what they computed.
///
/// Empty when no alternative has any, so a plain group carries nothing
/// extra. Otherwise one record per alternative, in order, the empty record
/// for those with none.
fn measure_metrics<I>(
    entries: &mut [Entry<I>],
    make_input: &mut GenInput<I>,
    clone_input: Option<&dyn Fn(&I) -> I>,
) -> Vec<Metrics> {
    if !entries.iter().any(|e| e.alt.has_metrics()) {
        return Vec::new();
    }
    let pristine = make_input();
    match clone_input {
        Some(clone) => entries
            .iter_mut()
            .map(|e| {
                if !e.alt.has_metrics() {
                    return Metrics::new();
                }
                let mut input = clone(&pristine);
                e.alt.measure(&mut input, Some(&pristine))
            })
            .collect(),
        // Only a lone alternative can lack a way to clone its input, and
        // then there is no copy to keep.
        None => {
            let mut input = pristine;
            entries
                .iter_mut()
                .take(1)
                .map(|e| e.alt.measure(&mut input, None))
                .collect()
        }
    }
}

/// Drop every input in `xs` and give its memory back.
///
/// `clear` alone would keep the allocation, which for a sample of hundreds
/// of thousands of inputs is the part that matters.
fn release<I>(xs: &mut Vec<I>) {
    *xs = Vec::new();
}

/// Generate a fresh batch of `unit` inputs, reusing `xs`'s allocation.
fn refill<I>(make_input: &mut GenInput<I>, xs: &mut Vec<I>, unit: usize) {
    xs.clear();
    xs.reserve(unit);
    for _ in 0..unit {
        xs.push(make_input());
    }
}

/// Give one alternative its own copy of the round's inputs, reusing `xs`'s
/// allocation. Not `Clone::clone_from`, which would clone element-wise into
/// whatever the last alternative left behind - here every element is
/// replaced outright, so what an alternative did to its copy cannot reach
/// the next one.
fn clone_into<I>(master: &[I], xs: &mut Vec<I>, clone_input: &dyn Fn(&I) -> I) {
    xs.clear();
    xs.extend(master.iter().map(clone_input));
}

/// One sample's per-iteration time, from its laps' times in nanoseconds.
///
/// With a long lap, the long lap less the short one: each lap carries the
/// same fixed cost of being timed, and the difference has none of it. With
/// only a short lap (see [`LONG_LAP_UNITS`]), that lap alone.
fn per_iteration(timed: [f64; 3], laps: [usize; 3]) -> f64 {
    if laps[2] > laps[1] {
        (timed[2] - timed[1]) / (laps[2] - laps[1]) as f64
    } else {
        timed[1] / laps[1] as f64
    }
}

/// Trimmed mean of `v`, the standard error of that trimmed mean, and its
/// degrees of freedom.
///
/// The standard error comes from the winsorised spread (Tukey and
/// McLaughlin): the values trimmed off each end are replaced by the nearest
/// kept value, so they still count as being far out without being allowed to
/// say how far. `NaN` for the error with fewer than two values.
fn trimmed(v: &[f64]) -> (f64, f64, f64) {
    let n = v.len();
    if n == 0 {
        return (f64::NAN, f64::NAN, 0.0);
    }
    let mut s = v.to_vec();
    s.sort_by(f64::total_cmp);
    let g = ((TRIM * n as f64) as usize).min((n - 1) / 2);
    let kept = &s[g..n - g];
    let mean = kept.iter().sum::<f64>() / kept.len() as f64;
    if n < 2 {
        return (mean, f64::NAN, 0.0);
    }
    let (lo, hi) = (s[g], s[n - g - 1]);
    let wmean = s.iter().map(|x| x.clamp(lo, hi)).sum::<f64>() / n as f64;
    let wvar = s
        .iter()
        .map(|x| (x.clamp(lo, hi) - wmean).powi(2))
        .sum::<f64>()
        / (n - 1) as f64;
    let h = kept.len();
    let se = wvar.max(0.0).sqrt() / (h as f64 / n as f64 * (n as f64).sqrt());
    (mean, se, (h - 1) as f64)
}

/// The log of `a`'s time over `b`'s, round by round, as [`trimmed`]: its
/// trimmed mean, standard error and degrees of freedom. Rounds where either
/// time is not positive (a benchmark the optimiser deleted) are skipped.
fn log_ratio(a: &[f64], b: &[f64]) -> (f64, f64, f64) {
    let v: Vec<f64> = a
        .iter()
        .zip(b)
        .filter(|(x, y)| **x > 0.0 && **y > 0.0)
        .map(|(x, y)| (x / y).ln())
        .collect();
    trimmed(&v)
}

/// A candidate compared with the baseline, round by round.
enum Paired {
    /// The log of their ratio: trimmed mean, standard error, degrees of
    /// freedom.
    Log(f64, f64, f64),
    /// Their difference in nanoseconds: its standard error and degrees of
    /// freedom (the difference itself is the gap between the two times).
    /// Only for times that hover around zero - a benchmark the optimiser
    /// deleted - where a ratio means nothing.
    Linear(f64, f64),
}

fn paired(cand: &[f64], base: &[f64]) -> Paired {
    let (ln, se, df) = log_ratio(cand, base);
    if df >= 1.0 && se.is_finite() {
        Paired::Log(ln, se, df)
    } else {
        let d: Vec<f64> = cand.iter().zip(base).map(|(a, b)| a - b).collect();
        let (_, se, df) = trimmed(&d);
        Paired::Linear(se, df)
    }
}

/// The Bonferroni limit for `family` comparisons at `df` degrees of
/// freedom; the normal one when there is nothing to count freedom from.
fn limit(family: u64, df: f64) -> f64 {
    let t = significant::bonferroni_t_limit(family, significant::FWER, df);
    if t.is_nan() {
        significant::bonferroni_z_limit(family, significant::FWER)
    } else {
        t
    }
}

/// Whether every number the report stands behind is known well enough.
///
/// A lone alternative: its own time, to the goal. Otherwise every comparison
/// with the baseline. For one that is of interest: would a change the size of
/// the goal be detected, at the family's Bonferroni level, by Student's t?
/// That is the question [`Difference::is_changed`] asks of the change
/// actually measured. For a `rough` one, which is not tested for a change:
/// is the ratio known to within [`Config::with_rough_error`], one standard
/// error either way?
fn all_precise(cfg: &Config, own: &[Vec<f64>], family: u64, rough: &[bool]) -> bool {
    let (base_mean, base_se, _) = trimmed(&own[0]);
    if own.len() == 1 {
        return cfg.accuracy_met(base_mean, base_se);
    }
    let goal_ln = if base_mean > 0.0 {
        (cfg.comparison_goal_ns(base_mean) / base_mean).ln_1p()
    } else {
        cfg.target_rel_error.ln_1p()
    };
    let rough_ln = cfg.target_rough_error.ln_1p();
    own[1..]
        .iter()
        .zip(&rough[1..])
        .all(|(cand, &rough)| match (paired(cand, &own[0]), rough) {
            (Paired::Log(_, se, _), true) => se <= rough_ln,
            (Paired::Log(_, se, df), false) => se == 0.0 || limit(family, df) * se <= goal_ln,
            (Paired::Linear(se, _), true) => se <= cfg.target_rough_error * base_mean.abs(),
            (Paired::Linear(se, df), false) => {
                cfg.comparison_accuracy_met(base_mean, se, limit(family, df))
            }
        })
}

/// Put `order` into a fresh uniformly random order, Fisher-Yates, drawing
/// from the xorshift state `rng`.
fn shuffle(order: &mut [usize], rng: &mut u64) {
    for i in (1..order.len()).rev() {
        *rng ^= *rng << 13;
        *rng ^= *rng >> 7;
        *rng ^= *rng << 17;
        order.swap(i, (*rng % (i as u64 + 1)) as usize);
    }
}

/// What calibration found out about a group.
struct Calibration {
    /// For each alternative, what one call costs in nanoseconds.
    per_call: Vec<f64>,
    /// For each alternative, what making one input and cloning it for the
    /// alternative costs, in nanoseconds, beyond the call itself.
    prep: Vec<f64>,
    /// What one input takes in memory: itself, and what it owns.
    input_bytes: usize,
    /// How many inputs one sample may use, when each call is to have one of
    /// its own.
    input_cap: usize,
    /// How many iterations each alternative's probes ran, which count
    /// towards [`Timing::iterations`] even though their timings are
    /// discarded.
    probed: Vec<u64>,
}

/// What one input takes in memory.
///
/// Its own size, and what it owns on the heap. The heap is only visible when
/// the counting allocator is installed, and then it is measured: a few
/// inputs are made under it (as many as take ten milliseconds, up to eight)
/// and the largest kept, since inputs need not all be the same size. Without it `size_of` sees an owning type such as `Vec`
/// as its handle only, so an allowance stands in for the heap behind it:
/// enough for the smallest allocation, not for what a large input really
/// takes. Inputs that are large and cheap to run on are not caught then, which
/// is what the bound on a sample's preparation time is for. An input with no
/// size and nothing on the heap costs no memory at all.
fn input_bytes<I>(make_input: &mut GenInput<I>) -> usize {
    const HEAP_ALLOWANCE: usize = 32;
    let inline = std::mem::size_of::<I>();
    if !crate::alloc::installed() {
        return if inline == 0 {
            0
        } else {
            inline + HEAP_ALLOWANCE
        };
    }
    let started = Instant::now();
    let mut heap = 0;
    for _ in 0..8 {
        let (input, held) = crate::alloc::measure(&mut *make_input);
        drop(input);
        heap = heap.max(held.net_allocated_bytes.max(0) as usize);
        // An input that is slow to make is not one to make eight of.
        if started.elapsed() > Duration::from_millis(10) {
            break;
        }
    }
    inline + heap
}

/// For each alternative, what one call costs in nanoseconds, found by timing
/// batches of growing size until one lasts a lap's base unit.
///
/// Each alternative's first call is run untimed: it is the coldest call
/// there will be, and believing it would make a first-touch cost look like
/// the function's own. Calibration yields between probes, so that in a suite
/// it is interleaved like everything else - and holds no inputs across the
/// yield, for the reason given at the round's own.
async fn calibrate<I>(
    make_input: &mut GenInput<I>,
    entries: &mut [Entry<I>],
    master: &mut Vec<I>,
    xs: &mut Vec<I>,
    clone_input: Option<&dyn Fn(&I) -> I>,
    clock: &Clock,
) -> Calibration {
    // A ceiling on the *total* cost of one probe, setup as well as timing:
    // when a benchmark's cost is optimised away its timed part never grows,
    // while untimed input construction does, unboundedly. Then a ceiling on
    // the batch that needs no timing at all, for a benchmark and input both
    // trivial enough for the optimiser to delete the whole batch. Between
    // them every `I` has some backstop, and none a hard guarantee.
    let probe_ceiling_ns = (clock.budget() / 100)
        .max(Duration::from_millis(5))
        .as_secs_f64()
        * 1e9;
    const MAX_CALIBRATION_UNIT: usize = 2_000_000;
    // What one sample's inputs may take, per copy: a sample holds the round's
    // inputs and then a clone of them for the alternative running.
    const MAX_SAMPLE_BYTES: usize = 16 * 1024 * 1024;
    // The most calls a probe of an alternative that reuses its inputs makes.
    // It consumes none, so only this and the clock bound it.
    const MAX_REUSED_CALLS: usize = 1 << 28;
    let target = lap_unit_ns();
    let singleton = entries.len() == 1;
    let bytes = input_bytes(make_input);
    let copies = if singleton { 1 } else { 2 };
    let input_cap = match bytes {
        0 => MAX_CALIBRATION_UNIT,
        bytes => MAX_CALIBRATION_UNIT.min(MAX_SAMPLE_BYTES / (bytes * copies)),
    }
    .max(1);
    let mut per_call = vec![0.0; entries.len()];
    let mut prep = vec![0.0; entries.len()];
    let mut probed = vec![0u64; entries.len()];
    for (i, e) in entries.iter_mut().enumerate() {
        let reuse = e.reuse;
        let mut n = 1usize;
        let mut warm = true;
        // What making one input cost in the last probe, to hold the next to
        // the same bounds a sample is: the heap behind an input is not
        // always visible, and what it costs to make is.
        let mut last_make = 0.0f64;
        loop {
            let probe_start = Instant::now();
            // An alternative that reuses its inputs goes round a pool of
            // them, so a probe of any length needs no more than the pool.
            let made = if reuse {
                let affordable = if last_make > 0.0 {
                    (MAX_POOL_PREPARATION_NS / last_make) as usize
                } else {
                    usize::MAX
                };
                n.min(pool_limit(bytes)).min(affordable).max(1)
            } else {
                n
            };
            refill(make_input, master, made);
            let batch_inputs = if singleton || reuse {
                &mut *master
            } else {
                clone_into(
                    master,
                    xs,
                    clone_input.expect("multiple alternatives need clonable inputs"),
                );
                &mut *xs
            };
            let timed_ns = e.alt.batch(batch_inputs, n);
            let total_ns = probe_start.elapsed().as_secs_f64() * 1e9;
            let made_ns = (total_ns - timed_ns).max(0.0);
            last_make = made_ns / made as f64;
            probed[i] += n as u64;
            if std::mem::take(&mut warm) {
                // The untimed first call: run, and not believed.
                continue;
            }
            let at_limit = if reuse {
                n >= MAX_REUSED_CALLS
            } else {
                n >= input_cap
            };
            // Inputs that are slow to make end the growth when making them
            // has cost as much as a sample may spend on it. A pool is made
            // once and gone round, so its probes are not bounded this way.
            let too_dear = !reuse && made_ns >= MAX_PREPARATION_NS;
            let mut finished = timed_ns >= target
                || total_ns >= probe_ceiling_ns
                || at_limit
                || too_dear
                || clock.exhausted();
            if !finished {
                release(master);
                release(xs);
                finished = !clock.yield_now().await;
            }
            if finished {
                per_call[i] = timed_ns / n as f64;
                prep[i] = last_make;
                break;
            }
            // Grow towards the target, more gently as the timed part nears
            // it, the whole probe nears its ceiling, or making the inputs
            // nears what a sample may spend on that.
            let factor_time = (target / timed_ns.max(1.0)).clamp(2.0, 100.0);
            let factor_safety = (probe_ceiling_ns / total_ns.max(1.0)).max(1.0);
            let factor_make = if reuse {
                f64::INFINITY
            } else {
                (MAX_PREPARATION_NS / made_ns.max(1.0)).max(1.0)
            };
            let growth = factor_time.min(factor_safety).min(factor_make);
            n = ((n as f64 * growth).ceil() as usize)
                .max(n + 1)
                .min(if reuse { MAX_REUSED_CALLS } else { input_cap });
        }
    }
    Calibration {
        per_call,
        prep,
        input_bytes: bytes,
        input_cap,
        probed,
    }
}

impl Calibration {
    /// Whether samples laid out as `shape` can be afforded, one input to a
    /// call, by every alternative that is not given a pool.
    ///
    /// Not when the inputs are so large, or so slow to make, that the units
    /// of a sample would not fit in what a sample may use even at one call
    /// to a unit. The sample is then the smaller one (see
    /// [`LONG_LAP_UNITS`] for the other way it gets there): a warm-up and a
    /// measured lap, which needs two inputs rather than eleven.
    fn affords(&self, shape: [usize; 3], reuse: &[bool]) -> bool {
        let units: usize = shape.iter().sum();
        self.prep.iter().zip(reuse).all(|(&make, &pooled)| {
            // An alternative given a pool needs one input however long its laps.
            pooled || (self.input_cap >= units && make * units as f64 <= MAX_PREPARATION_NS)
        })
    }
}

/// How one alternative's sample is laid out.
struct Plan {
    /// Calls in each lap.
    laps: [usize; 3],
    /// Inputs it is run on: one for each call, or for an alternative that
    /// reuses its input, the pool the calls go round.
    inputs: usize,
}

impl Plan {
    fn calls(&self) -> usize {
        self.laps.iter().sum()
    }
}

/// Each alternative's laps and inputs, from what calibration found.
///
/// A lap lasts `lap_ns` for everyone, which fixes how many calls it holds -
/// as far as the inputs allow. An alternative that is given one of its own for
/// every call is held to what `cal` says a sample's inputs may be: so many in
/// all, and no more than [`MAX_PREPARATION_NS`] to make. One that reuses its
/// input needs only a pool, and it is held to that pool being small: at most
/// [`MAX_POOL_INPUTS`], taking at most [`MAX_POOL_BYTES`] and
/// [`MAX_POOL_PREPARATION_NS`] to make. Its laps are not otherwise limited:
/// calls on an input it hands back as it found it cost nothing to supply.
fn plan_samples(
    cal: &Calibration,
    reuse: &[bool],
    lap_ns: f64,
    shape: [usize; 3],
    base: f64,
) -> Vec<Plan> {
    cal.per_call
        .iter()
        .zip(&cal.prep)
        .zip(reuse)
        .map(|((&t, &make), &reuse)| {
            // A call long enough to need no warm-up is not given one.
            let shape = if t >= NO_WARMUP_UNITS * base {
                [0, 1, 0]
            } else {
                shape
            };
            let units: usize = shape.iter().sum();
            // Floored so that a call the optimiser has deleted cannot ask for
            // an endless lap.
            let wanted = (lap_ns / t.max(0.1)).round().max(1.0) as usize;
            if reuse {
                let calls = wanted.saturating_mul(units);
                let by_time = if make > 0.0 {
                    (MAX_POOL_PREPARATION_NS / make) as usize
                } else {
                    usize::MAX
                };
                let pool = calls.min(pool_limit(cal.input_bytes)).min(by_time).max(1);
                Plan {
                    laps: shape.map(|s| s * wanted),
                    inputs: pool,
                }
            } else {
                let by_time = if make > 0.0 {
                    (MAX_PREPARATION_NS / (make * units as f64)) as usize
                } else {
                    usize::MAX
                };
                let unit = wanted.min(by_time).clamp(1, (cal.input_cap / units).max(1));
                Plan {
                    laps: shape.map(|s| s * unit),
                    inputs: units * unit,
                }
            }
        })
        .collect()
}

/// A comparison's results: a [`Timing`] for each candidate, baseline first.
///
/// [`Report::comparison`] gives one, and [`Report::all_comparisons`] gives each
/// of them. [`names`](Timings::names), [`timings`](Timings::timings) and
/// [`metrics`](Timings::metrics) agree on the order, so the *i*th of each
/// belongs to the same candidate. Each candidate after the baseline carries its
/// [`difference`](Timing::difference) from it, and
/// [`against_baseline`](Timings::against_baseline) pairs those with their
/// names. Printing one with `{}` shows just these candidates.
#[derive(Debug, Clone)]
pub struct Timings {
    names: Vec<String>,
    timings: Vec<Timing>,
    /// What each candidate produced besides a time, in the same order as
    /// `timings`. Empty when nothing was computed, so a plain run carries
    /// nothing extra.
    metrics: Vec<Metrics>,
}

impl Timings {
    #[cfg(test)]
    pub(crate) fn test_singleton(timing: Timing) -> Self {
        Timings {
            names: vec!["nothing".to_string()],
            timings: vec![timing],
            metrics: Vec::new(),
        }
    }

    #[cfg(test)]
    pub(crate) fn test_named(names: &[&str], timings: &[Timing]) -> Self {
        Timings {
            names: names.iter().map(|name| (*name).to_string()).collect(),
            timings: timings.to_vec(),
            metrics: Vec::new(),
        }
    }

    /// Attaches what each candidate produced besides a time, one record per
    /// candidate in the order they were added.
    #[allow(dead_code)] // until a run computes any
    pub(crate) fn with_metrics(mut self, metrics: Vec<Metrics>) -> Self {
        assert_eq!(
            metrics.len(),
            self.timings.len(),
            "one record of metrics per alternative"
        );
        self.metrics = metrics;
        self
    }

    /// What each candidate produced besides a time, one record for each, in the
    /// order of [`names`](Timings::names). The slice is empty, rather than full
    /// of empty records, when no candidate computed anything.
    ///
    /// To read one candidate's record by its name, ask the report:
    /// [`Report::metrics`].
    pub fn metrics(&self) -> &[Metrics] {
        &self.metrics
    }

    /// The name of the baseline, the first candidate.
    pub fn baseline_name(&self) -> &str {
        &self.names[0]
    }

    /// Every candidate's name, baseline first.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.names.iter().map(|s| s.as_str())
    }

    /// What each candidate measured, in the order of [`names`](Timings::names).
    pub fn timings(&self) -> &[Timing] {
        &self.timings
    }

    /// Every candidate's name and measurement, baseline first.
    pub(crate) fn measurements(&self) -> Vec<(String, crate::Measurement, Metrics)> {
        self.names()
            .enumerate()
            .map(|(i, name)| {
                (
                    name.to_string(),
                    crate::Measurement::Timing(self.timings[i]),
                    self.metrics.get(i).cloned().unwrap_or_default(),
                )
            })
            .collect()
    }

    /// Each candidate after the baseline, paired with its name. Each
    /// [`Timing`] carries its [`difference`](Timing::difference) from the
    /// baseline.
    pub fn against_baseline(&self) -> impl Iterator<Item = (&str, Timing)> {
        (1..self.timings.len()).map(move |i| (self.names[i].as_str(), self.timings[i]))
    }

    /// Whether any candidate differed significantly from the baseline.
    pub fn any_changed(&self) -> bool {
        self.against_baseline().any(|(_, c)| c.is_changed())
    }
}

impl Display for Timings {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        let width = self.names.iter().map(|n| n.len()).max().unwrap_or(0);
        writeln!(
            f,
            "{:width$}  {}  (baseline)",
            self.names[0], self.timings[0]
        )?;
        for (name, c) in self.against_baseline() {
            write!(f, "{:width$}  ", name)?;
            c.write_measurement(f)?;
            writeln!(f, "  {}", c)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;

    /// A function whose cost varies from call to call, so that the paired
    /// estimator has something to cancel. `iterations` sets the mean cost.
    fn variable_cost(seed: u64, iterations: usize) -> impl FnMut() -> u64 {
        let mut rng = XorShift(seed | 1);
        move || {
            let n = 1 + (rng.next() as usize % iterations);
            let mut acc = 0u64;
            for i in 0..n {
                acc = acc.wrapping_mul(31).wrapping_add(i as u64);
            }
            acc
        }
    }

    #[test]
    fn one_alternative_is_a_valid_input_group() {
        let cfg = Config::default().with_max_time(Duration::from_millis(20));
        let results = cfg.input_group().add("only", || 1u64).run();
        assert_eq!(results.timings().len(), 1);
        assert_eq!(results.against_baseline().count(), 0);
    }

    #[test]
    fn each_alternative_beyond_the_baseline_counts_as_one_comparison() {
        let cfg = Config::default().with_max_time(Duration::from_millis(200));
        // Four alternatives, three of them reported against the baseline.
        let _ = cfg
            .input_group()
            .add("a", || 1u64)
            .add("b", || 2u64)
            .add("c", || 3u64)
            .add("d", || 4u64)
            .run();
        // The `Drop` check asserts the plan matched what was made; reaching
        // the end of this test without it firing is the assertion.
    }

    /// Alternatives may disagree about what they return, and about the input
    /// they are handed - so long as they agree on its type.
    #[test]
    fn alternatives_need_not_share_a_return_type() {
        let cfg = Config::default().with_max_time(Duration::from_millis(200));
        let mut n = 0u64;
        let r = cfg
            .input_group_make_input(move || {
                n += 1;
                n
            })
            .add_input("sum", |x: &mut u64| *x + 1)
            .add_input("string", |x: &mut u64| *x % 7 == 0)
            .run();
        assert_eq!(r.baseline_name(), "sum");
        assert_eq!(r.names().collect::<Vec<_>>(), ["sum", "string"]);
        assert_eq!(r.timings().len(), 2);
    }

    /// The null case: three copies of one function differ from each other
    /// only by the noise of the machine, so none should be called changed.
    #[test]
    fn identical_alternatives_are_unchanged() {
        println!();
        if !quiesced() {
            println!("SKIPPED: machine is not quiesced (see `quiet-bench reserve`)");
            return;
        }
        const REPEATS: u64 = 10;
        let cfg = Config::relative(0.05).with_max_time(Duration::from_secs(2));
        let mut changed = 0u64;
        for r in 0..REPEATS {
            let seed = 0x9e37_79b9_7f4a_7c15u64.wrapping_mul(r + 1);
            let c = cfg
                .input_group()
                // Wrapped so that each is a *distinct* closure type, and so
                // gets its own instantiation of the timing loop, as three
                // genuinely different functions would. Identical code
                // compiled twice lands at a different address and runs
                // measurably differently - by 0.1% to 1%, well under the 5%
                // asked for here, but not nothing.
                .add("a", {
                    let mut f = variable_cost(seed, 2000);
                    move || f()
                })
                .add("b", {
                    let mut f = variable_cost(seed, 2000);
                    move || f()
                })
                .add("c", {
                    let mut f = variable_cost(seed, 2000);
                    move || f()
                })
                .run();
            println!("{c}");
            changed += c.against_baseline().filter(|(_, c)| c.is_changed()).count() as u64;
        }
        println!("changed {changed}/{}", 2 * REPEATS);
        // Each repeat is a family of 2 comparisons at a 5% family-wise
        // rate, so about one repeat in twenty should report something. The
        // observed rate is 0/20 across repeated runs; this leaves room for
        // both the false positives that were bought and paid for and for
        // the code layout lottery, which is real but is an order of
        // magnitude below the 5% goal.
        assert!(
            changed <= 3,
            "{changed} of {} comparisons of identical code came back changed",
            2 * REPEATS
        );
    }

    /// Every alternative in a round sees the same inputs, so when the cost
    /// is driven by the input, the draw cancels out of the differences
    /// rather than being noise each of them has to out-measure.
    ///
    /// What that buys is visible in the paired standard error against the
    /// combined one: about half, here. It only shows up when the batch is
    /// small enough that which inputs it drew still matters - a batch of a
    /// few thousand draws has already averaged the input away, and then
    /// there is nothing left for sharing them to cancel.
    #[test]
    fn shared_inputs_cancel_out_of_the_differences() {
        println!();
        if !quiesced() {
            println!("SKIPPED: machine is not quiesced (see `quiet-bench reserve`)");
            return;
        }
        const REPEATS: u64 = 8;
        let cfg = Config::relative(0.05).with_max_time(Duration::from_secs(2));
        let mut ratios = Vec::new();
        #[expect(clippy::unnecessary_fold)]
        for r in 0..REPEATS {
            let mut rng = XorShift(0x243f_6a88_85a3_08d3u64.wrapping_mul(r + 1) | 1);
            let results = cfg
                .input_group_make_input(move || {
                    // Lengths spread over 4000x, so what a batch costs is
                    // dominated by which lengths it happened to draw.
                    let n = 1 + (rng.next() as usize % 4000);
                    (0..n as u64).collect::<Vec<u64>>()
                })
                .add_input("a", |v: &mut Vec<u64>| v.iter().sum::<u64>())
                .add_input("b", |v: &mut Vec<u64>| v.iter().fold(0u64, |a, x| a + x))
                .add_input("c", |v: &mut Vec<u64>| v.iter().copied().sum::<u64>())
                .run();
            println!("{results}");
            for (_, c) in results.against_baseline() {
                let difference = c.difference().expect("candidate has a difference");
                ratios.push(difference.std_error / difference.combined_std_error(c.std_error));
            }
        }
        // Individually these run from about 0.4 to 0.9 - a ratio of two
        // error estimates from a few dozen rounds is itself a noisy thing -
        // so the claim is about their average.
        let mean = ratios.iter().sum::<f64>() / ratios.len() as f64;
        println!("mean paired/combined over {} = {mean:.3}", ratios.len());
        assert!(mean < 0.8, "paired error is not buying anything: {mean:.3}");
    }

    /// The headline promise a comparison makes: a difference twice the goal
    /// is caught nearly always.
    #[test]
    fn twice_the_goal_is_caught() {
        println!();
        if !quiesced() {
            println!("SKIPPED: machine is not quiesced (see `quiet-bench reserve`)");
            return;
        }
        const REPEATS: u64 = 10;
        let cfg = Config::relative(0.05).with_max_time(Duration::from_secs(3));
        let mut caught = 0u64;
        for r in 0..REPEATS {
            let seed = 0x9e37_79b9_7f4a_7c15u64.wrapping_mul(r + 1);
            let base = 2000;
            let c = cfg
                .input_group()
                .add("base", variable_cost(seed, base))
                .add("slower", variable_cost(seed, (base as f64 * 1.10) as usize))
                .add("faster", variable_cost(seed, (base as f64 * 0.90) as usize))
                .run();
            println!("{c}");
            caught += c.against_baseline().filter(|(_, c)| c.is_changed()).count() as u64;
        }
        let rate = caught as f64 / (2 * REPEATS) as f64;
        println!("detected {caught}/{} = {:.0}%", 2 * REPEATS, rate * 100.0);
        assert!(rate >= 0.70, "detection rate {rate:.2} below 0.70");
    }

    fn quick() -> Config {
        Config::relative(0.05).with_max_time(Duration::from_millis(30))
    }

    /// What an alternative returned is handed to its metrics, once the
    /// timing is done.
    #[test]
    fn metrics_are_computed_from_the_output() {
        let timings = quick()
            .input_group()
            .add_input_metrics(
                "short",
                |_: &mut ()| vec![0u8; 100],
                |out| Metrics::new().bytes("size", out.len()),
            )
            .add_input_metrics(
                "long",
                |_: &mut ()| vec![0u8; 400],
                |out| Metrics::new().bytes("size", out.len()),
            )
            .run();
        let sizes: Vec<f64> = timings
            .metrics()
            .iter()
            .map(|m| m.get("size").expect("each has a size").as_f64())
            .collect();
        assert_eq!(sizes, [100.0, 400.0]);
    }

    /// An alternative with no metrics gets the empty record, so that the
    /// records stay lined up with the timings.
    #[test]
    fn an_alternative_without_metrics_gets_an_empty_record() {
        let timings = quick()
            .input_group()
            .add_input("plain", |_: &mut ()| 1u8)
            .add_input_metrics(
                "counted",
                |_: &mut ()| vec![0u8; 8],
                |out| Metrics::new().count("items", out.len()),
            )
            .run();
        assert_eq!(timings.metrics().len(), 2);
        assert!(timings.metrics()[0].is_empty());
        assert_eq!(timings.metrics()[1].get("items").unwrap().as_f64(), 8.0);
    }

    /// A group that asks for nothing carries nothing.
    #[test]
    fn a_plain_group_has_no_metrics() {
        let timings = quick()
            .input_group()
            .add_input("a", |_: &mut ()| 1u8)
            .add_input("b", |_: &mut ()| 2u8)
            .run();
        assert!(timings.metrics().is_empty());
    }

    /// The input a metric sees is the one the alternative started from, not
    /// what it left behind.
    #[test]
    fn metrics_can_read_the_input_as_it_was() {
        let timings = quick()
            .input_group_make_input(|| vec![1u8, 2, 3])
            .add_input_metrics_with_input(
                "grows",
                |v: &mut Vec<u8>| {
                    v.push(4);
                    v.len()
                },
                |before, after| {
                    Metrics::new()
                        .count("before", before.len())
                        .count("after", after)
                },
            )
            .add_input("other", |v: &mut Vec<u8>| v.len())
            .run();
        let m = &timings.metrics()[0];
        assert_eq!(m.get("before").unwrap().as_f64(), 3.0);
        assert_eq!(m.get("after").unwrap().as_f64(), 4.0);
    }

    /// A lone alternative has no need of a clonable input unless its
    /// metrics read it.
    #[test]
    fn a_lone_alternative_computes_metrics_without_a_clonable_input() {
        struct NotClone(Vec<u8>);
        let timings = quick()
            .input_group_make_input_uncloned(|| NotClone(vec![0; 5]))
            .add_input_metrics(
                "only",
                |x: &mut NotClone| x.0.len(),
                |len| Metrics::new().count("len", len),
            )
            .run();
        assert_eq!(timings.metrics()[0].get("len").unwrap().as_f64(), 5.0);
    }

    #[test]
    #[should_panic(expected = "need an input that can be cloned")]
    fn reading_the_input_needs_it_to_be_clonable() {
        struct NotClone;
        let _ = quick()
            .input_group_make_input_uncloned(|| NotClone)
            .add_input_metrics_with_input("x", |_: &mut NotClone| 0u8, |_, _| Metrics::new());
    }

    /// Counting the run is what lets metrics ask for the counts. (Without
    /// the counting allocator installed they are all zero, which is why
    /// assembly refuses a program that asks for them without it.)
    #[test]
    fn counting_the_run_lets_metrics_ask_for_allocations() {
        let timings = quick()
            .input_group()
            .add_input_metrics(
                "only",
                |_: &mut ()| 1u8,
                |_| Metrics::new().peak_allocated_bytes().allocation_count(),
            )
            .counting_allocations()
            .run();
        let m = &timings.metrics()[0];
        assert_eq!(m.get("alloc peak").unwrap().as_f64(), 0.0);
        assert_eq!(m.get("alloc count").unwrap().as_f64(), 0.0);
    }

    #[test]
    #[should_panic(expected = "say `allocation`")]
    fn asking_for_allocations_of_an_uncounted_run_is_an_error() {
        quick()
            .input_group()
            .add_input_metrics(
                "only",
                |_: &mut ()| 1u8,
                |_| Metrics::new().peak_allocated_bytes(),
            )
            .run();
    }
}
