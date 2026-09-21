//! Benchmarks written the way they are meant to be written.
//!
//! `tests/registry.rs`'s equivalent (now `registered_by_hand` inside
//! `src/suite.rs`, since it needs direct `Suite` access that only exists
//! inside the crate) builds the same registrations by hand, which is what
//! proved the runtime path before any macro existed. This checks that the
//! macros produce the same thing from an attribute, so the two are worth
//! keeping side by side: if one passes and the other fails, the fault is in
//! the macro rather than in the registry.
//!
//! Driven through [`scaling::runner::measure`] rather than `Config::suite`:
//! `Suite`, `Token` and `RegisteredTokens` are `pub(crate)`, reachable only
//! from inside the crate, so an ordinary integration test - which links
//! `scaling` the way any downstream crate would - can only ever reach the
//! registry through the runner's public entry point.

use scaling::runner::{measure, Options};
use scaling::Config;
use std::time::Duration;

fn work(n: usize) -> u64 {
    (0..n as u64).fold(0u64, |a, x| a.wrapping_mul(31).wrapping_add(x))
}

// ---- a benchmark that takes nothing ----

#[scaling::bench]
fn plain() -> u64 {
    work(200)
}

// ---- one whose input is cloned per iteration ----

#[scaling::bench(input = vec![5i32, 3, 1, 4, 2])]
fn with_input(v: &mut Vec<i32>) {
    v.sort();
}

// ---- one whose input is built fresh ----

#[scaling::bench(make_input = || vec![9i32, 8, 7, 6, 5])]
fn with_make_input(v: &mut Vec<i32>) {
    v.sort_unstable();
}

// ---- one that only reads its input, taking &T rather than &mut T ----

#[scaling::bench(input = vec![9i32, 8, 7, 6, 5])]
fn with_ref_input(v: &Vec<i32>) -> i32 {
    v.iter().sum()
}

// ---- one that consumes its input, taking T by value ----

#[scaling::bench(input = vec![9i32, 8, 7, 6, 5])]
fn with_owned_input(v: Vec<i32>) -> i32 {
    v.into_iter().sum()
}

#[scaling::bench(make_input = || vec![9i32, 8, 7, 6, 5])]
fn with_owned_make_input(v: Vec<i32>) -> i32 {
    v.into_iter().sum()
}

// ---- one that builds state once and mutates it across every timed call,
// rather than rebuilding it fresh each time - the pattern none of the
// three input shapes above can express ----

#[scaling::bench]
fn with_persistent_state() -> impl FnMut() -> u64 {
    let mut n = 0u64;
    move || {
        n = n.wrapping_add(1);
        n
    }
}

// ---- the same, but with `input` moved into the setup function once ----

#[scaling::bench(input = vec![9i32, 8, 7, 6, 5])]
fn with_input_and_persistent_state(v: Vec<i32>) -> impl FnMut() -> i32 {
    let mut i = 0usize;
    move || {
        i = (i + 1) % v.len();
        v[i]
    }
}

// ---- the same, but the setup function only borrows `input` ----

#[scaling::bench(input = vec![9i32, 8, 7, 6, 5])]
fn with_ref_input_and_persistent_state(v: &mut Vec<i32>) -> impl FnMut() -> i32 {
    v.sort();
    let sorted = v.clone();
    let mut i = 0usize;
    move || {
        i = (i + 1) % sorted.len();
        sorted[i]
    }
}

// ---- a scaling benchmark ----

#[scaling::bench_scaling(nmin = 32)]
fn scales(n: usize) -> u64 {
    work(n)
}

// ---- a scaling benchmark whose input is built fresh, outside the timed
// call - `n` reaches the generator, not the timed function ----

#[scaling::bench_scaling(nmin = 8, make_input = |n: usize| (0..n as u64).collect::<Vec<u64>>())]
fn scales_with_fresh_input(v: &mut Vec<u64>) -> u64 {
    v.iter().fold(0u64, |a, x| a.wrapping_add(*x))
}

// ---- the same, but only reading its generated input. Named to share no
// suffix with any other registration here - see the comment on the
// scales_with_fresh_input rename a few commits back for why that matters ----

#[scaling::bench_scaling(nmin = 8, make_input = |n: usize| (0..n as u64).collect::<Vec<u64>>())]
fn scales_by_reading(v: &Vec<u64>) -> u64 {
    v.iter().fold(0u64, |a, x| a.wrapping_add(*x))
}

// ---- the same, but consuming its generated input by value ----

#[scaling::bench_scaling(nmin = 8, make_input = |n: usize| (0..n as u64).collect::<Vec<u64>>())]
fn scales_by_consuming(v: Vec<u64>) -> u64 {
    v.into_iter().fold(0u64, |a, x| a.wrapping_add(x))
}

// ---- a scaling benchmark whose setup runs once per size rather than once
// per timed call ----

#[scaling::bench_scaling(nmin = 8)]
fn scaling_setup_once(n: usize) -> impl FnMut() -> u64 {
    let base = work(n);
    let mut calls = 0u64;
    move || {
        calls = calls.wrapping_add(1);
        base.wrapping_add(calls)
    }
}

// ---- a renamed one ----

#[scaling::bench(name = "renamed")]
fn some_long_internal_name() -> u64 {
    work(100)
}

// ---- a comparison group: one shared input, three alternatives ----

#[scaling::bench_input(group = "sorting")]
fn sorting_data() -> Vec<u64> {
    (0..500u64).rev().collect()
}

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

// ---- a group with a Design A member: setup runs once, not every call, even
// though the group's shared input is still regenerated and cloned every
// round like any other member's ----

#[scaling::bench_input(group = "counting")]
fn counting_data() -> Vec<u64> {
    (0..300u64).collect()
}

#[scaling::bench(group = "counting", baseline)]
fn sum_it(v: &mut Vec<u64>) -> u64 {
    v.iter().sum()
}

#[scaling::bench(group = "counting")]
fn sum_it_stateful(v: &mut Vec<u64>) -> impl FnMut() -> u64 {
    let total: u64 = v.iter().sum();
    move || total
}

// ---- a group whose alternatives take no input at all ----

#[scaling::bench(group = "summing", baseline)]
fn fold_sum() -> u64 {
    (0..200u64).fold(0, |a, x| a.wrapping_add(x))
}

#[scaling::bench(group = "summing")]
fn iter_sum() -> u64 {
    (0..200u64).sum()
}

// ---------------------------------------------------------------------

fn options(max_time_ms: u64) -> Options {
    Options {
        cfg: Config::default().with_max_time(Duration::from_millis(max_time_ms)),
        ..Options::default()
    }
}

#[test]
fn every_written_benchmark_is_found_and_measured() {
    let report = measure(&options(50)).expect("these registrations compose");

    for name in [
        "plain",
        "with_input",
        "with_make_input",
        "with_ref_input",
        "with_owned_input",
        "with_owned_make_input",
        "with_persistent_state",
        "with_input_and_persistent_state",
        "with_ref_input_and_persistent_state",
    ] {
        let full = report
            .names()
            .find(|k| k.ends_with(name))
            .unwrap_or_else(|| panic!("{name} not registered"))
            .to_string();
        assert!(
            report.stats(&full).is_some(),
            "{name} was registered but never measured",
        );
    }
    assert!(report.stats("renamed").is_some());

    let scaling_key = report
        .names()
        .find(|k| k.ends_with("scales"))
        .expect("the scaling benchmark registered")
        .to_string();
    assert!(report.scaling(&scaling_key).is_some());

    let gen_scaling_key = report
        .names()
        .find(|k| k.ends_with("scales_with_fresh_input"))
        .expect("the make_input scaling benchmark registered")
        .to_string();
    assert!(
        report.scaling(&gen_scaling_key).is_some(),
        "a scaling benchmark with make_input should measure just like one without",
    );

    let ref_scaling_key = report
        .names()
        .find(|k| k.ends_with("scales_by_reading"))
        .expect("the &T scaling benchmark registered")
        .to_string();
    assert!(
        report.scaling(&ref_scaling_key).is_some(),
        "&T should measure exactly as &mut T does",
    );

    let owned_scaling_key = report
        .names()
        .find(|k| k.ends_with("scales_by_consuming"))
        .expect("the owned-input scaling benchmark registered")
        .to_string();
    assert!(
        report.scaling(&owned_scaling_key).is_some(),
        "T by value should measure exactly as &T and &mut T do",
    );

    let persistent_scaling_key = report
        .names()
        .find(|k| k.ends_with("scaling_setup_once"))
        .expect("the setup-once scaling benchmark registered")
        .to_string();
    assert!(
        report.scaling(&persistent_scaling_key).is_some(),
        "a scaling benchmark with a setup-once function should measure like any other",
    );

    let sorting = report.comparison("sorting").expect("the sorting group ran");
    assert_eq!(sorting.stats().len(), 3);
    assert_eq!(sorting.against_baseline().count(), 2);

    let summing = report
        .comparison("summing")
        .expect("the no-input group ran");
    assert_eq!(summing.stats().len(), 2);

    let counting = report
        .comparison("counting")
        .expect("the group with a Design A member ran");
    assert_eq!(counting.stats().len(), 2);
    assert_eq!(counting.against_baseline().count(), 1);

    let shown = format!("{report}");
    assert!(
        !shown.contains("(not measured)"),
        "something was registered but never ran:\n{shown}",
    );
}

/// The default name is module-qualified, so two benchmarks of the same name
/// in different modules do not collide.
#[test]
fn names_default_to_the_module_path() {
    let report = measure(&options(20)).expect("these registrations compose");

    assert!(
        report.names().any(|k| k.ends_with("::plain")),
        "expected a module-qualified name, got {:?}",
        report.names().collect::<Vec<_>>(),
    );
    // And `name = "..."` replaces it outright rather than qualifying it.
    assert!(
        report.contains("renamed"),
        "an explicit name is used as given: {:?}",
        report.names().collect::<Vec<_>>(),
    );
}

/// The baseline is the one that said so.
///
/// `stable` sorts after neither `thrice` nor `unstable` alphabetically, so
/// if it is the baseline that is the `baseline` word doing it.
#[test]
fn the_declared_baseline_is_used() {
    let report = measure(&options(20)).expect("these registrations compose");

    let cmps = report.comparison("sorting").unwrap();
    let against: Vec<&str> = cmps.against_baseline().map(|(n, _)| n).collect();
    assert_eq!(against.len(), 2);
    assert!(
        !against.iter().any(|n| n.ends_with("::stable")),
        "{against:?}"
    );
    assert!(
        against.iter().any(|n| n.ends_with("unstable")),
        "{against:?}"
    );
    assert!(against.iter().any(|n| n.ends_with("thrice")), "{against:?}");
}
