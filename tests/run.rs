//! `Config::run_and_print` and `Config::run`, driven the way a benchmark
//! binary's `main` drives them.
//!
//! A benchmark binary's `main` calls [`Config::run_and_print`] on the default
//! [`Config`]; these build one directly, so what is exercised is everything
//! that call would reach. Registrations are written with the attributes rather
//! than by hand, because the thing under test is the whole path from an
//! attribute to the printed results.

use scaling::Config;
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
fn quick() -> Config {
    Config::relative(0.02).with_max_time(Duration::from_millis(40))
}

#[test]
fn a_run_measures_what_was_registered() {
    quick()
        .run_and_print()
        .expect("these registrations compose");
}

/// A script can measure and then read the numbers, rather than reading a
/// printout of them.
///
/// This is the path that has to survive the removal of the hand-assembled
/// API: `run_and_print` prints, which answers "did anything regress" but not
/// "which of these is actually fastest here". Names are how results are
/// reached, because nobody wrote them down - they come from the module and
/// function a benchmark was declared in.
#[test]
fn a_script_can_read_the_numbers_it_measured() {
    let report = quick().run().expect("these registrations compose");

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
        against.difference().expect("it was compared").ns > 0.0,
        "ten times the work should measure slower, not faster",
    );

    assert!(report.timing("flat").is_ok());
}

/// Registrations that do not compose come back as a list rather than a
/// panic, so a script can say what is wrong in its own words.
#[test]
fn run_hands_back_what_it_could_not_assemble() {
    // Nothing here contradicts anything, so this is the `Ok` half; the `Err`
    // half is covered against deliberately broken registrations in
    // `suite::bad_registrations` (src/suite.rs), which cannot share a
    // binary with these.
    assert!(quick().run().is_ok());
}
