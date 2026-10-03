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
    let problems = match Config::default().run() {
        Ok(_) => panic!("two benchmarks called `same` should not compose"),
        Err(problems) => problems,
    };
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].to_string().contains("same"), "{problems:?}");
}

#[test]
fn run_and_print_exits_with_two_and_measures_nothing() {
    let code = Config::default().run_and_print();
    // `ExitCode` has no `PartialEq` on every Rust this crate supports, so this
    // compares how they print.
    assert_eq!(format!("{code:?}"), format!("{:?}", ExitCode::from(2)));
}
