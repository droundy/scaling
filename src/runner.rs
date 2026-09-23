//! Discover every benchmark registered in this binary, measure them, and
//! print the results.
//!
//! This is the half of the registry that is not about registration. Once
//! benchmarks are declared where they belong rather than assembled in one
//! place, nobody is left holding the suite - so choosing the accuracy,
//! choosing which ones to run, and choosing how to print them stop being
//! things a caller writes and start being things this asks for.
//!
//! ```ignore
//! // benches/bench.rs, in its entirety
//! scaling::main!();
//! ```
//!
//! ```none
//! cargo bench --bench bench -- --list
//! cargo bench --bench bench -- --filter sort --format list
//! ```
//!
//! # What it prints where
//!
//! Results go to stdout, everything else to stderr: how many benchmarks are
//! about to run, warnings about registrations that went unused, the reason a
//! run measured nothing. So `--format list > results.txt` gives a file
//! holding results and nothing else, without anybody having to remember to
//! silence the rest.
//!
//! # Flags
//!
//! * `--filter <pattern>` - measure only names containing this. Repeatable;
//!   several are an *or*. See [`Filter`] for what "name" means for a
//!   comparison or a matrix cell, and what `--exact` changes about the
//!   match.
//! * `--skip <pattern>` - drop names containing this from what `--filter`
//!   already matched. Repeatable, and independent of `--filter`: a `--skip`
//!   with no `--filter` narrows the whole suite.
//! * `--exact` - match the whole name rather than any part of it, for both
//!   `--filter` and `--skip`.
//! * `--list` - print what would run, and measure nothing.
//! * `--format <table|list>` - how to print results; see
//!   [`crate::runner::Format`]. Table is the default.
//! * `--rel-error <fraction>` - stop once the standard error is this
//!   fraction of the measurement, e.g. `0.01` for 1%.
//! * `--abs-error <duration>` - stop once the standard error is below this,
//!   e.g. `50ns`.
//! * `--max-time <duration>` - give up on each benchmark after roughly this
//!   long, e.g. `30s`.
//! * `--versions <all|latest>` - measure every registered version of a
//!   colliding benchmark, or only the newest per crate; see
//!   [`crate::runner::VersionPolicy`].
//! * `--baseline <oldest|newest|NAME@VERSION>` - which claimant a version
//!   collision's regression is judged against; see
//!   [`crate::runner::BaselinePolicy`].
//!
//! `--filter`, `--skip`, `--exact` and `--list` also read from the
//! environment - `SCALING_FILTER`, `SCALING_SKIP`, `SCALING_EXACT`,
//! `SCALING_LIST` - which survives a wrapper that does not pass arguments
//! through on its own: `make bench`, a CI step, `cargo bench --workspace`
//! fanning out over several crates. `SCALING_FILTER`/`SCALING_SKIP` hold
//! whitespace-separated patterns; `SCALING_EXACT`/`SCALING_LIST` count if
//! set to anything. The command line wins wherever it says anything, so
//! `SCALING_SKIP=slow cargo bench -- --filter sort` means both. The
//! remaining flags have no environment counterpart - see [`Filter::from_env`]
//! for the full behavior.

use crate::assemble::Lane;
use crate::{Config, Filter, Found, RegisteredTokens, Report, Suite};
// Only for test mocks.
#[cfg(test)]
use crate::Stats;
use auto_args::AutoArgs;

/// The four assembly types a caller actually touches, re-exported here
/// because here is where they are used.
///
/// [`crate::assemble`] is not documented - most of what is in it is the
/// pairing and version-resolution machinery, which nobody outside writes
/// against. These four are different: two are what `--versions` and
/// `--baseline` set, one is what a failure to assemble comes back as, and
/// one is what those two are carried in.
///
/// # What `--versions`/`--baseline` are actually for
///
/// They resolve a collision, not set one up: if two registrations of one
/// name, type and matrix arrive from different crates or versions -
/// something else in the dependency graph happening to register a
/// benchmark under a name yours also uses - `VersionPolicy` says whether
/// to keep every one (the default) or only the newest per crate, and
/// `BaselinePolicy` says which claimant a regression is judged against.
///
/// Deliberately comparing your current code against a past release is a
/// different question, and the recommended way to ask it does not involve
/// either of these: add the old release as a dev-dependency under a
/// renamed package, and write a `#[scaling::bench(group = "...")]`
/// wrapper in `benches/` that calls straight into its public API,
/// alongside one that calls your current code - the same `group`/
/// `baseline` machinery any other comparison uses, sidestepping version
/// resolution entirely.
///
/// A crate can also be set up so an old release's own registrations are
/// picked up automatically, without a wrapper, once it is pulled in the
/// same way - but only if that release already carried the attributes,
/// and only as long as every crate in the graph resolves to the *same*
/// `scaling`: registration is keyed on the literal monomorphized type
/// `inventory` collects, so two `scaling` versions anywhere in the
/// dependency graph split the registry silently rather than erroring -
/// the old registrations compile fine and simply never appear. That risk
/// is why the manual wrapper above is the recommended default rather
/// than this automatic path.
pub use crate::assemble::{BaselinePolicy, Diagnostic, RegistryOptions, VersionPolicy};
use std::collections::{BTreeMap, BTreeSet};
use std::process::ExitCode;
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

impl Format {
    fn parse(s: &str) -> Result<Format, String> {
        match s {
            "table" => Ok(Format::Table),
            "list" => Ok(Format::List),
            other => Err(format!("`{other}` is not a format; try table or list")),
        }
    }
}

/// Everything the runner needs, which is everything a caller would otherwise
/// have written a `main` to decide.
///
/// Public and plainly built so that a crate wanting one thing different -
/// its own default budget, say - can build one of these and call [`run`],
/// rather than being pushed out of the runner entirely.
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// Accuracy and budget, as [`Config`] describes them.
    pub cfg: Config,
    /// Which benchmarks to measure.
    pub filter: Filter,
    /// How to print what they measured.
    pub format: Format,
    /// What to do about registrations from more than one crate or version.
    pub registry: RegistryOptions,
}

/// The flags, as `auto-args` reads them.
///
/// The four filtering flags are flattened in from [`Filter`]'s own set
/// rather than restated, so there is one list of them rather than two to
/// keep in step.
#[derive(AutoArgs, Debug, Default)]
struct Flags {
    _filter: crate::filter::cli::Flags,
    /// How to print results: table (default) or list.
    format: Option<String>,
    /// Stop once the standard error is this fraction of the measurement.
    rel_error: Option<f64>,
    /// Stop once the standard error is below this, eg 50ns.
    abs_error: Option<String>,
    /// Give up on each benchmark after roughly this long, eg 5s.
    max_time: Option<String>,
    /// Measure `all` registered versions, or only the `latest` of each crate.
    versions: Option<String>,
    /// Baseline among several claimants: oldest, newest, or NAME@VERSION.
    baseline: Option<String>,
}

impl Options {
    /// Read the command line, then fill in from the environment what it did
    /// not say.
    ///
    /// The environment half is what survives a wrapper: `make bench`, a CI
    /// step, `cargo bench --workspace` fanning out over several crates -
    /// none of those pass arguments through without being taught to, and
    /// `SCALING_FILTER=sort` needs nobody's cooperation. See
    /// [`Filter::from_env`].
    pub fn from_env_and_args() -> Result<Options, String> {
        let mut options = Options::from_arg_iter(std::env::args())?;
        options.filter = options.filter.or(Filter::from_env());
        Ok(options)
    }

    /// [`Options::from_env_and_args`] from an iterator, and without the
    /// environment.
    ///
    /// The first item is the program name and is discarded, as it is in
    /// `std::env::args`.
    pub fn from_arg_iter<I, S>(args: I) -> Result<Options, String>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let kept = crate::filter::cli::without_cargo_adds(args);
        let flags = Flags::from_iter(kept).map_err(|e| e.to_string())?;

        let mut cfg = Config::default();
        if let Some(f) = flags.rel_error {
            if !(f.is_finite() && f > 0.0) {
                return Err(format!("--rel-error wants a positive fraction, not {f}"));
            }
            cfg.target_rel_error = f;
        }
        if let Some(d) = &flags.abs_error {
            cfg.target_abs_error = parse_duration(d).map_err(|e| format!("--abs-error: {e}"))?;
        }
        if let Some(d) = &flags.max_time {
            cfg.max_time = parse_duration(d).map_err(|e| format!("--max-time: {e}"))?;
        }

        let mut registry = RegistryOptions::default();
        if let Some(v) = &flags.versions {
            registry.versions = match v.as_str() {
                "all" => VersionPolicy::All,
                "latest" => VersionPolicy::LatestPerCrate,
                other => return Err(format!("--versions wants all or latest, not `{other}`")),
            };
        }
        if let Some(b) = &flags.baseline {
            registry.baseline = parse_baseline(b)?;
        }

        Ok(Options {
            cfg,
            filter: flags._filter.to_filter(),
            format: match &flags.format {
                Some(f) => Format::parse(f)?,
                None => Format::default(),
            },
            registry,
        })
    }
}

/// What `--baseline` accepts.
///
/// `Exact` holds `&'static str`, because a registration does; a name read
/// from the command line is leaked to match. That is a bounded leak of two
/// short strings, once, in a process whose whole job is the run that
/// follows - the alternative is threading a lifetime through the assembly
/// types to buy back a few bytes at exit.
fn parse_baseline(s: &str) -> Result<BaselinePolicy, String> {
    match s {
        "oldest" => Ok(BaselinePolicy::Oldest),
        "newest" => Ok(BaselinePolicy::Newest),
        other => match other.split_once('@') {
            Some((name, version)) if !name.is_empty() && !version.is_empty() => {
                Ok(BaselinePolicy::Exact {
                    crate_name: Box::leak(name.to_string().into_boxed_str()),
                    crate_version: Box::leak(version.to_string().into_boxed_str()),
                })
            }
            _ => Err(format!(
                "--baseline wants oldest, newest, or NAME@VERSION, not `{other}`"
            )),
        },
    }
}

/// Parse `5s`, `500ms`, `50ns`, `1.5m`. A bare number is seconds.
fn parse_duration(s: &str) -> Result<Duration, String> {
    let t = s.trim();
    let split = t.find(|c: char| c.is_alphabetic()).unwrap_or(t.len());
    let (number, unit) = t.split_at(split);
    let value: f64 = number
        .trim()
        .parse()
        .map_err(|_| format!("`{s}` is not a duration: `{number}` is not a number"))?;
    let seconds = match unit.trim() {
        "ns" => 1e-9,
        "us" | "µs" => 1e-6,
        "ms" => 1e-3,
        "s" | "" => 1.0,
        "m" => 60.0,
        other => {
            return Err(format!(
                "`{s}` is not a duration: `{other}` is not a unit (try ns, us, ms, s, m)"
            ))
        }
    };
    let seconds = value * seconds;
    // `try_from_secs_f64` rather than `from_secs_f64`: the latter panics on
    // anything it cannot represent, and a mistyped flag is not a reason to
    // abort with a backtrace. A hand-rolled bound here would have to
    // reproduce `Duration`'s own overflow check exactly - `u64::MAX as f64`
    // rounds up to 2^64, one past what `Duration` can actually hold, so a
    // bound written that way would accept a value that still panics.
    Duration::try_from_secs_f64(seconds)
        .map_err(|_| format!("`{s}` is not a duration this can represent"))
}

/// What a run came to.
///
/// A named answer rather than a bare [`ExitCode`], which cannot be compared
/// or read back - so a caller wrapping the runner, and the tests here, can
/// see what happened rather than only pass it on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Everything the filter kept was measured. Exit `0`.
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

/// Every flag that takes the argument after it as a value, so `-h`/`--help`
/// right after one of these is that value - `--filter -h`, say, filtering
/// for a benchmark whose name contains "-h" - not a help request.
const VALUE_FLAGS: &[&str] = &[
    "--filter",
    "--skip",
    "--format",
    "--rel-error",
    "--abs-error",
    "--max-time",
    "--versions",
    "--baseline",
];

/// Whether `args` asks for help, in the sense `main` intercepts: a bare
/// `-h`/`--help` not itself the value of a preceding flag.
fn wants_help(args: &[String]) -> bool {
    args.iter().enumerate().any(|(i, a)| {
        (a == "--help" || a == "-h")
            && !i
                .checked_sub(1)
                .is_some_and(|prev| VALUE_FLAGS.contains(&args[prev].as_str()))
    })
}

/// The whole of a benchmark binary. See [`crate::main!`].
pub fn main() -> ExitCode {
    // Intercepted before parsing: `auto-args` has no idea what `-h` is, and
    // its own `--help` handling belongs to a code path this does not use.
    let args: Vec<String> = std::env::args().collect();
    if wants_help(&args) {
        println!("{}", Flags::help());
        return ExitCode::SUCCESS;
    }
    match Options::from_env_and_args() {
        Ok(options) => run(options).into(),
        Err(e) => {
            eprintln!("error: {e}\n\n{}", Flags::usage());
            Outcome::NotRun.into()
        }
    }
}

/// Discover everything registered and assemble it into a suite, ready to
/// run.
///
/// Shared by [`run`] and [`measure`] so that the two cannot drift: what
/// `measure` hands back is what `run` would have printed, assembled by the
/// same code under the same options.
fn assemble(options: &Options) -> Result<(Suite<'_>, RegisteredTokens), Vec<Diagnostic>> {
    let mut suite = options.cfg.suite().with_filter(options.filter.clone());
    let tokens = suite.try_add_registered_with(options.registry)?;
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
/// The filter applies, so a script can measure the one comparison it cares
/// about. `Err` carries every reason the registrations do not compose, the
/// same list [`run`] would have printed.
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
/// let mut options = Options::default();
/// options.filter = scaling::Filter::everything().matching("lookup");
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

    // Before `is_listing`: an empty suite has nothing to list either, and
    // `--list` on one - nothing registered at all, or a filter that matched
    // nothing - should say so rather than print zero bytes and exit clean.
    if suite.is_empty() {
        eprintln!("{}", nothing_to_run(&tokens));
        return Outcome::Measured;
    }

    if suite.filter().is_listing() {
        print!("{}", listing(&suite, &tokens));
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

/// What would run, and for a comparison, what it holds.
///
/// The alternatives matter here more than anywhere else: a comparison
/// reaches the report under one name, and a listing that stopped at that
/// name would answer "is my benchmark linked in" with a maybe.
fn listing(suite: &Suite<'_>, tokens: &RegisteredTokens) -> String {
    let alternatives = alternatives(tokens);
    let mut out = String::new();
    for name in suite.names() {
        out.push_str(name);
        out.push('\n');
        for (i, alt) in alternatives.get(name).into_iter().flatten().enumerate() {
            let mark = if i == 0 { "  (baseline)" } else { "" };
            out.push_str(&format!("    {alt}{mark}\n"));
        }
    }
    out
}

/// Every comparison's alternatives, baseline first, by the name the
/// comparison itself is reported under.
fn alternatives(tokens: &RegisteredTokens) -> BTreeMap<String, Vec<String>> {
    let mut out = BTreeMap::new();
    for lane in &tokens.lanes {
        // A lane with one candidate has nothing to compare against, so its
        // cells are plain benchmarks and their names say what they are.
        if lane.candidates.len() < 2 {
            continue;
        }
        let names: Vec<String> = lane.candidates.iter().map(|c| c.name.clone()).collect();
        for input in &lane.inputs {
            out.insert(lane.comparison_name(input), names.clone());
        }
    }
    out
}

/// Why a run measured nothing, which is nearly always one of two things.
fn nothing_to_run(tokens: &RegisteredTokens) -> String {
    let registered = tokens.flat.len() + tokens.scaling.len() + tokens.comparisons.len();
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
        format!(
            "the filter matched none of the {registered} registered benchmarks; \
             --list shows what there is"
        )
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
fn table(report: &Report, tokens: &RegisteredTokens) -> String {
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
/// Returns `None` when nothing in the lane was measured - a filtered run is
/// entitled to leave a whole matrix out, and an empty grid says less than no
/// grid at all. Names it did show are added to `gridded`, so the caller
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
    fn durations_parse_in_the_units_people_write() {
        assert_eq!(parse_duration("5s").unwrap(), Duration::from_secs(5));
        assert_eq!(parse_duration("500ms").unwrap(), Duration::from_millis(500));
        assert_eq!(parse_duration("50ns").unwrap(), Duration::from_nanos(50));
        assert_eq!(parse_duration("10us").unwrap(), Duration::from_micros(10));
        assert_eq!(parse_duration("10µs").unwrap(), Duration::from_micros(10));
        assert_eq!(parse_duration("2m").unwrap(), Duration::from_secs(120));
        assert_eq!(parse_duration("1.5s").unwrap(), Duration::from_millis(1500));
        assert_eq!(parse_duration(" 3 ").unwrap(), Duration::from_secs(3));
    }

    /// A mistyped flag must come back as a message, not a panic:
    /// `Duration::from_secs_f64` aborts on anything it cannot represent.
    #[test]
    fn a_duration_that_is_not_one_is_an_error() {
        for bad in ["", "fast", "5 fortnights", "-1s", "1e400s", "nans"] {
            assert!(
                parse_duration(bad).is_err(),
                "`{bad}` should not parse as a duration",
            );
        }
    }

    #[test]
    fn the_flags_reach_the_options_they_name() {
        let options = Options::from_arg_iter([
            "bench",
            "--filter",
            "sort",
            "--skip",
            "slow",
            "--exact",
            "--format",
            "list",
            "--rel-error",
            "0.005",
            "--max-time",
            "250ms",
            "--versions",
            "latest",
            "--baseline",
            "newest",
        ])
        .unwrap();
        assert!(options.filter.matches("sort"));
        assert!(!options.filter.matches("mymod::sort"), "--exact");
        assert_eq!(options.format, Format::List);
        assert_eq!(options.cfg.target_rel_error, 0.005);
        assert_eq!(options.cfg.max_time, Duration::from_millis(250));
        assert_eq!(options.registry.versions, VersionPolicy::LatestPerCrate);
        assert_eq!(options.registry.baseline, BaselinePolicy::Newest);
    }

    fn s(args: &[&str]) -> Vec<String> {
        args.iter().map(|a| a.to_string()).collect()
    }

    #[test]
    fn a_bare_help_flag_is_a_help_request() {
        assert!(wants_help(&s(&["bench", "-h"])));
        assert!(wants_help(&s(&["bench", "--help"])));
        assert!(wants_help(&s(&["bench", "--filter", "sort", "--help"])));
    }

    /// The bug this guards: `--filter -h` means "filter for a benchmark
    /// whose name contains -h", not "show help". Scanning every argument
    /// for a literal `-h` cannot tell these apart; only knowing which flags
    /// consume the next argument as their value can.
    #[test]
    fn a_help_flag_used_as_a_value_is_not_a_help_request() {
        assert!(!wants_help(&s(&["bench", "--filter", "-h"])));
        assert!(!wants_help(&s(&["bench", "--skip", "--help"])));
        assert!(!wants_help(&s(&["bench", "--baseline", "-h"])));
    }

    #[test]
    fn no_help_flag_is_not_a_help_request() {
        assert!(!wants_help(&s(&["bench", "--filter", "sort"])));
        assert!(!wants_help(&s(&["bench"])));
    }

    /// The plainest invocation there is: `cargo bench` appends `--bench` on
    /// its own account, having been given nothing by anybody.
    #[test]
    fn what_cargo_appends_is_not_an_error() {
        let options = Options::from_arg_iter(["bench", "--bench"]).unwrap();
        assert_eq!(options.format, Format::Table);
        assert!(options.filter.matches("anything"));
    }

    #[test]
    fn an_unknown_format_says_what_the_formats_are() {
        let e = Options::from_arg_iter(["bench", "--format", "yaml"]).unwrap_err();
        assert!(e.contains("table"), "{e}");
        assert!(e.contains("list"), "{e}");
    }

    #[test]
    fn a_baseline_can_name_a_crate_and_version() {
        let policy = parse_baseline("scaling@0.8.1").unwrap();
        assert_eq!(
            policy,
            BaselinePolicy::Exact {
                crate_name: "scaling",
                crate_version: "0.8.1",
            }
        );
        assert!(parse_baseline("scaling@").is_err());
        assert!(parse_baseline("whenever").is_err());
    }

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
    use crate::registry::{noop_alt, Adder, Candidate, ErasedInput, Handle, Input, MakeInput};
    use std::any::TypeId;

    // Never called: `grid` pairs and prints, it does not measure. They exist
    // because a registration is a struct and its fields have to be filled.
    fn unused_flat(_: &mut Adder<'_, '_>, _: &str, _: MakeInput) -> Handle<Stats> {
        unreachable!("a grid does not measure")
    }
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
        add_flat: unused_flat,
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
        let _ = suite.add_comparison(
            "sorting@reversed",
            cfg.comparison()
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

    /// A filtered run is entitled to leave a whole matrix out, and an empty
    /// grid says less than no grid at all.
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
        let tokens = RegisteredTokens::default();
        assert_eq!(table(&report, &tokens), format!("{report}\n"));
    }
}
