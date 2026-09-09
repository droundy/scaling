//! The runner, driven the way `scaling::main!()` drives it.
//!
//! `scaling::main!()` reads the command line and calls
//! [`scaling::runner::run`]; these build the same [`Options`] directly, so
//! what is exercised is everything that macro would reach. Registrations are
//! written with the attributes rather than by hand, because the thing under
//! test is the whole path from an attribute to an exit status.
//!
//! Stages 7 and 8 of `REGISTRATION.md`.

use scaling::runner::{measure, run, Format, Options, Outcome};
use scaling::{Config, Filter};
use std::time::Duration;

fn work(n: usize) -> u64 {
    (0..n as u64).fold(0u64, |a, x| a.wrapping_mul(31).wrapping_add(x))
}

// ---- a comparison one of whose alternatives is deliberately slower ----
//
// Not "might be slower on a bad day": ten times the work, so the difference
// is far outside anything the machine's noise reaches, and the test does not
// depend on a quiet machine.

#[scaling::gen_input(group = "regressing")]
fn regressing_input() -> Vec<u64> {
    (0..64u64).collect()
}

#[scaling::bench(group = "regressing", baseline, name = "fast")]
fn fast(v: &mut Vec<u64>) -> u64 {
    v.iter().fold(0u64, |a, x| a.wrapping_add(*x))
}

#[scaling::bench(group = "regressing", name = "slow")]
fn slow(v: &mut Vec<u64>) -> u64 {
    let mut total = 0u64;
    for _ in 0..10 {
        total = total.wrapping_add(v.iter().fold(0u64, |a, x| a.wrapping_add(*x)));
    }
    total
}

// ---- and one where the *baseline* is the slow half ----
//
// The same ten-times difference the other way round. A comparison that
// detects a change is not a regression unless the change is a slowdown, and
// this is what says so.

#[scaling::gen_input(group = "improving")]
fn improving_input() -> Vec<u64> {
    (0..64u64).collect()
}

#[scaling::bench(group = "improving", baseline, name = "was_slow")]
fn was_slow(v: &mut Vec<u64>) -> u64 {
    let mut total = 0u64;
    for _ in 0..10 {
        total = total.wrapping_add(v.iter().fold(0u64, |a, x| a.wrapping_add(*x)));
    }
    total
}

#[scaling::bench(group = "improving", name = "now_fast")]
fn now_fast(v: &mut Vec<u64>) -> u64 {
    v.iter().fold(0u64, |a, x| a.wrapping_add(*x))
}

// ---- a flat benchmark and a matrix, so every branch of the output has
// something to print ----

#[scaling::bench(name = "flat")]
fn flat() -> u64 {
    work(64)
}

#[scaling::candidate(matrix = "sorting", baseline, name = "stable")]
fn stable(v: &mut Vec<u64>) {
    v.sort();
}

#[scaling::candidate(matrix = "sorting", name = "unstable")]
fn unstable(v: &mut Vec<u64>) {
    v.sort_unstable();
}

#[scaling::input(matrix = "sorting", name = "reversed")]
fn reversed() -> Vec<u64> {
    (0..128u64).rev().collect()
}

/// Short enough that the whole file stays a test rather than a benchmark.
fn quick() -> Options {
    Options {
        cfg: Config::relative(0.02).with_max_time(Duration::from_millis(40)),
        ..Options::default()
    }
}

#[test]
fn a_run_measures_what_was_registered() {
    let outcome = run(quick());
    assert_eq!(outcome, Outcome::Measured);
}

/// Every format prints, and none of them changes the verdict.
#[test]
fn each_format_runs() {
    for format in [Format::Table, Format::List, Format::Json] {
        let outcome = run(Options { format, ..quick() });
        assert_eq!(outcome, Outcome::Measured, "{format:?}");
    }
}

/// The check is opt-in, so the same measurement passes without it and fails
/// with it. Asserting both is what shows the flag is doing the work, rather
/// than the run happening to fail for some other reason.
#[test]
fn fail_on_regression_notices_the_slower_alternative() {
    let filter = Filter::everything().matching("regressing");

    let quiet = run(Options {
        filter: filter.clone(),
        ..quick()
    });
    assert_eq!(
        quiet,
        Outcome::Measured,
        "a slower alternative is not a failure unless asked about",
    );

    let asked = run(Options {
        filter,
        fail_on_regression: true,
        ..quick()
    });
    assert_eq!(
        asked,
        Outcome::Failed,
        "ten times the work should read as a regression",
    );
}

/// A detected change is not a regression unless it is a slowdown.
///
/// The direction is the whole content of the check, and it is easy to leave
/// out - `is_changed()` alone reads as though it means "something is wrong",
/// and it would fail every run where anything got faster.
///
/// An earlier version of this compared `sort` against `sort_unstable` on the
/// theory that they measure the same. They do not: on Rust 1.71 the unstable
/// sort came out 7.3% slower on this input, correctly detected, and the test
/// failed - it had been asserting a property of the standard library rather
/// than of the runner. What it asks now is the ten-times difference used
/// above, pointed the other way, which no library change moves.
#[test]
fn fail_on_regression_is_quiet_when_something_got_faster() {
    let outcome = run(Options {
        filter: Filter::everything().matching("improving"),
        fail_on_regression: true,
        ..quick()
    });
    assert_eq!(outcome, Outcome::Measured);
}

/// A filter that matches nothing is a successful run of nothing, not an
/// error: `--filter` is how a person narrows a run, and mistyping it should
/// say so rather than look like a broken build.
#[test]
fn a_filter_matching_nothing_still_succeeds() {
    let outcome = run(Options {
        filter: Filter::everything().matching("no-such-benchmark"),
        ..quick()
    });
    assert_eq!(outcome, Outcome::Measured);
}

/// A script can measure and then read the numbers, rather than reading a
/// printout of them.
///
/// This is the path that has to survive the removal of the hand-assembled
/// API: `run` prints and returns a verdict, which answers "did anything
/// regress" but not "which of these is actually fastest here". Names are how
/// results are reached, because nobody wrote them down - they come from the
/// module and function a benchmark was declared in.
#[test]
fn a_script_can_read_the_numbers_it_measured() {
    let report = measure(&Options {
        filter: Filter::everything().matching("regressing"),
        ..quick()
    })
    .expect("these registrations compose");

    let comparison = report
        .comparison("regressing")
        .expect("the group was measured");
    assert_eq!(comparison.baseline_name(), "fast");

    let (name, against) = comparison
        .against_baseline()
        .next()
        .expect("one alternative beyond the baseline");
    assert_eq!(name, "slow");
    assert!(
        against.difference_ns() > 0.0,
        "ten times the work should measure slower, not faster",
    );

    assert!(
        report.stats("flat").is_none(),
        "the filter kept one group, so nothing else is in the report",
    );
}

/// Registrations that do not compose come back as a list rather than a
/// panic, so a script can say what is wrong in its own words.
#[test]
fn measure_hands_back_what_it_could_not_assemble() {
    // Nothing here contradicts anything, so this is the `Ok` half; the `Err`
    // half is covered against deliberately broken registrations in
    // `tests/registry_bad.rs`, which cannot share a binary with these.
    assert!(measure(&quick()).is_ok());
}

/// `--list` measures nothing, so it is fast whatever the budget says - and
/// it is the answer to "did my benchmark get linked in", which is the
/// question `inventory`'s failure mode provokes.
#[test]
fn listing_measures_nothing() {
    let started = std::time::Instant::now();
    let outcome = run(Options {
        cfg: Config::default(),
        filter: Filter::everything().listing(true),
        ..Options::default()
    });
    assert_eq!(outcome, Outcome::Measured);
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "listing took {:?}, so it measured something",
        started.elapsed(),
    );
}
