//! Running the registered benchmarks: [`Config::run_and_print`] and
//! [`Config::run`].

use crate::assemble::Diagnostic;
use crate::{Assembled, Config, Report, Suite};
use std::process::ExitCode;

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
    /// other, which gives `2`. [`Config::run`] says which registrations.
    pub fn run_and_print(&self) -> ExitCode {
        let (suite, tokens) = match self.assemble() {
            Ok(assembled) => assembled,
            Err(problems) => {
                eprintln!("registered benchmarks do not make sense together:");
                for p in &problems {
                    eprintln!("  - {p}");
                }
                return ExitCode::from(2);
            }
        };

        for w in &tokens.warnings {
            eprintln!("warning: {w}");
        }

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
    /// `Err` carries every reason the registrations do not compose, the same
    /// list [`run_and_print`] would have printed.
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
    pub fn run(&self) -> Result<Report, Vec<Diagnostic>> {
        let (suite, _tokens) = self.assemble()?;
        Ok(suite.run())
    }

    /// Discover everything registered and assemble it into a suite, ready to
    /// run.
    ///
    /// Shared by [`Config::run_and_print`] and [`Config::run`] so that the two
    /// cannot drift: what `run` hands back is what `run_and_print` would have
    /// printed, assembled by the same code under the same options.
    fn assemble(&self) -> Result<(Suite, Assembled), Vec<Diagnostic>> {
        let mut suite = self.suite();
        let tokens = suite.try_add_registered()?;
        Ok((suite, tokens))
    }
}
