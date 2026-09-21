//! `#[scaling::bench]` on a function returning `impl Fn() -> O` / `impl
//! FnMut() -> O`: setup runs once, and the returned closure is what gets
//! timed repeatedly - the pattern an ordinary `input = <value>` /
//! `make_input = <closure>` benchmark can't express, since both rebuild the
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

// ---- `#[bench_input]` itself may use the setup-once shape too: unlike
// `make_input = <closure>` on an ordinary benchmark, which is user code
// free to capture whatever persistent state it likes, `#[bench_input]`'s
// function becomes a plain `fn` once registered - no closure environment
// of its own to hold anything in - so this is the one place the shape
// needs crate support rather than being achievable with an ordinary
// closure. The expensive part builds once, ever, for the group's whole
// run; the returned closure still runs the same number of times an
// ordinary #[bench_input] function would - once per batch entry, several
// per round (see `refill`/`time_batch`) - and its result is shared,
// cheaply cloned, by every member of that round ----

static BENCH_INPUT_BUILD_CALLS: AtomicU64 = AtomicU64::new(0);
static BENCH_INPUT_ROUND_CALLS: AtomicU64 = AtomicU64::new(0);

#[scaling::bench_input(group = "counts_bench_input_build_calls")]
fn setup_once_shared_data() -> impl FnMut() -> ::std::sync::Arc<Vec<u64>> {
    let big = ::std::sync::Arc::new({
        BENCH_INPUT_BUILD_CALLS.fetch_add(1, Ordering::SeqCst);
        (0..1000u64).collect::<Vec<u64>>()
    });
    move || {
        BENCH_INPUT_ROUND_CALLS.fetch_add(1, Ordering::SeqCst);
        big.clone()
    }
}

#[scaling::bench(group = "counts_bench_input_build_calls", baseline)]
fn setup_once_sum_a(v: &mut ::std::sync::Arc<Vec<u64>>) -> u64 {
    v.iter().sum()
}

#[scaling::bench(group = "counts_bench_input_build_calls")]
fn setup_once_sum_b(v: &mut ::std::sync::Arc<Vec<u64>>) -> u64 {
    v.iter().fold(0u64, |a, x| a.wrapping_add(*x))
}

#[test]
fn bench_input_setup_runs_once_ever_while_its_closure_keeps_running() {
    let options = Options {
        filter: Filter::everything().matching("counts_bench_input_build_calls"),
        cfg: Options::default()
            .cfg
            .with_max_time(Duration::from_millis(200)),
        ..Options::default()
    };

    let report = measure(&options).expect("the registrations compose");
    let cmp = report
        .comparison("counts_bench_input_build_calls")
        .expect("the group should have run");
    let rounds: Vec<usize> = cmp.stats().iter().map(|s| s.samples).collect();
    assert!(
        rounds.iter().all(|&n| n > 10),
        "expected several rounds: {rounds:?}",
    );

    assert_eq!(
        BENCH_INPUT_BUILD_CALLS.load(Ordering::SeqCst),
        1,
        "the expensive part must build exactly once, ever, regardless of how \
         many rounds or timed calls follow"
    );
    // Each round refills a whole batch of `unit` entries - see
    // `time_batch`/`refill` - and a group's generator supplies one entry at
    // a time, so its closure runs `unit` times per round, not once; the
    // cached expensive part is what makes that cheap. Rather than pin down
    // `unit` exactly, this just checks the closure kept running throughout
    // - at least once per round, and demonstrably more than that.
    let round_calls = BENCH_INPUT_ROUND_CALLS.load(Ordering::SeqCst);
    assert!(
        round_calls >= rounds[0] as u64,
        "the generator's returned closure should run at least once per \
         round - got {round_calls} calls across {} rounds",
        rounds[0],
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

/// The recommended way to combine "an expensive size-`n` structure, built
/// once" with "a cheap value that must differ every call": an ordinary
/// `make_input` closure that caches the expensive part behind an `Arc`,
/// keyed by size, and returns a fresh cheap value alongside a clone of it
/// each call - no special shape on the benchmark function at all, which is
/// what makes this compose with comparisons (every alternative shares the
/// same cached `Arc`) in a way a setup function returning `impl
/// Fn(K)/FnMut(K) -> O` could not.
static GEN_SCALING_BUILD_LOG: SetupLog = Mutex::new(None);
static GEN_SCALING_GEN_CALLS: AtomicU64 = AtomicU64::new(0);

#[scaling::bench_scaling(
    name = "counts_gen_scaling_build_calls",
    nmin = 4,
    make_input = {
        let mut cache: HashMap<usize, ::std::sync::Arc<u64>> = HashMap::new();
        move |n: usize| {
            let call_num = GEN_SCALING_GEN_CALLS.fetch_add(1, Ordering::SeqCst);
            let big = cache.entry(n).or_insert_with(|| {
                record(&GEN_SCALING_BUILD_LOG, n);
                ::std::sync::Arc::new(n as u64)
            });
            (big.clone(), call_num)
        }
    }
)]
fn gen_scaling_shares_cached_state(input: &(::std::sync::Arc<u64>, u64)) -> u64 {
    input.0.wrapping_add(input.1)
}

#[test]
fn make_input_can_cache_state_per_size_entirely_in_user_code() {
    let options = Options {
        filter: Filter::everything().matching("counts_gen_scaling_build_calls"),
        cfg: Options::default()
            .cfg
            .with_max_time(Duration::from_millis(200)),
        ..Options::default()
    };

    let _ = measure(&options).expect("the registrations compose");

    let build_log = GEN_SCALING_BUILD_LOG.lock().unwrap();
    let build_log = build_log
        .as_ref()
        .expect("the cache should have built something");
    assert!(
        build_log.len() >= 2,
        "expected several sizes: {build_log:?}"
    );
    for (&n, &count) in build_log.iter() {
        assert_eq!(
            count, 1,
            "the expensive part was expected to build once for size {n}, not \
             {count} times - rebuilding it on every timed call is exactly what \
             the cache exists to avoid",
        );
    }

    let gen_calls = GEN_SCALING_GEN_CALLS.load(Ordering::SeqCst);
    assert!(
        gen_calls > 10 * build_log.len() as u64,
        "make_input itself should still run fresh on every timed call, not be \
         cached the way the expensive part inside it is - got {gen_calls} \
         calls across {} sizes",
        build_log.len(),
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
