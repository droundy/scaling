//! Registrations that contradict each other: `Config::run` says what is wrong
//! and `Config::run_and_print` exits with `2` instead of measuring anything.
//!
//! Its own binary because the registry is the binary's: these two benchmarks
//! share a name, which would make every run in a file with other tests fail.

use scaling::Config;
use std::process::ExitCode;

#[scaling::bench(name = "same")]
fn first() -> u64 {
    1 + 1
}

#[scaling::bench(name = "same")]
fn second() -> u64 {
    2 + 2
}

#[test]
fn run_says_what_does_not_compose() {
    let said = match Config::default().run() {
        Ok(_) => panic!("two benchmarks called `same` should not compose"),
        Err(error) => error.to_string(),
    };
    let problems = said.lines().filter(|l| l.starts_with("  - ")).count();
    assert_eq!(problems, 1, "{said}");
    assert!(said.contains("same"), "{said}");
}

#[test]
fn run_and_print_exits_with_two_and_measures_nothing() {
    let code = Config::default().run_and_print();
    // `ExitCode` has no `PartialEq` on every Rust this crate supports, so this
    // compares how they print.
    assert_eq!(format!("{code:?}"), format!("{:?}", ExitCode::from(2)));
}
