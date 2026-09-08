//! Discovering benchmarks that were never assembled by hand.
//!
//! An integration test rather than a unit one, deliberately: it links the
//! crate the way a user's benchmark binary does, and the thing being checked
//! is that registrations written in one place are found and measured in
//! another, with nothing listing them in between.
//!
//! Stage 3 of `REGISTRATION.md`, which is where the registry first does
//! something useful end to end.
//!
//! Registrations go through `scaling::inventory`, the re-export, rather than
//! naming `inventory` directly - which is what a crate using this will do,
//! since it depends on `scaling` and has no reason to depend on `inventory`
//! as well. Generated registration code will name the same path.

#![cfg(feature = "registry")]

use scaling::registry::{ErasedInput, GenInputRegistration, Kind, Registered};
use scaling::{ComparisonSet, Config, ScalingStats, Stats, Suite, Token};
use std::any::TypeId;
use std::time::Duration;

fn work(n: usize) -> u64 {
    (0..n as u64).fold(0u64, |a, x| a.wrapping_mul(31).wrapping_add(x))
}

// ---------------------------------------------------------------------
// A flat benchmark, registered in one place.
// ---------------------------------------------------------------------

fn add_flat(suite: &mut Suite<'_>, _cfg: &Config, name: &str) -> Token<Stats> {
    suite.add(name, || work(200))
}

scaling::inventory::submit! {
    Registered {
        name: "e2e::flat",
        crate_name: env!("CARGO_PKG_NAME"),
        crate_version: env!("CARGO_PKG_VERSION"),
        group: None,
        is_baseline: false,
        kind: Kind::Flat(add_flat),
    }
}

// ---------------------------------------------------------------------
// A scaling benchmark, registered somewhere else entirely.
// ---------------------------------------------------------------------

fn add_scaling(suite: &mut Suite<'_>, _cfg: &Config, name: &str) -> Token<ScalingStats> {
    // `nmin` is baked in here, since the shim signature has nowhere to pass
    // it - which is the whole reason it must be a literal at the macro.
    suite.add_scaling(name, |n: usize| work(n), 32)
}

scaling::inventory::submit! {
    Registered {
        name: "e2e::scaling",
        crate_name: env!("CARGO_PKG_NAME"),
        crate_version: env!("CARGO_PKG_VERSION"),
        group: None,
        is_baseline: false,
        kind: Kind::Scaling(add_scaling),
    }
}

// ---------------------------------------------------------------------
// A comparison group: three alternatives and one shared input, each
// registered independently and none of them naming the others.
// ---------------------------------------------------------------------

fn make_input() -> ErasedInput {
    ErasedInput::new((0..600u64).collect::<Vec<u64>>())
}

scaling::inventory::submit! {
    GenInputRegistration {
        group: "e2e-sort",
        type_id: TypeId::of::<Vec<u64>>,
        type_name: "Vec<u64>",
        make: make_input,
    }
}

fn alt_baseline<'a>(
    set: ComparisonSet<'a, ErasedInput>,
    name: &str,
) -> ComparisonSet<'a, ErasedInput> {
    set.add_input(name, |e: &mut ErasedInput| {
        let v = e.get_mut::<Vec<u64>>();
        v.sort();
        v.len()
    })
}

fn alt_unstable<'a>(
    set: ComparisonSet<'a, ErasedInput>,
    name: &str,
) -> ComparisonSet<'a, ErasedInput> {
    set.add_input(name, |e: &mut ErasedInput| {
        let v = e.get_mut::<Vec<u64>>();
        v.sort_unstable();
        v.len()
    })
}

/// Deliberately slower, so the comparison has something real to find.
fn alt_slow<'a>(set: ComparisonSet<'a, ErasedInput>, name: &str) -> ComparisonSet<'a, ErasedInput> {
    set.add_input(name, |e: &mut ErasedInput| {
        let v = e.get_mut::<Vec<u64>>();
        v.sort();
        v.sort_unstable();
        v.sort();
        v.len()
    })
}

scaling::inventory::submit! {
    Registered {
        name: "e2e::sort_stable",
        crate_name: env!("CARGO_PKG_NAME"),
        crate_version: env!("CARGO_PKG_VERSION"),
        group: Some("e2e-sort"),
        is_baseline: true,
        kind: Kind::Alt {
            add: alt_baseline,
            input_type: TypeId::of::<Vec<u64>>,
            input_type_name: "Vec<u64>",
        },
    }
}

scaling::inventory::submit! {
    Registered {
        name: "e2e::sort_unstable",
        crate_name: env!("CARGO_PKG_NAME"),
        crate_version: env!("CARGO_PKG_VERSION"),
        group: Some("e2e-sort"),
        is_baseline: false,
        kind: Kind::Alt {
            add: alt_unstable,
            input_type: TypeId::of::<Vec<u64>>,
            input_type_name: "Vec<u64>",
        },
    }
}

scaling::inventory::submit! {
    Registered {
        name: "e2e::sort_thrice",
        crate_name: env!("CARGO_PKG_NAME"),
        crate_version: env!("CARGO_PKG_VERSION"),
        group: Some("e2e-sort"),
        is_baseline: false,
        kind: Kind::Alt {
            add: alt_slow,
            input_type: TypeId::of::<Vec<u64>>,
            input_type_name: "Vec<u64>",
        },
    }
}

// ---------------------------------------------------------------------

/// Everything above is found and measured, without one line listing it.
#[test]
fn a_suite_discovers_what_was_registered() {
    let cfg = Config::default().with_max_time(Duration::from_millis(60));
    let mut suite = cfg.suite();
    let tokens = suite.add_registered();

    // Three entries: two standalone benchmarks and one comparison group -
    // the group counts once, since it is measured as a unit.
    assert_eq!(suite.len(), 3, "two flat entries and one comparison");

    let report = suite.run();

    let flat = tokens.flat["e2e::flat"]
        .get()
        .expect("the flat benchmark ran");
    assert!(flat.ns_per_iter > 0.0);

    let scaling = tokens.scaling["e2e::scaling"]
        .get()
        .expect("the scaling benchmark ran");
    assert!(scaling.iterations > 0);

    let cmps = tokens.comparisons["e2e-sort"]
        .get()
        .expect("the comparison ran");
    // Three alternatives, two of them reported against the baseline.
    assert_eq!(cmps.stats().len(), 3);
    assert_eq!(cmps.against_baseline().count(), 2);

    // Everything appears in the report, under the name it registered with.
    let shown = format!("{report}");
    for name in ["e2e::flat", "e2e::scaling", "e2e-sort"] {
        assert!(shown.contains(name), "{name} missing from report:\n{shown}");
    }
    assert!(
        !shown.contains("(not measured)"),
        "something was added but never ran:\n{shown}",
    );
}

/// The baseline is the one that said so, not the one that happened to be
/// registered or sorted first.
///
/// `sort_stable` is neither: `sort_thrice` and `sort_unstable` both sort
/// before it alphabetically. So if this passes, the `is_baseline` flag is
/// what decided, which is the only thing that can decide when registrations
/// have no order.
#[test]
fn the_declared_baseline_is_the_one_used() {
    let cfg = Config::default().with_max_time(Duration::from_millis(60));
    let mut suite = cfg.suite();
    let tokens = suite.add_registered();
    suite.run();

    let cmps = tokens.comparisons["e2e-sort"].get().unwrap();
    let against: Vec<&str> = cmps.against_baseline().map(|(name, _)| name).collect();
    assert!(
        !against.contains(&"e2e::sort_stable"),
        "the baseline must not be reported against itself: {against:?}",
    );
    assert_eq!(against.len(), 2);
    assert!(against.contains(&"e2e::sort_unstable"), "{against:?}");
    assert!(against.contains(&"e2e::sort_thrice"), "{against:?}");
}

/// Discovered benchmarks and hand-added ones share one suite and are
/// measured together, which is the whole claim of the hybrid design.
#[test]
fn registered_and_hand_added_benchmarks_mix() {
    let cfg = Config::default().with_max_time(Duration::from_millis(60));
    let mut suite = cfg.suite();
    let by_hand = suite.add("by_hand", || work(150));
    let tokens = suite.add_registered();
    let after = suite.add("after", || work(150));

    assert_eq!(suite.len(), 5, "two by hand plus three discovered");
    let report = suite.run();

    assert!(by_hand.get().is_some(), "the hand-added one ran");
    assert!(after.get().is_some(), "so did the one added afterwards");
    assert!(
        tokens.flat["e2e::flat"].get().is_some(),
        "so did the registered one"
    );

    let shown = format!("{report}");
    assert!(shown.contains("by_hand"), "{shown}");
    assert!(shown.contains("e2e::flat"), "{shown}");
}
