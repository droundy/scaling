//! Building a filter from the command line.
//!
//! Stage 1 of the filtering plan in `REGISTRATION.md`.

use scaling::Filter;

/// `from_arg_iter` takes what `std::env::args` gives, program name and all.
fn parse(args: &[&str]) -> Filter {
    let argv = std::iter::once("prog").chain(args.iter().copied());
    Filter::from_arg_iter(argv).expect("these parse")
}

#[test]
fn no_arguments_means_measure_everything() {
    let f = parse(&[]);
    assert!(f.matches("anything"));
    assert!(!f.is_listing());
}

#[test]
fn filters_skips_and_flags_are_read() {
    let f = parse(&["--filter", "sort", "--filter", "hash", "--skip", "slow"]);
    assert!(f.matches("sorting"));
    assert!(f.matches("hashing"));
    assert!(!f.matches("fib"));
    assert!(!f.matches("sorting_slow"), "the skip still applies");

    let f = parse(&["--exact", "--filter", "sort"]);
    assert!(f.matches("sort"));
    assert!(!f.matches("sorting"));

    assert!(parse(&["--list"]).is_listing());
}

/// The one that matters, and the one a hand-rolled parser gets wrong.
///
/// `cargo bench` appends `--bench` to a `harness = false` binary whether or
/// not the caller passed anything, and appends it *after* whatever they did
/// pass. Measured:
///
/// ```none
/// cargo bench --bench b            ->  ["…/b", "--bench"]
/// cargo bench --bench b -- --list  ->  ["…/b", "--list", "--bench"]
/// ```
///
/// `auto-args` rejects it - `Err(UnexpectedOption)` - so without dropping it
/// first, the plainest invocation there is would fail having been given
/// nothing by anybody.
#[test]
fn the_flag_cargo_adds_by_itself_is_ignored() {
    let f = parse(&["--bench"]);
    assert!(f.matches("anything"), "plain `cargo bench` must just work");

    // And after the caller's own arguments, where cargo actually puts it.
    let f = parse(&["--filter", "sort", "--bench"]);
    assert!(f.matches("sorting"));
    assert!(!f.matches("hashing"));

    // `cargo test --bench x` passes `--test` in the same spirit.
    assert!(parse(&["--test"]).matches("anything"));
}

#[test]
fn an_unknown_flag_is_an_error_rather_than_being_ignored() {
    let argv = ["prog", "--flitter", "sort"];
    assert!(
        Filter::from_arg_iter(argv).is_err(),
        "a typo in a flag name must not quietly measure everything",
    );
}

/// The environment is what survives a wrapper that was never taught to pass
/// arguments through.
///
/// Run as one test rather than several: the environment is process-wide, and
/// tests share a process.
#[test]
fn the_environment_is_read_too() {
    std::env::set_var("SCALING_FILTER", "sort hash");
    std::env::set_var("SCALING_SKIP", "slow");
    let f = Filter::from_env();
    assert!(f.matches("sorting"));
    assert!(f.matches("hashing"));
    assert!(!f.matches("fib"));
    assert!(!f.matches("sorting_slow"));

    std::env::set_var("SCALING_EXACT", "1");
    assert!(!Filter::from_env().matches("sorting"), "exact now");
    assert!(Filter::from_env().matches("sort"));

    std::env::remove_var("SCALING_FILTER");
    std::env::remove_var("SCALING_SKIP");
    std::env::remove_var("SCALING_EXACT");
    assert!(Filter::from_env().matches("anything"));
}
