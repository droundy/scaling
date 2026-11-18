//! A benchmark program whose benchmarks are mostly in another crate, which
//! exits with an error if any of them is missing from the run.
//!
//! Run it natively with
//!
//! ```text
//! cargo run --release -p scaling-wasm-runner
//! ```
//!
//! and under node's WASI with `--target wasm32-wasip1` added. It is the check
//! that benchmarks in a dependency survive the wasm linker; see the README.

use std::hint::black_box;
use std::process::ExitCode;
use std::time::Duration;

use scaling::Config;

#[scaling::bench]
fn in_the_runner() -> u64 {
    (0..130u64).fold(0, |a, x| a.wrapping_mul(37).wrapping_add(x))
}

/// Every benchmark this program should find, by the end of its name.
const EXPECTED: [&str; 4] = [
    "in_the_runner",
    "at_the_root",
    "first::in_a_module",
    "second::in_another_module",
];

fn main() -> ExitCode {
    // Without this the linker drops the other crate's benchmarks on wasm,
    // even with the crate in `[dependencies]` and `use ... as _`.
    black_box(&scaling_wasm_registrar::SCALING_ANCHOR);

    // Quick and rough: this checks that benchmarks are found and run, not how
    // fast they are.
    let config = Config::relative(0.2)
        .with_rough_error(0.5)
        .with_max_time(Duration::from_secs(2));
    let report = match config.run() {
        Ok(report) => report,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };

    let found: Vec<&str> = report.names().collect();
    let missing: Vec<&str> = EXPECTED
        .iter()
        .copied()
        .filter(|want| !found.iter().any(|name| name.ends_with(want)))
        .collect();
    if missing.is_empty() {
        println!(
            "found all {} expected benchmarks: {found:?}",
            EXPECTED.len()
        );
        ExitCode::SUCCESS
    } else {
        eprintln!("missing benchmarks {missing:?}; found {found:?}");
        ExitCode::FAILURE
    }
}
