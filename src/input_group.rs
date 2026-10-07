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
//!
//! This module runs the rounds. How a sample is timed, and how many calls and
//! inputs it takes, is [`laps`]; what is made of the times is [`estimate`].

use super::*;
use crate::estimate::Paired;
use std::fmt::{self, Display, Formatter};

/// Never stop *voluntarily* on fewer rounds than this.
///
/// Each round gives one value per alternative, so this is the fewest values
/// a standard error is ever judged from. The bar is itself an estimate, and
/// from very few rounds it comes out small by luck often enough that a rule
/// which keeps looking would stop on exactly those runs. With eight that was
/// not seen to happen, and Student's t, which the comparisons use, charges
/// for whatever uncertainty remains.
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

/// How to copy an input, type-erased for the same reason.
type CloneInput<I> = dyn Fn(&I) -> I + 'static;

/// One alternative, type-erased so that alternatives may differ in what they
/// return.
///
/// The erasure is at the level of a whole batch rather than a single call:
/// what is behind the pointer is [`time_loop`] or [`time_laps`] with its `F`
/// already chosen, so the loop it runs is a direct call, and the indirection
/// is paid once per batch rather than once per iteration. On a 9 ns function
/// the per-iteration form costs 14%; this costs nothing measurable.
///
/// Both methods take inputs already prepared rather than making their own,
/// so that every alternative in a round is handed the *same* inputs. That is
/// what makes the per-round differences genuinely paired: if each drew its
/// own inputs and the cost varied with the input, the difference between two
/// alternatives would carry the difference between two draws as well, and no
/// amount of averaging distinguishes the two.
pub(crate) trait Alternative<I> {
    /// Time `n` calls over `xs`, returning the total in nanoseconds. With
    /// fewer than `n` inputs the calls go round them again. For calibration,
    /// which wants one figure.
    fn batch(&mut self, xs: &mut [I], n: usize) -> f64;

    /// Time consecutive laps of `laps[j]` calls over `xs`, returning each
    /// lap's time in nanoseconds. See [`time_laps`]. For a sample, which
    /// wants to discard the first lap and subtract the second from the third
    /// (see [`laps`]).
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

/// A generator of inputs, and the copies an alternative is handed to run on.
///
/// A round makes one batch of inputs, the master. An alternative that may
/// change its input is handed a clone of as many as it needs, so that it
/// cannot leave anything behind for the next; one that leaves it as it found
/// it runs on the master itself, which saves the clone.
///
/// Nothing is kept across a yield to the scheduler (see
/// [`Inputs::release`]), so what an input group holds while it waits for its
/// turn is nothing at all.
pub(crate) struct Inputs<I> {
    make: Box<GenInput<I>>,
    /// How to copy an input, if it can be. Only a group that has a single
    /// alternative can go without.
    clone: Option<Box<CloneInput<I>>>,
    /// The round's inputs, as made.
    master: Vec<I>,
    /// What an alternative that may change its input is handed, and may leave
    /// in any state.
    copy: Vec<I>,
}

impl<I> Inputs<I> {
    fn new(make: Box<GenInput<I>>, clone: Option<Box<CloneInput<I>>>) -> Self {
        Inputs {
            make,
            clone,
            master: Vec::new(),
            copy: Vec::new(),
        }
    }

    /// Whether an input can be copied.
    pub(crate) fn can_clone(&self) -> bool {
        self.clone.is_some()
    }

    /// How to copy an input, if it can be.
    fn cloner(&self) -> Option<&dyn Fn(&I) -> I> {
        self.clone.as_deref()
    }

    /// One input, which is not part of any round.
    pub(crate) fn make_one(&mut self) -> I {
        (self.make)()
    }

    /// Make a fresh master of `n` inputs, reusing the last one's allocation.
    pub(crate) fn fill(&mut self, n: usize) {
        self.master.clear();
        self.master.reserve(n);
        for _ in 0..n {
            self.master.push((self.make)());
        }
    }

    /// The first `n` inputs of the master for an alternative to run on.
    ///
    /// With `shared` they are the master's own, for an alternative that
    /// leaves them as it found them, or the only one there is. Otherwise they
    /// are clones, made afresh: every element is replaced outright, not
    /// `clone_from`-ed into whatever the last alternative left behind, so
    /// that what an alternative did to its copy cannot reach the next.
    pub(crate) fn take(&mut self, n: usize, shared: bool) -> &mut [I] {
        if shared {
            return &mut self.master[..n];
        }
        let clone = self
            .clone
            .as_deref()
            .expect("multiple alternatives need clonable inputs");
        self.copy.clear();
        self.copy.extend(self.master[..n].iter().map(clone));
        &mut self.copy
    }

    /// Drop every input held and give its memory back.
    ///
    /// `clear` alone would keep the allocation, which for a sample of
    /// hundreds of thousands of inputs is the part that matters.
    pub(crate) fn release(&mut self) {
        self.master = Vec::new();
        self.copy = Vec::new();
    }
}

/// Benchmarks sharing an input, gathered before any of them runs.
pub struct InputGroup<I> {
    cfg: Config,
    inputs: Inputs<I>,
    entries: Vec<Entry<I>>,
}

/// An alternative, and what is known about how to measure it.
pub(crate) struct Entry<I> {
    pub(crate) name: String,
    pub(crate) alt: Box<dyn Alternative<I>>,
    /// Whether the alternative leaves its input as it found it, so that one
    /// input can serve many calls. See [`InputGroup::reusing_input`].
    pub(crate) reuse: bool,
    /// Whether anyone wants to know if it differs from the baseline, or only
    /// roughly by how much. See [`InputGroup::uninteresting`].
    pub(crate) interesting: bool,
}

impl<I: 'static> Entry<I> {
    /// An alternative that is of interest and is given an input of its own
    /// for every call.
    fn new(name: &str, alt: impl Alternative<I> + 'static) -> Self {
        Entry {
            name: name.to_string(),
            alt: Box::new(alt),
            reuse: false,
            interesting: true,
        }
    }
}

impl Config {
    /// Create an InputGroup for testing.
    #[cfg(test)]
    pub(crate) fn input_group(&self) -> InputGroup<()> {
        InputGroup {
            cfg: self.clone(),
            inputs: Inputs::new(Box::new(|| ()), Some(Box::new(Clone::clone))),
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
            inputs: Inputs::new(Box::new(make_input), Some(Box::new(Clone::clone))),
            entries: Vec::new(),
        }
    }

    pub(crate) fn input_group_make_input_uncloned<G, I>(&self, make_input: G) -> InputGroup<I>
    where
        G: FnMut() -> I + 'static,
    {
        InputGroup {
            cfg: self.clone(),
            inputs: Inputs::new(Box::new(make_input), None),
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
        self.entries.push(Entry::new(name, Timed(f)));
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
        self.entries.push(Entry::new(
            name,
            Measured {
                f,
                metrics: move |_: Option<&I>, output| metrics(output),
                count: false,
                _output: std::marker::PhantomData,
            },
        ));
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
            self.inputs.can_clone(),
            "metrics that read the input need an input that can be cloned"
        );
        self.entries.push(Entry::new(
            name,
            Measured {
                f,
                metrics: move |pristine: Option<&I>, output| {
                    metrics(pristine.expect("a clone of the input was kept"), output)
                },
                count: false,
                _output: std::marker::PhantomData,
            },
        ));
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
            mut inputs,
            mut entries,
        } = self;
        let k = entries.len();
        assert!(
            k > 0,
            "an input group needs at least one alternative, got {k}"
        );
        assert!(
            k == 1 || inputs.can_clone(),
            "multiple alternatives need clonable shared inputs"
        );
        let cal = laps::calibrate(&mut inputs, &mut entries, clock).await;
        inputs.release();
        let mut iterations = cal.probed.clone();

        let reuse: Vec<bool> = entries.iter().map(|e| e.reuse).collect();
        let rough: Vec<bool> = entries.iter().map(|e| !e.interesting).collect();
        let plans = laps::plan(&cal, &reuse);
        let most_inputs = plans.iter().map(|p| p.inputs).max().unwrap_or(1);
        // Each alternative's time per iteration, in every round so far.
        let mut times: Vec<Vec<f64>> = vec![Vec::new(); k];
        let mut order: Vec<usize> = (0..k).collect();
        let mut rng: u64 = (0x9E37_79B9_7F4A_7C15 ^ seed.wrapping_mul(0x2545_F491_4F6C_DD1D)) | 1;
        let mut rounds = 0usize;
        let mut next_look = MIN_SAMPLES;

        let precise_enough = loop {
            inputs.fill(most_inputs);
            shuffle(&mut order, &mut rng);
            for &i in &order {
                let plan = &plans[i];
                // An alternative that leaves its input as it found it is run
                // on the round's own inputs, and the others, which are handed
                // a copy to do as they like with, still find them untouched.
                let xs = inputs.take(plan.inputs, k == 1 || reuse[i]);
                let timed = entries[i].alt.laps(xs, plan.laps);
                times[i].push(estimate::per_iteration(timed, plan.laps));
                iterations[i] += plan.calls() as u64;
            }
            rounds += 1;

            let out_of_budget = rounds >= MAX_SAMPLES || clock.exhausted();
            if rounds >= next_look || out_of_budget {
                next_look = ((next_look as f64 * CHECK_GROWTH).ceil() as usize).max(next_look + 1);
                let precise =
                    rounds >= MIN_SAMPLES && estimate::all_precise(&cfg, &times, family, &rough);
                if precise || out_of_budget {
                    break precise;
                }
            }
            // One whole round per poll, never part of one. And nothing held
            // across the yield: every group in a suite waits here between
            // its rounds, so inputs kept would add up over all of them
            // instead of being only the one group's that is running.
            inputs.release();
            clock.yield_now().await;
        };

        // Nothing more is run on them, and what follows (the metrics) needs
        // the memory more than they do.
        inputs.release();

        let mut timings: Vec<Timing> = (0..k)
            .map(|i| {
                let time = estimate::trimmed(&times[i]);
                Timing {
                    ns_per_iter: time.mean,
                    std_error: time.std_error,
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
            let difference = match estimate::paired(&times[i], &times[0]) {
                Paired::Log(ratio) => Difference::from_log_ratio(
                    &baseline,
                    ratio.mean,
                    ratio.std_error,
                    estimate::limit(family, ratio.df),
                ),
                Paired::Linear(difference) => Difference::from_parts(
                    &baseline,
                    &timings[i],
                    estimate::limit(family, difference.df),
                    difference.std_error,
                ),
            };
            timings[i].difference = Some(if rough[i] {
                difference.rough()
            } else {
                difference
            });
        }
        let metrics = measure_metrics(&mut entries, &mut inputs);
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
fn measure_metrics<I>(entries: &mut [Entry<I>], inputs: &mut Inputs<I>) -> Vec<Metrics> {
    if !entries.iter().any(|e| e.alt.has_metrics()) {
        return Vec::new();
    }
    let pristine = inputs.make_one();
    match inputs.cloner() {
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
