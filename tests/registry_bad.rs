//! What happens when registrations do not make sense together.
//!
//! A separate test binary from `registry.rs`, and necessarily so: a registry
//! covers everything linked into one binary, so deliberately broken
//! registrations cannot share a binary with working ones. That separation is
//! itself worth knowing - it is the same reason two `[[bench]]` targets
//! linking one library would each discover that library's registrations.

#![cfg(feature = "registry")]

use scaling::assemble::Diagnostic;
use scaling::registry::{Kind, Registered};
use scaling::{Config, Stats, Suite, Token};

fn add(suite: &mut Suite<'_>, _cfg: &Config, name: &str) -> Token<Stats> {
    suite.add(name, || (0..16u64).sum::<u64>())
}

// Two registrations under one name, which a report could not tell apart.
scaling::inventory::submit! {
    Registered {
        name: "collides",
        crate_name: env!("CARGO_PKG_NAME"),
        crate_version: env!("CARGO_PKG_VERSION"),
        group: None,
        is_baseline: false,
        kind: Kind::Flat(add),
    }
}

scaling::inventory::submit! {
    Registered {
        name: "collides",
        crate_name: env!("CARGO_PKG_NAME"),
        crate_version: env!("CARGO_PKG_VERSION"),
        group: None,
        is_baseline: false,
        kind: Kind::Flat(add),
    }
}

// A comparison group nobody claimed the baseline of. Order cannot decide
// this, since registrations have none.
scaling::inventory::submit! {
    Registered {
        name: "orphan_a",
        crate_name: env!("CARGO_PKG_NAME"),
        crate_version: env!("CARGO_PKG_VERSION"),
        group: Some("no-baseline"),
        is_baseline: false,
        kind: Kind::Alt {
            add: |set, name| set.add_input(name, |_| ()),
            input_type: std::any::TypeId::of::<()>,
            input_type_name: "()",
        },
    }
}

scaling::inventory::submit! {
    Registered {
        name: "orphan_b",
        crate_name: env!("CARGO_PKG_NAME"),
        crate_version: env!("CARGO_PKG_VERSION"),
        group: Some("no-baseline"),
        is_baseline: false,
        kind: Kind::Alt {
            add: |set, name| set.add_input(name, |_| ()),
            input_type: std::any::TypeId::of::<()>,
            input_type_name: "()",
        },
    }
}

/// Both problems are reported together, and nothing is added.
///
/// Reporting every complaint at once is what stops fixing a set of
/// registrations from being one rebuild per mistake, and it is only possible
/// because they are all found before anything runs.
#[test]
fn bad_registrations_are_reported_together_and_nothing_is_added() {
    let cfg = Config::default();
    let mut suite = cfg.suite();
    let problems = suite
        .try_add_registered()
        .expect_err("these registrations contradict each other");

    assert_eq!(problems.len(), 2, "{problems:?}");
    assert!(
        problems
            .iter()
            .any(|p| matches!(p, Diagnostic::DuplicateName { name, .. } if name == "collides")),
        "{problems:?}",
    );
    assert!(
        problems
            .iter()
            .any(|p| matches!(p, Diagnostic::NoBaseline { group, .. } if group == "no-baseline")),
        "{problems:?}",
    );

    // Nothing half-added: the checks all run before the first benchmark is
    // handed to the suite, so a rejected set leaves no trace in it.
    assert!(
        suite.is_empty(),
        "a rejected set of registrations must not leave anything behind",
    );
}

/// The panicking form says everything that is wrong, not just the first
/// thing - someone reading a panic gets the whole list.
#[test]
fn the_panic_lists_every_problem() {
    let cfg = Config::default();
    let mut suite = cfg.suite();
    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        suite.add_registered();
    }))
    .expect_err("add_registered panics on registrations like these");

    let msg = panicked
        .downcast_ref::<String>()
        .expect("panicked with a message");
    assert!(msg.contains("collides"), "{msg}");
    assert!(msg.contains("no-baseline"), "{msg}");
}
