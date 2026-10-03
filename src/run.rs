//! Running the registered benchmarks: [`Config::run_and_print`] and
//! [`Config::run`].

use crate::assemble::Diagnostic;
use crate::{Config, Report, Suite};
use std::fmt::{self, Display, Formatter};
use std::process::ExitCode;

/// Why [`Config::run`] measured nothing: the registered benchmarks contradict
/// each other.
///
/// Printing it, with `{}` or `{:?}`, lists every contradiction found, one to a
/// line, so `.expect("..")` on a failed run says what to fix. What to fix is
/// in your own `#[scaling::bench]`, `#[scaling::input]` and
/// `#[scaling::metrics]` attributes, which is why this carries only the text.
#[derive(Clone)]
pub struct RegistrationError {
    problems: Vec<String>,
}

impl Display for RegistrationError {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        write!(f, "registered benchmarks do not make sense together:")?;
        for problem in &self.problems {
            write!(f, "\n  - {problem}")?;
        }
        Ok(())
    }
}

/// The same text as [`Display`], so that `unwrap` and `expect` show it as it
/// reads rather than as a list of escaped strings.
impl fmt::Debug for RegistrationError {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        Display::fmt(self, f)
    }
}

impl std::error::Error for RegistrationError {}

impl Config {
    /// Discover every benchmark registered in this binary, measure them
    /// together, print the results, and give back the exit status for `main` to
    /// return.
    ///
    /// It is the whole of a benchmark binary:
    ///
    /// ```no_run
    /// // benches/bench.rs, in its entirety
    /// fn main() -> std::process::ExitCode {
    ///     scaling::Config::default().run_and_print()
    /// }
    /// ```
    ///
    /// and a tighter accuracy target or time budget is the same call on a
    /// [`Config`] built by hand:
    ///
    /// ```no_run
    /// use scaling::Config;
    ///
    /// fn main() -> std::process::ExitCode {
    ///     Config::default()
    ///         .with_max_time(std::time::Duration::from_secs(1))
    ///         .run_and_print()
    /// }
    /// ```
    ///
    /// "This binary" means it literally: everything `#[scaling::bench]` and its
    /// siblings mark, anywhere in your crate's own `src/` - which the compiler
    /// links into every target regardless - is discovered automatically. A
    /// second *file* under `benches/` is not automatically part of it; Cargo
    /// treats each top-level file there as its own separate binary with its own
    /// `main`.
    ///
    /// # What it prints where
    ///
    /// Results go to stdout, everything else to stderr: how many benchmarks
    /// are about to run, warnings about registrations that went unused, the
    /// reason a run measured nothing. So redirecting only stdout captures the
    /// results and nothing else, without anybody having to remember to silence
    /// the rest.
    ///
    /// # Its exit status means something
    ///
    /// Zero, unless the run never started because registrations contradict each
    /// other, which gives `2` and prints what is wrong to stderr. [`Config::run`]
    /// hands the same text back as a [`RegistrationError`].
    pub fn run_and_print(&self) -> ExitCode {
        let suite = match self.assemble() {
            Ok(suite) => suite,
            Err(error) => {
                eprintln!("{error}");
                return ExitCode::from(2);
            }
        };

        if suite.is_empty() {
            eprintln!("There are no benchmarks to run!");
            return ExitCode::SUCCESS;
        }
        eprintln!(
            "measuring {} benchmark{}",
            suite.len(),
            if suite.len() == 1 { "" } else { "s" }
        );

        let report = suite.run();

        print!("{report}");

        ExitCode::SUCCESS
    }

    /// Discover and measure, handing back the results rather than printing
    /// them.
    ///
    /// For a benchmark-driven script rather than a benchmark run: "is the fast
    /// path actually being taken under these conditions?" is a question you
    /// answer by measuring and then *looking at* the numbers, and
    /// [`run_and_print`] prints them and returns a verdict. [`Report`] reaches
    /// them by name - [`Report::timing`], [`Report::comparison`],
    /// [`Report::scaling`] - which is what makes this usable without knowing
    /// in advance what a run will hold.
    ///
    /// # Errors
    ///
    /// [`RegistrationError`] says every way the registrations contradict each
    /// other - two benchmarks of one name, say - and nothing is measured. It is
    /// the text [`run_and_print`] would have printed.
    ///
    /// Registrations that are merely unused, such as an input no candidate takes,
    /// are not errors. A warning for each goes to stderr, as in
    /// [`run_and_print`], and the run goes ahead.
    ///
    /// ```no_run
    /// use scaling::Config;
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
    /// let report = Config::default().run().expect("the registrations compose");
    /// let fast = report.comparison("lookup").expect("it ran");
    /// assert_eq!(fast.baseline_name(), "linear_scan");
    /// ```
    ///
    /// [`run_and_print`]: Config::run_and_print
    pub fn run(&self) -> Result<Report, RegistrationError> {
        Ok(self.assemble()?.run())
    }

    /// Discover everything registered and assemble it into a suite, ready to
    /// run, saying on stderr what went unused.
    ///
    /// Shared by [`Config::run_and_print`] and [`Config::run`] so that the two
    /// cannot drift: what `run` hands back is what `run_and_print` would have
    /// printed, assembled by the same code under the same options, and the two
    /// warn alike.
    fn assemble(&self) -> Result<Suite, RegistrationError> {
        let mut suite = self.suite();
        let assembled = suite.try_add_registered().map_err(|diagnostics| {
            // Warnings come with the errors, so they are said whether or not
            // the run goes ahead.
            let (fatal, warnings): (Vec<Diagnostic>, Vec<Diagnostic>) =
                diagnostics.into_iter().partition(Diagnostic::is_fatal);
            warn(&warnings);
            RegistrationError {
                problems: fatal.iter().map(ToString::to_string).collect(),
            }
        })?;
        warn(&assembled.warnings);
        Ok(suite)
    }
}

/// Say on stderr what went unused.
fn warn(warnings: &[Diagnostic]) {
    for warning in warnings {
        eprintln!("warning: {warning}");
    }
}
