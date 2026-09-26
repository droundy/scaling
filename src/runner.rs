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
//! Anything else - a tighter budget or different output format - is an
//! [`Options`] built by hand and passed to [`run`] or
//! [`measure`] from your own `main`:
//!
//! ```no_run
//! use scaling::runner::{run, Format, Options};
//!
//! fn main() -> std::process::ExitCode {
//!     let options = Options {
//!         format: Format::List,
//!         ..Options::default()
//!     };
//!     run(options).into()
//! }
//! ```
//!
//! See [`Options`] for every field and [`Config`] for the accuracy/budget
//! knobs.
//!
//! # What it prints where
//!
//! Results go to stdout, everything else to stderr: how many benchmarks are
//! about to run, warnings about registrations that went unused, the reason a
//! run measured nothing. So redirecting only stdout captures the results and
//! nothing else, without anybody having to remember to silence the rest.

use crate::assemble::Lane;
use crate::{Assembled, Config, Found, Report, Suite};

/// What a failure to assemble the registered benchmarks comes back as.
///
/// This is the diagnostic used when registration metadata is ambiguous or
/// conflicting; the runner exposes it so a benchmark binary can report the
/// cause in a user-facing way.
pub use crate::assemble::Diagnostic;
use std::collections::{BTreeMap, BTreeSet};
use std::process::ExitCode;
#[cfg(test)]
use std::time::Duration;

/// How to print the results.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Format {
    /// A group with several inputs as a grid, everything else as one line
    /// each.
    ///
    /// The default. A group sharing several inputs reaches the report as one
    /// comparison per input under a flattened `group@input` name - which is
    /// the right thing to measure and the wrong shape to read, so it is
    /// gridded back together here. A group with only one input has no grid
    /// worth drawing and prints as an ordinary comparison instead.
    #[default]
    Table,
    /// Every entry as one line, grids included.
    ///
    /// What the grid gives up is precision: a cell shows a time and a
    /// percentage, where the line form shows the error bar on both and says
    /// outright when a difference was too small to call. Use this when the
    /// question is "is that number real", rather than "which of these wins".
    List,
}

/// Everything the runner needs, which is everything a caller would otherwise
/// have written a `main` to decide.
///
/// Public and plainly built: a crate wanting anything other than table output
/// and the default budget builds one of these by hand and calls [`run`] or
/// [`measure`] from its own `main`, rather than using [`crate::main!`].
///
/// ```no_run
/// use scaling::runner::{run, Options};
/// fn main() -> std::process::ExitCode {
///     run(Options::default()).into()
/// }
/// ```
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// Accuracy and budget, as [`Config`] describes them.
    pub cfg: Config,
    /// How to print what they measured.
    pub format: Format,
}

/// What a run came to.
///
/// A named answer rather than a bare [`ExitCode`], which cannot be compared
/// or read back - so a caller wrapping the runner, and the tests here, can
/// see what happened rather than only pass it on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The registered benchmarks were measured. Exit `0`.
    Measured,
    /// The run never started: a command line that did not parse, or
    /// registrations that contradict each other. Exit `2`.
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
/// wanting anything else builds its own [`Options`] and calls [`run`] or
/// [`measure`] from a hand-written `main` instead - see [`Options`]'s own
/// docs for that.
pub fn main() -> ExitCode {
    run(Options::default()).into()
}

/// Discover everything registered and assemble it into a suite, ready to
/// run.
///
/// Shared by [`run`] and [`measure`] so that the two cannot drift: what
/// `measure` hands back is what `run` would have printed, assembled by the
/// same code under the same options.
fn assemble(options: &Options) -> Result<(Suite, Assembled), Vec<Diagnostic>> {
    let mut suite = options.cfg.suite();
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
/// use scaling::runner::{measure, Options};
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
/// let options = Options::default();
/// let report = measure(&options).expect("the registrations compose");
/// let fast = report.comparison("lookup").expect("it ran");
/// assert_eq!(fast.baseline_name(), "linear_scan");
/// ```
pub fn measure(options: &Options) -> Result<Report, Vec<Diagnostic>> {
    let (suite, _tokens) = assemble(options)?;
    Ok(suite.run())
}

/// Discover, measure and print, under options built by someone else.
pub fn run(options: Options) -> Outcome {
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
    let Options { format, .. } = options;

    for w in &tokens.warnings {
        eprintln!("warning: {w}");
    }

    if suite.is_empty() {
        eprintln!("{}", nothing_to_run(&tokens));
        return Outcome::Measured;
    }
    eprintln!(
        "measuring {} benchmark{}",
        suite.len(),
        if suite.len() == 1 { "" } else { "s" }
    );

    let report = suite.run();

    match format {
        Format::List => println!("{report}"),
        Format::Table => print!("{}", table(&report, &tokens)),
    }

    Outcome::Measured
}

/// Why a run measured nothing, which is nearly always one of two things.
fn nothing_to_run(tokens: &Assembled) -> String {
    let registered = tokens.flat + tokens.scaling + tokens.comparisons;
    if registered == 0 {
        // The failure `inventory` actually produces: it collects through
        // linker sections, so a module that was not compiled into this
        // binary registers nothing and says nothing about it.
        "no benchmarks are registered in this binary.\n\
         Nothing carrying #[scaling::bench] or its siblings was linked in - check that the \
         code holding them is compiled into this target, and that any #[cfg] written above \
         the attribute is enabled here."
            .to_string()
    } else {
        format!("no measurements were produced from the {registered} registered benchmarks")
    }
}

/// Lanes with more than one input as grids, everything else as the report
/// prints it.
///
/// A lane with only one input - the common case, and the only shape a plain
/// `group = "..."` comparison with no declared `#[input]` ever has - is a
/// one-column grid, which says nothing a grid is for. So only a lane with
/// several inputs earns one; a lane with one falls through to the plain
/// per-name rendering below, same as a flat benchmark or a lone comparison
/// always has.
fn table(report: &Report, tokens: &Assembled) -> String {
    if tokens.lanes.is_empty() {
        return format!("{report}\n");
    }
    let mut out = String::new();
    let mut gridded = BTreeSet::new();
    for lane in &tokens.lanes {
        if lane.inputs.len() < 2 {
            continue;
        }
        if let Some(grid) = grid(report, lane, &mut gridded) {
            out.push_str(&grid);
            out.push('\n');
        }
    }
    // Whatever was not part of a grid - flat benchmarks, lone comparisons,
    // and single-input lanes alike - printed the way the report would.
    let rest: Vec<&str> = report.names().filter(|n| !gridded.contains(*n)).collect();
    let width = rest.iter().map(|n| n.len()).max().unwrap_or(0);
    for name in rest {
        let shown = render(report, name);
        if shown.contains('\n') {
            out.push_str(&format!("{name}:\n{}\n", shown.trim_end()));
        } else {
            out.push_str(&format!("{name:<width$}  {shown}\n"));
        }
    }
    out
}

/// One entry, as its own type prints it.
fn render(report: &Report, name: &str) -> String {
    match report.find(name) {
        Some(Found::Stats(s)) => s.to_string(),
        Some(Found::Scaling(s)) => s.to_string(),
        Some(Found::Comparison(c)) => c.to_string(),
        None => "(not measured)".to_string(),
    }
}

/// One cell of a matrix: what it measured, and how that compares.
struct Cell {
    ns: f64,
    /// The difference from this input's baseline, as a percentage, and
    /// whether it cleared the run's threshold. `None` on the baseline row,
    /// and on a lane too small to have one.
    percent: Option<(f64, bool)>,
}

/// One matrix lane as a grid: candidates down the side, inputs across.
///
/// Returns `None` when nothing in the lane was measured. Names it did show
/// are added to `gridded`, so the caller
/// knows not to print them again.
fn grid(report: &Report, lane: &Lane, gridded: &mut BTreeSet<String>) -> Option<String> {
    let mut columns: Vec<String> = Vec::new();
    let mut cells: BTreeMap<(String, String), Cell> = BTreeMap::new();
    let rows: Vec<String> = lane.candidates.iter().map(|c| c.name.clone()).collect();

    for input in &lane.inputs {
        if lane.candidates.len() < 2 {
            let candidate = &lane.candidates[0];
            let entry = lane.flat_name(candidate, input);
            let Some(stats) = report.stats(&entry) else {
                continue;
            };
            gridded.insert(entry);
            columns.push(input.name.clone());
            cells.insert(
                (candidate.name.clone(), input.name.clone()),
                Cell {
                    ns: stats.ns_per_iter,
                    percent: None,
                },
            );
        } else {
            let entry = lane.comparison_name(input);
            let Some(comparisons) = report.comparison(&entry) else {
                continue;
            };
            gridded.insert(entry);
            columns.push(input.name.clone());
            let baseline = comparisons.stats()[0].ns_per_iter;
            cells.insert(
                (comparisons.baseline_name().to_string(), input.name.clone()),
                Cell {
                    ns: baseline,
                    percent: None,
                },
            );
            for (alt, c) in comparisons.against_baseline() {
                cells.insert(
                    (alt.to_string(), input.name.clone()),
                    Cell {
                        ns: c.candidate.ns_per_iter,
                        percent: Some((100.0 * c.difference_ns() / baseline, c.is_changed())),
                    },
                );
            }
        }
    }
    if columns.is_empty() {
        return None;
    }

    let value = |row: &str, column: &str| -> (String, Option<String>) {
        match cells.get(&(row.to_string(), column.to_string())) {
            None => ("-".to_string(), None),
            Some(cell) => (
                short_time(cell.ns),
                cell.percent.map(|(pct, changed)| {
                    if changed {
                        format!("{pct:+.1}%")
                    } else {
                        // Parenthesised rather than hidden: the number is
                        // real, it just did not clear the threshold this run
                        // was judged at, and blanking it would read as "the
                        // same" when it means "not shown to differ".
                        format!("({pct:+.1}%)")
                    }
                }),
            ),
        }
    };

    let label_width = rows.iter().map(|r| r.len()).max().unwrap_or(0) + 2;
    let mut widths: Vec<usize> = columns.iter().map(|c| c.len()).collect();
    for (i, column) in columns.iter().enumerate() {
        for row in &rows {
            let (v, p) = value(row, column);
            widths[i] = widths[i].max(v.len()).max(p.map_or(0, |p| p.len()));
        }
        widths[i] += 2;
    }

    let mut out = format!(
        "{}  ({})  baseline: {}\n",
        lane.group,
        lane.type_name,
        rows.first().map_or("-", |r| r.as_str()),
    );
    out.push_str(&" ".repeat(label_width));
    for (i, column) in columns.iter().enumerate() {
        out.push_str(&format!("{column:>width$}", width = widths[i]));
    }
    out.push('\n');

    let mut any_insignificant = false;
    for row in &rows {
        out.push_str(&format!("  {row:<width$}", width = label_width - 2));
        let mut percents = String::new();
        let mut any_percent = false;
        for (i, column) in columns.iter().enumerate() {
            let (v, p) = value(row, column);
            out.push_str(&format!("{v:>width$}", width = widths[i]));
            match p {
                Some(p) => {
                    any_percent = true;
                    any_insignificant |= p.starts_with('(');
                    percents.push_str(&format!("{p:>width$}", width = widths[i]));
                }
                None => percents.push_str(&" ".repeat(widths[i])),
            }
        }
        out.push('\n');
        if any_percent {
            out.push_str(&" ".repeat(label_width));
            out.push_str(percents.trim_end());
            out.push('\n');
        }
    }
    if any_insignificant {
        out.push_str("  (percentages in brackets did not clear this run's threshold)\n");
    }
    Some(out)
}

/// A time for a grid cell: one number, in the unit that suits it.
///
/// No error bar, deliberately - a grid is for reading across and down, and
/// two figures per cell defeats that. [`Format::List`] is where the `±`
/// lives.
fn short_time(ns: f64) -> String {
    if !ns.is_finite() {
        return "-".to_string();
    }
    let (divisor, unit) = crate::unit_for(ns);
    format!("{:.3}{unit}", ns / divisor)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_grid_cell_shows_one_number_in_a_readable_unit() {
        assert_eq!(short_time(22.0), "22.000ns");
        assert_eq!(short_time(22_000.0), "22.000µs");
        assert_eq!(short_time(f64::NAN), "-");
    }
}

/// The grid, built from a lane assembled by hand.
///
/// By hand because `inventory` collects per binary, and registering a matrix
/// inside the library's own tests would put it in every other test's registry
/// too. Nothing here needs a real registration: laying out a grid reads names
/// and looks results up by them, so a lane made of `static`s and a report
/// from an ordinary suite exercise exactly the same code.
#[cfg(test)]
mod grids {
    use super::*;
    use crate::assemble::{Named, Origin};
    use crate::registry::{noop_alt, Candidate, ErasedInput, Input};
    use std::any::TypeId;

    // Never called: `grid` pairs and prints, it does not measure. They exist
    // because a registration is a struct and its fields have to be filled.
    fn unused_make() -> ErasedInput {
        ErasedInput::new(0u64)
    }

    static STABLE: Candidate = Candidate {
        groups: &["sorting"],
        name: "stable",
        input_type: TypeId::of::<Vec<u64>>,
        input_type_name: "Vec<u64>",
        is_baseline: true,
        crate_name: "scaling",
        crate_version: "0.9.0",
        add_alt: noop_alt,
    };
    static UNSTABLE: Candidate = Candidate {
        name: "unstable",
        is_baseline: false,
        ..STABLE
    };
    static REVERSED: Input = Input {
        groups: &["sorting"],
        name: "reversed",
        crate_name: "scaling",
        crate_version: "0.9.0",
        type_id: TypeId::of::<Vec<u64>>,
        type_name: "Vec<u64>",
        make: unused_make,
    };

    fn origin() -> Origin {
        Origin {
            crate_name: "scaling",
            crate_version: "0.9.0",
        }
    }

    fn lane() -> Lane {
        Lane {
            group: "sorting",
            type_name: "Vec<u64>",
            candidates: vec![
                Named {
                    name: "stable".to_string(),
                    reg: &STABLE,
                    origin: origin(),
                },
                Named {
                    name: "unstable".to_string(),
                    reg: &UNSTABLE,
                    origin: origin(),
                },
            ],
            inputs: vec![Named {
                name: "reversed".to_string(),
                reg: &REVERSED,
                origin: origin(),
            }],
            needs_type_suffix: false,
        }
    }

    /// A report holding one comparison under the name a lane's cell has.
    fn measured() -> Report {
        let cfg = Config::relative(0.05).with_max_time(Duration::from_millis(30));
        let mut suite = cfg.suite();
        suite.add_input_group(
            "sorting@reversed",
            cfg.input_group()
                .add("stable", || (0..64u64).sum::<u64>())
                // Ten times the work, so the percentage in the grid is a
                // number this can actually assert on.
                .add("unstable", || (0..640u64).sum::<u64>()),
        );
        suite.run()
    }

    #[test]
    fn a_lane_prints_as_a_grid() {
        let mut gridded = BTreeSet::new();
        let out = grid(&measured(), &lane(), &mut gridded).expect("the cell was measured");

        assert!(out.contains("sorting"), "{out}");
        assert!(out.contains("(Vec<u64>)"), "the lane's type: {out}");
        assert!(
            out.contains("baseline: stable"),
            "the baseline is named, because nothing else says what the percentages are \
             against: {out}",
        );
        assert!(out.contains("reversed"), "the input is a column: {out}");
        assert!(out.contains("stable"), "{out}");
        assert!(out.contains("unstable"), "{out}");
        assert!(out.contains('%'), "the difference from the baseline: {out}");
        assert!(
            gridded.contains("sorting@reversed"),
            "what the grid showed must not be printed again below it",
        );
    }

    /// An empty grid says less than no grid at all.
    #[test]
    fn a_lane_nothing_measured_prints_nothing() {
        let cfg = Config::default();
        let empty = cfg.suite().run();
        let mut gridded = BTreeSet::new();
        assert!(grid(&empty, &lane(), &mut gridded).is_none());
        assert!(gridded.is_empty());
    }

    /// With no matrices there is no grid to draw, so the table is the report
    /// as the report prints itself - not a reimplementation of it that could
    /// drift.
    #[test]
    fn without_lanes_the_table_is_the_report() {
        let report = measured();
        let tokens = Assembled::default();
        assert_eq!(table(&report, &tokens), format!("{report}\n"));
    }
}
