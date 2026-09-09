//! Matrices: implementations and inputs written apart from each other.
//!
//! Nothing below lists a pairing. Six declarations make a three-by-two
//! matrix, and adding a seventh - either an implementation or an input -
//! would extend it without touching anything already written. That is the
//! thing distributed registration buys which hand assembly cannot: a central
//! list of pairings is exactly what there is nowhere to put.
//!
//! Stage 5 of `REGISTRATION.md`.

use scaling::Config;
use std::time::Duration;

// ---- three implementations of the same thing ----

#[scaling::candidate(matrix = "sorting", baseline)]
fn stable(v: &mut Vec<u64>) {
    v.sort();
}

#[scaling::candidate(matrix = "sorting")]
fn unstable(v: &mut Vec<u64>) {
    v.sort_unstable();
}

#[scaling::candidate(matrix = "sorting")]
fn thrice(v: &mut Vec<u64>) {
    v.sort();
    v.sort_unstable();
    v.sort();
}

// ---- two inputs, written somewhere else entirely ----

#[scaling::input(matrix = "sorting", name = "reversed")]
fn reversed() -> Vec<u64> {
    (0..400u64).rev().collect()
}

#[scaling::input(matrix = "sorting", name = "sorted")]
fn already_sorted() -> Vec<u64> {
    (0..400u64).collect()
}

// ---- a second matrix, of a different type, to show lanes keep apart ----

#[scaling::candidate(matrix = "hashing", baseline)]
fn sum_bytes(s: &mut String) -> u64 {
    s.bytes().map(u64::from).sum()
}

#[scaling::candidate(matrix = "hashing")]
fn fold_bytes(s: &mut String) -> u64 {
    s.bytes().fold(0u64, |a, b| a.wrapping_add(u64::from(b)))
}

#[scaling::input(matrix = "hashing", name = "short")]
fn short_text() -> String {
    "the quick brown fox".repeat(4)
}

// ---- an input registered at several sizes from one function ----

#[scaling::input(matrix = "sized", sizes(64, 256))]
fn ramp(n: usize) -> Vec<u64> {
    (0..n as u64).collect()
}

#[scaling::candidate(matrix = "sized", baseline)]
fn total(v: &mut Vec<u64>) -> u64 {
    v.iter().sum()
}

#[scaling::candidate(matrix = "sized")]
fn total_folded(v: &mut Vec<u64>) -> u64 {
    v.iter().fold(0u64, |a, b| a.wrapping_add(*b))
}

// ---------------------------------------------------------------------

fn run() -> (scaling::Report, scaling::RegisteredTokens) {
    let cfg = Config::default().with_max_time(Duration::from_millis(30));
    let mut suite = cfg.suite();
    let tokens = suite.add_registered();
    (suite.run(), tokens)
}

/// The cross-product forms itself, and every cell is a comparison.
#[test]
fn every_pairing_is_measured() {
    let (_report, tokens) = run();

    // sorting: 3 candidates x 2 inputs -> one comparison per input.
    for input in ["reversed", "sorted"] {
        let cmps = tokens.comparisons[&format!("sorting@{input}")]
            .get()
            .unwrap_or_else(|| panic!("sorting@{input} did not run"));
        assert_eq!(cmps.stats().len(), 3, "three candidates on {input}");
        assert_eq!(cmps.against_baseline().count(), 2);
    }

    // hashing is a different type, so it is its own lane and unaffected.
    let hashing = tokens.comparisons["hashing@short"].get().unwrap();
    assert_eq!(hashing.stats().len(), 2);

    // sized: one input function registered at two sizes.
    for size in [64, 256] {
        assert!(
            tokens
                .comparisons
                .contains_key(&format!("sized@ramp@{size}")),
            "expected a comparison per size, have {:?}",
            tokens.comparisons.keys().collect::<Vec<_>>(),
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
    let (_report, tokens) = run();
    assert!(tokens.comparisons.contains_key("sorting@reversed"));
    assert!(tokens.comparisons.contains_key("hashing@short"));
    assert!(
        !tokens.comparisons.contains_key("sorting@short"),
        "a Vec<u64> candidate must not be paired with a String input",
    );
    assert!(
        !tokens.comparisons.contains_key("hashing@reversed"),
        "a String candidate must not be paired with a Vec<u64> input",
    );
}

/// Nothing is wrong with any of the above, so nothing is warned about.
#[test]
fn a_well_formed_matrix_warns_about_nothing() {
    let (_report, tokens) = run();
    assert!(
        tokens.warnings.is_empty(),
        "unexpected warnings: {:?}",
        tokens
            .warnings
            .iter()
            .map(|w| w.to_string())
            .collect::<Vec<_>>(),
    );
}

/// A comparison carries each candidate's own timing as well as its
/// difference from the baseline, which is why always comparing costs nothing
/// - the plain grid of numbers is still in there.
#[test]
fn a_comparison_still_has_every_absolute_timing() {
    let (_report, tokens) = run();
    let cmps = tokens.comparisons["sorting@reversed"].get().unwrap();
    for stats in cmps.stats() {
        assert!(
            stats.ns_per_iter > 0.0,
            "every cell keeps its own measured time, not only a ratio",
        );
    }
}

/// The declared baseline is used. `stable` is not first alphabetically -
/// `thrice` is not either, but `stable` sorts after `sorting`'s other
/// entries only because of the word `baseline`.
#[test]
fn the_declared_baseline_wins_over_alphabetical_order() {
    let (_report, tokens) = run();
    let cmps = tokens.comparisons["sorting@reversed"].get().unwrap();
    let against: Vec<&str> = cmps.against_baseline().map(|(n, _)| n).collect();
    assert_eq!(against.len(), 2);
    assert!(!against.contains(&"stable"), "{against:?}");
    assert!(against.contains(&"unstable"), "{against:?}");
    assert!(against.contains(&"thrice"), "{against:?}");
}

// ---- one generic implementation, registered at two concrete types ----
//
// Monomorphisation is the one place a matrix has to be explicit: Rust has no
// way to instantiate a generic function at "whatever types are around", so
// the types are listed. It is still one annotation rather than one wrapper
// function per type.

#[scaling::candidate(matrix = "generic", types(String, Vec<u8>))]
fn byte_sum<T: AsRef<[u8]>>(x: &mut T) -> u64 {
    x.as_ref().iter().map(|b| u64::from(*b)).sum()
}

#[scaling::candidate(matrix = "generic", types(String, Vec<u8>))]
fn byte_fold<T: AsRef<[u8]>>(x: &mut T) -> u64 {
    x.as_ref()
        .iter()
        .fold(0u64, |a, b| a.wrapping_add(u64::from(*b)))
}

#[scaling::input(matrix = "generic", name = "text")]
fn generic_text() -> String {
    "abcdefghij".repeat(8)
}

#[scaling::input(matrix = "generic", name = "bytes")]
fn generic_bytes() -> Vec<u8> {
    (0..80u8).collect()
}

/// A generic candidate listed at two types becomes two registrations, which
/// land in the two lanes their types belong to.
#[test]
fn a_generic_candidate_is_registered_once_per_listed_type() {
    let (_report, tokens) = run();

    // One comparison per input, and the two inputs are of different types -
    // so each lane found both candidates at its own type.
    for input in ["text", "bytes"] {
        let cmps = tokens.comparisons[&format!("generic@{input}")]
            .get()
            .unwrap_or_else(|| panic!("generic@{input} did not run"));
        assert_eq!(
            cmps.stats().len(),
            2,
            "both candidates should be instantiated for {input}",
        );
    }
    assert!(
        tokens.warnings.is_empty(),
        "listing exactly the types that have inputs should warn about nothing: {:?}",
        tokens
            .warnings
            .iter()
            .map(|w| w.to_string())
            .collect::<Vec<_>>(),
    );
}
