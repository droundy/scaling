//! The runner, driven the way `scaling::main!()` drives it.
//!
//! `scaling::main!()` reads the command line and calls
//! [`scaling::runner::run`]; these build the same [`Options`] directly, so
//! what is exercised is everything that macro would reach. Registrations are
//! written with the attributes rather than by hand, because the thing under
//! test is the whole path from an attribute to an exit status.

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

#[scaling::input(group = "regressing")]
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

// ---- a flat benchmark and a matrix, so every branch of the output has
// something to print ----

#[scaling::bench(name = "flat")]
fn flat() -> u64 {
    work(64)
}

#[scaling::bench(group = "sorting", baseline, name = "stable")]
fn stable(v: &mut Vec<u64>) {
    v.sort();
}

#[scaling::bench(group = "sorting", name = "unstable")]
fn unstable(v: &mut Vec<u64>) {
    v.sort_unstable();
}

#[scaling::input(group = "sorting", name = "reversed")]
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
    for format in [Format::Table, Format::List] {
        let outcome = run(Options { format, ..quick() });
        assert_eq!(outcome, Outcome::Measured, "{format:?}");
    }
}

/// A filter that matches nothing is a successful run of nothing, not an
/// error: a `Filter` is how a caller narrows a run, and mistyping a pattern
/// should say so rather than look like a broken build.
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
        .comparison("regressing@regressing_input")
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

/// `Filter::listing(true)` measures nothing, so it is fast whatever the
/// budget says - and it is the answer to "did my benchmark get linked in",
/// which is the question `inventory`'s failure mode provokes.
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
