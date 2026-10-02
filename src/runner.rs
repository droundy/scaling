//! Discover every benchmark registered in this binary, measure them, and
//! print the results.
//!
//! [`crate::main!`] runs every registered benchmark with the default
//! [`Config`] and prints a table:
//!
//! ```no_run
//! // benches/bench.rs, in its entirety
//! scaling::main!();
//! ```
//!
//! Anything else - a tighter budget or different settings - is a
//! [`Config`] built by hand and passed to [`run`] or [`measure`] from your
//! own `main`:
//!
//! ```no_run
//! use scaling::{runner, Config};
//!
//! fn main() -> std::process::ExitCode {
//!     let config = Config::default().with_max_time(std::time::Duration::from_secs(1));
//!     runner::run(config).into()
//! }
//! ```
//!
//! See [`Config`] for the accuracy and time-budget knobs.
//!
//! # What it prints where
//!
//! Results go to stdout, everything else to stderr: how many benchmarks are
//! about to run, warnings about registrations that went unused, the reason a
//! run measured nothing. So redirecting only stdout captures the results and
//! nothing else, without anybody having to remember to silence the rest.

use crate::{Assembled, Config, Report, Suite};

/// What a failure to assemble the registered benchmarks comes back as.
///
/// This is the diagnostic used when registration metadata is ambiguous or
/// conflicting; the runner exposes it so a benchmark binary can report the
/// cause in a user-facing way.
pub use crate::assemble::Diagnostic;
use std::process::ExitCode;

/// What a run came to.
///
/// A named answer rather than a bare [`ExitCode`], which cannot be compared
/// or read back - so a caller wrapping the runner, and the tests here, can
/// see what happened rather than only pass it on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The registered benchmarks were measured. Exit `0`.
    Measured,
    /// The run never started because registrations contradict each other.
    /// Exit `2`.
    NotRun,
}

impl From<Outcome> for ExitCode {
    fn from(outcome: Outcome) -> ExitCode {
        match outcome {
            Outcome::Measured => ExitCode::SUCCESS,
            Outcome::NotRun => ExitCode::from(2),
        }
    }
}

/// The whole of a benchmark binary. See [`crate::main!`].
///
/// Every registered benchmark, table output, the default budget. A crate
/// wanting anything else builds its own [`Config`] and calls [`run`] or
/// [`measure`] from a hand-written `main` instead.
pub fn main() -> ExitCode {
    run(Config::default()).into()
}

/// Discover everything registered and assemble it into a suite, ready to
/// run.
///
/// Shared by [`run`] and [`measure`] so that the two cannot drift: what
/// `measure` hands back is what `run` would have printed, assembled by the
/// same code under the same options.
fn assemble(config: &Config) -> Result<(Suite, Assembled), Vec<Diagnostic>> {
    let mut suite = config.suite();
    let tokens = suite.try_add_registered()?;
    Ok((suite, tokens))
}

/// Discover and measure, handing back the results rather than printing them.
///
/// For a benchmark-driven script rather than a benchmark run: "is the fast
/// path actually being taken under these conditions?" is a question you
/// answer by measuring and then *looking at* the numbers, and [`run`] prints
/// them and returns a verdict. [`Report`] reaches them by name -
/// [`Report::stats`], [`Report::comparison`], [`Report::scaling`] - which is
/// what makes this usable without knowing in advance what a run will hold.
///
/// `Err` carries every reason the registrations do not compose, the same list
/// [`run`] would have printed.
///
/// ```no_run
/// use scaling::{runner, Config};
///
/// #[scaling::bench(group = "lookup", name = "linear_scan", baseline)]
/// fn scan() -> bool {
///     (0..1000u64).any(|x| x == 42)
/// }
///
/// #[scaling::bench(group = "lookup", name = "binary_search")]
/// fn bsearch() -> bool {
///     (0..1000u64).collect::<Vec<_>>().binary_search(&42).is_ok()
/// }
///
/// let report = runner::measure(&Config::default()).expect("the registrations compose");
/// let fast = report.comparison("lookup").expect("it ran");
/// assert_eq!(fast.baseline_name(), "linear_scan");
/// ```
pub fn measure(options: &Config) -> Result<Report, Vec<Diagnostic>> {
    let (suite, _tokens) = assemble(options)?;
    Ok(suite.run())
}

/// Discover, measure and print, under options built by someone else.
pub fn run(options: Config) -> Outcome {
    let (suite, tokens) = match assemble(&options) {
        Ok(assembled) => assembled,
        Err(problems) => {
            eprintln!("registered benchmarks do not make sense together:");
            for p in &problems {
                eprintln!("  - {p}");
            }
            return Outcome::NotRun;
        }
    };

    for w in &tokens.warnings {
        eprintln!("warning: {w}");
    }

    if suite.is_empty() {
        eprintln!("There are no benchmarks to run!");
        return Outcome::Measured;
    }
    eprintln!(
        "measuring {} benchmark{}",
        suite.len(),
        if suite.len() == 1 { "" } else { "s" }
    );

    let report = suite.run();

    print!("{}", crate::formatting::table(&report));

    Outcome::Measured
}
