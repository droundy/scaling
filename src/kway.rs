//! Comparing more than two alternatives at once: [`ComparisonSet`].
//!
//! The reason to time k things together rather than k-1 times in pairs is
//! the reason [`Config::compare`] beats two separate [`bench`] calls.
//! Whatever the machine does slowly - a clock drifting, a package warming -
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
type GenInput<'a, I> = dyn FnMut() -> I + 'a;

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
type Batch<'a, I> = Box<dyn FnMut(&mut [I]) -> f64 + 'a>;

/// Alternatives to be timed against one another, gathered before any of them
/// runs.
///
/// The first one added is the baseline; every other is reported against it.
/// Build one with [`Config::comparison`], add the alternatives, then
/// [`ComparisonSet::run`]:
///
/// ```no_run
/// # fn old() -> u64 { 0 }
/// # fn new() -> u64 { 0 }
/// # fn newer() -> u64 { 0 }
/// let cfg = scaling::Config::default().with_comparisons_planned(2);
/// let results = cfg
///     .comparison()
///     .add("old", old)
///     .add("new", new)
///     .add("newer", newer)
///     .run();
/// println!("{results}");
/// ```
///
/// Two of the alternatives are reported against the baseline, so this plans
/// two comparisons, not three.
pub struct ComparisonSet<'a, I> {
    cfg: &'a Config,
    gen_input: Box<GenInput<'a, I>>,
    entries: Vec<Entry<'a, I>>,
}

struct Entry<'a, I> {
    name: String,
    batch: Batch<'a, I>,
}

impl Config {
    /// Start gathering alternatives that take no input. See
    /// [`ComparisonSet`].
    pub fn comparison(&self) -> ComparisonSet<'_, ()> {
        ComparisonSet {
            cfg: self,
            gen_input: Box::new(|| ()),
            entries: Vec::new(),
        }
    }

    /// Start gathering alternatives that each need freshly generated input.
    ///
    /// One batch of inputs is generated per round and then *cloned* for each
    /// alternative, so that within a round they are all measured on the same
    /// inputs while no alternative can leave anything behind for the next.
    /// That is why `I` must be [`Clone`] here, and why the clone should be a
    /// faithful one: an alternative that is handed a shallow copy sharing a
    /// buffer with the original is not being measured on its own input.
    ///
    /// Neither the generating nor the cloning is timed, but both are paid
    /// out of [`Config::max_time`]. See [`Config::compare_gen_input`] for
    /// the rest of what per-iteration inputs cost.
    pub fn comparison_gen_input<'a, G, I: Clone>(&'a self, gen_input: G) -> ComparisonSet<'a, I>
    where
        G: FnMut() -> I + 'a,
    {
        ComparisonSet {
            cfg: self,
            gen_input: Box::new(gen_input),
            entries: Vec::new(),
        }
    }
}

impl<'a> ComparisonSet<'a, ()> {
    /// Add an alternative that takes no input. The first one added is the
    /// baseline.
    pub fn add<F, O>(self, name: &str, mut f: F) -> Self
    where
        F: FnMut() -> O + 'a,
    {
        self.add_input(name, move |_: &mut ()| f())
    }
}

impl<'a, I: Clone + 'a> ComparisonSet<'a, I> {
    /// Add an alternative that takes the generated input. The first one
    /// added is the baseline.
    ///
    /// The alternatives must agree on the input type, but not on what they
    /// return: each is timed by its own instantiation of the timing loop,
    /// and only that loop, not its `O`, is visible to [`run`].
    ///
    /// [`run`]: ComparisonSet::run
    pub fn add_input<F, O>(mut self, name: &str, mut f: F) -> Self
    where
        F: FnMut(&mut I) -> O + 'a,
    {
        self.entries.push(Entry {
            name: name.to_string(),
            batch: Box::new(move |xs: &mut [I]| time_loop(&mut f, xs)),
        });
        self
    }

    /// How many alternatives have been added.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether no alternatives have been added yet.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
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
    /// two separately measured means - see [`Comparison::std_error`] for why
    /// that is much the better estimate.
    ///
    /// # Panics
    ///
    /// If fewer than two alternatives were added: there is nothing to
    /// compare a lone alternative against.
    pub fn run(self) -> Comparisons {
        quiet::pin_if_reserved();
        // Serialise while pinned: two benchmarks sharing one core measure
        // each other rather than themselves.
        let _exclusive = quiet::exclusive_if_pinned();
        // `k` times the budget, because `k` `Stats` come out of this: at the
        // single budget each alternative would get a `k`th of the wall clock
        // a lone `bench` call is allowed, for the same target.
        let clock = Clock::new(self.cfg.max_time * self.entries.len().max(1) as u32);
        block_on(&clock, self.run_async(&clock))
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
    /// If fewer than two alternatives were added.
    pub(crate) async fn run_async(self, clock: &Clock) -> Comparisons {
        assert!(
            self.entries.len() >= 2,
            "a comparison needs at least two alternatives, got {}",
            self.entries.len()
        );
        let ComparisonSet {
            cfg,
            mut gen_input,
            mut entries,
        } = self;
        let k = entries.len();
        // One comparison is reported per alternative beyond the baseline,
        // and each of those is a chance at a false positive, so each is
        // counted against the plan.
        let made = cfg.claim_comparisons(k as u64 - 1);
        // `master` holds the round's inputs; `xs` is the copy an alternative
        // is actually handed, and may be left in any state.
        let mut master: Vec<I> = Vec::new();
        let mut xs: Vec<I> = Vec::new();
        let (unit, probed) =
            calibrate(&mut gen_input, &mut entries, &mut master, &mut xs, clock).await;

        let mut per = vec![Running::default(); k];
        let mut diffs = vec![Running::default(); k];
        let mut times = vec![0.0f64; k];
        let mut measured_ns = 0.0;
        let mut rounds = 0usize;
        // `MIN_SAMPLE_TIME` is per alternative, and a round buys evidence
        // about all of them, so the round-total floor is `k` times as large.
        let floor_ns = k as f64 * MIN_SAMPLE_TIME.as_secs_f64() * 1e9;
        let mut flip: u64 = 0x9E37_79B9_7F4A_7C15 ^ made.wrapping_mul(0x2545_F491_4F6C_DD1D);

        let precise_enough = loop {
            flip ^= flip << 13;
            flip ^= flip >> 7;
            flip ^= flip << 17;
            let offset = (flip % k as u64) as usize;
            refill(&mut gen_input, &mut master, unit);
            for step in 0..k {
                let i = (offset + step) % k;
                clone_into(&master, &mut xs);
                let t = (entries[i].batch)(&mut xs);
                times[i] = t / unit as f64;
                measured_ns += t;
            }
            for i in 0..k {
                per[i].push(times[i]);
                if i > 0 {
                    diffs[i].push(times[i] - times[0]);
                }
            }
            rounds += 1;

            let out_of_budget = rounds >= MAX_SAMPLES || clock.exhausted();
            let (base_mean, _) = per[0].mean_and_stderr();
            // Good enough only when every difference is, since the report
            // stands behind all of them at once.
            let all_precise = (1..k).all(|i| {
                let (_, std_error) = diffs[i].mean_and_stderr();
                cfg.comparison_accuracy_met(base_mean, std_error)
            });
            let precise_enough = rounds >= MIN_SAMPLES && measured_ns >= floor_ns && all_precise;
            if precise_enough || out_of_budget {
                break precise_enough;
            }
            // One whole round per poll, never part of one.
            clock.yield_now().await;
        };

        let iterations = probed + rounds as u64 * unit as u64;
        let stats = (0..k)
            .map(|i| {
                let (ns_per_iter, std_error) = per[i].mean_and_stderr();
                Stats {
                    ns_per_iter,
                    std_error,
                    iterations,
                    samples: rounds,
                    hit_limit: !precise_enough,
                    untrustworthy: rounds < MIN_SAMPLES,
                }
            })
            .collect();
        // Index zero is the baseline, which has no difference from itself to
        // report an error for.
        let paired = (0..k)
            .map(|i| {
                if i == 0 {
                    f64::NAN
                } else {
                    diffs[i].mean_and_stderr().1
                }
            })
            .collect();
        Comparisons {
            names: entries.into_iter().map(|e| e.name).collect(),
            stats,
            paired,
            z_alpha: cfg.z_alpha(),
        }
    }
}

/// Generate a fresh batch of `unit` inputs, reusing `xs`'s allocation.
fn refill<I>(gen_input: &mut GenInput<I>, xs: &mut Vec<I>, unit: usize) {
    xs.clear();
    xs.reserve(unit);
    for _ in 0..unit {
        xs.push(gen_input());
    }
}

/// Give one alternative its own copy of the round's inputs, reusing `xs`'s
/// allocation. Not `Clone::clone_from`, which would clone element-wise into
/// whatever the last alternative left behind - here every element is
/// replaced outright, so what an alternative did to its copy cannot reach
/// the next one.
fn clone_into<I: Clone>(master: &[I], xs: &mut Vec<I>) {
    xs.clear();
    xs.extend_from_slice(master);
}

/// Find a batch size whose measured duration, summed over every alternative,
/// reaches [`SAMPLE_TIME`]: the same extrapolation
/// [`Config::bench_gen_input`] does, over a whole round.
async fn calibrate<'a, I: Clone>(
    gen_input: &mut GenInput<'a, I>,
    entries: &mut [Entry<'a, I>],
    master: &mut Vec<I>,
    xs: &mut Vec<I>,
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
        refill(gen_input, master, unit);
        for e in entries.iter_mut() {
            clone_into(master, xs);
            timed_ns += (e.batch)(xs);
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

/// What [`ComparisonSet::run`] measured: a [`Stats`] for every alternative,
/// and every alternative's difference from the baseline.
#[derive(Debug, Clone)]
pub struct Comparisons {
    names: Vec<String>,
    stats: Vec<Stats>,
    /// Standard error of each alternative's difference from the baseline,
    /// accumulated per round. `NaN` at index zero, the baseline itself.
    paired: Vec<f64>,
    z_alpha: f64,
}

impl Comparisons {
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
    pub fn stats(&self) -> &[Stats] {
        &self.stats
    }

    /// Each alternative beyond the baseline, paired with its name, as a
    /// [`Comparison`] against the baseline.
    pub fn against_baseline(&self) -> impl Iterator<Item = (&str, Comparison)> {
        (1..self.stats.len()).map(move |i| {
            (
                self.names[i].as_str(),
                Comparison::from_parts(
                    self.stats[0].clone(),
                    self.stats[i].clone(),
                    self.z_alpha,
                    self.paired[i],
                ),
            )
        })
    }

    /// Whether any alternative differed from the baseline.
    pub fn any_changed(&self) -> bool {
        self.against_baseline().any(|(_, c)| c.is_changed())
    }
}

impl Display for Comparisons {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        let width = self.names.iter().map(|n| n.len()).max().unwrap_or(0);
        writeln!(f, "{:width$}  {}  (baseline)", self.names[0], self.stats[0])?;
        for (name, c) in self.against_baseline() {
            writeln!(f, "{:width$}  {}  {}", name, c.candidate, c)?;
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
    #[should_panic(expected = "at least two alternatives")]
    fn one_alternative_is_not_a_comparison() {
        let cfg = Config::default().with_comparisons_planned(1);
        cfg.comparison().add("only", || 1u64).run();
        // Unreachable, but were the panic ever to stop happening, `Drop`
        // would report a plan of 1 against 0 made rather than the missing
        // panic, which is a confusing way to fail.
        std::mem::forget(cfg);
    }

    #[test]
    fn each_alternative_beyond_the_baseline_counts_as_one_comparison() {
        let cfg = Config::default()
            .with_max_time(Duration::from_millis(200))
            .with_comparisons_planned(3);
        // Four alternatives, three of them reported against the baseline.
        let _ = cfg
            .comparison()
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
        let cfg = Config::default()
            .with_max_time(Duration::from_millis(200))
            .with_comparisons_planned(1);
        let mut n = 0u64;
        let r = cfg
            .comparison_gen_input(move || {
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
        let cfg = Config::relative(0.05)
            .with_max_time(Duration::from_secs(2))
            .with_comparisons_planned(2 * REPEATS);
        let mut changed = 0u64;
        for r in 0..REPEATS {
            let seed = 0x9e37_79b9_7f4a_7c15u64.wrapping_mul(r + 1);
            let c = cfg
                .comparison()
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
        let cfg = Config::relative(0.05)
            .with_max_time(Duration::from_secs(2))
            .with_comparisons_planned(2 * REPEATS);
        let mut ratios = Vec::new();
        for r in 0..REPEATS {
            let mut rng = XorShift(0x243f_6a88_85a3_08d3u64.wrapping_mul(r + 1) | 1);
            let results = cfg
                .comparison_gen_input(move || {
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
                let combined =
                    (c.baseline.std_error.powi(2) + c.candidate.std_error.powi(2)).sqrt();
                ratios.push(c.std_error() / combined);
            }
        }
        // Individually these run from about 0.4 to 0.9 - a ratio of two
        // error estimates from a few dozen rounds is itself a noisy thing -
        // so the claim is about their average.
        let mean = ratios.iter().sum::<f64>() / ratios.len() as f64;
        println!("mean paired/combined over {} = {mean:.3}", ratios.len());
        assert!(mean < 0.8, "paired error is not buying anything: {mean:.3}");
    }

    /// The headline promise, as [`Config::compare`] makes it: a difference
    /// twice the goal is caught nearly always.
    #[test]
    fn twice_the_goal_is_caught() {
        println!();
        if !quiesced() {
            println!("SKIPPED: machine is not quiesced (see `quiet-bench reserve`)");
            return;
        }
        const REPEATS: u64 = 10;
        let cfg = Config::relative(0.05)
            .with_max_time(Duration::from_secs(3))
            .with_comparisons_planned(2 * REPEATS);
        let mut caught = 0u64;
        for r in 0..REPEATS {
            let seed = 0x9e37_79b9_7f4a_7c15u64.wrapping_mul(r + 1);
            let base = 2000;
            let c = cfg
                .comparison()
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
