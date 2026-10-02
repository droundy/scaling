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

use crate::assemble::Lane;
use crate::{Assembled, Config, Report, Suite};

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

    print!("{}", stdout(&report, &tokens));

    Outcome::Measured
}

/// Everything [`run`] prints to stdout once the benchmarks are measured.
///
/// The table alone. It already prints every result the report holds, each
/// one once, so printing the report above it as well showed every
/// comparison twice.
fn stdout(report: &Report, tokens: &Assembled) -> String {
    table(report, tokens)
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
    report
        .find(name)
        .expect("name came from report.names()")
        .to_string()
}

/// One cell of a matrix: what it measured, and how that compares.
struct Cell {
    ns: f64,
    /// [`caveat`]'s mark for this measurement, so a grid does not drop the
    /// `(limit)` and `(untrusted)` the list form would have shown.
    caveat: &'static str,
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
                    caveat: caveat(&stats),
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
            let baseline = comparisons.stats()[0];
            cells.insert(
                (comparisons.baseline_name().to_string(), input.name.clone()),
                Cell {
                    ns: baseline.ns_per_iter,
                    caveat: caveat(&baseline),
                    percent: None,
                },
            );
            for (alt, c) in comparisons.against_baseline() {
                cells.insert(
                    (alt.to_string(), input.name.clone()),
                    Cell {
                        ns: c.ns_per_iter,
                        caveat: caveat(&c),
                        percent: Some((
                            c.difference()
                                .expect("candidate has a difference")
                                .percent(),
                            c.is_changed(),
                        )),
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
                format!("{}{}", short_time(cell.ns), cell.caveat),
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
    let (mut any_limit, mut any_untrusted) = (false, false);
    for cell in cells.values() {
        any_limit |= cell.caveat.contains('*');
        any_untrusted |= cell.caveat.contains('?');
    }
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
    if any_limit {
        out.push_str("  * (limit): the time budget ran out before the target precision\n");
    }
    if any_untrusted {
        out.push_str("  ? (untrusted): too few samples for the error bar to mean anything\n");
    }
    Some(out)
}

/// The grid's short form of what [`Timing`](crate::Timing)'s `Display` spells
/// out as `(limit)` and `(untrusted)`, appended to a cell's time: `*` and `?`
/// respectively, explained in a note under the grid.
fn caveat(t: &crate::Timing) -> &'static str {
    match (t.hit_limit, t.untrustworthy) {
        (true, true) => "*?",
        (true, false) => "*",
        (false, true) => "?",
        (false, false) => "",
    }
}

/// A time for a grid cell: one number, in the unit that suits it.
///
/// No error bar, deliberately - a grid is for reading across and down, and
/// two figures per cell defeats that. The `±` is in the [`Report`], which
/// [`measure`] hands back.
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
    static SORTED: Input = Input {
        name: "sorted",
        ..REVERSED
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

    /// What `run` prints shows each result once: a comparison drawn as a
    /// grid, and one that is not, are not printed again as a list.
    #[test]
    fn run_prints_each_comparison_once() {
        let mut lane = lane();
        lane.inputs.push(Named {
            name: "sorted".to_string(),
            reg: &SORTED,
            origin: origin(),
        });
        let tokens = Assembled {
            lanes: vec![lane],
            ..Assembled::default()
        };
        let cfg = Config::relative(0.05).with_max_time(Duration::from_millis(30));
        let mut suite = cfg.suite();
        for input in ["reversed", "sorted"] {
            suite.add_input_group(
                &format!("sorting@{input}"),
                cfg.input_group()
                    .add("stable", || (0..64u64).sum::<u64>())
                    .add("unstable", || (0..640u64).sum::<u64>()),
            );
        }
        suite.add_input_group(
            "summing",
            cfg.input_group()
                .add("by_loop", || (0..64u64).sum::<u64>())
                .add("by_fold", || (0..640u64).sum::<u64>()),
        );
        let out = stdout(&suite.run(), &tokens);

        assert_eq!(out.matches("unstable").count(), 1, "one grid row: {out}");
        assert!(!out.contains("sorting@"), "no list of the gridded: {out}");
        assert_eq!(out.matches("by_fold").count(), 1, "{out}");
    }

    /// A grid cell keeps the `(limit)` and `(untrusted)` the list form shows.
    #[test]
    fn a_grid_cell_marks_a_doubtful_measurement() {
        let timing = |hit_limit, untrustworthy| crate::Timing {
            ns_per_iter: 1.0,
            std_error: 0.0,
            iterations: 1,
            samples: 1,
            hit_limit,
            untrustworthy,
            difference: None,
        };
        assert_eq!(caveat(&timing(false, false)), "");
        assert_eq!(caveat(&timing(true, false)), "*");
        assert_eq!(caveat(&timing(false, true)), "?");
        assert_eq!(caveat(&timing(true, true)), "*?");
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
