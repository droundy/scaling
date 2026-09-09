//! Interleaving a whole suite of benchmarks.
//!
//! The reason to run fifty benchmarks together rather than one after another
//! is the reason [`Config::compare`] beats two separate [`bench`] calls, and
//! the reason [`ComparisonSet`] beats k-1 comparisons in pairs. Run in
//! sequence, benchmark #1 samples the machine at t=0 and #50 samples it at
//! t=500s, by which time the package is warmer and the clock has drifted;
//! their numbers are then not comparable, and neither is either of them
//! against the same suite run yesterday. Interleaved, every benchmark's
//! samples spread across the whole session, so all of them average the same
//! drift.
//!
//! What makes this harder than [`ComparisonSet`] is that the benchmarks do
//! not match: they take different input types, run for wildly different
//! times, and want different batch sizes. A `ComparisonSet` can share one
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
//! makes [`ComparisonSet`]'s batch-level erasure free, rather than the
//! per-iteration erasure that cost 14% on a 9ns function.
//!
//! **`async` is an implementation detail.** No future, and nothing to
//! `.await`, appears in this crate's public API.
//!
//! # Why there is no runtime here
//!
//! A general executor would be the wrong tool, not merely a heavy one.
//!
//! * **The scheduling policy is the point.** Which benchmark runs next, and
//!   in what order within a round, is the feature being built. A runtime's
//!   ready queue would have to be fought rather than used.
//! * **Runtimes park.** A general `block_on` sleeps the thread when every
//!   task returns `Poll::Pending`, waiting for something outside to wake one.
//!   Here there is no outside: `Pending` always means "I have had my turn",
//!   never "I am blocked", so every pending task is immediately runnable and
//!   parking would be a deadlock. That is also why the waker below can do
//!   nothing at all.

use super::*;
#[cfg(feature = "registry")]
use crate::assemble::RegistryOptions;
#[cfg(feature = "registry")]
use crate::registry::{GenInputRegistration, Kind, MatrixCandidate, MatrixInput, Registered};
use std::any::Any;
use std::cell::Cell;
#[cfg(feature = "registry")]
use std::collections::BTreeMap;
use std::fmt::{self, Display, Formatter};
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};
use std::time::{Duration, Instant};

/// A `Waker` whose every operation is a no-op.
///
/// Sound because nothing in this crate ever blocks. The only source of
/// `Poll::Pending` is [`Clock::yield_now`], which means "I have had my turn",
/// so the scheduler already knows to poll the task again and has no use for
/// being told. Nothing is ever registered, so nothing ever needs waking.
fn noop_waker() -> Waker {
    fn clone(_: *const ()) -> RawWaker {
        raw()
    }
    fn noop(_: *const ()) {}
    fn raw() -> RawWaker {
        RawWaker::new(std::ptr::null(), &VTABLE)
    }
    static VTABLE: RawWakerVTable = RawWakerVTable::new(clone, noop, noop, noop);
    // SAFETY: every function in the vtable ignores the data pointer, so the
    // null pointer is never dereferenced, and `clone` returns a `RawWaker`
    // built from the same vtable and the same null pointer.
    unsafe { Waker::from_raw(raw()) }
}

/// Yields to the scheduler exactly once.
///
/// It does not wake the waker before returning `Pending`, as a general
/// `yield_now` must: this crate's executor never parks, so there is nothing
/// to wake it from.
struct YieldOnce {
    yielded: bool,
}

impl Future for YieldOnce {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<()> {
        if self.yielded {
            Poll::Ready(())
        } else {
            self.yielded = true;
            Poll::Pending
        }
    }
}

/// A benchmark's own clock, kept by the scheduler on its behalf.
///
/// [`Config::max_time`] is documented as wall-clock time rather than measured
/// time, because it is a promise about how long the caller waits - a
/// benchmark whose input is slow to build has still taken that long. Under
/// interleaving a benchmark's wall-clock span is the *whole session*, so that
/// reading no longer works, and the quantity that preserves the promise is
/// how long this benchmark itself was running.
///
/// That is exactly the time the scheduler spends inside its `poll`, so the
/// scheduler measures it and the benchmark no longer keeps an `Instant` of
/// its own. Poll duration includes input construction, just as the wall clock
/// did before.
pub(crate) struct Clock {
    /// When the poll now in progress began, if one is.
    poll_started: Cell<Option<Instant>>,
    /// Own-time across every *completed* poll.
    spent: Cell<Duration>,
    /// The budget, from [`Config::max_time`].
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

    /// How long the poll now in progress has been running.
    ///
    /// This is what a benchmark with a long round consults to decide whether
    /// to yield part-way through it, rather than holding the CPU for the
    /// whole round.
    pub(crate) fn this_poll(&self) -> Duration {
        self.poll_started
            .get()
            .map_or(Duration::ZERO, |t| t.elapsed())
    }

    /// Own-time so far, *including* the poll in progress - which is the
    /// number a running benchmark needs, since it is asking about itself.
    pub(crate) fn spent(&self) -> Duration {
        self.spent.get() + self.this_poll()
    }

    pub(crate) fn exhausted(&self) -> bool {
        self.spent() >= self.max
    }

    /// The whole budget this benchmark was given.
    ///
    /// Calibration sizes its probe ceiling as a fraction of this, so that a
    /// benchmark on a short budget cannot spend all of it probing.
    pub(crate) fn budget(&self) -> Duration {
        self.max
    }

    /// Give the scheduler a turn, and come back with whether there is budget
    /// left to continue.
    ///
    /// The verdict is delivered *here* rather than by the scheduler dropping
    /// the future, so that a benchmark which runs out can still return the
    /// measurement it has, marked `hit_limit`, rather than vanishing.
    pub(crate) async fn yield_now(&self) -> bool {
        YieldOnce { yielded: false }.await;
        !self.exhausted()
    }

    fn begin_poll(&self) {
        self.poll_started.set(Some(Instant::now()));
    }

    fn end_poll(&self) {
        if let Some(started) = self.poll_started.take() {
            self.spent.set(self.spent.get() + started.elapsed());
        }
    }
}

/// The machine, claimed for measuring, for as long as this value lives.
///
/// Every blocking entry point begins by pinning to the reserved CPUs and
/// then serialising against every other benchmark on the machine, and the
/// two belong together: pinning without the lock puts a benchmark on cores
/// it has not claimed, and claiming without pinning serialises for nothing.
/// Bundling them means a new entry point cannot copy half of it - dropping
/// the guard would mean two benchmarks sharing one core, each measuring the
/// other rather than itself, and nothing about the resulting numbers would
/// look wrong.
///
/// A [`Suite`] takes one of these for the whole session rather than one per
/// benchmark. The guard is re-entrant within a thread, so the benchmarks it
/// interleaves cost nothing extra when they are also run individually.
pub(crate) struct Machine(#[allow(dead_code)] Option<quiet::Exclusive>);

impl Machine {
    pub(crate) fn claim() -> Self {
        quiet::pin_if_reserved();
        Machine(quiet::exclusive_if_pinned())
    }
}

/// Drive one future to completion, polling it in a tight loop.
///
/// Spinning is right rather than lazy: a `Pending` from [`Clock::yield_now`]
/// is immediately runnable, so there is nothing to wait for. This is what the
/// blocking entry points - [`bench`] and friends - become, so that there is
/// one sampling loop per kind of benchmark rather than a synchronous copy and
/// an asynchronous one drifting apart.
pub(crate) fn block_on<F: Future>(clock: &Clock, future: F) -> F::Output {
    let waker = noop_waker();
    let mut cx = Context::from_waker(&waker);
    let mut future = Box::pin(future);
    loop {
        clock.begin_poll();
        let polled = future.as_mut().poll(&mut cx);
        clock.end_poll();
        if let Poll::Ready(value) = polled {
            return value;
        }
    }
}

/// One benchmark in flight.
struct Task<'a> {
    future: Pin<Box<dyn Future<Output = ()> + 'a>>,
    clock: Rc<Clock>,
    done: bool,
}

/// Round-robin over a set of benchmarks, one sample each per round.
///
/// Deliberately dumb: it does not weight by how long a benchmark's sample
/// takes, or by how far any of them is from its accuracy target. A round is a
/// poll of every benchmark still running, in a rotated order, and that is the
/// whole policy.
struct Scheduler<'a> {
    tasks: Vec<Task<'a>>,
    /// Xorshift state for the per-round starting offset.
    seed: u64,
}

impl<'a> Scheduler<'a> {
    fn new(seed: u64) -> Self {
        Scheduler {
            tasks: Vec::new(),
            // Zero is a fixed point of xorshift, and would leave every round
            // starting at position zero.
            seed: seed | 1,
        }
    }

    fn push(&mut self, clock: Rc<Clock>, future: Pin<Box<dyn Future<Output = ()> + 'a>>) {
        self.tasks.push(Task {
            future,
            clock,
            done: false,
        });
    }

    fn next_rand(&mut self) -> u64 {
        self.seed ^= self.seed << 13;
        self.seed ^= self.seed >> 7;
        self.seed ^= self.seed << 17;
        self.seed
    }

    /// Put `live` into a fresh uniformly random order, Fisher-Yates.
    ///
    /// A *shuffle*, not a rotation. Rotating gives every benchmark a
    /// different position each round but leaves the order they sit in
    /// unchanged, so each one is always polled immediately after the same
    /// neighbour: with A, B and C the only orders a rotation ever produces
    /// are ABC, BCA and CAB, never ACB.
    ///
    /// That matters here in a way it does not inside a comparison, where
    /// every alternative is the same size and shape. A suite's benchmarks are
    /// not: if A has a large working set, then under a rotation B pays for
    /// evicting it in every single sample and C never does, which is a
    /// systematic difference between B and C that no amount of averaging
    /// removes. It is the same kind of fixed-position artifact this scheduler
    /// exists to destroy, one level down - so destroy it properly.
    fn shuffle(&mut self, live: &mut [usize]) {
        for i in (1..live.len()).rev() {
            let j = (self.next_rand() % (i as u64 + 1)) as usize;
            live.swap(i, j);
        }
    }

    /// Poll every benchmark once per round until all of them finish.
    ///
    /// The order is redrawn every round, so no benchmark keeps a fixed
    /// position *or* a fixed neighbour. Position matters for the reason it
    /// mattered within a comparison - a fixed position samples a fixed phase
    /// of whatever the machine does periodically, and this machine has a
    /// measured moire at the scheduler tick - and the neighbour matters
    /// because of what it leaves in the caches; see [`Scheduler::shuffle`].
    ///
    /// Finished benchmarks are retired *between* rounds rather than as they
    /// finish, so that removing one cannot disturb the rest of the round's
    /// order and skip somebody.
    fn run(&mut self) {
        let waker = noop_waker();
        let mut cx = Context::from_waker(&waker);
        let mut live: Vec<usize> = (0..self.tasks.len()).collect();
        while !live.is_empty() {
            self.shuffle(&mut live);
            for &i in &live {
                let task = &mut self.tasks[i];
                task.clock.begin_poll();
                let polled = task.future.as_mut().poll(&mut cx);
                task.clock.end_poll();
                task.done = polled.is_ready();
            }
            live.retain(|&i| !self.tasks[i].done);
        }
    }
}

/// Where one benchmark's answer will appear once the suite has run.
///
/// Returned by every `Suite::add*` method. Holding a token rather than
/// looking the answer up by name is what lets one suite mix benchmarks whose
/// results have different types: a flat benchmark hands back a [`Stats`], a
/// comparison a [`Comparisons`], and each token remembers which.
///
/// [`Token::get`] is `None` until the suite has run.
pub struct Token<T>(Arc<Mutex<Option<T>>>);

// Not `#[derive(Clone)]`, which would demand `T: Clone` for no reason: what
// is cloned is the handle, not the answer behind it.
impl<T> Clone for Token<T> {
    fn clone(&self) -> Self {
        Token(self.0.clone())
    }
}

impl<T> Token<T> {
    fn new() -> Self {
        Token(Arc::new(Mutex::new(None)))
    }

    /// The lock is only ever taken to store a result or to read one, never
    /// across a benchmark, so a poisoned lock means some *other* benchmark
    /// panicked and this one's answer is still perfectly good.
    fn cell(&self) -> std::sync::MutexGuard<'_, Option<T>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl<T: Clone> Token<T> {
    /// The answer, or `None` if the suite has not run yet.
    pub fn get(&self) -> Option<T> {
        self.cell().clone()
    }
}

impl<T> fmt::Debug for Token<T> {
    /// Deliberately not `where T: Debug`. A token is a handle, and what a
    /// reader wants of one is whether its answer has arrived yet; the answer
    /// itself is what [`Token::get`] and [`Report`] are for.
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        f.debug_struct("Token")
            .field("measured", &self.cell().is_some())
            .finish()
    }
}

/// A result that can be shown in the suite's table, whatever its type.
///
/// `Arc<Mutex<Option<T>>>` coerces straight to `Arc<dyn Reportable>`, so the
/// suite can keep every benchmark's cell in one list - in declaration order,
/// for printing - while the caller keeps the same cells typed, in tokens.
/// No enum of result kinds is needed.
///
/// # Why there is an `as_any` as well
///
/// Rendering was once all this had to do, because a caller who wanted the
/// measurement rather than its text held a [`Token`] for it. That stops
/// being true as soon as the caller did not write the `add` call:
/// [`Suite::add_registered`] adds benchmarks nobody named, so nobody holds
/// their tokens, and a script wanting to *ask* something of the results -
/// which of these is fastest, is the one we ship still the best - has only
/// the [`Report`]. Recovering the value from it needs the type back, and
/// that means a downcast.
trait Reportable {
    fn render(&self) -> Option<String>;
    /// The cell itself, for [`Report::get`] to downcast.
    fn as_any(&self) -> &(dyn Any + 'static);
}

impl<T: Display + 'static> Reportable for Mutex<Option<T>> {
    fn render(&self) -> Option<String> {
        self.lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|v| v.to_string())
    }

    fn as_any(&self) -> &(dyn Any + 'static) {
        self
    }
}

/// A set of benchmarks measured together, their samples interleaved.
///
/// ```
/// let cfg = scaling::Config::default();
/// let mut suite = cfg.suite();
/// let sort = suite.add("sort", || {
///     let mut v = vec![5, 3, 1, 4, 2];
///     v.sort();
///     v
/// });
/// let sum = suite.add("sum", || (0..100u64).sum::<u64>());
/// let report = suite.run();
/// println!("{report}");
/// # let _ = (sort.get().unwrap(), sum.get().unwrap());
/// ```
///
/// Every benchmark here gets [`Config::max_time`] of its *own* running time,
/// so a suite of `n` may take `n` times as long as one benchmark - the same
/// arithmetic [`Config::compare`] uses for two and [`ComparisonSet`] uses for
/// `k`. What interleaving changes is not how long it takes but *when* each
/// benchmark's samples are drawn: across the whole session rather than in one
/// stretch of it, so that no benchmark is measured in a machine state its
/// neighbours never saw.
pub struct Suite<'a> {
    cfg: &'a Config,
    scheduler: Scheduler<'a>,
    /// Names and type-erased cells, in declaration order, for [`Report`].
    entries: Vec<(String, Arc<dyn Reportable>)>,
    /// Comparisons added so far, so [`Suite::run`] can fill in the plan.
    comparisons: u64,
    /// Which entries to measure. Everything, unless a caller says otherwise.
    filter: Filter,
    /// The Bonferroni limit, shared with every comparison this suite holds.
    ///
    /// A cell because the comparisons are boxed as they are added, before the
    /// total is known: [`Suite::run`] fills this in once, and every
    /// comparison reads it when it starts sampling, which is strictly
    /// afterwards. That ordering is the whole reason a suite can correct for
    /// its own size without anyone promising the count in advance - and it is
    /// why the read has to stay lazy. An eager one would hand every
    /// comparison `NaN`, which fails silently: nothing is ever significant,
    /// each spends its whole budget, and the report reads as a page of honest
    /// "unchanged" results.
    z_alpha: Rc<Cell<f64>>,
}

impl Config {
    /// Begin a suite of benchmarks to be measured together.
    ///
    /// See [`Suite`].
    pub fn suite(&self) -> Suite<'_> {
        Suite {
            cfg: self,
            // A fixed seed: what must vary is the starting position from one
            // round to the next, which it does. Varying it between runs as
            // well would only make a suite harder to reproduce.
            scheduler: Scheduler::new(0x9E37_79B9_7F4A_7C15),
            entries: Vec::new(),
            comparisons: 0,
            filter: Filter::everything(),
            // `NaN` until `run` sets it. Nothing reads it before then, and a
            // suite holding no comparisons never reads it at all.
            z_alpha: Rc::new(Cell::new(f64::NAN)),
        }
    }
}

impl<'a> Suite<'a> {
    /// How many benchmarks have been added.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Measure only the entries this [`Filter`] keeps.
    ///
    /// It applies to everything the suite holds, hand-added and discovered
    /// alike - a run that honoured a filter for registered benchmarks and
    /// quietly ignored it for the two added by hand would be worse than not
    /// having one.
    ///
    /// A filtered-out benchmark is not added at all: no clock, no place in
    /// the round robin, no time spent. Its token is still returned, and
    /// simply never fills - the same thing that happens to any token when a
    /// suite is built and not run.
    ///
    /// ```
    /// # use scaling::Filter;
    /// let cfg = scaling::Config::default();
    /// let mut suite = cfg.suite().with_filter(Filter::everything().matching("sort"));
    /// let sorted = suite.add("sorting", || { let mut v = vec![3, 1, 2]; v.sort(); v });
    /// let summed = suite.add("summing", || (0..10u64).sum::<u64>());
    /// let report = suite.run();
    /// assert!(sorted.get().is_some(), "kept");
    /// assert!(summed.get().is_none(), "filtered out, so never measured");
    /// # let _ = report;
    /// ```
    pub fn with_filter(mut self, filter: Filter) -> Self {
        self.filter = filter;
        self
    }

    /// The filter this suite is measuring under.
    pub fn filter(&self) -> &Filter {
        &self.filter
    }

    /// What this suite would measure, in the order it was added.
    ///
    /// Everything the filter kept and nothing it did not, which is what
    /// answers [`Filter::is_listing`] - acted on by the caller rather than
    /// here, printing not being a library's business.
    ///
    /// ```no_run
    /// # let cfg = scaling::Config::default();
    /// # let suite = cfg.suite();
    /// if suite.filter().is_listing() {
    ///     for name in suite.names() {
    ///         println!("{name}");
    ///     }
    /// } else {
    ///     println!("{}", suite.run());
    /// }
    /// ```
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|(name, _)| name.as_str())
    }

    fn push<T: Display + 'static>(
        &mut self,
        name: &str,
        token: &Token<T>,
        clock: Rc<Clock>,
        future: Pin<Box<dyn Future<Output = ()> + 'a>>,
    ) {
        self.entries.push((name.to_string(), token.0.clone()));
        self.scheduler.push(clock, future);
    }

    /// Add a benchmark, as [`bench`](fn@bench) would run it.
    pub fn add<F, O>(&mut self, name: &str, f: F) -> Token<Stats>
    where
        F: FnMut() -> O + 'a,
        O: 'a,
    {
        self.add_with(self.cfg, name, f)
    }

    /// [`Suite::add`], measured against `cfg` rather than the suite's own.
    ///
    /// See [`Suite::add_gen_input_with`] for what a per-benchmark `Config`
    /// is for and what it does not change.
    pub fn add_with<F, O>(&mut self, cfg: &'a Config, name: &str, mut f: F) -> Token<Stats>
    where
        F: FnMut() -> O + 'a,
        O: 'a,
    {
        self.add_gen_input_with(cfg, name, || (), move |_: &mut ()| f())
    }

    /// Add a benchmark over a mutable input, as [`bench_input`] would run it.
    pub fn add_input<F, I, O>(&mut self, name: &str, input: I, f: F) -> Token<Stats>
    where
        F: FnMut(&mut I) -> O + 'a,
        I: Clone + 'a,
        O: 'a,
    {
        self.add_input_with(self.cfg, name, input, f)
    }

    /// [`Suite::add_input`], measured against `cfg` rather than the suite's
    /// own. See [`Suite::add_gen_input_with`].
    pub fn add_input_with<F, I, O>(
        &mut self,
        cfg: &'a Config,
        name: &str,
        input: I,
        f: F,
    ) -> Token<Stats>
    where
        F: FnMut(&mut I) -> O + 'a,
        I: Clone + 'a,
        O: 'a,
    {
        self.add_gen_input_with(cfg, name, move || input.clone(), f)
    }

    /// Add a benchmark over generated inputs, as [`bench_gen_input`] would
    /// run it.
    pub fn add_gen_input<G, F, I, O>(&mut self, name: &str, gen_input: G, f: F) -> Token<Stats>
    where
        G: FnMut() -> I + 'a,
        F: FnMut(&mut I) -> O + 'a,
        I: 'a,
        O: 'a,
    {
        self.add_gen_input_with(self.cfg, name, gen_input, f)
    }

    /// [`Suite::add_gen_input`], measured against `cfg` rather than the
    /// suite's own.
    ///
    /// One benchmark in a suite may want a different accuracy goal or a
    /// different budget from the rest - a slow one nobody wants to spend the
    /// default on, or a fickle one worth chasing further. The `Config` given
    /// here governs this benchmark alone: its goals, and the budget its clock
    /// is built from.
    ///
    /// It must outlive the suite, since the benchmark holds it until it runs.
    /// A caller with several wants them somewhere stable - a `Vec<Config>`
    /// declared before the suite will do - rather than built inline per call.
    ///
    /// # What it does not change
    ///
    /// The multiple-comparison threshold, which belongs to the whole suite
    /// and is worked out in [`Suite::run`] from the number of comparisons the
    /// suite holds. A benchmark cannot opt out of the family it is part of by
    /// bringing its own `Config`.
    ///
    /// ```
    /// use std::time::Duration;
    /// let cfg = scaling::Config::default();
    /// // Declared before the suite, so it outlives it.
    /// let quick = scaling::Config::relative(0.1)
    ///     .with_max_time(Duration::from_millis(5));
    /// let mut suite = cfg.suite();
    /// let slow = suite.add_with(&quick, "slow_one", || (0..1000u64).sum::<u64>());
    /// let rest = suite.add("rest", || (0..10u64).sum::<u64>());
    /// let report = suite.run();
    /// # let _ = (slow.get(), rest.get(), report);
    /// ```
    pub fn add_gen_input_with<G, F, I, O>(
        &mut self,
        cfg: &'a Config,
        name: &str,
        gen_input: G,
        f: F,
    ) -> Token<Stats>
    where
        G: FnMut() -> I + 'a,
        F: FnMut(&mut I) -> O + 'a,
        I: 'a,
        O: 'a,
    {
        // Filtered out: build nothing. The token is still handed back and
        // simply never fills, which is what happens to any token when a
        // suite is not run.
        if !self.filter.matches(name) {
            return Token::new();
        }
        let clock = Rc::new(Clock::new(cfg.max_time));
        let token = Token::new();
        let cell = token.clone();
        let mine = clock.clone();
        self.push(
            name,
            &token,
            clock,
            Box::pin(async move {
                let stats = cfg.bench_gen_input_async(&mine, gen_input, f).await;
                *cell.cell() = Some(stats);
            }),
        );
        token
    }

    /// Add a scaling benchmark, as [`bench_scaling`](fn@bench_scaling) would run it.
    pub fn add_scaling<F, O>(&mut self, name: &str, f: F, nmin: usize) -> Token<ScalingStats>
    where
        F: Fn(usize) -> O + 'a,
        O: 'a,
    {
        self.add_scaling_with(self.cfg, name, f, nmin)
    }

    /// [`Suite::add_scaling`], measured against `cfg` rather than the suite's
    /// own. See [`Suite::add_gen_input_with`].
    pub fn add_scaling_with<F, O>(
        &mut self,
        cfg: &'a Config,
        name: &str,
        f: F,
        nmin: usize,
    ) -> Token<ScalingStats>
    where
        F: Fn(usize) -> O + 'a,
        O: 'a,
    {
        // Filtered out: build nothing. The token is still handed back and
        // simply never fills, which is what happens to any token when a
        // suite is not run.
        if !self.filter.matches(name) {
            return Token::new();
        }
        let clock = Rc::new(Clock::new(cfg.max_time));
        let token = Token::new();
        let cell = token.clone();
        let mine = clock.clone();
        self.push(
            name,
            &token,
            clock,
            Box::pin(async move {
                *cell.cell() = Some(cfg.bench_scaling_async(&mine, f, nmin).await);
            }),
        );
        token
    }

    /// Add a scaling benchmark over generated inputs, as
    /// [`bench_scaling_gen`] would run it.
    pub fn add_scaling_gen<G, F, I, O>(
        &mut self,
        name: &str,
        gen_input: G,
        f: F,
        nmin: usize,
    ) -> Token<ScalingStats>
    where
        G: FnMut(usize) -> I + 'a,
        F: Fn(&mut I) -> O + 'a,
        I: 'a,
        O: 'a,
    {
        self.add_scaling_gen_with(self.cfg, name, gen_input, f, nmin)
    }

    /// [`Suite::add_scaling_gen`], measured against `cfg` rather than the
    /// suite's own. See [`Suite::add_gen_input_with`].
    pub fn add_scaling_gen_with<G, F, I, O>(
        &mut self,
        cfg: &'a Config,
        name: &str,
        gen_input: G,
        f: F,
        nmin: usize,
    ) -> Token<ScalingStats>
    where
        G: FnMut(usize) -> I + 'a,
        F: Fn(&mut I) -> O + 'a,
        I: 'a,
        O: 'a,
    {
        // Filtered out: build nothing. The token is still handed back and
        // simply never fills, which is what happens to any token when a
        // suite is not run.
        if !self.filter.matches(name) {
            return Token::new();
        }
        let clock = Rc::new(Clock::new(cfg.max_time));
        let token = Token::new();
        let cell = token.clone();
        let mine = clock.clone();
        self.push(
            name,
            &token,
            clock,
            Box::pin(async move {
                *cell.cell() = Some(cfg.bench_scaling_gen_async(&mine, gen_input, f, nmin).await);
            }),
        );
        token
    }

    /// Add a whole k-way comparison, built with [`Config::comparison`].
    ///
    /// The comparison counts as *one* participant in the round robin, not
    /// `k`, because its round has to stay whole: the paired error bars it
    /// reports only cancel the machine's slow movement because every
    /// alternative met that movement inside the same round. That is also
    /// fair rather than merely necessary - one poll here runs `k` batches
    /// where a flat benchmark runs one, and it is producing `k` [`Stats`].
    ///
    /// ```
    /// let cfg = scaling::Config::default();
    /// let mut suite = cfg.suite();
    /// let hashing = suite.add_comparison(
    ///     "hashing",
    ///     cfg.comparison()
    ///         .add("old", || (0..50u64).fold(0u64, |a, x| a ^ x))
    ///         .add("new", || (0..50u64).sum::<u64>()),
    /// );
    /// let report = suite.run();
    /// # let _ = (report, hashing.get().unwrap());
    /// ```
    ///
    /// # Panics
    ///
    /// If the set holds fewer than two alternatives - checked here rather
    /// than when the suite runs, so the mistake is reported at the line that
    /// made it.
    pub fn add_comparison<I>(&mut self, name: &str, set: ComparisonSet<'a, I>) -> Token<Comparisons>
    where
        I: Clone + 'a,
    {
        let k = set.len();
        assert!(
            k >= 2,
            "a comparison needs at least two alternatives, got {k}"
        );
        // After the assertion, so that filtering cannot hide a mistake, and
        // before the count, so the Bonferroni limit is taken over what is
        // really going to be measured.
        // Filtered out: build nothing. The token is still handed back and
        // simply never fills, which is what happens to any token when a
        // suite is not run.
        if !self.filter.matches(name) {
            return Token::new();
        }

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
        let clock = Rc::new(Clock::new(set.cfg().max_time * k as u32));
        let token = Token::new();
        let cell = token.clone();
        let mine = clock.clone();
        self.push(
            name,
            &token,
            clock,
            Box::pin(async move {
                let results = set.run_async(&mine, z_alpha.get(), seed).await;
                *cell.cell() = Some(results);
            }),
        );
        token
    }

    /// Measure every benchmark, interleaved, and report them together.
    ///
    /// The suite's own comparisons are its family, and the Bonferroni limit
    /// they are judged against is worked out here from the count. It can only
    /// be done at this point - the count is not known until the last one is
    /// added - and it is only possible at all because a suite collects
    /// everything before running anything, which is exactly what a caller
    /// invoking [`Config::compare`] in a loop cannot do.
    ///
    /// Nothing is promised in advance and nothing is checked afterwards: the
    /// scheduler runs every entry that was added, so the number corrected for
    /// and the number made are the same number by construction.
    pub fn run(mut self) -> Report {
        self.z_alpha.set(Config::z_alpha_for(self.comparisons));
        // Claimed once for the whole session rather than once per benchmark.
        // The guard is re-entrant within a thread, so the benchmarks' own
        // claims - taken when they are run individually - cost nothing here.
        let _machine = Machine::claim();
        self.scheduler.run();
        Report {
            entries: self.entries,
        }
    }
}

/// Where the answers of registered benchmarks appear once the suite has run.
///
/// A [`Report`] shows everything, but only as text. This is how a caller
/// reaches one particular answer afterwards - to assert on it in a test, or
/// to feed it somewhere - without parsing the report back.
///
/// Keyed by the name the benchmark registered under. Split by kind rather
/// than mixed, because the three answers are different types and a token
/// remembers which: that is exactly what stops a caller having to downcast.
#[cfg(feature = "registry")]
#[derive(Debug, Default)]
pub struct RegisteredTokens {
    /// Flat benchmarks, by name.
    pub flat: BTreeMap<String, Token<Stats>>,
    /// Scaling benchmarks, by name.
    pub scaling: BTreeMap<String, Token<ScalingStats>>,
    /// Comparison groups, by group name. A matrix contributes one per input,
    /// named `matrix@input`.
    pub comparisons: BTreeMap<String, Token<Comparisons>>,
    /// Things worth saying that did not stop the run - a matrix candidate no
    /// input matches, say. Errors come back through
    /// [`Suite::try_add_registered`] instead; these are the complaints that
    /// leave the rest of the run perfectly good.
    pub warnings: Vec<crate::assemble::Diagnostic>,
    /// The comparison groups as they were assembled, in the order they were
    /// added.
    ///
    /// Here because a comparison reaches the report under one name while
    /// holding several alternatives, and nothing else can say what they
    /// were. A caller listing what would run - which is the only way to find
    /// out what a binary registered - would otherwise print the group and
    /// leave the reader guessing what is in it.
    pub groups: Vec<crate::assemble::Group>,
    /// The matrix lanes as they were assembled.
    ///
    /// Same reason as [`RegisteredTokens::groups`], plus one more: a matrix
    /// is a grid, and the grid is only recoverable from the lane. The report
    /// holds one comparison per input under a flattened `matrix@input` name,
    /// which is the right thing to *measure* and the wrong shape to read.
    pub lanes: Vec<crate::assemble::Lane>,
}

#[cfg(feature = "registry")]
impl<'a> Suite<'a> {
    /// Add every benchmark registered anywhere in this binary.
    ///
    /// Discovery only; the suite is otherwise unchanged, and benchmarks added
    /// by hand before or after this call sit alongside the discovered ones
    /// and are measured the same way. Calling it twice would add everything
    /// twice, so do not.
    ///
    /// Registered comparisons go through [`Suite::add_comparison`] like any
    /// other, so they are counted towards the suite's multiple-comparison
    /// plan by the machinery that was already there.
    ///
    /// # Panics
    ///
    /// If the registrations do not make sense together - a duplicate name, a
    /// comparison group with no baseline or two, an alternative whose input
    /// type is not the one its group generates. The panic lists *every*
    /// problem rather than the first, since they are found before anything
    /// runs and fixing them one rebuild at a time would be tedious.
    ///
    /// Use [`Suite::try_add_registered`] to handle them instead, which is
    /// what a runner printing diagnostics of its own should do.
    pub fn add_registered(&mut self) -> RegisteredTokens {
        self.add_registered_with(RegistryOptions::default())
    }

    /// [`Suite::add_registered`], saying what to do about registrations that
    /// come from more than one crate or version.
    ///
    /// The case this is for: a crate pulls an older copy of itself, or a
    /// rival crate, in as a dev-dependency with registrations enabled. Both
    /// register, and both may use the same names for the same ideas.
    ///
    /// ```no_run
    /// # let cfg = scaling::Config::default();
    /// # let mut suite = cfg.suite();
    /// use scaling::assemble::RegistryOptions;
    /// // Measure only the newest version of each crate that registered.
    /// suite.add_registered_with(RegistryOptions::latest_per_crate());
    /// # let _ = suite;
    /// ```
    pub fn add_registered_with(&mut self, options: RegistryOptions) -> RegisteredTokens {
        match self.try_add_registered_with(options) {
            Ok(tokens) => tokens,
            Err(problems) => {
                let mut msg = String::from("registered benchmarks do not make sense together:");
                for p in &problems {
                    msg.push_str("\n  - ");
                    msg.push_str(&p.to_string());
                }
                panic!("{msg}");
            }
        }
    }

    /// [`Suite::add_registered`], handing back what is wrong rather than
    /// panicking.
    ///
    /// Nothing is added when this returns `Err`: the registrations are
    /// checked in full before the first one is added, so a suite is never
    /// left holding half of a set that did not check out.
    pub fn try_add_registered(
        &mut self,
    ) -> Result<RegisteredTokens, Vec<crate::assemble::Diagnostic>> {
        self.try_add_registered_with(RegistryOptions::default())
    }

    /// [`Suite::try_add_registered`], with [`RegistryOptions`]. See
    /// [`Suite::add_registered_with`].
    pub fn try_add_registered_with(
        &mut self,
        options: RegistryOptions,
    ) -> Result<RegisteredTokens, Vec<crate::assemble::Diagnostic>> {
        let regs: Vec<&'static Registered> = inventory::iter::<Registered>().collect();
        let gens: Vec<&'static GenInputRegistration> =
            inventory::iter::<GenInputRegistration>().collect();
        let plan = crate::assemble::plan(&regs, &gens, options)?;

        let cfg = self.cfg;
        let mut tokens = RegisteredTokens::default();

        for r in plan.flat {
            match r.reg.kind {
                Kind::Flat(add) => {
                    let token = add(self, cfg, &r.name);
                    tokens.flat.insert(r.name, token);
                }
                Kind::Scaling(add) => {
                    let token = add(self, cfg, &r.name);
                    tokens.scaling.insert(r.name, token);
                }
                // `plan` puts anything with a group in `groups`, so a bare
                // alternative cannot reach here.
                Kind::Alt { .. } => unreachable!("an alternative without a group"),
            }
        }

        for group in &plan.groups {
            // One generator for the whole group, cloned per alternative, which
            // is what makes the differences paired - see `ErasedInput`.
            let make = group.make_input();
            let mut set = cfg.comparison_gen_input(make);
            for m in &group.members {
                match m.reg.kind {
                    Kind::Alt { add, .. } => set = add(set, &m.name),
                    // `plan` only puts alternatives in a group.
                    _ => unreachable!("a group member that is not an alternative"),
                }
            }
            tokens
                .comparisons
                .insert(group.name.to_string(), self.add_comparison(group.name, set));
        }
        tokens.groups = plan.groups;

        // Matrices: candidates and inputs registered apart from each other,
        // paired by type into lanes, every pairing measured.
        let cands: Vec<&'static MatrixCandidate> = inventory::iter::<MatrixCandidate>().collect();
        let mins: Vec<&'static MatrixInput> = inventory::iter::<MatrixInput>().collect();
        let (lanes, lane_problems) = crate::assemble::lanes(&cands, &mins, options);
        // A contradiction inside a lane discards that lane, so benchmarks
        // that were written measure nothing - that has to be as loud as any
        // other error, not a field on the returned value that a caller
        // discarding the result never sees. An orphan is different: it means
        // something registered went unused, and everything else still ran.
        let (fatal, warnings): (Vec<_>, Vec<_>) =
            lane_problems.into_iter().partition(|p| p.is_fatal());
        if !fatal.is_empty() {
            return Err(fatal);
        }
        tokens.warnings = warnings;

        for lane in &lanes {
            for input in &lane.inputs {
                if lane.candidates.len() < 2 {
                    // Nothing to compare against, so this is a plain
                    // benchmark rather than a one-sided comparison.
                    let c = &lane.candidates[0];
                    let name = lane.flat_name(c, input);
                    let token = (c.reg.add_flat)(self, cfg, &name, input.reg.make);
                    tokens.flat.insert(name, token);
                    continue;
                }
                let make = input.reg.make;
                let mut set = cfg.comparison_gen_input(make);
                for c in &lane.candidates {
                    set = (c.reg.add_alt)(set, &c.name);
                }
                let name = lane.comparison_name(input);
                let token = self.add_comparison(&name, set);
                tokens.comparisons.insert(name, token);
            }
        }
        tokens.lanes = lanes;

        Ok(tokens)
    }
}

/// Everything a [`Suite`] measured, in the order it was declared.
pub struct Report {
    entries: Vec<(String, Arc<dyn Reportable>)>,
}

impl Report {
    /// What every entry is called, in the order they were added.
    ///
    /// The way to find out what a run produced when the names were not
    /// written by hand - a registered benchmark is called after its module
    /// and function, and a matrix cell after its matrix and input.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|(name, _)| name.as_str())
    }

    /// Whether anything was measured under this name.
    pub fn contains(&self, name: &str) -> bool {
        self.entries.iter().any(|(n, _)| n == name)
    }

    /// One measurement, by name and type.
    ///
    /// `None` if nothing of that name was measured, if it was measured but
    /// is of another type, or if the suite has not run. The three concrete
    /// forms - [`Report::stats`], [`Report::scaling`],
    /// [`Report::comparison`] - are usually what you want; this is here for
    /// completeness and for anything added later.
    ///
    /// ```
    /// let cfg = scaling::Config::default();
    /// let mut suite = cfg.suite();
    /// let _ = suite.add("sum", || (0..100u64).sum::<u64>());
    /// let report = suite.run();
    /// let stats: scaling::Stats = report.get("sum").expect("it ran");
    /// assert!(stats.ns_per_iter > 0.0);
    /// ```
    pub fn get<T: Clone + 'static>(&self, name: &str) -> Option<T> {
        let (_, cell) = self.entries.iter().find(|(n, _)| n == name)?;
        let typed = cell.as_any().downcast_ref::<Mutex<Option<T>>>()?;
        typed
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .cloned()
    }

    /// A flat benchmark's measurement, by name.
    ///
    /// `None` if that name was something else - a comparison, say - so a
    /// caller that does not know what it is looking at can simply ask.
    pub fn stats(&self, name: &str) -> Option<Stats> {
        self.get(name)
    }

    /// A scaling benchmark's measurement, by name.
    pub fn scaling(&self, name: &str) -> Option<ScalingStats> {
        self.get(name)
    }

    /// A comparison's results, by name.
    ///
    /// For a matrix, the name is `matrix@input`; for a group, the group's
    /// name. What comes back carries every alternative's own measurement as
    /// well as its difference from the baseline, so this is what a script
    /// asking "which of these is actually fastest here" wants.
    pub fn comparison(&self, name: &str) -> Option<Comparisons> {
        self.get(name)
    }

    /// Every flat measurement, with its name, in the order they were added.
    pub fn all_stats(&self) -> impl Iterator<Item = (&str, Stats)> {
        self.all()
    }

    /// Every comparison, with its name, in the order they were added.
    pub fn all_comparisons(&self) -> impl Iterator<Item = (&str, Comparisons)> {
        self.all()
    }

    /// Every entry of one type, with its name. Entries of other types are
    /// skipped rather than being an error, which is what makes this usable
    /// on a report holding a mixture.
    fn all<T: Clone + 'static>(&self) -> impl Iterator<Item = (&str, T)> {
        self.entries.iter().filter_map(|(name, cell)| {
            let typed = cell.as_any().downcast_ref::<Mutex<Option<T>>>()?;
            let value = typed
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()
                .cloned()?;
            Some((name.as_str(), value))
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
        for (i, (name, cell)) in self.entries.iter().enumerate() {
            if i > 0 {
                writeln!(f)?;
            }
            let shown = cell.render();
            // Trimmed because a multi-line result brings its own trailing
            // newline - `Comparisons` writes every line with `writeln!` - and
            // this loop supplies the separators itself. Leaving it produced a
            // blank line after any comparison that was not the last entry.
            let shown = shown.as_deref().unwrap_or("(not measured)").trim_end();
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
    use crate::testutil::{fixed_cost, mean_and_spread};

    /// A future that yields `n` times and then reports how many rounds it
    /// took, appending its identity to a shared log every time it is polled.
    /// Nothing here is timed, so these tests say the same thing on a busy
    /// machine as on a quiet one.
    async fn scripted(id: usize, yields: usize, log: Rc<RefCell<Vec<usize>>>, clock: Rc<Clock>) {
        for _ in 0..yields {
            log.borrow_mut().push(id);
            clock.yield_now().await;
        }
        log.borrow_mut().push(id);
    }

    use std::cell::RefCell;

    fn scheduler_of(yields: &[usize], seed: u64) -> (Scheduler<'static>, Rc<RefCell<Vec<usize>>>) {
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut s = Scheduler::new(seed);
        for (id, &n) in yields.iter().enumerate() {
            let clock = Rc::new(Clock::new(Duration::from_secs(3600)));
            let fut = scripted(id, n, log.clone(), clock.clone());
            s.push(clock, Box::pin(fut));
        }
        (s, log)
    }

    /// The core scheduling promise: within a round, every benchmark still
    /// running is polled exactly once. If this ever fails, some benchmark is
    /// sampling a different stretch of the session than its neighbours, which
    /// is the whole thing interleaving exists to prevent.
    #[test]
    fn every_round_polls_everyone_exactly_once() {
        const N: usize = 5;
        const ROUNDS: usize = 4;
        let (mut s, log) = scheduler_of(&[ROUNDS; N], 0x243f_6a88_85a3_08d3);
        s.run();
        let log = log.borrow();
        assert_eq!(log.len(), N * (ROUNDS + 1));
        for round in log.chunks(N) {
            let mut seen = round.to_vec();
            seen.sort_unstable();
            seen.dedup();
            assert_eq!(seen.len(), N, "a round polled someone twice: {round:?}");
        }
    }

    /// The starting position must actually move, or "shuffled order" is a
    /// comment rather than a behaviour.
    #[test]
    fn the_starting_position_moves_between_rounds() {
        let (mut s, log) = scheduler_of(&[20; 4], 0x9e37_79b9_7f4a_7c15);
        s.run();
        let log = log.borrow();
        let firsts: Vec<usize> = log.chunks(4).map(|r| r[0]).collect();
        let distinct = {
            let mut f = firsts.clone();
            f.sort_unstable();
            f.dedup();
            f.len()
        };
        assert!(
            distinct > 1,
            "every round started with the same benchmark: {firsts:?}"
        );
    }

    /// Who a benchmark is polled *after* must vary too, not just where it
    /// sits. A rotation moves every position while leaving the order intact,
    /// so each benchmark keeps one fixed predecessor and therefore always
    /// inherits the same neighbour's cache state - a systematic difference
    /// between benchmarks that averaging cannot touch.
    ///
    /// With three benchmarks a rotation can only ever produce ABC, BCA and
    /// CAB, in all of which A precedes B; this asserts that A is sometimes
    /// preceded by each of the others, which no rotation can satisfy.
    #[test]
    fn the_neighbour_order_varies_too() {
        const N: usize = 3;
        let (mut s, log) = scheduler_of(&[40; N], 0x2545_f491_4f6c_dd1d);
        s.run();
        let log = log.borrow();
        // Read adjacency straight off the flat log rather than within
        // rounds, so the pairing across a round boundary counts too - the
        // machine does not know where a round ended.
        let mut predecessors: Vec<usize> =
            log.windows(2).filter(|w| w[1] == 0).map(|w| w[0]).collect();
        predecessors.sort_unstable();
        predecessors.dedup();
        // Both of the others must appear; a rotation would give exactly one,
        // always the same one. Benchmark 0 may also follow *itself*, when it
        // ends one round and begins the next - independent shuffles allow
        // that, and it costs nothing: each round still polls everyone once.
        assert!(
            predecessors.contains(&1) && predecessors.contains(&2),
            "benchmark 0 did not follow every other: {predecessors:?}"
        );
    }

    /// Benchmarks finish at different times, and a short one leaving must not
    /// cost its neighbour a turn. Retiring between rounds rather than during
    /// one is what guarantees it.
    #[test]
    fn retiring_early_does_not_skip_a_neighbour() {
        // Three benchmarks wanting very different numbers of rounds.
        let (mut s, log) = scheduler_of(&[0, 3, 7], 0x2545_f491_4f6c_dd1d);
        s.run();
        let log = log.borrow();
        let polls = |id: usize| log.iter().filter(|&&x| x == id).count();
        // Each is polled once per yield plus once to finish.
        assert_eq!(polls(0), 1);
        assert_eq!(polls(1), 4);
        assert_eq!(polls(2), 8);
    }

    /// An empty suite is a legitimate thing to ask for and must not hang or
    /// divide by zero in the offset.
    #[test]
    fn an_empty_scheduler_finishes() {
        let (mut s, log) = scheduler_of(&[], 1);
        s.run();
        assert!(log.borrow().is_empty());
    }

    /// `block_on` must reach the same answer as the scheduler would, since
    /// every blocking entry point in the crate becomes a call to it.
    #[test]
    fn block_on_drives_a_yielding_future() {
        let clock = Rc::new(Clock::new(Duration::from_secs(3600)));
        let inner = clock.clone();
        let n = block_on(&clock, async move {
            let mut rounds = 0;
            while rounds < 5 {
                rounds += 1;
                inner.yield_now().await;
            }
            rounds
        });
        assert_eq!(n, 5);
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

    /// A suite mixes result types, and each token must hand back its own.
    /// This is the thing an enum of result kinds was avoided for.
    #[test]
    fn tokens_keep_their_own_types() {
        let cfg = Config::default().with_max_time(Duration::from_millis(50));
        let mut suite = cfg.suite();
        let flat: Token<Stats> = suite.add("flat", || (0..20u64).sum::<u64>());
        let cmp: Token<Comparisons> = suite.add_comparison(
            "pair",
            cfg.comparison()
                .add("a", || (0..20u64).sum::<u64>())
                .add("b", || (0..20u64).sum::<u64>()),
        );
        assert!(flat.get().is_none(), "nothing is measured before the run");
        let report = suite.run();
        println!("{report}");

        let stats = flat.get().expect("the flat benchmark reported");
        assert!(stats.ns_per_iter > 0.0);
        let comparisons = cmp.get().expect("the comparison reported");
        assert_eq!(comparisons.stats().len(), 2);
    }

    /// All three kinds in one suite, interleaved: this is the whole point of
    /// erasing the input type and reporting through tokens.
    #[test]
    fn all_three_kinds_share_one_suite() {
        let cfg = Config::default().with_max_time(Duration::from_millis(80));
        let mut suite = cfg.suite();
        let flat: Token<Stats> = suite.add("flat", || (0..50u64).sum::<u64>());
        // A different input type from the comparison below, which is the
        // thing a `ComparisonSet` alone cannot do.
        let with_input: Token<Stats> =
            suite.add_input("with input", vec![3u8; 32], |v: &mut Vec<u8>| {
                v.iter().map(|&x| x as u64).sum::<u64>()
            });
        let scaled: Token<ScalingStats> =
            suite.add_scaling("scaled", |n| (0..n as u64).sum::<u64>(), 1000);
        let cmp: Token<Comparisons> = suite.add_comparison(
            "pair",
            cfg.comparison()
                .add("a", || (0..50u64).sum::<u64>())
                .add("b", || (0..50u64).sum::<u64>()),
        );
        let report = suite.run();
        println!("{report}");

        assert!(flat.get().is_some());
        assert!(with_input.get().is_some());
        assert!(scaled.get().is_some(), "the scaling benchmark reported");
        assert_eq!(cmp.get().unwrap().stats().len(), 2);
    }

    /// The table is in declaration order, not alphabetical and not whatever
    /// order the benchmarks happened to finish in.
    #[test]
    fn the_report_is_in_declaration_order() {
        let cfg = Config::default().with_max_time(Duration::from_millis(20));
        let mut suite = cfg.suite();
        // Declared in an order that is neither alphabetical nor the order
        // they will finish in - "zebra" is the cheapest and finishes first.
        let _ = suite.add("middle", || (0..200u64).sum::<u64>());
        let _ = suite.add("zebra", || 1u64 + 1);
        let _ = suite.add("apple", || (0..400u64).sum::<u64>());
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
        let _ = suite.add_comparison(
            "three",
            cfg.comparison()
                .add("a", || 1u64 + 1)
                .add("b", || 1u64 + 1)
                .add("c", || 1u64 + 1),
        );
        let _ = suite.add_comparison(
            "two",
            cfg.comparison().add("a", || 1u64 + 1).add("b", || 1u64 + 1),
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
            let _ = suite.add_comparison(
                "pair",
                cfg.comparison().add("a", || 1u64 + 1).add("b", || 1u64 + 1),
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
        let pair = suite.add_comparison(
            "pair",
            cfg.comparison().add("a", || 1u64 + 1).add("b", || 1u64 + 1),
        );
        let trio = suite.add_comparison(
            "trio",
            cfg.comparison()
                .add("a", || 1u64 + 1)
                .add("b", || 1u64 + 1)
                .add("c", || 1u64 + 1),
        );
        suite.run();
        for token in [pair, trio] {
            for (name, cmp) in token.get().unwrap().against_baseline() {
                assert!(
                    cmp.min_detectable_difference().is_finite(),
                    "{name} was judged against a NaN threshold",
                );
            }
        }
    }

    /// A comparison that is not the last entry must not leave a blank line
    /// behind it: `Comparisons` ends its own output with a newline, and this
    /// loop supplies the separators.
    ///
    /// A blank line is not merely untidy - anything parsing the table a line
    /// at a time meets an empty one, as this module's own declaration-order
    /// test would.
    #[test]
    fn a_comparison_before_another_entry_leaves_no_blank_line() {
        let cfg = Config::default().with_max_time(Duration::from_millis(20));
        let mut suite = cfg.suite();
        let _ = suite.add_comparison(
            "pair",
            cfg.comparison().add("a", || 1u64 + 1).add("b", || 1u64 + 1),
        );
        let _ = suite.add("flat", || (0..20u64).sum::<u64>());
        let shown = format!("{}", suite.run());
        assert!(
            !shown.lines().any(|l| l.trim().is_empty()),
            "blank line in report:\n{shown}"
        );
        assert!(shown.lines().last().unwrap().starts_with("flat"), "{shown}");
    }

    /// A lone alternative is caught where the mistake was made, rather than
    /// deep inside the scheduler once the suite is already running.
    #[test]
    #[should_panic(expected = "at least two alternatives")]
    fn one_alternative_is_not_a_comparison() {
        let cfg = Config::default();
        let mut suite = cfg.suite();
        // No `forget` afterwards, because this line never returns - and
        // `Config::drop` skips its check while panicking, so the unwind is
        // clean.
        let _ = suite.add_comparison("lonely", cfg.comparison().add("only", || 1u64 + 1));
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

    /// A fixed amount of integer work, as one closure type however many are
    /// made - so every copy shares one compiled loop and the layout lottery
    /// cannot be mistaken for a positional effect.
    fn spin(rounds: u64) -> impl FnMut() -> u64 {
        move || {
            let mut acc = 0u64;
            for i in 0..rounds {
                acc = acc.wrapping_mul(31).wrapping_add(i);
            }
            acc
        }
    }

    /// Does where a benchmark sits in the suite change what it reads?
    ///
    /// Measures the same eight workloads twice, forward and then in reverse
    /// declaration order, and reports how far each one moved between the two.
    /// A benchmark measured in sequence at t=0 and again at t=N sees a
    /// different machine; interleaved, both runs spread it over the whole
    /// session.
    ///
    /// **The passes below are not independent replicates.** They share one
    /// session and therefore one drift state, and the sequential arm is
    /// enormously sensitive to it: byte-identical code has read 0.10% in one
    /// session and 1.19% in another. Three passes that agree tell you about
    /// that session, not about the technique - which this entry got wrong
    /// once already, and TODO item 4 records. Run it several times, at
    /// different times, and compare the *ranges*:
    ///
    /// ```none
    /// cargo test --release -- --ignored --nocapture position_bias
    /// ```
    ///
    /// Ignored, and it prints rather than asserts, because it is a
    /// measurement and not a test - four tests of exactly this shape were
    /// deleted from this crate for being asserted as though they were
    /// deterministic.
    #[test]
    #[ignore]
    fn position_bias_interleaved_versus_sequential() {
        const N: usize = 8;
        const ROUNDS: u64 = 2_000;
        println!();
        // Deliberately *not* gated on `quiesced()`. What interleaving
        // defends against is the machine moving underneath a benchmark, and
        // quiescing is the other way of stopping that - so a quiesced
        // machine is the one place this can least be seen. Run it both ways.
        println!("quiesced: {}", crate::testutil::quiesced());
        let cfg = Config::default().with_max_time(Duration::from_millis(100));

        // Sequential, as a caller writes it today: one `bench` per line.
        let sequential = |reverse: bool| -> Vec<f64> {
            let mut out = vec![0.0; N];
            let order: Vec<usize> = if reverse {
                (0..N).rev().collect()
            } else {
                (0..N).collect()
            };
            for &i in &order {
                out[i] = cfg.bench(spin(ROUNDS)).ns_per_iter;
            }
            out
        };

        // Interleaved.
        let interleaved = |reverse: bool| -> Vec<f64> {
            let mut suite = cfg.suite();
            let order: Vec<usize> = if reverse {
                (0..N).rev().collect()
            } else {
                (0..N).collect()
            };
            let tokens: Vec<(usize, Token<Stats>)> = order
                .iter()
                .map(|&i| (i, suite.add(&format!("b{i}"), spin(ROUNDS))))
                .collect();
            suite.run();
            let mut out = vec![0.0; N];
            for (i, t) in tokens {
                out[i] = t.get().unwrap().ns_per_iter;
            }
            out
        };

        let report = |label: &str, fwd: &[f64], rev: &[f64]| {
            let moved: Vec<f64> = fwd
                .iter()
                .zip(rev)
                .map(|(a, b)| (a - b).abs() / (0.5 * (a + b)))
                .collect();
            let worst = moved.iter().cloned().fold(0.0f64, f64::max);
            let mean = moved.iter().sum::<f64>() / N as f64;
            println!(
                "  {label:<12} mean {:.3}%  worst {:.3}%",
                100.0 * mean,
                100.0 * worst
            );
        };

        println!("How far each benchmark moves when the declaration order is reversed");
        println!("({N} identical workloads, one shared compiled loop)");
        for pass in 0..3 {
            println!("pass {pass}:");
            // Alternate which method goes first, so neither always gets the
            // colder machine.
            if pass % 2 == 0 {
                let (sf, sr) = (sequential(false), sequential(true));
                let (i_f, ir) = (interleaved(false), interleaved(true));
                report("sequential", &sf, &sr);
                report("interleaved", &i_f, &ir);
            } else {
                let (i_f, ir) = (interleaved(false), interleaved(true));
                let (sf, sr) = (sequential(false), sequential(true));
                report("interleaved", &i_f, &ir);
                report("sequential", &sf, &sr);
            }
        }
    }

    /// Does reaching a benchmark through a [`Suite`] change what it reads?
    ///
    /// It must not. A suite of one goes through the scheduler, the boxed
    /// future and the async sampling loop, but has nothing to interleave
    /// with, so it should measure exactly what [`bench`] measures. Anything
    /// else is overhead the suite is adding, and would mean a suite's numbers
    /// cannot be compared with a lone `bench` call even in principle.
    ///
    /// This exists because a 5% gap turned up between the two while measuring
    /// something else, in a session with long runs in it and not in a session
    /// without - which is either a real cost that only shows under some
    /// conditions, or the machine wandering. The two arms here are the same
    /// length as each other and alternate, so a gap that survives is the code
    /// path.
    ///
    /// Medians, not means: this machine produces occasional runs at twice the
    /// cost, and one of those moves a mean several percent.
    ///
    /// ```none
    /// cargo test --release -- --ignored --nocapture suite_path_costs
    /// ```
    #[test]
    #[ignore]
    fn suite_path_costs_nothing() {
        const REPEATS: usize = 24;
        println!();
        println!("quiesced: {}", crate::testutil::quiesced());
        let cfg = Config::default().with_max_time(Duration::from_millis(100));

        let mut direct: Vec<f64> = Vec::new();
        let mut viasuite: Vec<f64> = Vec::new();
        for r in 0..REPEATS {
            let seed = 0x243f_6a88_85a3_08d3u64.wrapping_mul(r as u64 + 1);
            let mut run_direct = || direct.push(cfg.bench(fixed_cost(seed)).ns_per_iter);
            let mut run_suite = || {
                let mut suite = cfg.suite();
                let subject = suite.add("subject", fixed_cost(seed));
                suite.run();
                viasuite.push(subject.get().unwrap().ns_per_iter);
            };
            // Alternate, so neither is always the one that runs cold.
            if r % 2 == 0 {
                run_direct();
                run_suite();
            } else {
                run_suite();
                run_direct();
            }
        }

        let median = |xs: &[f64]| {
            let mut v = xs.to_vec();
            v.sort_by(|a, b| a.partial_cmp(b).unwrap());
            v[v.len() / 2]
        };
        let (d, s) = (median(&direct), median(&viasuite));
        println!("  bench()      median {d:7.2}ns");
        println!("  suite of 1   median {s:7.2}ns");
        println!("  difference          {:+7.2}%", 100.0 * (s - d) / d);
    }

    /// Does interleaving stop a benchmark believing an error bar it has not
    /// earned?
    ///
    /// This is the question item 4 should have asked first. Sampling stops
    /// when the standard error of the mean gets small enough - but that
    /// standard error is computed as though the samples were independent, and
    /// on a drifting machine samples taken back to back are not: they share a
    /// drift state, agree with each other for that reason, and make the run
    /// stop early on a `±` that no repeat of it will honour. Interleaved, a
    /// benchmark's samples are spread across the whole session, so the spread
    /// it sees while sampling is the spread that is really there.
    ///
    /// The metric is the one item 1 used: **the ratio of the spread actually
    /// observed between runs to the `±` those runs claimed.** One is honest.
    /// Above one is an error bar that understates, which is the failure worth
    /// catching - a tighter number that is less true.
    ///
    /// The workload is deterministic, so everything that varies is the
    /// machine rather than the workload. And there is no long reference run:
    /// `estimates_the_mean_not_the_minimum` records what that costs, its bias
    /// swinging between -14% and +17% because twenty seconds flat out on a
    /// core is a different thermal regime than a millisecond. Both arms here
    /// are short, adaptive, and measured back to back.
    ///
    /// Ignored, and prints rather than asserts, for the reason given on
    /// [`position_bias_interleaved_versus_sequential`]:
    ///
    /// ```none
    /// cargo test --release -- --ignored --nocapture early_stop
    /// ```
    #[test]
    #[ignore]
    fn early_stop_bias_interleaved_versus_sequential() {
        const REPEATS: usize = 16;
        const FILLERS: usize = 5;
        println!();
        println!("quiesced: {}", crate::testutil::quiesced());
        let cfg = Config::default().with_max_time(Duration::from_millis(100));

        let mut seq: Vec<Stats> = Vec::new();
        let mut alone: Vec<Stats> = Vec::new();
        let mut inter: Vec<Stats> = Vec::new();
        // A suite of one is the control. It goes through every line of the
        // scheduler, the async loop and the boxed future that the real arm
        // does, but has nothing to be interleaved *with* - so its samples are
        // back to back, exactly as `bench`'s are. If the effect is
        // interleaving it should look like `bench`; if it is anything else
        // about the suite machinery, it should look like the interleaved arm.
        let run_suite = |subject_only: bool, out: &mut Vec<Stats>, seed: u64| {
            let mut suite = cfg.suite();
            let subject = suite.add("subject", fixed_cost(seed));
            if !subject_only {
                for i in 0..FILLERS {
                    let _ = suite.add(
                        &format!("filler{i}"),
                        fixed_cost(seed.wrapping_add(i as u64 + 1)),
                    );
                }
            }
            suite.run();
            out.push(subject.get().unwrap());
        };
        for r in 0..REPEATS {
            let seed = 0x9e37_79b9_7f4a_7c15u64.wrapping_mul(r as u64 + 1);
            // Rotate which arm goes first, so none of them always meets the
            // same phase of whatever the machine is doing.
            for arm in 0..3 {
                match (arm + r) % 3 {
                    0 => seq.push(cfg.bench(fixed_cost(seed))),
                    1 => run_suite(true, &mut alone, seed),
                    _ => run_suite(false, &mut inter, seed),
                }
            }
        }

        let report = |label: &str, runs: &[Stats]| {
            let mut means: Vec<f64> = runs.iter().map(|s| s.ns_per_iter).collect();
            let (_, rel_spread) = mean_and_spread(&means);
            means.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let median = means[means.len() / 2];
            // A *robust* spread beside the standard-deviation one, because
            // one run at twice the others moves an sd a long way and this
            // machine produces such runs. Half-interquartile against the
            // median: if the two disagree, the sd is describing outliers
            // rather than the distribution, and the sd is the one to
            // distrust.
            let q = |f: f64| means[((means.len() - 1) as f64 * f).round() as usize];
            let robust = (q(0.75) - q(0.25)) / 2.0 / median;
            let claimed = runs.iter().map(|s| s.rel_std_error()).sum::<f64>() / runs.len() as f64;
            let samples = runs.iter().map(|s| s.samples).sum::<usize>() as f64 / runs.len() as f64;
            println!(
                "  {label:<12} median {median:7.1}ns  min {:6.1} max {:7.1}  \
                 sd {:6.2}%  robust {:5.2}%  claimed {:.3}%  robust-honesty {:6.2}x  \
                 samples {samples:5.1}",
                means[0],
                means[means.len() - 1],
                100.0 * rel_spread,
                100.0 * robust,
                100.0 * claimed,
                robust / claimed,
            );
        };
        println!(
            "Error-bar honesty over {REPEATS} runs: observed between-run spread \
             against the claimed +/-."
        );
        println!("1.00x is honest; above 1 is an error bar that understates.");
        report("sequential", &seq);
        report("suite of 1", &alone);
        report("interleaved", &inter);
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

    /// A measurement can be had from a finished report by name, without
    /// having kept the token that was handed out when it was added.
    ///
    /// Which is the whole point: under `add_registered` nobody wrote the
    /// `add` call, so nobody holds those tokens, and a script that wants to
    /// ask something of the results has only the report.
    #[test]
    fn a_measurement_can_be_had_by_name_without_its_token() {
        let cfg = cfg();
        let mut suite = cfg.suite();
        // Deliberately dropped: this is the situation being tested.
        drop(suite.add("summing", || (0..64u64).sum::<u64>()));
        let report = suite.run();

        let stats = report.stats("summing").expect("it was measured");
        assert!(stats.ns_per_iter > 0.0);
        assert!(report.contains("summing"));
        assert_eq!(report.names().collect::<Vec<_>>(), ["summing"]);
    }

    /// Asking for the wrong type gives nothing rather than the wrong thing,
    /// so a caller that does not know what a name refers to can simply ask.
    #[test]
    fn asking_for_the_wrong_type_gives_nothing() {
        let cfg = cfg();
        let mut suite = cfg.suite();
        let _ = suite.add("flat", || (0..64u64).sum::<u64>());
        let _ = suite.add_comparison(
            "pair",
            cfg.comparison()
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
        let _ = suite.add("flat", || (0..64u64).sum::<u64>());
        let _ = suite.add_scaling("scaled", |n: usize| (0..n as u64).sum::<u64>(), 32);
        let _ = suite.add_comparison(
            "pair",
            cfg.comparison()
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
        let _ = suite.add("one", || (0..64u64).sum::<u64>());
        let _ = suite.add("two", || (0..64u64).sum::<u64>());
        let _ = suite.add_comparison(
            "pair",
            cfg.comparison()
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
        let _ = suite.add_comparison(
            "hashing",
            cfg.comparison()
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
mod filtering {
    use super::*;
    use std::time::Duration;

    fn cfg() -> Config {
        Config::default().with_max_time(Duration::from_millis(20))
    }

    #[test]
    fn only_what_the_filter_keeps_is_measured() {
        let cfg = cfg();
        let mut suite = cfg
            .suite()
            .with_filter(Filter::everything().matching("sort"));
        let sorting = suite.add("sorting", || {
            let mut v = vec![3u8, 1, 2];
            v.sort();
            v
        });
        let summing = suite.add("summing", || (0..32u64).sum::<u64>());
        assert_eq!(suite.len(), 1, "only the kept one was added at all");
        let report = suite.run();

        assert!(sorting.get().is_some());
        assert!(
            summing.get().is_none(),
            "a filtered-out benchmark is never measured",
        );
        let shown = format!("{report}");
        assert!(shown.contains("sorting"), "{shown}");
        assert!(
            !shown.contains("summing"),
            "and does not appear in the report at all: {shown}",
        );
    }

    /// The filter reaches every kind, not only flat benchmarks.
    #[test]
    fn scaling_benchmarks_and_comparisons_are_filtered_too() {
        let cfg = cfg();
        let mut suite = cfg
            .suite()
            .with_filter(Filter::everything().matching("keep"));
        let _ = suite.add_scaling("drop_scaling", |n: usize| (0..n as u64).sum::<u64>(), 32);
        let _ = suite.add_scaling("keep_scaling", |n: usize| (0..n as u64).sum::<u64>(), 32);
        let _ = suite.add_comparison(
            "drop_cmp",
            cfg.comparison()
                .add("a", || (0..32u64).sum::<u64>())
                .add("b", || (0..32u64).sum::<u64>()),
        );
        let _ = suite.add_comparison(
            "keep_cmp",
            cfg.comparison()
                .add("a", || (0..32u64).sum::<u64>())
                .add("b", || (0..32u64).sum::<u64>()),
        );
        assert_eq!(suite.len(), 2);
        let names: Vec<&str> = suite.names().collect();
        assert_eq!(names, ["keep_scaling", "keep_cmp"]);
    }

    /// A comparison is matched on its own name, not its alternatives'.
    ///
    /// Its alternatives are measured in one interleaved round so that their
    /// differences are paired; running one of them is a different and worse
    /// measurement, not a smaller one. So a comparison is in or out entire.
    #[test]
    fn a_comparison_is_filtered_whole_and_by_its_own_name() {
        let cfg = cfg();
        // `slower` is the name of an alternative, not of the comparison.
        let mut suite = cfg
            .suite()
            .with_filter(Filter::everything().matching("slower"));
        let _ = suite.add_comparison(
            "hashing",
            cfg.comparison()
                .add("base", || (0..32u64).sum::<u64>())
                .add("slower", || (0..256u64).sum::<u64>()),
        );
        assert_eq!(
            suite.len(),
            0,
            "matching an alternative does not pull in its comparison",
        );

        let mut suite = cfg
            .suite()
            .with_filter(Filter::everything().matching("hashing"));
        let cmp = suite.add_comparison(
            "hashing",
            cfg.comparison()
                .add("base", || (0..32u64).sum::<u64>())
                .add("slower", || (0..256u64).sum::<u64>()),
        );
        suite.run();
        assert_eq!(
            cmp.get().expect("kept").stats().len(),
            2,
            "and matching the comparison takes all of it",
        );
    }

    /// Filtering changes the Bonferroni limit, and should: five comparisons
    /// are five chances at a false positive and one is one. The correction
    /// is for the size of the family, so the family has to be what was
    /// actually run.
    #[test]
    fn the_threshold_follows_what_the_filter_kept() {
        let cfg = cfg();
        let mut all = cfg.suite();
        for name in ["a", "b", "c"] {
            let _ = all.add_comparison(
                name,
                cfg.comparison()
                    .add("x", || (0..32u64).sum::<u64>())
                    .add("y", || (0..32u64).sum::<u64>()),
            );
        }
        assert_eq!(all.comparisons, 3);

        let mut one = cfg.suite().with_filter(Filter::everything().matching("b"));
        for name in ["a", "b", "c"] {
            let _ = one.add_comparison(
                name,
                cfg.comparison()
                    .add("x", || (0..32u64).sum::<u64>())
                    .add("y", || (0..32u64).sum::<u64>()),
            );
        }
        assert_eq!(
            one.comparisons, 1,
            "a filtered-out comparison must not count towards the correction \
             for comparisons that were never made",
        );
    }

    /// A mistake is still a mistake when it is filtered out - a filter is
    /// for choosing what to measure, not for silencing complaints.
    #[test]
    #[should_panic(expected = "at least two alternatives")]
    fn a_filtered_out_comparison_still_has_to_be_a_comparison() {
        let cfg = cfg();
        let mut suite = cfg
            .suite()
            .with_filter(Filter::everything().matching("nothing matches this"));
        let _ = suite.add_comparison("lonely", cfg.comparison().add("only", || 1u64));
    }
}

#[cfg(test)]
mod per_benchmark_config {
    use super::*;
    use std::time::Duration;

    /// A per-benchmark `Config` governs that benchmark and nothing else.
    ///
    /// The budget is the visible half: a benchmark given a microsecond gives
    /// up early and says so, while its neighbour on the suite's own generous
    /// budget does not.
    #[test]
    fn a_per_benchmark_config_governs_only_that_benchmark() {
        let cfg = Config::default().with_max_time(Duration::from_millis(500));
        let stingy = Config::relative(1e-9).with_max_time(Duration::from_micros(1));
        let mut suite = cfg.suite();
        let ordinary = suite.add("ordinary", || (0..50u64).sum::<u64>());
        let starved = suite.add_with(&stingy, "starved", || (0..50u64).sum::<u64>());
        suite.run();
        assert!(
            starved.get().unwrap().hit_limit,
            "the benchmark given a microsecond should have run out",
        );
        assert!(
            !ordinary.get().unwrap().hit_limit,
            "its neighbour keeps the suite's budget",
        );
    }

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
        let starved = suite.add_comparison(
            "starved",
            stingy
                .comparison()
                .add("a", || (0..50u64).sum::<u64>())
                .add("b", || (0..50u64).sum::<u64>()),
        );
        let _held = crate::quiet::exclusive();
        let started = Instant::now();
        suite.run();
        let elapsed = started.elapsed();
        assert!(
            starved.get().unwrap().stats().iter().any(|s| s.hit_limit),
            "an unreachable goal must end at the budget",
        );
        assert!(
            elapsed < generous,
            "the comparison spent the suite's budget, not its own: {elapsed:?}",
        );
    }

    /// A benchmark cannot opt out of the family it is in by bringing its own
    /// `Config`: the threshold belongs to the suite.
    #[test]
    fn a_per_benchmark_config_does_not_change_the_threshold() {
        let cfg = Config::default().with_max_time(Duration::from_millis(20));
        let other = Config::relative(0.5);
        let mut suite = cfg.suite();
        let _ = suite.add_with(&other, "flat", || (0..32u64).sum::<u64>());
        for name in ["one", "two"] {
            let _ = suite.add_comparison(
                name,
                cfg.comparison()
                    .add("a", || (0..32u64).sum::<u64>())
                    .add("b", || (0..32u64).sum::<u64>()),
            );
        }
        let z = suite.z_alpha.clone();
        suite.run();
        assert_eq!(
            z.get(),
            Config::z_alpha_for(2),
            "the limit counts the suite's comparisons, whatever config each \
             benchmark was measured under",
        );
    }
}
