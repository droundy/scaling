//! Measuring one or more alternatives over a shared input: [`InputGroup`].
//!
//! The reason to time k things together rather than k-1 times in pairs is
//! the same reason a comparison's alternatives beat two separate [`bench`]
//! calls. Whatever the machine does slowly - a clock drifting, a package warming -
//! lands on every alternative within the same round and cancels out of the
//! differences between them. Measured one after another instead, each would
//! sample a different stretch of that drift, and the differences would carry
//! it.

use super::*;
use std::fmt::{self, Display, Formatter};
use std::time::{Duration, Instant};

/// Never stop *voluntarily* on fewer rounds than this. See
/// [`crate::MIN_SAMPLE_TIME`], which does most of the work.
const MIN_SAMPLES: usize = 6;

/// How long one round - a batch of every alternative - should take.
/// Calibration picks the batch size aiming for this.
const SAMPLE_TIME: Duration = Duration::from_micros(100);

/// A backstop on the round count, so the accumulators cannot grow without
/// bound.
const MAX_SAMPLES: usize = 1_000_000;

/// A generator of inputs, type-erased so that all the alternatives can share
/// one.
type GenInput<I> = dyn FnMut() -> I + 'static;

/// One alternative's timing loop, type-erased so that alternatives may
/// differ in what they return.
///
/// The erasure is at the level of a whole batch rather than a single call:
/// what is behind the pointer is [`time_loop`] with its `F` already chosen,
/// so the loop it runs is a direct call, and the indirection is paid once
/// per batch rather than once per iteration. On a 9 ns function the
/// per-iteration form costs 14%; this costs nothing measurable.
///
/// It takes a batch of inputs already prepared rather than making its own,
/// so that every alternative in a round is handed the *same* inputs. That is
/// what makes the per-round differences genuinely paired: if each drew its
/// own inputs and the cost varied with the input, the difference between two
/// alternatives would carry the difference between two draws as well, and no
/// amount of averaging distinguishes the two.
type Batch<I> = Box<dyn FnMut(&mut [I]) -> f64 + 'static>;

/// Benchmarks sharing an input, gathered before any of them runs.
pub struct InputGroup<I> {
    cfg: Config,
    make_input: Box<GenInput<I>>,
    clone_input: Option<Box<dyn Fn(&I) -> I + 'static>>,
    entries: Vec<Entry<I>>,
}

struct Entry<I> {
    name: String,
    batch: Batch<I>,
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
    /// out of [`Config::max_time`].
    ///
    /// Like [`Config::input_group`]: this assembles a registered input group
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
    /// and only that loop, not its `O`, is visible to [`run`].
    ///
    /// [`run`]: InputGroup::run
    pub fn add_input<F, O>(mut self, name: &str, mut f: F) -> Self
    where
        F: FnMut(&mut I) -> O + 'static,
    {
        self.entries.push(Entry {
            name: name.to_string(),
            batch: Box::new(move |xs: &mut [I]| time_loop(&mut f, xs)),
        });
        self
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
    /// 1. **Calibrate.** Find the smallest batch size `unit` for which one
    ///    batch of every alternative *together* reach `SAMPLE_TIME`,
    ///    extrapolating multiplicatively from the last probe.
    /// 2. **Sample.** Each round times a batch of `unit` iterations of every
    ///    alternative, starting from a rotating position so that none of
    ///    them keeps a fixed place in the order. Under a fixed order each
    ///    alternative would sample a fixed and different phase of anything
    ///    periodic in the machine, and a difference produced that way is
    ///    indistinguishable from a real one.
    /// 3. **Stop** once there are at least `MIN_SAMPLES` rounds, every
    ///    alternative has had [`crate::MIN_SAMPLE_TIME`] of measuring, and
    ///    *every* difference from the baseline is measured finely enough to
    ///    detect a change the size of the accuracy goal. Running out of
    ///    [`Config::max_time`] stops it too, and marks the results.
    ///
    /// Each difference is accumulated per round rather than assembled from
    /// two separately measured means - see [`Timing::std_error`] for why
    /// that is much the better estimate.
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
        // a lone `bench` call is allowed, for the same target.
        let clock = Clock::new(self.cfg.max_time * self.entries.len().max(1) as u32);
        // A family of `k - 1`: one comparison is reported per alternative
        // beyond the baseline, and nothing outside this set shares the
        // threshold.
        let z_alpha = Config::z_alpha_for(self.entries.len() as u64 - 1);
        block_on(
            &clock,
            self.run_async(&clock, z_alpha, Config::next_comparison_seed()),
        )
    }

    /// The k-way sampling loop, which yields to the scheduler between rounds.
    ///
    /// A round - every alternative once, from a rotated starting position -
    /// is atomic for the same reason a two-way comparison's is: the
    /// differences this reports cancel the machine's slow movement only
    /// because every alternative met that movement within the same round.
    ///
    /// `clock` must be built with `k` times [`Config::max_time`], as the
    /// caller above does.
    ///
    /// # Panics
    ///
    /// If no alternatives were added, or multiple alternatives were added
    /// without a way to clone their shared inputs.
    /// `z_alpha` is the Bonferroni limit for the family this set belongs to -
    /// its own `k - 1` when run alone, or the whole suite's total when run in
    /// one - and `seed` distinguishes its random stream from its siblings'.
    pub(crate) async fn run_async(self, clock: &Clock, z_alpha: f64, seed: u64) -> Timings {
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
        let (unit, probed) = calibrate(
            &mut make_input,
            &mut entries,
            &mut master,
            &mut xs,
            clone_input,
            clock,
        )
        .await;

        let mut own = vec![Running::default(); k];
        let mut diffs = vec![Running::default(); k];
        let mut times = vec![0.0f64; k];
        let mut measured_ns = 0.0;
        let mut rounds = 0usize;
        // `MIN_SAMPLE_TIME` is per alternative, and a round buys evidence
        // about all of them, so the round-total floor is `k` times as large.
        let floor_ns = k as f64 * MIN_SAMPLE_TIME.as_secs_f64() * 1e9;
        let mut flip: u64 = 0x9E37_79B9_7F4A_7C15 ^ seed.wrapping_mul(0x2545_F491_4F6C_DD1D);

        let precise_enough = loop {
            flip ^= flip << 13;
            flip ^= flip >> 7;
            flip ^= flip << 17;
            let offset = (flip % k as u64) as usize;
            refill(&mut make_input, &mut master, unit);
            for step in 0..k {
                let i = (offset + step) % k;
                let batch_inputs = if k == 1 {
                    &mut master
                } else {
                    clone_into(
                        &master,
                        &mut xs,
                        clone_input.expect("multiple alternatives need clonable inputs"),
                    );
                    &mut xs
                };
                let t = (entries[i].batch)(batch_inputs);
                times[i] = t / unit as f64;
                measured_ns += t;
            }
            for i in 0..k {
                own[i].push(times[i]);
                if i > 0 {
                    diffs[i].push(times[i] - times[0]);
                }
            }
            rounds += 1;

            let out_of_budget = rounds >= MAX_SAMPLES || clock.exhausted();
            let (base_mean, _) = own[0].mean_and_stderr();
            // Good enough only when every difference is, since the report
            // stands behind all of them at once.
            let all_precise = (1..k).all(|i| {
                let (_, std_error) = diffs[i].mean_and_stderr();
                cfg.comparison_accuracy_met(base_mean, std_error, z_alpha)
            });
            let precise_enough = rounds >= MIN_SAMPLES && measured_ns >= floor_ns && all_precise;
            if precise_enough || out_of_budget {
                break precise_enough;
            }
            // One whole round per poll, never part of one.
            clock.yield_now().await;
        };

        let iterations = probed + rounds as u64 * unit as u64;
        let mut timings: Vec<Timing> = (0..k)
            .map(|i| {
                let (ns_per_iter, std_error) = own[i].mean_and_stderr();
                Timing {
                    ns_per_iter,
                    std_error,
                    iterations,
                    samples: rounds,
                    hit_limit: !precise_enough,
                    untrustworthy: rounds < MIN_SAMPLES,
                    difference: None,
                }
            })
            .collect();
        let baseline = timings[0];
        for (timing, diff) in timings[1..].iter_mut().zip(diffs[1..].iter()) {
            timing.difference = Some(Difference::from_parts(
                &baseline,
                timing,
                z_alpha,
                diff.mean_and_stderr().1,
            ));
        }
        Timings {
            names: entries.into_iter().map(|e| e.name).collect(),
            timings,
        }
    }
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

/// Find a batch size whose measured duration, summed over every alternative,
/// reaches [`SAMPLE_TIME`]: the same extrapolation
/// [`Config::bench_make_input`] does, over a whole round.
async fn calibrate<I>(
    make_input: &mut GenInput<I>,
    entries: &mut [Entry<I>],
    master: &mut Vec<I>,
    xs: &mut Vec<I>,
    clone_input: Option<&dyn Fn(&I) -> I>,
    clock: &Clock,
) -> (usize, u64) {
    let probe_ceiling_ns = (clock.budget() / 100)
        .max(Duration::from_millis(5))
        .as_secs_f64()
        * 1e9;
    const MAX_CALIBRATION_UNIT: usize = 2_000_000;
    const MAX_CALIBRATION_BYTES: usize = 64 * 1024 * 1024;
    let unit_cap =
        MAX_CALIBRATION_UNIT.min(MAX_CALIBRATION_BYTES / std::mem::size_of::<I>().max(1));
    let target = SAMPLE_TIME.as_secs_f64() * 1e9;
    let mut unit = 1usize;
    let mut probed = 0u64;
    loop {
        let mut timed_ns = 0.0;
        let probe_start = Instant::now();
        refill(make_input, master, unit);
        let singleton = entries.len() == 1;
        for e in entries.iter_mut() {
            let batch_inputs = if singleton {
                &mut *master
            } else {
                clone_into(
                    master,
                    xs,
                    clone_input.expect("multiple alternatives need clonable inputs"),
                );
                &mut *xs
            };
            timed_ns += (e.batch)(batch_inputs);
        }
        // Everything the probe cost, generating and cloning included: what
        // the ceiling below is protecting against is a probe that takes an
        // age, and it does not matter which part of it was slow.
        let total_ns = probe_start.elapsed().as_secs_f64() * 1e9;
        probed += unit as u64;
        if timed_ns >= target
            || total_ns >= probe_ceiling_ns
            || unit >= unit_cap
            || clock.exhausted()
        {
            return (unit, probed);
        }
        // Before the extrapolation, so `unit` and the probe agree.
        if !clock.yield_now().await {
            return (unit, probed);
        }
        let factor_time = (target / timed_ns.max(1.0)).clamp(2.0, 100.0);
        let factor_safety = (probe_ceiling_ns / total_ns.max(1.0)).max(1.0);
        unit = ((unit as f64 * factor_time.min(factor_safety)).ceil() as usize)
            .max(unit + 1)
            .min(unit_cap);
    }
}

/// What running an input group measured: a [`Timing`] for every alternative,
/// and every alternative's difference from the baseline when there is one.
#[derive(Debug, Clone)]
pub struct Timings {
    names: Vec<String>,
    timings: Vec<Timing>,
}

impl Timings {
    #[cfg(test)]
    pub(crate) fn test_singleton(timing: Timing) -> Self {
        Timings {
            names: vec!["nothing".to_string()],
            timings: vec![timing],
        }
    }

    /// The name of the baseline - the first alternative that was added.
    pub fn baseline_name(&self) -> &str {
        &self.names[0]
    }

    /// Every alternative's name, baseline first, in the order they were
    /// added.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.names.iter().map(|s| s.as_str())
    }

    /// What each alternative measured, in the order they were added.
    pub fn timings(&self) -> &[Timing] {
        &self.timings
    }

    /// What each alternative measured, in the order they were added.
    pub fn stats(&self) -> &[Timing] {
        self.timings()
    }

    /// Each alternative beyond the baseline, paired with its name, as a
    /// [`Timing`] against the baseline.
    pub fn against_baseline(&self) -> impl Iterator<Item = (&str, Timing)> {
        (1..self.timings.len()).map(move |i| (self.names[i].as_str(), self.timings[i].clone()))
    }

    /// Whether any alternative differed from the baseline.
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
        assert_eq!(results.stats().len(), 1);
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
        assert_eq!(r.stats().len(), 2);
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
}
