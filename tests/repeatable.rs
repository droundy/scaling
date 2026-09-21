//! `#[scaling::bench]` on a function returning `impl Fn() -> O` / `impl
//! FnMut() -> O`: setup runs once, and the returned closure is what gets
//! timed repeatedly - the pattern an ordinary `input = <value>` /
//! `gen_input = <closure>` benchmark can't express, since both rebuild the
//! input fresh every iteration (see [`scaling::bench`]'s own doc comment).
//! Covers the zero-argument shape, both input-taking ones - `input` given to
//! the setup function that one time instead, by value or by reference - and
//! the same shape used by a comparison group member and a matrix candidate.
//!
//! `#[scaling::bench_scaling]` gets its own section further down: a scaling
//! sweep revisits every discovered size once per round rather than advancing
//! through sizes once each, so setup there runs once *per distinct size*,
//! not once overall - a materially different cache shape from everything
//! above it.
//!
//! A separate process from `tests/macros.rs` on purpose: these tests use a
//! shared counter to prove setup ran exactly once, and any other test's
//! `measure()` call touching the same registration - unavoidable, since
//! `inventory` is one process-wide registry - would race it if this lived
//! alongside benchmarks other tests in that file measure unfiltered.

use scaling::runner::{measure, Options};
use scaling::Filter;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

static SETUP_CALLS: AtomicU64 = AtomicU64::new(0);

#[scaling::bench(name = "counts_its_own_setup_calls")]
fn counts_its_own_setup_calls() -> impl FnMut() -> u64 {
    SETUP_CALLS.fetch_add(1, Ordering::SeqCst);
    let mut n = 0u64;
    move || {
        n = n.wrapping_add(1);
        n
    }
}

#[test]
fn setup_runs_exactly_once_despite_many_timed_calls() {
    let options = Options {
        filter: Filter::everything().matching("counts_its_own_setup_calls"),
        cfg: Options::default()
            .cfg
            .with_max_time(Duration::from_millis(200)),
        ..Options::default()
    };

    let report = measure(&options).expect("the registrations compose");
    let stats = report
        .stats("counts_its_own_setup_calls")
        .expect("it should have measured");

    // A 200ms budget at whatever this closure costs (a handful of ns)
    // means many thousands to millions of timed calls - if setup ran once
    // per call rather than once total, this assertion on SETUP_CALLS below
    // would see that count instead of 1.
    assert!(
        stats.iterations > 1000,
        "expected many iterations, got {}",
        stats.iterations
    );
    assert_eq!(
        SETUP_CALLS.load(Ordering::SeqCst),
        1,
        "setup must run exactly once per measure() call, not once per timed call"
    );
}

// ---- the input-taking variant: `input` is moved into the setup function
// once, same as the setup function itself running once ----

static INPUT_TAKING_SETUP_CALLS: AtomicU64 = AtomicU64::new(0);

#[scaling::bench(name = "counts_its_own_input_taking_setup_calls", input = vec![1u8, 2, 3])]
fn counts_its_own_input_taking_setup_calls(buf: Vec<u8>) -> impl FnMut() -> u64 {
    INPUT_TAKING_SETUP_CALLS.fetch_add(1, Ordering::SeqCst);
    let mut pos = 0usize;
    move || {
        pos = (pos + 1) % buf.len();
        buf[pos] as u64
    }
}

#[test]
fn input_taking_setup_runs_exactly_once_despite_many_timed_calls() {
    let options = Options {
        filter: Filter::everything().matching("counts_its_own_input_taking_setup_calls"),
        cfg: Options::default()
            .cfg
            .with_max_time(Duration::from_millis(200)),
        ..Options::default()
    };

    let report = measure(&options).expect("the registrations compose");
    let stats = report
        .stats("counts_its_own_input_taking_setup_calls")
        .expect("it should have measured");

    assert!(
        stats.iterations > 1000,
        "expected many iterations, got {}",
        stats.iterations
    );
    assert_eq!(
        INPUT_TAKING_SETUP_CALLS.load(Ordering::SeqCst),
        1,
        "setup must run exactly once per measure() call, not once per timed call, \
         and `input` must be moved into it that same one time"
    );
}

// ---- the same, but the setup function only borrows `input` - it reads a
// seed out of it to configure the persistent state, rather than owning the
// state's data outright ----

static REF_INPUT_SETUP_CALLS: AtomicU64 = AtomicU64::new(0);

#[scaling::bench(name = "counts_its_own_ref_input_setup_calls", input = 7u64)]
fn counts_its_own_ref_input_setup_calls(seed: &u64) -> impl FnMut() -> u64 {
    REF_INPUT_SETUP_CALLS.fetch_add(1, Ordering::SeqCst);
    let mut n = *seed;
    move || {
        n = n.wrapping_add(1);
        n
    }
}

#[test]
fn ref_input_setup_runs_exactly_once_despite_many_timed_calls() {
    let options = Options {
        filter: Filter::everything().matching("counts_its_own_ref_input_setup_calls"),
        cfg: Options::default()
            .cfg
            .with_max_time(Duration::from_millis(200)),
        ..Options::default()
    };

    let report = measure(&options).expect("the registrations compose");
    let stats = report
        .stats("counts_its_own_ref_input_setup_calls")
        .expect("it should have measured");

    assert!(
        stats.iterations > 1000,
        "expected many iterations, got {}",
        stats.iterations
    );
    assert_eq!(
        REF_INPUT_SETUP_CALLS.load(Ordering::SeqCst),
        1,
        "setup must run exactly once per measure() call even when it only borrows \
         its input, not once per timed call"
    );
}

// ---- the same, but `gen_input` supplies the returned closure with a value
// that must keep changing every timed call, which a setup function that
// runs once cannot supply itself - the flat counterpart of what
// `#[bench_scaling]`'s own combination with `gen_input` does per size ----

static ONE_ARG_SETUP_CALLS: AtomicU64 = AtomicU64::new(0);
static ONE_ARG_GEN_CALLS: AtomicU64 = AtomicU64::new(0);

#[scaling::bench(
    name = "counts_its_own_one_arg_setup_calls",
    gen_input = || ONE_ARG_GEN_CALLS.fetch_add(1, Ordering::SeqCst) % 1000
)]
fn counts_its_own_one_arg_setup_calls() -> impl FnMut(u64) -> bool {
    ONE_ARG_SETUP_CALLS.fetch_add(1, Ordering::SeqCst);
    let sorted: Vec<u64> = (0..1000u64).collect();
    move |target: u64| sorted.binary_search(&target).is_ok()
}

#[test]
fn one_arg_setup_runs_once_but_gen_input_runs_every_timed_call() {
    let options = Options {
        filter: Filter::everything().matching("counts_its_own_one_arg_setup_calls"),
        cfg: Options::default()
            .cfg
            .with_max_time(Duration::from_millis(200)),
        ..Options::default()
    };

    let report = measure(&options).expect("the registrations compose");
    let stats = report
        .stats("counts_its_own_one_arg_setup_calls")
        .expect("it should have measured");

    assert!(
        stats.iterations > 1000,
        "expected many iterations, got {}",
        stats.iterations
    );
    assert_eq!(
        ONE_ARG_SETUP_CALLS.load(Ordering::SeqCst),
        1,
        "setup must run exactly once despite many timed calls"
    );
    let gen_calls = ONE_ARG_GEN_CALLS.load(Ordering::SeqCst);
    assert!(
        gen_calls >= stats.iterations,
        "gen_input should run fresh on every timed call, not be cached like \
         setup is - got {gen_calls} calls against {} iterations",
        stats.iterations,
    );
}

// ---- the same, inside a comparison group: a group's own alternative closure
// is what gets called many times per round across many rounds (see
// `ComparisonSet::add_input`), exactly like `Adder::flat`/`input` is for an
// ordinary benchmark - so the same lazy `Option` setup-once shape applies
// unchanged. The group's shared input is still regenerated and cloned every
// round like any other member's; only the setup function's own work is
// skipped after the first call ----

static GROUP_SETUP_CALLS: AtomicU64 = AtomicU64::new(0);

#[scaling::bench_input(group = "counts_group_setup_calls")]
fn group_setup_data() -> u64 {
    3
}

#[scaling::bench(group = "counts_group_setup_calls", baseline)]
fn group_setup_plain(v: &mut u64) -> u64 {
    *v
}

#[scaling::bench(group = "counts_group_setup_calls")]
fn group_setup_stateful(v: &mut u64) -> impl FnMut() -> u64 {
    GROUP_SETUP_CALLS.fetch_add(1, Ordering::SeqCst);
    let base = *v;
    let mut n = 0u64;
    move || {
        n = n.wrapping_add(1);
        base + n
    }
}

#[test]
fn group_member_setup_runs_exactly_once_despite_many_timed_calls() {
    let options = Options {
        filter: Filter::everything().matching("counts_group_setup_calls"),
        cfg: Options::default()
            .cfg
            .with_max_time(Duration::from_millis(200)),
        ..Options::default()
    };

    let report = measure(&options).expect("the registrations compose");
    let cmp = report
        .comparison("counts_group_setup_calls")
        .expect("the group should have run");
    assert!(
        cmp.stats().iter().all(|s| s.iterations > 1000),
        "expected many iterations: {:?}",
        cmp.stats().iter().map(|s| s.iterations).collect::<Vec<_>>()
    );
    assert_eq!(
        GROUP_SETUP_CALLS.load(Ordering::SeqCst),
        1,
        "setup must run exactly once for a group member despite many timed calls \
         and many rounds of freshly regenerated shared input"
    );
}

// ---- the same, for a matrix candidate: `add_flat`/`add_alt` are each
// called once per (candidate, input) pairing, so `__action` is correctly
// scoped to that one pairing's whole run ----

static CANDIDATE_SETUP_CALLS: AtomicU64 = AtomicU64::new(0);

#[scaling::candidate(matrix = "counts_candidate_setup_calls", baseline)]
fn candidate_setup_plain(v: &mut u64) -> u64 {
    *v
}

#[scaling::candidate(matrix = "counts_candidate_setup_calls")]
fn candidate_setup_stateful(v: &mut u64) -> impl FnMut() -> u64 {
    CANDIDATE_SETUP_CALLS.fetch_add(1, Ordering::SeqCst);
    let base = *v;
    let mut n = 0u64;
    move || {
        n = n.wrapping_add(1);
        base + n
    }
}

#[scaling::input(matrix = "counts_candidate_setup_calls", name = "seed")]
fn candidate_setup_seed() -> u64 {
    5
}

#[test]
fn matrix_candidate_setup_runs_exactly_once_despite_many_timed_calls() {
    let options = Options {
        filter: Filter::everything().matching("counts_candidate_setup_calls@seed"),
        cfg: Options::default()
            .cfg
            .with_max_time(Duration::from_millis(200)),
        ..Options::default()
    };

    let report = measure(&options).expect("the registrations compose");
    let cmp = report
        .comparison("counts_candidate_setup_calls@seed")
        .expect("the matrix lane should have run");
    assert!(
        cmp.stats().iter().all(|s| s.iterations > 1000),
        "expected many iterations: {:?}",
        cmp.stats().iter().map(|s| s.iterations).collect::<Vec<_>>()
    );
    assert_eq!(
        CANDIDATE_SETUP_CALLS.load(Ordering::SeqCst),
        1,
        "setup must run exactly once for a matrix candidate despite many timed calls"
    );
}

// =======================================================================
// `#[scaling::bench_scaling]`: a sweep revisits every discovered size once
// per round (n1, n2, ..., nk, n1, n2, ..., not advancing monotonically -
// see `measure_scaling`'s round loop), so setup here is cached per size
// rather than in the single slot the shapes above use.
// =======================================================================

/// How many times setup ran for each size it was asked to build, across the
/// whole run - `None` until the first call creates it.
type SetupLog = Mutex<Option<HashMap<usize, u32>>>;

fn record(log: &SetupLog, n: usize) {
    let mut log = log.lock().unwrap();
    *log.get_or_insert_with(HashMap::new).entry(n).or_insert(0) += 1;
}

static SCALING_SETUP_LOG: SetupLog = Mutex::new(None);

#[scaling::bench_scaling(name = "counts_scaling_setup_calls", nmin = 4)]
fn scaling_setup_once_per_size(n: usize) -> impl FnMut() -> usize {
    record(&SCALING_SETUP_LOG, n);
    let mut calls = 0usize;
    move || {
        calls += 1;
        n + calls
    }
}

#[test]
fn scaling_setup_runs_at_most_once_per_distinct_size() {
    let options = Options {
        filter: Filter::everything().matching("counts_scaling_setup_calls"),
        cfg: Options::default()
            .cfg
            .with_max_time(Duration::from_millis(200)),
        ..Options::default()
    };

    let report = measure(&options).expect("the registrations compose");
    let stats = report
        .scaling("counts_scaling_setup_calls")
        .expect("it should have measured");

    let log = SCALING_SETUP_LOG.lock().unwrap();
    let log = log.as_ref().expect("setup should have run at least once");
    assert!(
        log.len() >= 2,
        "expected the sweep to visit several sizes, got {log:?}",
    );
    assert!(
        stats.iterations > 10 * log.len() as u64,
        "expected many more timed calls than distinct sizes (iterations={}, \
         sizes={}) - otherwise the cache bought nothing worth testing",
        stats.iterations,
        log.len(),
    );
    for (&n, &count) in log.iter() {
        assert_eq!(
            count, 1,
            "size {n} was set up {count} times, expected exactly once"
        );
    }
}

/// The `gen_input` variant: an expensive size-`n` structure (a `BTreeMap`
/// full of entries, in the motivating case) is built once per size by
/// setup, exactly like the plain shape above - `gen_input` itself is
/// unchanged, still called fresh on every timed call, and its result is fed
/// to the *cached* closure as an argument (a random lookup key, say, so
/// every call queries something different even though the map it queries
/// was only ever built once per size).
static GEN_SCALING_SETUP_LOG: SetupLog = Mutex::new(None);
static GEN_SCALING_GEN_CALLS: AtomicU64 = AtomicU64::new(0);

#[scaling::bench_scaling(
    name = "counts_gen_scaling_setup_calls",
    nmin = 4,
    gen_input = |n: usize| {
        let call_num = GEN_SCALING_GEN_CALLS.fetch_add(1, Ordering::SeqCst);
        (n as u64).wrapping_add(call_num)
    }
)]
fn gen_scaling_setup_once_per_size(n: usize) -> impl FnMut(u64) -> u64 {
    record(&GEN_SCALING_SETUP_LOG, n);
    let base = n as u64;
    move |k: u64| base.wrapping_add(k)
}

#[test]
fn gen_input_scaling_setup_runs_once_per_size_but_gen_input_every_call() {
    let options = Options {
        filter: Filter::everything().matching("counts_gen_scaling_setup_calls"),
        cfg: Options::default()
            .cfg
            .with_max_time(Duration::from_millis(200)),
        ..Options::default()
    };

    let _ = measure(&options).expect("the registrations compose");

    let setup_log = GEN_SCALING_SETUP_LOG.lock().unwrap();
    let setup_log = setup_log.as_ref().expect("setup should have run");
    assert!(
        setup_log.len() >= 2,
        "expected several sizes: {setup_log:?}"
    );
    for (&n, &count) in setup_log.iter() {
        assert_eq!(
            count, 1,
            "setup was expected to run once for size {n}, not {count} times - \
             an expensive size-n structure rebuilt on every timed call is \
             exactly what this shape exists to avoid",
        );
    }

    let gen_calls = GEN_SCALING_GEN_CALLS.load(Ordering::SeqCst);
    assert!(
        gen_calls > 10 * setup_log.len() as u64,
        "gen_input should run fresh on every timed call, not be cached per \
         size like setup is - got {gen_calls} calls across {} sizes",
        setup_log.len(),
    );
}

static FILTERED_OUT_SETUP_CALLS: AtomicU64 = AtomicU64::new(0);

#[scaling::bench(name = "should_stay_filtered_out")]
fn should_stay_filtered_out() -> impl FnMut() -> u64 {
    FILTERED_OUT_SETUP_CALLS.fetch_add(1, Ordering::SeqCst);
    move || 0
}

#[test]
fn setup_does_not_run_for_a_benchmark_the_filter_excluded() {
    // Matches neither registration in this file - `should_stay_filtered_out`
    // must be excluded (that's what this test checks), and it must not
    // incidentally match `counts_its_own_setup_calls` either, which would
    // race that other test's own count of the same shared counter.
    let options = Options {
        filter: Filter::everything().matching("nothing_registered_in_this_file_matches"),
        ..Options::default()
    };
    let _ = measure(&options).expect("the registrations compose");

    assert_eq!(
        FILTERED_OUT_SETUP_CALLS.load(Ordering::SeqCst),
        0,
        "setup must not run for a benchmark the filter excluded - a filtered-out \
         benchmark should not pay for setup it will never use",
    );
}
