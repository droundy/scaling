//! Registrations arriving from more than one crate, or more than one version
//! of one crate.
//!
//! This is what happens when a crate pulls an older copy of itself, or a
//! rival crate, in as a dev-dependency with registrations enabled: both
//! register, and both use the same names for the same ideas, because they
//! *are* the same source a version apart.
//!
//! Registrations are written by hand here rather than by the macros, because
//! the macros necessarily stamp every registration with *this* crate's name
//! and version - there is no second crate in a test binary to register from.
//! Writing them out is the only way to have two origins present at once.
//!
//! Stage 5b of `REGISTRATION.md`.

#![cfg(feature = "registry")]
// These registrations are written by hand, so they do not get the
// `allow(clippy::ptr_arg)` that `#[scaling::candidate]` puts on what it
// emits. The reason for it is the same: a benchmark's argument type is the
// input type the registry keys it on, not a borrow chosen for convenience,
// so taking `&mut [u64]` instead would change what is registered.
#![allow(clippy::ptr_arg)]

use scaling::assemble::{BaselinePolicy, RegistryOptions};
use scaling::registry::{ErasedInput, MatrixCandidate, MatrixInput};
use scaling::{ComparisonSet, Config, Stats, Suite, Token};
use std::any::TypeId;
use std::time::Duration;

fn work(v: &[u64], rounds: usize) -> u64 {
    let mut acc = 0u64;
    for _ in 0..rounds {
        acc = v
            .iter()
            .fold(acc, |a, x| a.wrapping_mul(31).wrapping_add(*x));
    }
    acc
}

// The current crate's implementation, and the same function a version back.
// They differ in speed so the comparison has something to find.
fn sort_new(v: &mut Vec<u64>) -> u64 {
    work(v, 1)
}
fn sort_old(v: &mut Vec<u64>) -> u64 {
    work(v, 3)
}

fn add_flat_new(
    suite: &mut Suite<'_>,
    _: &Config,
    name: &str,
    make: fn() -> ErasedInput,
) -> Token<Stats> {
    suite.add_gen_input(name, make, |e: &mut ErasedInput| {
        sort_new(e.get_mut::<Vec<u64>>())
    })
}
fn add_flat_old(
    suite: &mut Suite<'_>,
    _: &Config,
    name: &str,
    make: fn() -> ErasedInput,
) -> Token<Stats> {
    suite.add_gen_input(name, make, |e: &mut ErasedInput| {
        sort_old(e.get_mut::<Vec<u64>>())
    })
}
fn add_alt_new<'a>(
    set: ComparisonSet<'a, ErasedInput>,
    name: &str,
) -> ComparisonSet<'a, ErasedInput> {
    set.add_input(name, |e: &mut ErasedInput| {
        sort_new(e.get_mut::<Vec<u64>>())
    })
}
fn add_alt_old<'a>(
    set: ComparisonSet<'a, ErasedInput>,
    name: &str,
) -> ComparisonSet<'a, ErasedInput> {
    set.add_input(name, |e: &mut ErasedInput| {
        sort_old(e.get_mut::<Vec<u64>>())
    })
}

fn make_data() -> ErasedInput {
    ErasedInput::new((0..300u64).rev().collect::<Vec<u64>>())
}

// Both versions call the function `sort`, and both call themselves the
// baseline - because they are the same line of source, a version apart.
scaling::inventory::submit! {
    MatrixCandidate {
        matrix: "regress",
        name: "sort",
        input_type: TypeId::of::<Vec<u64>>,
        input_type_name: "Vec<u64>",
        is_baseline: true,
        crate_name: "mycrate",
        crate_version: "0.9.0",
        add_flat: add_flat_new,
        add_alt: add_alt_new,
    }
}

scaling::inventory::submit! {
    MatrixCandidate {
        matrix: "regress",
        name: "sort",
        input_type: TypeId::of::<Vec<u64>>,
        input_type_name: "Vec<u64>",
        is_baseline: true,
        crate_name: "mycrate",
        crate_version: "0.8.0",
        add_flat: add_flat_old,
        add_alt: add_alt_old,
    }
}

// A rival crate, on a *lower* version number than ours.
scaling::inventory::submit! {
    MatrixCandidate {
        matrix: "regress",
        name: "sort",
        input_type: TypeId::of::<Vec<u64>>,
        input_type_name: "Vec<u64>",
        is_baseline: false,
        crate_name: "theircrate",
        crate_version: "0.1.0",
        add_flat: add_flat_new,
        add_alt: add_alt_new,
    }
}

// And both versions of our crate register the same input, which is
// redundant: they are meant to build the same data.
scaling::inventory::submit! {
    MatrixInput {
        matrix: "regress",
        name: "data",
        crate_name: "mycrate",
        crate_version: "0.9.0",
        type_id: TypeId::of::<Vec<u64>>,
        type_name: "Vec<u64>",
        make: make_data,
    }
}

scaling::inventory::submit! {
    MatrixInput {
        matrix: "regress",
        name: "data",
        crate_name: "mycrate",
        crate_version: "0.8.0",
        type_id: TypeId::of::<Vec<u64>>,
        type_name: "Vec<u64>",
        make: make_data,
    }
}

fn run(options: RegistryOptions) -> scaling::RegisteredTokens {
    let cfg = Config::default().with_max_time(Duration::from_millis(30));
    let mut suite = cfg.suite();
    let tokens = suite.add_registered_with(options);
    suite.run();
    tokens
}

/// Two versions of one function, and a rival, all measured against each
/// other - and told apart, rather than colliding as duplicates.
#[test]
fn versions_and_rivals_are_all_measured_and_distinguished() {
    let tokens = run(RegistryOptions::default());
    assert!(
        tokens.warnings.is_empty(),
        "{:?}",
        tokens
            .warnings
            .iter()
            .map(|w| w.to_string())
            .collect::<Vec<_>>(),
    );

    let cmps = tokens.comparisons["regress@data"]
        .get()
        .expect("the comparison ran");
    let names: Vec<&str> = cmps.names().collect();
    assert_eq!(names.len(), 3, "two of ours and one of theirs: {names:?}");
    assert!(names.contains(&"sort@mycrate-0.9.0"), "{names:?}");
    assert!(names.contains(&"sort@mycrate-0.8.0"), "{names:?}");
    assert!(names.contains(&"sort@theircrate-0.1.0"), "{names:?}");
}

/// The redundant input is dropped: one comparison, not one per version of
/// the generator.
#[test]
fn the_redundant_input_is_measured_once() {
    let tokens = run(RegistryOptions::default());
    let matrix_entries: Vec<&String> = tokens
        .comparisons
        .keys()
        .filter(|k| k.starts_with("regress@"))
        .collect();
    assert_eq!(
        matrix_entries.len(),
        1,
        "both versions register `data`, but it is one input: {matrix_entries:?}",
    );
}

/// The older version is the baseline by default, so a regression reads the
/// right way round: the new code is reported *against* the old.
#[test]
fn the_old_version_is_what_the_new_one_is_measured_against() {
    let tokens = run(RegistryOptions::default());
    let cmps = tokens.comparisons["regress@data"].get().unwrap();
    let against: Vec<&str> = cmps.against_baseline().map(|(n, _)| n).collect();
    assert!(
        !against.contains(&"sort@mycrate-0.8.0"),
        "the old version is the baseline, not a candidate: {against:?}",
    );
    assert!(against.contains(&"sort@mycrate-0.9.0"), "{against:?}");
}

/// `Newest` flips which end the comparison is anchored at.
#[test]
fn the_baseline_policy_can_anchor_on_the_newest_instead() {
    let tokens = run(RegistryOptions::default().with_baseline(BaselinePolicy::Newest));
    let cmps = tokens.comparisons["regress@data"].get().unwrap();
    let against: Vec<&str> = cmps.against_baseline().map(|(n, _)| n).collect();
    assert!(!against.contains(&"sort@mycrate-0.9.0"), "{against:?}");
    assert!(against.contains(&"sort@mycrate-0.8.0"), "{against:?}");
}

/// Asking for only the latest of each crate drops our old copy but keeps
/// the rival, whose version number is lower than ours.
#[test]
fn latest_per_crate_keeps_the_rival_and_drops_our_old_copy() {
    let tokens = run(RegistryOptions::latest_per_crate());
    let cmps = tokens.comparisons["regress@data"].get().unwrap();
    let names: Vec<&str> = cmps.names().collect();
    assert_eq!(names.len(), 2, "one per crate: {names:?}");
    assert!(names.contains(&"sort@mycrate"), "{names:?}");
    assert!(
        names.contains(&"sort@theircrate"),
        "a rival on a lower version number must survive: {names:?}",
    );
    // Only the crate tells them apart now, so the version is not in the name.
    assert!(!names.iter().any(|n| n.contains("0.")), "{names:?}");
}
