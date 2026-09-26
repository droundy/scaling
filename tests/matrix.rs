//! Matrices: implementations and inputs written apart from each other.
//!
//! Nothing below lists a pairing. Six declarations make a three-by-two
//! matrix, and adding a seventh - either an implementation or an input -
//! would extend it without touching anything already written. That is the
//! thing distributed registration buys which hand assembly cannot: a central
//! list of pairings is exactly what there is nowhere to put.
//!
//! Driven through [`scaling::runner::measure`] rather than `Config::suite` -
//! see `tests/macros.rs`'s doc comment for why.

use scaling::Config;
use std::time::Duration;

// ---- three implementations of the same thing ----

#[scaling::bench(group = "sorting", baseline)]
fn stable(v: &mut Vec<u64>) {
    v.sort();
}

#[scaling::bench(group = "sorting")]
fn unstable(v: &mut Vec<u64>) {
    v.sort_unstable();
}

#[scaling::bench(group = "sorting")]
fn thrice(v: &mut Vec<u64>) {
    v.sort();
    v.sort_unstable();
    v.sort();
}

// ---- two inputs, written somewhere else entirely ----

#[scaling::input(group = "sorting", name = "reversed")]
fn reversed() -> Vec<u64> {
    (0..400u64).rev().collect()
}

#[scaling::input(group = "sorting", name = "sorted")]
fn already_sorted() -> Vec<u64> {
    (0..400u64).collect()
}

// ---- a second matrix, of a different type, to show lanes keep apart ----

#[scaling::bench(group = "hashing", baseline)]
fn sum_bytes(s: &mut String) -> u64 {
    s.bytes().map(u64::from).sum()
}

#[scaling::bench(group = "hashing")]
fn fold_bytes(s: &mut String) -> u64 {
    s.bytes().fold(0u64, |a, b| a.wrapping_add(u64::from(b)))
}

#[scaling::input(group = "hashing", name = "short")]
fn short_text() -> String {
    "the quick brown fox".repeat(4)
}

// ---- an input registered at several sizes from one function ----

#[scaling::input(group = "sized", sizes(64, 256))]
fn ramp(n: usize) -> Vec<u64> {
    (0..n as u64).collect()
}

// ---- and one with `name` overriding the bare function name too ----

#[scaling::input(group = "sized", name = "flat", sizes(32, 128))]
fn flat_of_len(n: usize) -> Vec<u64> {
    vec![7u64; n]
}

#[scaling::bench(group = "sized", baseline)]
fn total(v: &mut Vec<u64>) -> u64 {
    v.iter().sum()
}

#[scaling::bench(group = "sized")]
fn total_folded(v: &mut Vec<u64>) -> u64 {
    v.iter().fold(0u64, |a, b| a.wrapping_add(*b))
}

// ---------------------------------------------------------------------

fn run() -> scaling::Report {
    let options = scaling::runner::Options {
        cfg: Config::default().with_max_time(Duration::from_millis(30)),
        ..scaling::runner::Options::default()
    };
    scaling::runner::measure(&options).expect("these registrations compose")
}

/// The cross-product forms itself, and every cell is a comparison.
#[test]
fn every_pairing_is_measured() {
    let report = run();

    // sorting: 3 candidates x 2 inputs -> one comparison per input.
    for input in ["reversed", "sorted"] {
        let cmps = report
            .comparison(&format!("sorting@{input}"))
            .unwrap_or_else(|| panic!("sorting@{input} did not run"));
        assert_eq!(cmps.stats().len(), 3, "three candidates on {input}");
        assert_eq!(cmps.against_baseline().count(), 2);
    }

    // hashing is a different type, so it is its own lane and unaffected.
    let hashing = report.comparison("hashing@short").unwrap();
    assert_eq!(hashing.stats().len(), 2);

    // sized: one input function registered at two sizes.
    for size in [64, 256] {
        assert!(
            report.contains(&format!("sized@ramp@{size}")),
            "expected a comparison per size",
        );
    }

    // sized: `name = "flat"` must override the bare function name here too,
    // not just when `sizes(..)` is absent.
    for size in [32, 128] {
        assert!(
            report.contains(&format!("sized@flat@{size}")),
            "name should override the bare function name under sizes(..) too",
        );
        assert!(
            !report.contains(&format!("sized@flat_of_len@{size}")),
            "the bare function name should not have been used instead",
        );
    }
}

/// A matrix candidate is never handed an input of another lane's type.
///
/// `sorting` takes `Vec<u64>` and `hashing` takes `String`. If lanes leaked
/// into one another this would not merely measure the wrong thing, it would
/// panic on the downcast - so passing is the assertion.
#[test]
fn lanes_of_different_types_stay_apart() {
    let report = run();
    assert!(report.contains("sorting@reversed"));
    assert!(report.contains("hashing@short"));
    assert!(
        !report.contains("sorting@short"),
        "a Vec<u64> candidate must not be paired with a String input",
    );
    assert!(
        !report.contains("hashing@reversed"),
        "a String candidate must not be paired with a Vec<u64> input",
    );
}

/// A comparison carries each candidate's own timing as well as its
/// difference from the baseline, which is why always comparing costs nothing
/// - the plain grid of numbers is still in there.
#[test]
fn a_comparison_still_has_every_absolute_timing() {
    let report = run();
    let cmps = report.comparison("sorting@reversed").unwrap();
    for stats in cmps.stats() {
        assert!(
            stats.ns_per_iter > 0.0,
            "every cell keeps its own measured time, not only a ratio",
        );
    }
}

/// The declared baseline is used. `stable` is not first alphabetically -
/// `thrice` is not either, but `stable` sorts after the others only because
/// of the word `baseline`.
#[test]
fn the_declared_baseline_wins_over_alphabetical_order() {
    let report = run();
    let cmps = report.comparison("sorting@reversed").unwrap();
    let against: Vec<&str> = cmps.against_baseline().map(|(n, _)| n).collect();
    assert_eq!(against.len(), 2);
    assert!(!against.contains(&"stable"), "{against:?}");
    assert!(against.contains(&"unstable"), "{against:?}");
    assert!(against.contains(&"thrice"), "{against:?}");
}

// ---- a matrix with a Design A candidate: setup runs once per (candidate,
// input) pairing, not every timed call ----

#[scaling::bench(group = "stateful", baseline)]
fn plain_sum(v: &mut Vec<u64>) -> u64 {
    v.iter().sum()
}

#[scaling::bench(group = "stateful")]
fn cached_sum(v: &mut Vec<u64>) -> impl FnMut() -> u64 {
    let total: u64 = v.iter().sum();
    move || total
}

#[scaling::input(group = "stateful", name = "data")]
fn stateful_data() -> Vec<u64> {
    (0..300u64).collect()
}

/// The Design A candidate above measures alongside an ordinary one, just
/// like any other pairing in this matrix.
#[test]
fn a_design_a_candidate_is_measured_like_any_other() {
    let report = run();
    let cmps = report
        .comparison("stateful@data")
        .expect("stateful@data did not run");
    assert_eq!(cmps.stats().len(), 2);
    assert_eq!(cmps.against_baseline().count(), 1);
}

// ---- one generic implementation, registered at two concrete types ----
//
// Monomorphisation is the one place a matrix has to be explicit: Rust has no
// way to instantiate a generic function at "whatever types are around", so
// the types are listed. It is still one annotation rather than one wrapper
// function per type.

#[scaling::bench(group = "generic", types(String, Vec<u8>))]
fn byte_sum<T: AsRef<[u8]>>(x: &mut T) -> u64 {
    x.as_ref().iter().map(|b| u64::from(*b)).sum()
}

#[scaling::bench(group = "generic", types(String, Vec<u8>))]
fn byte_fold<T: AsRef<[u8]>>(x: &mut T) -> u64 {
    x.as_ref()
        .iter()
        .fold(0u64, |a, b| a.wrapping_add(u64::from(*b)))
}

#[scaling::input(group = "generic", name = "text")]
fn generic_text() -> String {
    "abcdefghij".repeat(8)
}

#[scaling::input(group = "generic", name = "bytes")]
fn generic_bytes() -> Vec<u8> {
    (0..80u8).collect()
}

/// A generic candidate listed at two types becomes two registrations, which
/// land in the two lanes their types belong to.
#[test]
fn a_generic_candidate_is_registered_once_per_listed_type() {
    let report = run();

    // One comparison per input, and the two inputs are of different types -
    // so each lane found both candidates at its own type.
    for input in ["text", "bytes"] {
        let cmps = report
            .comparison(&format!("generic@{input}"))
            .unwrap_or_else(|| panic!("generic@{input} did not run"));
        assert_eq!(
            cmps.stats().len(),
            2,
            "both candidates should be instantiated for {input}",
        );
    }
}
