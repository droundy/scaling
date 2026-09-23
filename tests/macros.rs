//! Benchmarks written the way they are meant to be written.
//!
//! `tests/registry.rs` builds the same registrations by hand, which is what
//! proved the runtime path before any macro existed. This checks that the
//! macros produce the same thing from an attribute, so the two are worth
//! keeping side by side: if one passes and the other fails, the fault is in
//! the macro rather than in the registry.
//!
//! Stage 4 of `REGISTRATION.md`.

#![cfg(feature = "registry")]

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

#[scaling::bench(gen_input = || vec![9i32, 8, 7, 6, 5])]
fn with_gen_input(v: &mut Vec<i32>) {
    v.sort_unstable();
}

// ---- a scaling benchmark ----

#[scaling::bench_scaling(nmin = 32)]
fn scales(n: usize) -> u64 {
    work(n)
}

// ---- a renamed one ----

#[scaling::bench(name = "renamed")]
fn some_long_internal_name() -> u64 {
    work(100)
}

// ---- a comparison group: one shared input, three alternatives ----

#[scaling::gen_input(group = "sorting")]
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

#[test]
fn every_written_benchmark_is_found_and_measured() {
    let cfg = Config::default().with_max_time(Duration::from_millis(50));
    let mut suite = cfg.suite();
    let tokens = suite.add_registered();

    // Four standalone benchmarks and two comparison groups; a group is one
    // entry, since it is measured as a unit.
    assert_eq!(suite.len(), 7, "five flat/scaling entries and two groups");
    let report = suite.run();

    for name in ["plain", "with_input", "with_gen_input", "renamed"] {
        let full = tokens
            .flat
            .keys()
            .find(|k| k.ends_with(name))
            .unwrap_or_else(|| panic!("{name} not registered: {:?}", tokens.flat.keys()));
        assert!(
            tokens.flat[full].get().is_some(),
            "{name} was registered but never measured",
        );
    }

    let scaling_key = tokens
        .scaling
        .keys()
        .find(|k| k.ends_with("scales"))
        .expect("the scaling benchmark registered");
    assert!(tokens.scaling[scaling_key].get().is_some());

    let sorting = tokens.comparisons["sorting"]
        .get()
        .expect("the sorting group ran");
    assert_eq!(sorting.stats().len(), 3);
    assert_eq!(sorting.against_baseline().count(), 2);

    let summing = tokens.comparisons["summing"]
        .get()
        .expect("the no-input group ran");
    assert_eq!(summing.stats().len(), 2);

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
    let cfg = Config::default().with_max_time(Duration::from_millis(20));
    let mut suite = cfg.suite();
    let tokens = suite.add_registered();
    suite.run();

    assert!(
        tokens.flat.contains_key("macros::plain"),
        "expected a module-qualified name, got {:?}",
        tokens.flat.keys().collect::<Vec<_>>(),
    );
    // And `name = "..."` replaces it outright rather than qualifying it.
    assert!(
        tokens.flat.contains_key("renamed"),
        "an explicit name is used as given: {:?}",
        tokens.flat.keys().collect::<Vec<_>>(),
    );
}

/// The baseline is the one that said so.
///
/// `stable` sorts after neither `thrice` nor `unstable` alphabetically, so
/// if it is the baseline that is the `baseline` word doing it.
#[test]
fn the_declared_baseline_is_used() {
    let cfg = Config::default().with_max_time(Duration::from_millis(20));
    let mut suite = cfg.suite();
    let tokens = suite.add_registered();
    suite.run();

    let cmps = tokens.comparisons["sorting"].get().unwrap();
    let against: Vec<&str> = cmps.against_baseline().map(|(n, _)| n).collect();
    assert_eq!(against.len(), 2);
    assert!(
        !against
            .iter()
            .any(|n| n.ends_with("stable") && !n.ends_with("unstable")),
        "{against:?}"
    );
    assert!(
        against.iter().any(|n| n.ends_with("unstable")),
        "{against:?}"
    );
    assert!(against.iter().any(|n| n.ends_with("thrice")), "{against:?}");
}
