//! Running the registered benchmarks: [`Config::run_and_print`] and
//! [`Config::run`].

use crate::assemble::Diagnostic;
use crate::interrupt;
use crate::progress::Progress;
use crate::{Config, Report, Suite};
use std::fmt::{self, Display, Formatter};

/// Why [`Config::run`] measured nothing: what is registered cannot be run as
/// written. Two benchmarks may share a name, a group may have two baselines, or
/// a metrics function may count allocations in a program that has not installed
/// [`Allocator`](crate::Allocator).
///
/// Printing it, with `{}` or `{:?}`, lists every problem found, one to a line, so `.expect("..")` on a failed run says what to fix, and so does a
/// `main` that returns it, which prints `Error: ` and the list and exits with
/// status 1. What to fix is in your own `#[scaling::bench]`,
/// `#[scaling::input]` and `#[scaling::metrics]` attributes, which is why this
/// carries only the text.
#[derive(Clone)]
pub struct RegistrationError {
    problems: Vec<String>,
}

impl Display for RegistrationError {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        write!(f, "the registered benchmarks cannot be run:")?;
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
    /// together, and print the results.
    ///
    /// It is the whole of a benchmark binary:
    ///
    /// ```no_run
    /// // benches/bench.rs, in its entirety
    /// fn main() -> Result<(), scaling::RegistrationError> {
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
    /// fn main() -> Result<(), scaling::RegistrationError> {
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
    /// are about to run, warnings about registrations that went unused, and,
    /// once a run has gone on for a few seconds, a line now and then saying
    /// how many are done and at most how long the rest can take (less, if
    /// they reach their accuracy goals sooner). So redirecting only stdout
    /// captures the results and nothing else, without anybody having to
    /// remember to silence the rest.
    ///
    /// # Stopping early
    ///
    /// Ctrl-C, or `SIGTERM` on Unix, ends a run with what it has. Every
    /// benchmark still being measured stops where it is, and is printed with
    /// the answer so far, marked `(limit)` if that is less precise than asked
    /// for. Then the process exits with status 130, as it would have without
    /// this, so that `cargo bench && next-step` does not go on after a partial
    /// run. A second Ctrl-C ends it at once. A benchmark call that is already
    /// running finishes first.
    ///
    /// A program that has its own handler for these signals keeps it, and the
    /// run is never stopped early. Because the signal handler of the `ctrlc`
    /// crate can be set only once in a process, the first call of this
    /// function sets it, and a later `ctrlc::set_handler` of the program's own
    /// fails.
    ///
    /// # Errors
    ///
    /// [`RegistrationError`] if the run never started because the registered
    /// benchmarks cannot be run as written, and nothing is measured. It is the
    /// same error [`Config::run`] returns, and it is not printed here: a `main`
    /// that returns it prints it to stderr and exits with a nonzero status, and
    /// a test that calls `.expect("..")` on it fails with it.
    pub fn run_and_print(&self) -> Result<(), RegistrationError> {
        let suite = self.assemble()?;

        if suite.is_empty() {
            eprintln!("There are no benchmarks to run!");
            return Ok(());
        }
        let count = suite.len();
        eprintln!(
            "measuring {count} benchmark{}",
            if count == 1 { "" } else { "s" }
        );

        let _listening = interrupt::Listen::start();
        let mut progress = Progress::new();
        let (report, stopped) = suite.run_with(interrupt::asked, |sweep, names| {
            progress.show(sweep, names);
        });

        print!("{report}");
        if stopped > 0 {
            eprintln!(
                "interrupted: {} of {count} benchmarks had finished, and {stopped} were \
                 stopped early; those not yet as precise as asked for are marked `(limit)`",
                count - stopped
            );
        }
        interrupt::exit_if_asked();

        Ok(())
    }

    /// Discover and measure, handing back the results rather than printing
    /// them.
    ///
    /// For a benchmark-driven script rather than a benchmark run: "is the fast
    /// path actually being taken under these conditions?" is a question you
    /// answer by measuring and then *looking at* the numbers, and
    /// [`run_and_print`] prints them instead. [`Report`] reaches
    /// them by name - [`Report::timing`], [`Report::comparison`],
    /// [`Report::scaling`] - which is what makes this usable without knowing
    /// in advance what a run will hold.
    ///
    /// # Errors
    ///
    /// [`RegistrationError`] says every way the registrations cannot be run as
    /// written - two benchmarks of one name, say - and nothing is measured.
    /// [`run_and_print`] returns the same error.
    ///
    /// Registrations that are merely unused, such as an input no candidate takes,
    /// are not errors. A warning for each goes to stderr, as in
    /// [`run_and_print`], and the run goes ahead.
    ///
    /// Unlike [`run_and_print`], this says nothing about how the run is going
    /// and does not listen for Ctrl-C: that is for a program that owns the
    /// process, and a script that calls this may have its own ideas.
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
