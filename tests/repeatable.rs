//! `#[scaling::bench]` on a zero-argument function returning `impl Fn() ->
//! O` / `impl FnMut() -> O`: setup runs once, and the returned closure is
//! what gets timed repeatedly - the pattern neither `&mut I`, `&I`, nor
//! owned `I` can express, since all three rebuild the input fresh every
//! iteration (see [`scaling::bench`]'s own doc comment).
//!
//! A separate process from `tests/macros.rs` on purpose: these tests use a
//! shared counter to prove setup ran exactly once, and any other test's
//! `measure()` call touching the same registration - unavoidable, since
//! `inventory` is one process-wide registry - would race it if this lived
//! alongside benchmarks other tests in that file measure unfiltered.

use scaling::runner::{measure, Options};
use scaling::Filter;
use std::sync::atomic::{AtomicU64, Ordering};
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
