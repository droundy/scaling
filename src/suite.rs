//! Interleaving a whole suite of benchmarks.
//!
//! The reason to run fifty benchmarks together rather than one after another
//! is the same reason a comparison's alternatives beat two separate
//! [`bench`] calls, and the reason [`InputGroup`] beats pairwise comparisons
//! in pairs. Run in
//! sequence, benchmark #1 samples the machine at t=0 and #50 samples it at
//! t=500s, by which time the package is warmer and the clock has drifted;
//! their numbers are then not comparable, and neither is either of them
//! against the same suite run yesterday. Interleaved, every benchmark's
//! samples spread across the whole session, so all of them average the same
//! drift.
//!
//! What makes this harder than [`InputGroup`] is that the benchmarks do
//! not match: they take different input types, run for wildly different
//! times, and want different batch sizes. An `InputGroup` can share one
//! calibrated `unit` across its alternatives and step them in lockstep. A
//! suite cannot, so the scheduling has to be in *time* rather than in
//! batches, and each benchmark has to be able to stop in the middle and be
//! picked up again later.
//!
//! # Why `async`
//!
//! "Stop in the middle and be picked up later" is exactly what an `async fn`
//! compiles to. Writing it by hand would mean lifting every local of the
//! sampling loop - the batch size, the calibration probe count, the running
//! mean, the accumulated measured time, and the scratch buffer of inputs -
//! into a struct with a `step` method, and then doing it again, differently,
//! for each kind of benchmark. Writing it as an `async fn` leaves the loop
//! looking like the loop it replaced and lets the compiler generate the
//! struct.
//!
//! Boxing the resulting future erases the input type at the same time, which
//! is the other thing a heterogeneous suite needed. The cost is one indirect
//! call per poll, and a poll is a whole sample - the same amortisation that
//! makes [`InputGroup`]'s batch-level erasure free, rather than the
//! per-iteration erasure that cost 14% on a 9ns function.
//!
//! **`async` is an implementation detail.** No future, and nothing to
//! `.await`, appears in this crate's public API.
//!
//! # Why there is no runtime here
//!
//! A general executor would be the wrong tool, not merely a heavy one.

use super::*;
use crate::registry::{Candidate, Input, Registered};
use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::{self, Display, Formatter};
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::time::{Duration, Instant};

mod scheduler;
pub(crate) use scheduler::block_on;
use scheduler::Scheduler;

struct YieldOnce {
    yielded: bool,
}

impl Future for YieldOnce {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, _cx: &mut std::task::Context<'_>) -> std::task::Poll<()> {
        if self.yielded {
            std::task::Poll::Ready(())
        } else {
            self.yielded = true;
            std::task::Poll::Pending
        }
    }
}

pub(crate) struct Clock {
    poll_started: Cell<Option<Instant>>,
    spent: Cell<Duration>,
    max: Duration,
}

impl Clock {
    pub(crate) fn new(max: Duration) -> Self {
        Clock {
            poll_started: Cell::new(None),
            spent: Cell::new(Duration::ZERO),
            max,
        }
    }

    pub(crate) fn this_poll(&self) -> Duration {
        self.poll_started
            .get()
            .map_or(Duration::ZERO, |time| time.elapsed())
    }

    pub(crate) fn spent(&self) -> Duration {
        self.spent.get() + self.this_poll()
    }

    pub(crate) fn exhausted(&self) -> bool {
        self.spent() >= self.max
    }

    pub(crate) fn budget(&self) -> Duration {
        self.max
    }

    pub(crate) async fn yield_now(&self) -> bool {
        YieldOnce { yielded: false }.await;
        !self.exhausted()
    }

    pub(crate) fn begin_poll(&self) {
        self.poll_started.set(Some(Instant::now()));
    }

    pub(crate) fn end_poll(&self) {
        if let Some(started) = self.poll_started.take() {
            self.spent.set(self.spent.get() + started.elapsed());
        }
    }
}

pub(crate) struct Machine {
    _exclusive: Option<quiet::Exclusive>,
}

impl Machine {
    pub(crate) fn claim() -> Self {
        quiet::pin_if_reserved();
        Machine {
            _exclusive: quiet::exclusive_if_pinned(),
        }
    }
}

/// The result of one scheduled benchmark task before it is arranged into groups.
#[derive(Debug, Clone)]
pub(crate) enum Found {
    Scaling(ScalingStats),
    Timing(Timings),
}

impl Display for Found {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        match self {
            Found::Scaling(s) => write!(f, "{s}"),
            Found::Timing(c) => write!(f, "{c}"),
        }
    }
}

/// One cell of a [`Group`]: what a candidate measured on one input.
#[derive(Debug, Clone, Copy)]
pub enum Measurement {
    /// A scaling law, from a benchmark that varied its own input size.
    Scaling(ScalingStats),
    /// A timing. In a comparison, every candidate but the baseline carries
    /// its [`Difference`] from the baseline.
    Timing(Timing),
}

impl Display for Measurement {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        match self {
            Measurement::Scaling(s) => write!(f, "{s}"),
            Measurement::Timing(timing) => write!(f, "{timing}"),
        }
    }
}

pub struct Suite {
    cfg: Config,
    scheduler: Scheduler,
    names: Vec<String>,
    lanes: Vec<crate::assemble::Lane>,
    comparisons: u64,
    z_alpha: Rc<Cell<f64>>,
}

impl Config {
    /// Begin a suite of benchmarks to be measured together. What
    /// [`crate::runner`] calls, not what a benchmark is written against.
    pub(crate) fn suite(&self) -> Suite {
        Suite {
            cfg: self.clone(),
            // A fixed seed: what must vary is the starting position from one
            // round to the next, which it does. Varying it between runs as
            // well would only make a suite harder to reproduce.
            scheduler: Scheduler::new(0x9E37_79B9_7F4A_7C15),
            names: Vec::new(),
            lanes: Vec::new(),
            comparisons: 0,
            // `NaN` until `run` sets it. Nothing reads it before then, and a
            // suite holding no comparisons never reads it at all.
            z_alpha: Rc::new(Cell::new(f64::NAN)),
        }
    }
}

impl Suite {
    /// How many benchmarks have been added.
    pub(crate) fn len(&self) -> usize {
        self.names.len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.names.is_empty()
    }

    fn push(&mut self, name: &str, clock: Rc<Clock>, future: Pin<Box<dyn Future<Output = Found>>>) {
        self.names.push(name.to_string());
        self.scheduler.push(clock, future);
    }

    /// The clock and [`Suite::push`] shared by every benchmark task.
    fn add_task(
        &mut self,
        name: &str,
        max_time: Duration,
        body: impl FnOnce(Rc<Clock>) -> Pin<Box<dyn Future<Output = Found>>> + 'static,
    ) {
        let clock = Rc::new(Clock::new(max_time));
        self.push(name, clock.clone(), body(clock));
    }

    /// Add a benchmark, as [`bench`](fn@bench) would run it.
    pub fn add<F, O>(&mut self, name: &str, mut f: F)
    where
        F: FnMut() -> O + 'static,
    {
        self.add_make_input(name, || (), move |_: &mut ()| f())
    }

    /// Add a benchmark over a mutable input, as [`bench_clone_input`] would run it.
    pub fn add_input<F, I, O>(&mut self, name: &str, input: I, f: F)
    where
        F: FnMut(&mut I) -> O + 'static,
        I: Clone + 'static,
    {
        self.add_make_input(name, move || input.clone(), f)
    }

    /// Add a benchmark over generated inputs, as [`bench_make_input`] would
    /// run it.
    pub fn add_make_input<G, F, I, O>(&mut self, name: &str, make_input: G, f: F)
    where
        G: FnMut() -> I + 'static,
        F: FnMut(&mut I) -> O + 'static,
        I: 'static,
    {
        let group = self
            .cfg
            .input_group_make_input_uncloned(make_input)
            .add_input(name, f);
        self.add_input_group(name, group);
    }

    /// Add a scaling benchmark, as [`bench_scaling`](fn@bench_scaling) would run it.
    pub fn add_scaling<F, O>(&mut self, name: &str, f: F, nmin: usize)
    where
        F: FnMut(usize) -> O + 'static,
    {
        let cfg = self.cfg.clone();
        self.add_task(name, cfg.max_time, move |clock| {
            Box::pin(async move { Found::Scaling(cfg.bench_scaling_async(&clock, f, nmin).await) })
        })
    }

    /// Add a scaling benchmark over generated inputs, as
    /// [`bench_scaling_gen`] would run it.
    pub fn add_scaling_gen<G, F, I, O>(&mut self, name: &str, make_input: G, f: F, nmin: usize)
    where
        G: FnMut(usize) -> I + 'static,
        F: Fn(&mut I) -> O + 'static,
        I: 'static,
    {
        let cfg = self.cfg.clone();
        self.add_task(name, cfg.max_time, move |clock| {
            Box::pin(async move {
                Found::Scaling(
                    cfg.bench_scaling_gen_async(&clock, make_input, f, nmin)
                        .await,
                )
            })
        })
    }

    /// Add an input group, built with [`Config::input_group`].
    ///
    /// # Panics
    ///
    /// If the set holds no alternatives, or has multiple alternatives but no
    /// way to clone their shared inputs.
    pub(crate) fn add_input_group<I: 'static>(&mut self, name: &str, set: InputGroup<I>) {
        let k = set.len();
        assert!(
            k > 0,
            "an input group needs at least one alternative, got {k}"
        );
        // After the assertion and before the count, so the Bonferroni limit
        // is taken over what is really going to be measured.
        // Every alternative beyond the baseline is a chance at a false
        // positive, and so counts against the plan `run` will set.
        self.comparisons += k as u64 - 1;
        // Each comparison gets a different seed, so two sitting in one suite
        // do not draw the same order of alternatives round after round.
        let seed = self.comparisons;
        let z_alpha = self.z_alpha.clone();
        // The set's own `Config`, not the suite's: it is what `run_async`
        // consults for the accuracy goal, so it must also be what the budget
        // comes from. A set built from a different `Config` than the suite
        // would otherwise chase one target on the other's clock.
        //
        // `checked_mul`, not `*`: a large but individually valid `max_time`
        // (see `parse_duration`) times enough alternatives can overflow
        // `Duration`, and a caller who asked for a huge budget should get
        // one clamped to the largest this can represent, not a panic.
        let budget = set
            .cfg()
            .max_time
            .checked_mul(k as u32)
            .unwrap_or(Duration::MAX);
        let clock = Rc::new(Clock::new(budget));
        let mine = clock.clone();
        self.push(
            name,
            clock,
            Box::pin(async move {
                let results = set.run_async(&mine, z_alpha.get(), seed).await;
                Found::Timing(results)
            }),
        );
    }

    /// Measure every benchmark, interleaved, and report them together.
    ///
    /// The Bonferroni limit is worked out here from the count. It can only
    /// be done at this point after we know how many comparisons will be made.
    pub(crate) fn run(self) -> Report {
        self.z_alpha.set(Config::z_alpha_for(self.comparisons));
        // Claimed once for the whole session rather than once per benchmark.
        // The guard is re-entrant within a thread, so the benchmarks' own
        // claims - taken when they are run individually - cost nothing here.
        let _machine = Machine::claim();
        let results = self.scheduler.run();
        let entries: Vec<(String, Found)> = self.names.into_iter().zip(results).collect();
        Report::new(entries, &self.lanes)
    }
}

/// What assembling the registered benchmarks produced.
#[derive(Debug, Default)]
pub(crate) struct Assembled {
    /// Things worth saying that did not stop the run - a candidate no input
    /// matches, say. Errors come back through [`Suite::try_add_registered`]
    /// instead; these are the complaints that leave the rest of the run
    /// perfectly good.
    pub warnings: Vec<crate::assemble::Diagnostic>,
}

impl Suite {
    /// Add every benchmark registered anywhere in this binary, handing back
    /// what is wrong rather than panicking - what a runner printing
    /// diagnostics of its own should do.
    ///
    /// Discovery only; the suite is otherwise unchanged, and benchmarks added
    /// by hand before or after this call sit alongside the discovered ones
    /// and are measured the same way. Calling it twice would add everything
    /// twice, so do not.
    ///
    /// Registered groups go through [`Suite::add_input_group`] like any
    /// other, so they are counted towards the suite's multiple-comparison
    /// plan by the machinery that was already there.
    ///
    /// Nothing is added when this returns `Err`: the registrations are
    /// checked in full before the first one is added, so a suite is never
    /// left holding half of a set that did not check out.
    pub(crate) fn try_add_registered(
        &mut self,
    ) -> Result<Assembled, Vec<crate::assemble::Diagnostic>> {
        let regs: Vec<&'static Registered> = inventory::iter::<Registered>().collect();
        let cands: Vec<&'static Candidate> = inventory::iter::<Candidate>().collect();
        let inputs: Vec<&'static Input> = inventory::iter::<Input>().collect();
        self.assemble_registered(&regs, &cands, &inputs)
    }

    /// [`Suite::try_add_registered`], taking the registrations as explicit
    /// slices rather than reading them off `inventory` - which is what they
    /// really are outside a test, but reading them there would mean a set
    /// built to test one contradiction shares a process-wide registry with
    /// every other test's registrations.
    fn assemble_registered(
        &mut self,
        regs: &[&'static Registered],
        cands: &[&'static Candidate],
        inputs: &[&'static Input],
    ) -> Result<Assembled, Vec<crate::assemble::Diagnostic>> {
        let (plan, problems) = crate::assemble::plan(regs, cands, inputs);
        // A contradiction inside a lane discards that lane, so benchmarks
        // that were written measure nothing - that has to be as loud as any
        // other error, not a field on the returned value that a caller
        // discarding the result never sees. An orphan is different: it means
        // something registered went unused, and everything else still ran.
        let (fatal, warnings): (Vec<_>, Vec<_>) = problems.into_iter().partition(|p| p.is_fatal());
        if !fatal.is_empty() {
            return Err(fatal);
        }

        let cfg = self.cfg.clone();
        let tokens = Assembled { warnings };

        for r in plan.flat {
            (r.reg.add)(&mut *self, &r.name);
        }

        for lane in &plan.lanes {
            for input in &lane.inputs {
                // One generator per input. Multiple candidates clone its
                // values; a singleton uses them directly.
                let make = input.reg.make;
                let mut group = cfg.input_group_make_input(make);
                for c in &lane.candidates {
                    group = (c.reg.add_alt)(group, &c.name);
                }
                let name = if lane.candidates.len() == 1 {
                    let c = lane
                        .candidates
                        .first()
                        .expect("assemble never builds a lane with no candidates");
                    lane.flat_name(c, input)
                } else {
                    lane.comparison_name(input)
                };
                self.add_input_group(&name, group);
            }
        }
        self.lanes = plan.lanes;
        Ok(tokens)
    }
}

/// One typed input axis entry in a [`Group`].
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct TypedInput {
    /// The input's name. Empty for the implicit unit input of a group that
    /// declares none.
    pub name: String,
    /// The input's type, as the source spells it. Empty when there is none.
    pub type_name: String,
}

/// Measurements for one logical group, arranged as a dense candidate-by-input table.
///
/// Rows align with [`Group::candidates`] and columns with [`Group::inputs`].
/// Unsupported candidate/input combinations are `None`.
#[derive(Debug, Clone)]
pub struct Group {
    /// The rows. A candidate that is the baseline of some column comes
    /// first; the rest follow in the order their lanes were declared.
    pub candidates: Vec<String>,
    /// The columns, in the order of the lanes they came from: one type's
    /// inputs together, each lane's inputs as it ordered them - by name,
    /// except that sized inputs go by size.
    pub inputs: Vec<TypedInput>,
    /// Candidate-major rectangular data: `measurements[candidate][input]`.
    /// `None` means that candidate does not support that typed input.
    pub measurements: Vec<Vec<Option<Measurement>>>,
    /// For each column, the row its other cells were compared against, or
    /// `None` when that column compares nothing - a lone candidate has no
    /// baseline. The baseline's own cell is an absolute time; every other
    /// cell in the column carries its [`Difference`] from it.
    ///
    /// A baseline belongs to a column rather than the group because one
    /// group can span several input types, and each type is its own
    /// comparison with its own baseline.
    pub baselines: Vec<Option<usize>>,
    /// What else was computed for each cell, one column per metric name,
    /// laid out like [`Group::measurements`]. Empty when nothing was
    /// computed. A metric's columns are the union of the names across the
    /// group, in the order first met going down the rows and across the
    /// inputs.
    pub metrics: Vec<MetricColumn>,
}

/// Everything a suite measured, keyed by logical group name.
pub struct Report {
    entries: Vec<(String, Found)>,
    groups: BTreeMap<String, Group>,
}

/// One column of a group while it is being assembled.
#[derive(Default)]
struct Column {
    baseline: Option<String>,
    cells: BTreeMap<String, (Measurement, Metrics)>,
}

/// A [`Group`] before its rows and columns are laid out.
#[derive(Default)]
struct GroupBuilder {
    /// In the order first seen.
    candidates: Vec<String>,
    /// In the order first seen, which is the order a lane puts its inputs
    /// in: sorting them again here would undo the lane's ordering of sized
    /// inputs, putting `sets@10` ahead of `sets@2`.
    columns: Vec<(TypedInput, Column)>,
}

impl GroupBuilder {
    fn add(
        &mut self,
        input: TypedInput,
        results: Vec<(String, Measurement, Metrics)>,
        compared: bool,
    ) {
        let at = match self.columns.iter().position(|(seen, _)| *seen == input) {
            Some(at) => at,
            None => {
                self.columns.push((input, Column::default()));
                self.columns.len() - 1
            }
        };
        let column = &mut self.columns[at].1;
        column.baseline = compared
            .then(|| results.first().map(|(name, _, _)| name.clone()))
            .flatten();
        for (candidate, measurement, metrics) in results {
            if !self.candidates.contains(&candidate) {
                self.candidates.push(candidate.clone());
            }
            column.cells.insert(candidate, (measurement, metrics));
        }
    }

    /// One column per metric name met in any cell, taking its unit
    /// from the first cell that has it.
    fn metric_columns(&self) -> Vec<MetricColumn> {
        let mut columns: Vec<MetricColumn> = Vec::new();
        for candidate in &self.candidates {
            for (_, column) in &self.columns {
                let Some((_, metrics)) = column.cells.get(candidate) else {
                    continue;
                };
                for metric in metrics.iter() {
                    if !columns.iter().any(|c| c.name == metric.name) {
                        columns.push(MetricColumn {
                            name: metric.name.clone(),
                            unit: metric.unit,
                            values: Vec::new(),
                        });
                    }
                }
            }
        }
        for column in &mut columns {
            column.values = self
                .candidates
                .iter()
                .map(|candidate| {
                    self.columns
                        .iter()
                        .map(|(_, cells)| {
                            let (_, metrics) = cells.cells.get(candidate)?;
                            Some(metrics.get(&column.name)?.value)
                        })
                        .collect()
                })
                .collect();
        }
        columns
    }

    fn build(mut self) -> Group {
        // Baselines first, so the row every percentage is measured against
        // is at the top. The sort is stable, so everything else keeps its
        // declaration order.
        let columns = &self.columns;
        self.candidates.sort_by_key(|candidate| {
            !columns
                .iter()
                .any(|(_, column)| column.baseline.as_ref() == Some(candidate))
        });
        let inputs = self
            .columns
            .iter()
            .map(|(input, _)| input.clone())
            .collect();
        let baselines = self
            .columns
            .iter()
            .map(|(_, column)| {
                let baseline = column.baseline.as_ref()?;
                self.candidates.iter().position(|c| c == baseline)
            })
            .collect();
        let measurements = self
            .candidates
            .iter()
            .map(|candidate| {
                self.columns
                    .iter()
                    .map(|(_, column)| column.cells.get(candidate).map(|(m, _)| *m))
                    .collect()
            })
            .collect();
        let metrics = self.metric_columns();
        Group {
            candidates: self.candidates,
            inputs,
            measurements,
            baselines,
            metrics,
        }
    }
}

/// The first alternative of `timings` as the only cell, under `name`.
///
/// For an entry that is not a comparison: its one result is the whole
/// cell, whatever the group it ran in called the alternative.
fn only_cell(name: &str, timings: &Timings) -> Vec<(String, Measurement, Metrics)> {
    timings
        .measurements()
        .into_iter()
        .next()
        .map(|(_, measurement, metrics)| (name.to_string(), measurement, metrics))
        .into_iter()
        .collect()
}

impl Report {
    fn new(entries: Vec<(String, Found)>, lanes: &[crate::assemble::Lane]) -> Self {
        let by_name: HashMap<&str, &Found> = entries
            .iter()
            .map(|(name, found)| (name.as_str(), found))
            .collect();
        let mut grouped: BTreeMap<String, GroupBuilder> = BTreeMap::new();
        let mut represented = BTreeSet::new();

        for lane in lanes {
            for input in &lane.inputs {
                let single = lane.candidates.len() == 1;
                let entry_name = if single {
                    lane.flat_name(&lane.candidates[0], input)
                } else {
                    lane.comparison_name(input)
                };
                let Some(found) = by_name.get(entry_name.as_str()) else {
                    continue;
                };
                // A lane's candidates are timed, so a scaling result here is
                // not something a lane produces. If one turns up anyway it is
                // left unclaimed, and so still shown below under its own name
                // rather than dropped.
                let results = match found {
                    Found::Timing(timings) if single => {
                        only_cell(&lane.candidates[0].name, timings)
                    }
                    Found::Timing(timings) => timings.measurements(),
                    Found::Scaling(scaling) if single => vec![(
                        lane.candidates[0].name.clone(),
                        Measurement::Scaling(*scaling),
                        Metrics::new(),
                    )],
                    Found::Scaling(_) => continue,
                };
                grouped.entry(lane.group.to_string()).or_default().add(
                    TypedInput {
                        name: input.name.clone(),
                        type_name: lane.type_name.to_string(),
                    },
                    results,
                    !single,
                );
                represented.insert(entry_name);
            }
        }

        // Whatever no lane claimed: standalone benchmarks, and groups built
        // by hand with `add_input_group`. Each is a group of its own, under
        // its own name, with the unit input.
        for (name, found) in &entries {
            if represented.contains(name) {
                continue;
            }
            let (results, compared) = match found {
                Found::Timing(timings) if timings.timings().len() > 1 => {
                    (timings.measurements(), true)
                }
                Found::Timing(timings) => (only_cell(name, timings), false),
                Found::Scaling(scaling) => (
                    vec![(name.clone(), Measurement::Scaling(*scaling), Metrics::new())],
                    false,
                ),
            };
            grouped
                .entry(name.clone())
                .or_default()
                .add(TypedInput::default(), results, compared);
        }

        let groups = grouped
            .into_iter()
            .map(|(name, builder)| (name, builder.build()))
            .collect();
        Report { entries, groups }
    }
}

impl Report {
    /// The measured groups, in name order.
    pub fn groups(&self) -> impl Iterator<Item = (&str, &Group)> + '_ {
        self.groups
            .iter()
            .map(|(name, group)| (name.as_str(), group))
    }

    /// What every entry is called, in the order they were added.
    ///
    /// The way to find out what a run produced when the names were not
    /// written by hand - a registered benchmark is called after its module
    /// and function, and a group's cell after its group and input.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|(name, _)| name.as_str())
    }

    /// Whether anything was measured under this name.
    pub fn contains(&self, name: &str) -> bool {
        self.entries.iter().any(|(n, _)| n == name)
    }

    /// Retrieve the timings for one group by name.
    ///
    /// `None` if nothing of that name was measured, if it was measured but
    /// is of another type, or if the suite has not run.
    /// ```
    /// use scaling::{runner, Config};
    ///
    /// #[scaling::bench(name = "sum_to_100")]
    /// fn my_benchmark() -> u64 {
    ///     (0..100u64).sum()
    /// }
    ///
    /// let report = runner::measure(&Config::default()).expect("the registrations compose");
    /// let timings = report.get_timings("sum_to_100").expect("it ran");
    /// let timing = timings.timings()[0];
    /// assert!(timing.ns_per_iter > 0.0);
    /// ```
    pub fn get_timings(&self, name: &str) -> Option<Timings> {
        if let Some((_, Found::Timing(timings))) = self.entries.iter().find(|(n, _)| n == name) {
            Some(timings.clone())
        } else {
            None
        }
    }

    /// Retrieve the scalings for one group by name.
    ///
    /// `None` if nothing of that name was measured, if it was measured but
    /// is of another type, or if the suite has not run.
    /// ```
    /// use scaling::{runner, Config};
    ///
    /// #[scaling::bench_scaling(name = "sum_to_100", nmin = 32)]
    /// fn my_benchmark(n: usize) -> u64 {
    ///     (0..n as u64).sum()
    /// }
    ///
    /// let report = runner::measure(&Config::default()).expect("the registrations compose");
    /// let scaling = report.get_scaling("sum_to_100").expect("it ran");
    /// assert!(scaling.iterations > 0);
    /// ```
    pub fn get_scaling(&self, name: &str) -> Option<ScalingStats> {
        if let Some((_, Found::Scaling(scaling))) = self.entries.iter().find(|(n, _)| n == name) {
            Some(*scaling)
        } else {
            None
        }
    }

    /// One measurement, whichever concrete kind it turns out to be - a
    /// single scan over the report's entries, for a caller (formatting
    /// output, say) that would otherwise need one scan per kind it tries in
    /// turn, as [`Report::stats`]/[`Report::scaling`]/[`Report::comparison`]
    /// each do their own.
    pub(crate) fn find(&self, name: &str) -> Option<Found> {
        self.entries
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, found)| found.clone())
    }

    /// A flat benchmark's measurement, by name.
    ///
    /// `None` if that name was something else - a comparison, say - so a
    /// caller that does not know what it is looking at can simply ask.
    pub fn stats(&self, name: &str) -> Option<Timing> {
        match self.find(name)? {
            Found::Timing(c) if c.stats().len() == 1 => Some(c.stats()[0]),
            _ => None,
        }
    }

    /// A scaling benchmark's measurement, by name.
    pub fn scaling(&self, name: &str) -> Option<ScalingStats> {
        match self.find(name)? {
            Found::Scaling(s) => Some(s),
            _ => None,
        }
    }

    /// A comparison's results, by name.
    ///
    /// A group sharing several inputs is reported under `group@input`; one
    /// with just the one, under its own plain name. What comes back carries
    /// every alternative's own measurement as
    /// well as its difference from the baseline, so this is what a script
    /// asking "which of these is actually fastest here" wants.
    pub fn comparison(&self, name: &str) -> Option<Timings> {
        match self.find(name)? {
            Found::Timing(c) if c.stats().len() > 1 => Some(c),
            _ => None,
        }
    }

    /// Every flat measurement, with its name, in the order they were added.
    pub fn all_stats(&self) -> impl Iterator<Item = (&str, Timing)> {
        self.entries.iter().filter_map(|(name, found)| match found {
            Found::Timing(c) if c.stats().len() == 1 => Some((name.as_str(), c.stats()[0])),
            _ => None,
        })
    }

    /// Every comparison, with its name, in the order they were added.
    pub fn all_comparisons(&self) -> impl Iterator<Item = (&str, Timings)> {
        self.entries.iter().filter_map(|(name, found)| match found {
            Found::Timing(c) if c.stats().len() > 1 => Some((name.as_str(), c.clone())),
            _ => None,
        })
    }
}

impl Display for Report {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        let width = self
            .entries
            .iter()
            .map(|(name, _)| name.len())
            .max()
            .unwrap_or(0);
        for (i, (name, found)) in self.entries.iter().enumerate() {
            if i > 0 {
                writeln!(f)?;
            }
            let shown = found.to_string();
            // Trimmed because a multi-line result brings its own trailing
            // newline - `Timings` writes every line with `writeln!` - and
            // this loop supplies the separators itself. Leaving it produced a
            // blank line after any comparison that was not the last entry.
            let shown = shown.trim_end();
            // A comparison prints several lines, so it is given its own
            // block rather than being crammed onto the name's line.
            if shown.contains('\n') {
                write!(f, "{name}:\n{shown}")?;
            } else {
                write!(f, "{name:<width$}  {shown}")?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nothing() -> Found {
        Found::Timing(Timings::test_singleton(Timing {
            ns_per_iter: 0.0,
            std_error: 0.0,
            iterations: 0,
            samples: 0,
            hit_limit: false,
            untrustworthy: false,
            difference: None,
        }))
    }

    /// The budget is spent in poll time, and the benchmark learns about it at
    /// the await rather than by being dropped - so it still gets to return a
    /// result and say that it was cut off.
    #[test]
    fn a_spent_budget_is_reported_at_the_await() {
        let clock = Rc::new(Clock::new(Duration::from_millis(20)));
        let inner = clock.clone();
        let (rounds, ran_out) = block_on(&clock, async move {
            let mut rounds = 0;
            loop {
                std::thread::sleep(Duration::from_millis(5));
                rounds += 1;
                if !inner.yield_now().await {
                    break (rounds, true);
                }
                if rounds > 100 {
                    break (rounds, false);
                }
            }
        });
        assert!(ran_out, "the budget should have run out");
        // 20ms of budget in 5ms bites: four or five, depending on where the
        // check lands relative to the sleep.
        assert!((4..=6).contains(&rounds), "took {rounds} rounds");
        assert!(clock.exhausted());
    }

    /// All three kinds in one suite, interleaved, each reported back under
    /// its own type: this is the whole point of erasing the input type and
    /// reporting through names.
    #[test]
    fn all_three_kinds_share_one_suite() {
        let cfg = Config::default().with_max_time(Duration::from_millis(80));
        let mut suite = cfg.suite();
        suite.add("flat", || (0..50u64).sum::<u64>());
        // A different input type from the comparison below, which is the
        // thing an `InputGroup` alone cannot do.
        suite.add_input("with input", vec![3u8; 32], |v: &mut Vec<u8>| {
            v.iter().map(|&x| x as u64).sum::<u64>()
        });
        suite.add_scaling("scaled", |n| (0..n as u64).sum::<u64>(), 1000);
        suite.add_input_group(
            "pair",
            cfg.input_group()
                .add("a", || (0..50u64).sum::<u64>())
                .add("b", || (0..50u64).sum::<u64>()),
        );
        let report = suite.run();
        println!("{report}");

        assert!(report.stats("flat").is_some());
        assert!(report.stats("with input").is_some());
        assert!(
            report.scaling("scaled").is_some(),
            "the scaling benchmark reported"
        );
        assert_eq!(report.comparison("pair").unwrap().stats().len(), 2);
    }

    #[test]
    fn a_singleton_input_group_accepts_non_clone_inputs() {
        struct NonClone(String);

        let cfg = Config::default().with_max_time(Duration::from_millis(20));
        let mut suite = cfg.suite();
        suite.add_make_input(
            "non-clone",
            || NonClone(String::from("owned input")),
            |input| input.0.len(),
        );
        let stats = suite.run().stats("non-clone").expect("it was measured");
        assert!(stats.ns_per_iter > 0.0);
    }

    /// The table is in declaration order, not alphabetical and not whatever
    /// order the benchmarks happened to finish in.
    #[test]
    fn the_report_is_in_declaration_order() {
        let cfg = Config::default().with_max_time(Duration::from_millis(20));
        let mut suite = cfg.suite();
        // Declared in an order that is neither alphabetical nor the order
        // they will finish in - "zebra" is the cheapest and finishes first.
        suite.add("middle", || (0..200u64).sum::<u64>());
        suite.add("zebra", || 1u64 + 1);
        suite.add("apple", || (0..400u64).sum::<u64>());
        let shown = format!("{}", suite.run());
        let names: Vec<&str> = shown
            .lines()
            .map(|l| l.split_whitespace().next().unwrap())
            .collect();
        assert_eq!(names, ["middle", "zebra", "apple"], "{shown}");
    }

    /// The suite counts its own comparisons and corrects for exactly that
    /// many, with nothing promised in advance.
    #[test]
    fn the_suite_computes_its_own_threshold() {
        let cfg = Config::default().with_max_time(Duration::from_millis(20));
        let mut suite = cfg.suite();
        // Three alternatives is two comparisons; two alternatives is one.
        suite.add_input_group(
            "three",
            cfg.input_group()
                .add("a", || 1u64 + 1)
                .add("b", || 1u64 + 1)
                .add("c", || 1u64 + 1),
        );
        suite.add_input_group(
            "two",
            cfg.input_group()
                .add("a", || 1u64 + 1)
                .add("b", || 1u64 + 1),
        );
        assert_eq!(suite.comparisons, 3);
        // The cell every comparison in this suite reads from.
        let z = suite.z_alpha.clone();
        suite.run();
        assert_eq!(z.get(), Config::z_alpha_for(3));
    }

    /// Two suites off one `Config` each correct for themselves alone.
    ///
    /// The `Config` carries no count now, so a second suite cannot inherit or
    /// disturb the first one's threshold - which was the whole failure mode
    /// the old shared plan had to be careful about.
    #[test]
    fn each_suite_corrects_for_itself_alone() {
        let cfg = Config::default().with_max_time(Duration::from_millis(20));
        for _ in 0..2 {
            let mut suite = cfg.suite();
            suite.add_input_group(
                "pair",
                cfg.input_group()
                    .add("a", || 1u64 + 1)
                    .add("b", || 1u64 + 1),
            );
            let z = suite.z_alpha.clone();
            suite.run();
            // One comparison each time, not two accumulating across suites.
            assert_eq!(z.get(), Config::z_alpha_for(1));
        }
    }

    /// The suite's threshold must actually *reach* the comparisons it holds.
    ///
    /// It travels by a different route from everything else here - a shared
    /// cell the comparisons read on their first poll, because they are boxed
    /// before the suite knows its own size. If that read ever happened before
    /// `run` filled the cell, every comparison would be judged against `NaN`:
    /// nothing would be significant, each would sample until its budget ran
    /// out, and the report would look like a pile of honest "unchanged"
    /// results. Nothing else in this file would fail, so check it here.
    #[test]
    fn the_threshold_reaches_every_comparison() {
        let cfg = Config::default().with_max_time(Duration::from_millis(20));
        let mut suite = cfg.suite();
        suite.add_input_group(
            "pair",
            cfg.input_group()
                .add("a", || 1u64 + 1)
                .add("b", || 1u64 + 1),
        );
        suite.add_input_group(
            "trio",
            cfg.input_group()
                .add("a", || 1u64 + 1)
                .add("b", || 1u64 + 1)
                .add("c", || 1u64 + 1),
        );
        let report = suite.run();
        for name in ["pair", "trio"] {
            for (alt, cmp) in report.comparison(name).unwrap().against_baseline() {
                assert!(
                    cmp.min_detectable_difference().is_finite(),
                    "{alt} was judged against a NaN threshold",
                );
            }
        }
    }

    /// A comparison that is not the last entry must not leave a blank line
    /// behind it: `Timings` ends its own output with a newline, and this
    /// loop supplies the separators.
    ///
    /// A blank line is not merely untidy - anything parsing the table a line
    /// at a time meets an empty one, as this module's own declaration-order
    /// test would.
    #[test]
    fn a_comparison_before_another_entry_leaves_no_blank_line() {
        let cfg = Config::default().with_max_time(Duration::from_millis(20));
        let mut suite = cfg.suite();
        suite.add_input_group(
            "pair",
            cfg.input_group()
                .add("a", || 1u64 + 1)
                .add("b", || 1u64 + 1),
        );
        suite.add("flat", || (0..20u64).sum::<u64>());
        let shown = format!("{}", suite.run());
        assert!(
            !shown.lines().any(|l| l.trim().is_empty()),
            "blank line in report:\n{shown}"
        );
        assert!(shown.lines().last().unwrap().starts_with("flat"), "{shown}");
    }

    /// A singleton group produces ordinary stats without a difference against
    /// its own baseline.
    #[test]
    fn one_alternative_runs_without_a_comparison() {
        let cfg = Config::default().with_max_time(Duration::from_millis(20));
        let mut suite = cfg.suite();
        suite.add_input_group("lonely", cfg.input_group().add("only", || 1u64 + 1));
        let stats = suite.run().stats("lonely").expect("it was measured");
        assert!(stats.ns_per_iter > 0.0);
    }

    /// An empty suite must run and report nothing, rather than dividing by
    /// zero picking a starting position.
    #[test]
    fn an_empty_suite_runs() {
        let cfg = Config::default();
        let suite = cfg.suite();
        assert!(suite.is_empty());
        assert_eq!(format!("{}", suite.run()), "");
    }

    /// Time spent by *other* benchmarks must not count against this one, or a
    /// large suite would exhaust every budget in its first round.
    #[test]
    fn one_benchmark_s_budget_ignores_the_others() {
        let idle = Rc::new(Clock::new(Duration::from_millis(50)));
        let busy = Rc::new(Clock::new(Duration::from_millis(50)));
        let mut s = Scheduler::new(7);
        {
            let idle = idle.clone();
            s.push(
                idle.clone(),
                Box::pin(async move {
                    for _ in 0..5 {
                        idle.yield_now().await;
                    }
                    nothing()
                }),
            );
        }
        {
            let busy = busy.clone();
            s.push(
                busy.clone(),
                Box::pin(async move {
                    for _ in 0..5 {
                        std::thread::sleep(Duration::from_millis(4));
                        busy.yield_now().await;
                    }
                    nothing()
                }),
            );
        }
        s.run();
        assert!(
            busy.spent() >= Duration::from_millis(20),
            "busy spent {:?}",
            busy.spent()
        );
        assert!(
            idle.spent() < Duration::from_millis(5),
            "idle was charged for its neighbour: {:?}",
            idle.spent()
        );
    }
}

#[cfg(test)]
mod report_lookup {
    use super::*;
    use std::time::Duration;

    fn cfg() -> Config {
        Config::default().with_max_time(Duration::from_millis(20))
    }

    /// A measurement can be had from a finished report by name alone - which
    /// is the whole point: under `try_add_registered` nobody wrote the `add`
    /// call, so a script that wants to ask something of the results has only
    /// the report.
    #[test]
    fn a_measurement_can_be_had_by_name() {
        let cfg = cfg();
        let mut suite = cfg.suite();
        suite.add("summing", || (0..64u64).sum::<u64>());
        let report = suite.run();

        let stats = report.stats("summing").expect("it was measured");
        assert!(stats.ns_per_iter > 0.0);
        assert!(report.contains("summing"));
        assert_eq!(report.names().collect::<Vec<_>>(), ["summing"]);
    }

    #[test]
    fn typed_lanes_combine_into_one_dense_group() {
        let timing = |ns_per_iter| Timing {
            ns_per_iter,
            std_error: 0.0,
            iterations: 1,
            samples: 2,
            hit_limit: false,
            untrustworthy: false,
            difference: None,
        };
        use crate::assemble::lane_tests::{cand, inp, leak_c, leak_i};
        let candidates = leak_c(vec![
            cand::<u32>("codec", "fast", "u32", true),
            cand::<u32>("codec", "small", "u32", false),
            cand::<i32>("codec", "small", "i32", true),
        ]);
        let inputs = leak_i(vec![
            inp::<u32>("codec", "random", "u32"),
            inp::<i32>("codec", "random", "i32"),
        ]);
        let (plan, problems) = crate::assemble::plan(&[], &candidates, &inputs);
        assert!(problems.is_empty(), "{problems:?}");
        let entries: Vec<(String, Found)> = plan
            .lanes
            .iter()
            .map(|lane| {
                let input = &lane.inputs[0];
                let name = if lane.candidates.len() == 1 {
                    lane.flat_name(&lane.candidates[0], input)
                } else {
                    lane.comparison_name(input)
                };
                let names: Vec<&str> = lane.candidates.iter().map(|c| c.name.as_str()).collect();
                let timings: Vec<Timing> = names
                    .iter()
                    .map(|candidate| timing(if *candidate == "fast" { 20.0 } else { 5.0 }))
                    .collect();
                (name, Found::Timing(Timings::test_named(&names, &timings)))
            })
            .collect();
        let report = Report::new(entries, &plan.lanes);
        let (group_name, group) = report
            .groups()
            .next()
            .expect("the report contains the codec group");
        assert_eq!(group_name, "codec");
        assert_eq!(group.candidates, ["fast", "small"]);
        assert_eq!(group.inputs.len(), 2);
        assert_eq!(group.measurements.len(), 2);
        assert_eq!(group.measurements[0].len(), 2);
        assert!(group.measurements[0][0].is_none());
        assert!(group.measurements[0][1].is_some());
        assert!(group.measurements[1][0].is_some());
        assert!(group.measurements[1][1].is_some());
        // Only the `u32` column is a comparison, so only it has a baseline,
        // and that baseline is the row that was put first.
        assert_eq!(group.baselines, [None, Some(0)]);
    }

    /// A comparison reaches the group as one cell per candidate, each of the
    /// others carrying its difference from the baseline.
    #[test]
    fn a_comparison_lane_keeps_every_candidate_and_its_difference() {
        let timing = |ns_per_iter: f64| Timing {
            ns_per_iter,
            std_error: ns_per_iter * 0.01,
            iterations: 1,
            samples: 2,
            hit_limit: false,
            untrustworthy: false,
            difference: None,
        };
        use crate::assemble::lane_tests::{cand, inp, leak_c, leak_i};
        let candidates = leak_c(vec![
            // Named so that the baseline is neither first alphabetically
            // nor last.
            cand::<u32>("sorting", "mid", "u32", true),
            cand::<u32>("sorting", "alpha", "u32", false),
            cand::<u32>("sorting", "zed", "u32", false),
        ]);
        let inputs = leak_i(vec![inp::<u32>("sorting", "reversed", "u32")]);
        let (plan, problems) = crate::assemble::plan(&[], &candidates, &inputs);
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(plan.lanes.len(), 1);
        let lane = &plan.lanes[0];
        let baseline = timing(10.0);
        let with_difference = |ns| {
            let mut t = timing(ns);
            t.difference = Some(crate::Difference::from_parts(&baseline, &t, 0.5, 0.12));
            t
        };
        let names: Vec<&str> = lane.candidates.iter().map(|c| c.name.as_str()).collect();
        let timings: Vec<Timing> = names
            .iter()
            .map(|n| {
                if *n == "mid" {
                    baseline
                } else {
                    with_difference(20.0)
                }
            })
            .collect();
        let entry = (
            lane.comparison_name(&lane.inputs[0]),
            Found::Timing(Timings::test_named(&names, &timings)),
        );
        let report = Report::new(vec![entry], &plan.lanes);
        let (name, group) = report.groups().next().expect("the sorting group");
        assert_eq!(name, "sorting");
        assert_eq!(group.candidates, ["mid", "alpha", "zed"]);
        assert_eq!(group.baselines, [Some(0)]);
        let cell = |row: usize| match group.measurements[row][0] {
            Some(Measurement::Timing(t)) => t,
            other => panic!("expected a timing, got {other:?}"),
        };
        assert!(cell(0).difference.is_none(), "the baseline has none");
        assert!(cell(1).difference.is_some());
        assert!(cell(2).difference.is_some());
    }

    /// The columns of a group read in the order the lane put them in, so
    /// inputs registered at several sizes run `2`, `10`, `100` and not
    /// `10`, `100`, `2`.
    #[test]
    fn a_groups_columns_keep_the_lanes_order() {
        let timing = |ns_per_iter: f64| Timing {
            ns_per_iter,
            std_error: 0.0,
            iterations: 1,
            samples: 2,
            hit_limit: false,
            untrustworthy: false,
            difference: None,
        };
        use crate::assemble::lane_tests::{cand, inp, leak_c, leak_i};
        let candidates = leak_c(vec![
            cand::<u8>("sets", "a", "u8", true),
            cand::<u8>("sets", "b", "u8", false),
        ]);
        let inputs = leak_i(vec![
            inp::<u8>("sets", "sets@10", "u8"),
            inp::<u8>("sets", "sets@2", "u8"),
            inp::<u8>("sets", "sets@100", "u8"),
        ]);
        let (plan, problems) = crate::assemble::plan(&[], &candidates, &inputs);
        assert!(problems.is_empty(), "{problems:?}");
        let lane = &plan.lanes[0];
        let entries = lane
            .inputs
            .iter()
            .map(|input| {
                let timings = Timings::test_named(&["a", "b"], &[timing(1.0), timing(2.0)]);
                (lane.comparison_name(input), Found::Timing(timings))
            })
            .collect();
        let report = Report::new(entries, &plan.lanes);
        let (_, group) = report.groups().next().expect("the sets group");
        let names: Vec<&str> = group.inputs.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, ["sets@2", "sets@10", "sets@100"]);
    }

    /// A group built by hand, which no lane knows about, keeps all of its
    /// alternatives rather than only the baseline.
    #[test]
    fn a_hand_built_group_is_not_reduced_to_its_baseline() {
        let cfg = Config::relative(0.05).with_max_time(Duration::from_millis(30));
        let mut suite = cfg.suite();
        suite.add_input_group(
            "sorting@reversed",
            cfg.input_group()
                .add("stable", || (0..64u64).sum::<u64>())
                .add("unstable", || (0..640u64).sum::<u64>()),
        );
        suite.add("lonely", || (0..64u64).sum::<u64>());
        let report = suite.run();
        let groups: Vec<_> = report.groups().collect();
        assert_eq!(groups.len(), 2);
        let (name, group) = groups[0];
        assert_eq!(name, "lonely");
        assert_eq!(group.candidates, ["lonely"]);
        assert_eq!(group.baselines, [None]);
        let (name, group) = groups[1];
        assert_eq!(name, "sorting@reversed");
        assert_eq!(group.candidates, ["stable", "unstable"]);
        assert_eq!(group.baselines, [Some(0)]);
    }

    /// Asking for the wrong type gives nothing rather than the wrong thing,
    /// so a caller that does not know what a name refers to can simply ask.
    #[test]
    fn asking_for_the_wrong_type_gives_nothing() {
        let cfg = cfg();
        let mut suite = cfg.suite();
        suite.add("flat", || (0..64u64).sum::<u64>());
        suite.add_input_group(
            "pair",
            cfg.input_group()
                .add("a", || (0..64u64).sum::<u64>())
                .add("b", || (0..64u64).sum::<u64>()),
        );
        let report = suite.run();

        assert!(report.stats("flat").is_some());
        assert!(
            report.comparison("flat").is_none(),
            "a flat benchmark is not a comparison",
        );
        assert!(report.comparison("pair").is_some());
        assert!(
            report.stats("pair").is_none(),
            "a comparison is not a flat benchmark",
        );
        assert!(report.stats("never added").is_none());
    }

    /// Each kind comes back as itself, from one report holding all three.
    #[test]
    fn every_kind_of_result_can_be_recovered() {
        let cfg = cfg();
        let mut suite = cfg.suite();
        suite.add("flat", || (0..64u64).sum::<u64>());
        suite.add_scaling("scaled", |n: usize| (0..n as u64).sum::<u64>(), 32);
        suite.add_input_group(
            "pair",
            cfg.input_group()
                .add("a", || (0..64u64).sum::<u64>())
                .add("b", || (0..64u64).sum::<u64>()),
        );
        let report = suite.run();

        assert!(report.stats("flat").is_some());
        assert!(report.scaling("scaled").is_some());
        let cmp = report.comparison("pair").expect("the comparison ran");
        assert_eq!(cmp.stats().len(), 2);
    }

    /// Iterating one kind skips the others rather than failing on them,
    /// which is what makes it usable on a report holding a mixture.
    #[test]
    fn iterating_one_kind_skips_the_rest() {
        let cfg = cfg();
        let mut suite = cfg.suite();
        suite.add("one", || (0..64u64).sum::<u64>());
        suite.add("two", || (0..64u64).sum::<u64>());
        suite.add_input_group(
            "pair",
            cfg.input_group()
                .add("a", || (0..64u64).sum::<u64>())
                .add("b", || (0..64u64).sum::<u64>()),
        );
        let report = suite.run();

        let flat: Vec<&str> = report.all_stats().map(|(n, _)| n).collect();
        assert_eq!(flat, ["one", "two"], "the comparison is not a `Stats`");
        let cmps: Vec<&str> = report.all_comparisons().map(|(n, _)| n).collect();
        assert_eq!(cmps, ["pair"]);
    }

    /// The use this exists for: a script asking whether the implementation
    /// being shipped is really the best one here.
    ///
    /// Nothing in it holds a token, and nothing in it knows the names in
    /// advance - it finds the comparison, reads every alternative's own
    /// measurement out of it, and decides.
    #[test]
    fn a_script_can_ask_which_alternative_is_actually_fastest() {
        let cfg = cfg();
        let mut suite = cfg.suite();
        suite.add_input_group(
            "hashing",
            cfg.input_group()
                // The one we ship, and a deliberately slower rival.
                .add("shipped", || (0..64u64).sum::<u64>())
                .add("rival", || (0..512u64).sum::<u64>()),
        );
        let report = suite.run();

        let cmp = report
            .all_comparisons()
            .find(|(name, _)| *name == "hashing")
            .map(|(_, c)| c)
            .expect("a comparison called `hashing`");

        let fastest = cmp
            .names()
            .zip(cmp.stats())
            .min_by(|a, b| {
                a.1.ns_per_iter
                    .partial_cmp(&b.1.ns_per_iter)
                    .expect("no NaN timings")
            })
            .map(|(name, _)| name)
            .expect("at least one alternative");
        assert_eq!(
            fastest, "shipped",
            "the shipped implementation should be the quick one here",
        );
    }
}

#[cfg(test)]
mod comparison_config {
    use super::*;
    use std::time::Duration;

    /// A comparison is measured against the `Config` it was built from, on
    /// every axis.
    ///
    /// The accuracy goal already came from the set's own `Config`; the clock
    /// used to be sized from the suite's, so a set built from a different
    /// `Config` chased one target on the other's budget.
    ///
    /// `hit_limit` cannot see this - an unreachable goal ends at the budget
    /// whichever budget it was - so what is measured is the elapsed time,
    /// where the two differ by a hundredfold.
    #[test]
    fn a_comparison_follows_its_own_config() {
        let generous = Duration::from_secs(2);
        let cfg = Config::default().with_max_time(generous);
        let stingy = Config::relative(1e-9).with_max_time(Duration::from_millis(10));
        let mut suite = cfg.suite();
        suite.add_input_group(
            "starved",
            stingy
                .input_group()
                .add("a", || (0..50u64).sum::<u64>())
                .add("b", || (0..50u64).sum::<u64>()),
        );
        let _held = crate::quiet::exclusive();
        let started = Instant::now();
        let report = suite.run();
        let elapsed = started.elapsed();
        assert!(
            report
                .comparison("starved")
                .unwrap()
                .stats()
                .iter()
                .any(|s| s.hit_limit),
            "an unreachable goal must end at the budget",
        );
        assert!(
            elapsed < generous,
            "the comparison spent the suite's budget, not its own: {elapsed:?}",
        );
    }

    /// Comparison groups can use different configs, but the family-wise
    /// threshold still counts every comparison in the suite.
    #[test]
    fn comparison_configs_do_not_change_the_suite_threshold() {
        let cfg = Config::default().with_max_time(Duration::from_millis(20));
        let other = Config::relative(0.5).with_max_time(Duration::from_millis(20));
        let mut suite = cfg.suite();
        suite.add_input_group(
            "one",
            cfg.input_group()
                .add("a", || (0..32u64).sum::<u64>())
                .add("b", || (0..32u64).sum::<u64>()),
        );
        suite.add_input_group(
            "two",
            other
                .input_group()
                .add("a", || (0..32u64).sum::<u64>())
                .add("b", || (0..32u64).sum::<u64>()),
        );
        let z = suite.z_alpha.clone();
        suite.run();
        assert_eq!(
            z.get(),
            Config::z_alpha_for(2),
            "the limit counts the suite's comparisons, whatever config each \
             comparison group uses",
        );
    }
}

/// Discovering benchmarks that were never assembled by hand.
///
/// Registrations here are written by hand rather than by the macros - the
/// same shims `#[scaling::bench]` and friends would generate against
/// [`crate::registry::Suite`], written out so the registry/assembly path is
/// proved independently of the macro crate. `tests/macros.rs` checks that
/// the macros produce the same thing from an attribute, so the two are
/// worth keeping side by side: if one passes and the other fails, the fault
/// is in the macro rather than in the registry.
///
/// Every `#[cfg(test)]` module in this crate shares one process-wide
/// `inventory` registry, so names here are prefixed `e2e::` and nothing
/// below asserts a raw [`Suite::len`] - only that its own tokens, found by
/// name, have real answers. A `tests/*.rs` integration test would not need
/// this care, since each file there is its own binary; a module here is not.
#[cfg(test)]
mod registered_by_hand {
    use super::*;
    use crate::registry::{Candidate, ErasedInput, Input};
    use std::any::TypeId;
    use std::time::Duration;

    fn work(n: usize) -> u64 {
        (0..n as u64).fold(0u64, |a, x| a.wrapping_mul(31).wrapping_add(x))
    }

    fn add_flat(adder: &mut Suite, name: &str) {
        adder.add(name, || work(200));
    }

    inventory::submit! {
        Registered {
            name: "e2e::flat",
            crate_name: env!("CARGO_PKG_NAME"),
            crate_version: env!("CARGO_PKG_VERSION"),
            add: add_flat,
        }
    }

    fn add_scaling(adder: &mut Suite, name: &str) {
        // `nmin` is baked in here, since the shim signature has nowhere to
        // pass it - which is the whole reason it must be a literal at the
        // macro.
        adder.add_scaling(name, |n: usize| work(n), 32);
    }

    inventory::submit! {
        Registered {
            name: "e2e::scaling",
            crate_name: env!("CARGO_PKG_NAME"),
            crate_version: env!("CARGO_PKG_VERSION"),
            add: add_scaling,
        }
    }

    // A comparison group: three candidates and one shared input, each
    // registered independently and none of them naming the others.

    fn make_input() -> ErasedInput {
        ErasedInput::new((0..600u64).collect::<Vec<u64>>())
    }

    inventory::submit! {
        Input {
            groups: &["e2e-sort"],
            name: "data",
            crate_name: env!("CARGO_PKG_NAME"),
            crate_version: env!("CARGO_PKG_VERSION"),
            type_id: TypeId::of::<Vec<u64>>,
            type_name: "Vec<u64>",
            make: make_input,
        }
    }

    fn alt_baseline(set: InputGroup<ErasedInput>, name: &str) -> InputGroup<ErasedInput> {
        set.add_input(name, |e: &mut ErasedInput| {
            let v = e.get_mut::<Vec<u64>>();
            v.sort();
            v.len()
        })
    }

    fn alt_unstable(set: InputGroup<ErasedInput>, name: &str) -> InputGroup<ErasedInput> {
        set.add_input(name, |e: &mut ErasedInput| {
            let v = e.get_mut::<Vec<u64>>();
            v.sort_unstable();
            v.len()
        })
    }

    /// Deliberately slower, so the comparison has something real to find.
    fn alt_slow(set: InputGroup<ErasedInput>, name: &str) -> InputGroup<ErasedInput> {
        set.add_input(name, |e: &mut ErasedInput| {
            let v = e.get_mut::<Vec<u64>>();
            v.sort();
            v.sort_unstable();
            v.sort();
            v.len()
        })
    }

    inventory::submit! {
        Candidate {
            groups: &["e2e-sort"],
            name: "e2e::sort_stable",
            input_type: TypeId::of::<Vec<u64>>,
            input_type_name: "Vec<u64>",
            is_baseline: true,
            crate_name: env!("CARGO_PKG_NAME"),
            crate_version: env!("CARGO_PKG_VERSION"),
            add_alt: alt_baseline,
        }
    }

    inventory::submit! {
        Candidate {
            groups: &["e2e-sort"],
            name: "e2e::sort_unstable",
            input_type: TypeId::of::<Vec<u64>>,
            input_type_name: "Vec<u64>",
            is_baseline: false,
            crate_name: env!("CARGO_PKG_NAME"),
            crate_version: env!("CARGO_PKG_VERSION"),
            add_alt: alt_unstable,
        }
    }

    inventory::submit! {
        Candidate {
            groups: &["e2e-sort"],
            name: "e2e::sort_thrice",
            input_type: TypeId::of::<Vec<u64>>,
            input_type_name: "Vec<u64>",
            is_baseline: false,
            crate_name: env!("CARGO_PKG_NAME"),
            crate_version: env!("CARGO_PKG_VERSION"),
            add_alt: alt_slow,
        }
    }

    /// Everything above is found and measured, without one line listing it.
    #[test]
    fn a_suite_discovers_what_was_registered() {
        let cfg = Config::default().with_max_time(Duration::from_millis(60));
        // Restricted to this module's own names - see `versions_and_rivals`'s
        // `run` for why: one process-wide registry, shared with every other
        // `#[cfg(test)]` module in the crate.
        let mut suite = cfg.suite();
        suite.try_add_registered().unwrap();
        let report = suite.run();

        let flat = report.stats("e2e::flat").expect("the flat benchmark ran");
        assert!(flat.ns_per_iter > 0.0);

        let scaling = report
            .scaling("e2e::scaling")
            .expect("the scaling benchmark ran");
        assert!(scaling.iterations > 0);

        let cmps = report
            .comparison("e2e-sort@data")
            .expect("the comparison ran");
        // Three alternatives, two of them reported against the baseline.
        assert_eq!(cmps.stats().len(), 3);
        assert_eq!(cmps.against_baseline().count(), 2);

        // Everything appears in the report, under the name it registered with.
        let shown = format!("{report}");
        for name in ["e2e::flat", "e2e::scaling", "e2e-sort@data"] {
            assert!(shown.contains(name), "{name} missing from report:\n{shown}");
        }
    }

    /// The baseline is the one that said so, not the one that happened to be
    /// registered or sorted first.
    ///
    /// `sort_stable` is neither: `sort_thrice` and `sort_unstable` both sort
    /// before it alphabetically. So if this passes, the `is_baseline` flag is
    /// what decided, which is the only thing that can decide when registrations
    /// have no order.
    #[test]
    fn the_declared_baseline_is_the_one_used() {
        let cfg = Config::default().with_max_time(Duration::from_millis(60));
        let mut suite = cfg.suite();
        suite.try_add_registered().unwrap();
        let report = suite.run();

        let cmps = report.comparison("e2e-sort@data").unwrap();
        let against: Vec<&str> = cmps.against_baseline().map(|(name, _)| name).collect();
        assert!(
            !against.contains(&"e2e::sort_stable"),
            "the baseline must not be reported against itself: {against:?}",
        );
        assert_eq!(against.len(), 2);
        assert!(against.contains(&"e2e::sort_unstable"), "{against:?}");
        assert!(against.contains(&"e2e::sort_thrice"), "{against:?}");
    }

    /// Discovered benchmarks and hand-added ones share one suite and are
    /// measured together, which is the whole claim of the hybrid design.
    #[test]
    fn registered_and_hand_added_benchmarks_mix() {
        let cfg = Config::default().with_max_time(Duration::from_millis(60));
        let mut suite = cfg.suite();
        suite.add("e2e::by_hand", || work(150));
        suite.try_add_registered().unwrap();
        suite.add("e2e::after", || work(150));
        let report = suite.run();

        assert!(
            report.stats("e2e::by_hand").is_some(),
            "the hand-added one ran"
        );
        assert!(
            report.stats("e2e::after").is_some(),
            "so did the one added afterwards"
        );
        assert!(
            report.stats("e2e::flat").is_some(),
            "so did the registered one"
        );

        let shown = format!("{report}");
        assert!(shown.contains("e2e::by_hand"), "{shown}");
        assert!(shown.contains("e2e::flat"), "{shown}");
    }

    /// Results are recoverable from the report even when nobody ever held a
    /// token - which is the situation `try_add_registered` always
    /// creates, since nothing wrote the `add` call that would have returned
    /// one.
    #[test]
    fn registered_results_are_recoverable_from_the_report_alone() {
        let cfg = Config::default().with_max_time(Duration::from_millis(30));
        let mut suite = cfg.suite();
        // Deliberately thrown away: a script driving a benchmark binary has no
        // way to get hold of these.
        drop(suite.try_add_registered().unwrap());
        let report = suite.run();

        let stats = report
            .stats("e2e::flat")
            .expect("the flat benchmark's measurement comes back");
        assert!(stats.ns_per_iter > 0.0);

        let cmp = report
            .comparison("e2e-sort@data")
            .expect("the comparison comes back too");
        assert_eq!(cmp.stats().len(), 3);

        // And the scaling benchmark, which is a third type again.
        assert!(report.scaling("e2e::scaling").is_some());
    }

    /// The question the lookup exists to answer: is the implementation being
    /// shipped really the best one under these conditions?
    ///
    /// Nothing here holds a token and nothing knows a name in advance.
    #[test]
    fn a_script_can_check_which_registered_alternative_wins() {
        let cfg = Config::default().with_max_time(Duration::from_millis(30));
        let mut suite = cfg.suite();
        drop(suite.try_add_registered().unwrap());
        let report = suite.run();

        let (_, cmp) = report
            .all_comparisons()
            .find(|(name, _)| *name == "e2e-sort@data")
            .expect("the sorting comparison");

        let slowest = cmp
            .names()
            .zip(cmp.stats())
            .max_by(|a, b| {
                a.1.ns_per_iter
                    .partial_cmp(&b.1.ns_per_iter)
                    .expect("no NaN timings")
            })
            .map(|(name, _)| name)
            .expect("at least one alternative");
        assert_eq!(
            slowest, "e2e::sort_thrice",
            "the one that sorts three times should be the slow one",
        );
    }
}

/// What happens when registrations do not make sense together.
///
/// Built entirely from local, non-registered values passed straight to
/// [`Suite::assemble_registered`] rather than through `inventory::submit!`:
/// a registry covers everything linked into one binary, and every
/// `#[cfg(test)]` module in this crate shares that binary, so deliberately
/// broken registrations cannot go through the real global registry without
/// poisoning every other test that calls `try_add_registered`
/// unfiltered.
#[cfg(test)]
mod bad_registrations {
    use super::*;
    use crate::registry::{Candidate, ErasedInput};

    fn add(adder: &mut Suite, name: &str) {
        adder.add(name, || (0..16u64).sum::<u64>());
    }

    fn alt(set: InputGroup<ErasedInput>, name: &str) -> InputGroup<ErasedInput> {
        set.add_input(name, |_| ())
    }

    // Two registrations under one name, which a report could not tell apart.
    static COLLIDES_1: Registered = Registered {
        name: "collides",
        crate_name: "testcrate",
        crate_version: "1.0.0",
        add,
    };
    static COLLIDES_2: Registered = Registered {
        name: "collides",
        crate_name: "testcrate",
        crate_version: "1.0.0",
        add,
    };

    // A comparison group where two different candidates both claim the
    // baseline. Order cannot decide this, since registrations have none.
    static TWO_BASELINES_A: Candidate = Candidate {
        groups: &["two-baselines"],
        name: "two_baselines_a",
        input_type: std::any::TypeId::of::<()>,
        input_type_name: "()",
        is_baseline: true,
        crate_name: "testcrate",
        crate_version: "1.0.0",
        add_alt: alt,
    };
    static TWO_BASELINES_B: Candidate = Candidate {
        groups: &["two-baselines"],
        name: "two_baselines_b",
        input_type: std::any::TypeId::of::<()>,
        input_type_name: "()",
        is_baseline: true,
        crate_name: "testcrate",
        crate_version: "1.0.0",
        add_alt: alt,
    };

    /// Both problems are reported together, and nothing is added.
    ///
    /// Reporting every complaint at once is what stops fixing a set of
    /// registrations from being one rebuild per mistake, and it is only
    /// possible because they are all found before anything runs.
    #[test]
    fn bad_registrations_are_reported_together_and_nothing_is_added() {
        let cfg = Config::default();
        let mut suite = cfg.suite();
        let regs = [&COLLIDES_1, &COLLIDES_2];
        let cands = [&TWO_BASELINES_A, &TWO_BASELINES_B];
        let problems = suite
            .assemble_registered(&regs, &cands, &[])
            .expect_err("these registrations contradict each other");

        assert_eq!(problems.len(), 2, "{problems:?}");
        assert!(
            problems
                .iter()
                .any(|p| matches!(p, crate::assemble::Diagnostic::DuplicateName { name, .. } if name == "collides")),
            "{problems:?}",
        );
        assert!(
            problems.iter().any(
                |p| matches!(p, crate::assemble::Diagnostic::ManyBaselines { group, .. } if group == "two-baselines")
            ),
            "{problems:?}",
        );

        // Nothing half-added: the checks all run before the first benchmark is
        // handed to the suite, so a rejected set leaves no trace in it.
        assert!(
            suite.is_empty(),
            "a rejected set of registrations must not leave anything behind",
        );
    }
}

/// Registrations arriving from more than one crate, or more than one version
/// of one crate.
///
/// This is what happens when a crate pulls an older copy of itself, or a
/// rival crate, in as a dev-dependency with registrations enabled: both
/// register, and both use the same names for the same ideas, because they
/// *are* the same source a version apart.
///
/// Registrations are written by hand here rather than by the macros, because
/// the macros necessarily stamp every registration with *this* crate's name
/// and version - there is no second crate to register from. Writing them out
/// is the only way to have two origins present at once. The matrix name
/// `mixing` is unique to this module, so its registrations cannot be
/// confused with any other `#[cfg(test)]` module's despite sharing one
/// process-wide registry.
#[cfg(test)]
mod versions_and_rivals {
    // These registrations are written by hand, so they do not get the
    // `allow(clippy::ptr_arg)` that a `group`-bearing `#[scaling::bench]`
    // puts on what it emits. The reason for it is the same: a benchmark's argument type is
    // the input type the registry keys it on, not a borrow chosen for
    // convenience, so taking `&mut [u64]` instead would change what is
    // registered.
    #![allow(clippy::ptr_arg)]

    use super::*;
    use crate::registry::{Candidate, ErasedInput, Input};
    use std::any::TypeId;

    fn work(v: &[u64], rounds: usize) -> u64 {
        let mut acc = 0u64;
        for _ in 0..rounds {
            acc = v
                .iter()
                .fold(acc, |a, x| a.wrapping_mul(31).wrapping_add(*x));
        }
        acc
    }

    // The current crate's implementation, and the same function a version
    // back. They differ in speed so the comparison has something to find.
    fn mix_new(v: &mut Vec<u64>) -> u64 {
        work(v, 1)
    }
    fn mix_old(v: &mut Vec<u64>) -> u64 {
        work(v, 3)
    }

    fn add_alt_new(set: InputGroup<ErasedInput>, name: &str) -> InputGroup<ErasedInput> {
        set.add_input(name, |e: &mut ErasedInput| mix_new(e.get_mut::<Vec<u64>>()))
    }
    fn add_alt_old(set: InputGroup<ErasedInput>, name: &str) -> InputGroup<ErasedInput> {
        set.add_input(name, |e: &mut ErasedInput| mix_old(e.get_mut::<Vec<u64>>()))
    }

    fn make_data() -> ErasedInput {
        ErasedInput::new((0..300u64).rev().collect::<Vec<u64>>())
    }

    // Both versions call the function `mix`, and both call themselves the
    // baseline - because they are the same line of source, a version apart.
    inventory::submit! {
        Candidate {
            groups: &["mixing"],
            name: "mix",
            input_type: TypeId::of::<Vec<u64>>,
            input_type_name: "Vec<u64>",
            is_baseline: true,
            crate_name: "mycrate",
            crate_version: "0.9.0",
            add_alt: add_alt_new,
        }
    }

    inventory::submit! {
        Candidate {
            groups: &["mixing"],
            name: "mix",
            input_type: TypeId::of::<Vec<u64>>,
            input_type_name: "Vec<u64>",
            is_baseline: true,
            crate_name: "mycrate",
            crate_version: "0.8.0",
            add_alt: add_alt_old,
        }
    }

    // A rival crate, on a *lower* version number than ours.
    inventory::submit! {
        Candidate {
            groups: &["mixing"],
            name: "mix",
            input_type: TypeId::of::<Vec<u64>>,
            input_type_name: "Vec<u64>",
            is_baseline: false,
            crate_name: "theircrate",
            crate_version: "0.1.0",
            add_alt: add_alt_new,
        }
    }

    // And both versions of our crate register the same input, which is
    // redundant: they are meant to build the same data.
    inventory::submit! {
        Input {
            groups: &["mixing"],
            name: "data",
            crate_name: "mycrate",
            crate_version: "0.9.0",
            type_id: TypeId::of::<Vec<u64>>,
            type_name: "Vec<u64>",
            make: make_data,
        }
    }

    inventory::submit! {
        Input {
            groups: &["mixing"],
            name: "data",
            crate_name: "mycrate",
            crate_version: "0.8.0",
            type_id: TypeId::of::<Vec<u64>>,
            type_name: "Vec<u64>",
            make: make_data,
        }
    }

    fn run() -> (Assembled, Report) {
        let cfg = Config::default().with_max_time(Duration::from_millis(30));
        // Every `#[cfg(test)]` module in this crate shares one process-wide
        // `inventory` registry - restricted to this module's own names, so
        // `try_add_registered` does not also assemble and measure
        // `registered_by_hand`'s benchmarks on every call here.
        let mut suite = cfg.suite();
        let tokens = suite.try_add_registered().unwrap();
        let report = suite.run();
        (tokens, report)
    }

    /// Two versions of one function, and a rival, all measured against each
    /// other - and told apart, rather than colliding as duplicates.
    #[test]
    fn versions_and_rivals_are_all_measured_and_distinguished() {
        let (tokens, report) = run();
        assert!(
            tokens.warnings.is_empty(),
            "a well-formed set of registrations should warn about nothing: {:?}",
            tokens
                .warnings
                .iter()
                .map(|w| w.to_string())
                .collect::<Vec<_>>(),
        );

        let cmps = report
            .comparison("mixing@data")
            .expect("the comparison ran");
        let names: Vec<&str> = cmps.names().collect();
        assert_eq!(names.len(), 3, "two of ours and one of theirs: {names:?}");
        assert!(names.contains(&"mix@mycrate-0.9.0"), "{names:?}");
        assert!(names.contains(&"mix@mycrate-0.8.0"), "{names:?}");
        assert!(names.contains(&"mix@theircrate-0.1.0"), "{names:?}");
    }

    /// The redundant input is dropped: one comparison, not one per version of
    /// the generator.
    #[test]
    fn the_redundant_input_is_measured_once() {
        let (_tokens, report) = run();
        let matrix_entries: Vec<&str> = report
            .names()
            .filter(|k| k.starts_with("mixing@"))
            .collect();
        assert_eq!(
            matrix_entries.len(),
            1,
            "both versions register `data`, but it is one input: {matrix_entries:?}",
        );
    }

    /// The older version is the baseline by default, so a regression reads
    /// the right way round: the new code is reported *against* the old.
    #[test]
    fn the_old_version_is_what_the_new_one_is_measured_against() {
        let (_tokens, report) = run();
        let cmps = report.comparison("mixing@data").unwrap();
        let against: Vec<&str> = cmps.against_baseline().map(|(n, _)| n).collect();
        assert!(
            !against.contains(&"mix@mycrate-0.8.0"),
            "the old version is the baseline, not a candidate: {against:?}",
        );
        assert!(against.contains(&"mix@mycrate-0.9.0"), "{against:?}");
    }
}
