//! Running only some of a suite's benchmarks.
//!
//! A binary that registers every benchmark in a crate measures every
//! benchmark in the crate, and [`Suite`] gives each entry its own
//! [`Config::max_time`], so the cost of a run grows with how many there are.
//! That is the wrong shape for working on one function.
//!
//! The matching lives here and needs no dependency. Building a [`Filter`]
//! from the command line does need an argument parser, so that part is
//! behind the `cli` feature - a crate that wants to choose its benchmarks
//! some other way should not get a parser in its dependency tree.
//!
//! [`Suite`]: crate::Suite
//! [`Config::max_time`]: crate::Config::max_time

/// Which benchmarks of a suite to measure.
///
/// # What it matches
///
/// The name the report shows: `mymod::fib_200` for a benchmark,
/// `sorting@reversed` for a matrix cell, the group's name for a comparison.
/// A pattern is a substring unless [`Filter::exact`] is set, several
/// patterns are an *or*, and anything matching a skip pattern is dropped
/// afterwards - so a skip can carve a hole in a broad filter.
///
/// # A comparison is filtered whole
///
/// The alternatives of a comparison are measured in one interleaved round
/// precisely so that their differences are paired. Running one of them is
/// therefore not a smaller version of that comparison, it is a different and
/// worse measurement. So filtering happens per *entry*: a comparison is in
/// or out entire, and what a filter is matched against is the comparison's
/// own name rather than its alternatives'.
///
/// # It changes the verdicts, and should
///
/// [`Suite::run`] takes the Bonferroni limit from the number of comparisons
/// the suite actually holds. Run five and you have five chances at a false
/// positive; run one and you have one. So a comparison filtered down to on
/// its own is judged more leniently than the same comparison among others,
/// and a difference can be called a change alone that was not a change in
/// the suite. That is correct rather than surprising - the correction is
/// *for* the size of the family - but it does mean a filtered run and a full
/// one are not quite asking the same question.
///
/// [`Suite::run`]: crate::Suite::run
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Filter {
    patterns: Vec<String>,
    skip: Vec<String>,
    exact: bool,
    list: bool,
}

impl Filter {
    /// Measure everything, which is what a suite does without one.
    pub fn everything() -> Self {
        Filter::default()
    }

    /// Keep only names matching this. Repeating it widens the filter.
    pub fn matching(mut self, pattern: impl Into<String>) -> Self {
        self.patterns.push(pattern.into());
        self
    }

    /// Drop names matching this, after the patterns have been applied.
    pub fn skipping(mut self, pattern: impl Into<String>) -> Self {
        self.skip.push(pattern.into());
        self
    }

    /// Match whole names rather than any part of them.
    pub fn exact(mut self, exact: bool) -> Self {
        self.exact = exact;
        self
    }

    /// Whether the caller asked to be told what would run rather than to run
    /// it.
    ///
    /// Acted on by the caller rather than here, because printing is not this
    /// crate's business: ask a suite for [`Suite::names`] and print those.
    ///
    /// It earns its place because registered benchmarks are named after the
    /// module and function they were written in, and nobody wrote those names
    /// down anywhere - so "what is there?" has no other answer.
    ///
    /// [`Suite::names`]: crate::Suite::names
    pub fn is_listing(&self) -> bool {
        self.list
    }

    /// Ask to be told what would run rather than running it.
    pub fn listing(mut self, list: bool) -> Self {
        self.list = list;
        self
    }

    /// Fill in from `fallback` wherever this filter says nothing.
    ///
    /// Field by field rather than all or nothing, so that a filter from one
    /// source can narrow a filter from another without replacing it:
    /// `SCALING_SKIP=slow` alongside `--filter sort` means both, which is
    /// what it appears to mean.
    ///
    /// The two flags are an *or* rather than an override, there being no way
    /// to say "not exact" or "do not list" that could be overridden.
    pub fn or(self, fallback: Filter) -> Filter {
        Filter {
            patterns: if self.patterns.is_empty() {
                fallback.patterns
            } else {
                self.patterns
            },
            skip: if self.skip.is_empty() {
                fallback.skip
            } else {
                self.skip
            },
            exact: self.exact || fallback.exact,
            list: self.list || fallback.list,
        }
    }

    /// Whether a benchmark of this name should be measured.
    pub fn matches(&self, name: &str) -> bool {
        let hit = |p: &String| {
            if self.exact {
                name == p
            } else {
                name.contains(p.as_str())
            }
        };
        // No patterns means everything, so that a filter carrying only a
        // `skip` does what it looks like it does.
        if !self.patterns.is_empty() && !self.patterns.iter().any(hit) {
            return false;
        }
        !self.skip.iter().any(hit)
    }
}

#[cfg(feature = "cli")]
pub(crate) mod cli {
    use super::Filter;
    use auto_args::AutoArgs;

    /// The flags, as `auto-args` reads them.
    ///
    /// Named flags rather than a bare word for the filter, because
    /// `auto-args` has no positional arguments - so `--filter sort` where
    /// `cargo test` would take `sort`.
    ///
    /// Shared with the runner rather than restated there: it flattens this
    /// in as `_filter`, which `auto-args` reads as "these flags, without a
    /// prefix". Two lists of the same four flags would be two lists to keep
    /// in step.
    #[derive(AutoArgs, Debug, Default)]
    pub(crate) struct Flags {
        /// Measure only benchmarks whose name contains this.
        pub filter: Vec<String>,
        /// Do not measure benchmarks whose name contains this.
        pub skip: Vec<String>,
        /// Match the whole name rather than any part of it.
        pub exact: bool,
        /// Print what would be measured, and measure nothing.
        pub list: bool,
    }

    impl Flags {
        pub(crate) fn to_filter(&self) -> Filter {
            Filter {
                patterns: self.filter.clone(),
                skip: self.skip.clone(),
                exact: self.exact,
                list: self.list,
            }
        }
    }

    /// Drop what cargo appends on its own account. See [`CARGO_ADDS`].
    pub(crate) fn without_cargo_adds<I, S>(args: I) -> Vec<String>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        args.into_iter()
            .map(Into::into)
            .filter(|a| !CARGO_ADDS.contains(&a.as_str()))
            .collect()
    }

    /// What cargo passes on its own account, and what this must ignore.
    ///
    /// `cargo bench` appends `--bench` to a `harness = false` binary whether
    /// or not the caller passed anything, and appends it *after* the
    /// caller's own arguments. Measured:
    ///
    /// ```none
    /// cargo bench --bench b            ->  ["…/b", "--bench"]
    /// cargo bench --bench b -- --list  ->  ["…/b", "--list", "--bench"]
    /// cargo test  --bench b            ->  ["…/b"]
    /// ```
    ///
    /// So a parser that rejects what it does not know fails on the plainest
    /// invocation there is, `cargo bench`, having been given nothing by
    /// anybody. `auto-args` does reject it - `Err(UnexpectedOption)` - so it
    /// is dropped before parsing rather than taught about.
    pub(crate) const CARGO_ADDS: &[&str] = &["--bench", "--test"];

    impl Filter {
        /// Read the command line, and give up with a message if it does not
        /// make sense.
        ///
        /// Explicit rather than automatic: a library that reads `argv`
        /// because it was linked in, without being asked, is a library that
        /// surprises somebody.
        pub fn from_args() -> Filter {
            match Filter::from_arg_iter(std::env::args()) {
                Ok(f) => f,
                Err(e) => {
                    eprintln!("{e}\n\n{}", Flags::usage());
                    std::process::exit(2);
                }
            }
        }

        /// [`Filter::from_args`], from an iterator, for testing and for
        /// callers who have their own arguments.
        ///
        /// The first item is the program name and is discarded, as it is in
        /// `std::env::args`.
        pub fn from_arg_iter<I, S>(args: I) -> Result<Filter, String>
        where
            I: IntoIterator<Item = S>,
            S: Into<String>,
        {
            let kept = without_cargo_adds(args);
            let flags = Flags::from_iter(kept).map_err(|e| format!("{e:?}"))?;
            Ok(flags.to_filter())
        }

        /// Read `SCALING_FILTER`, `SCALING_SKIP` and `SCALING_EXACT`.
        ///
        /// The environment is what survives a wrapper. `make bench`, a CI
        /// step, a `cargo bench --workspace` fanning out over several
        /// crates: none of those pass arguments through without being
        /// taught to, and `SCALING_FILTER=sort make bench` needs nobody's
        /// cooperation.
        ///
        /// `SCALING_FILTER` and `SCALING_SKIP` hold whitespace-separated
        /// patterns; `SCALING_EXACT` counts if it is set to anything.
        pub fn from_env() -> Filter {
            let split = |k: &str| -> Vec<String> {
                std::env::var(k)
                    .ok()
                    .into_iter()
                    .flat_map(|v| v.split_whitespace().map(str::to_string).collect::<Vec<_>>())
                    .collect()
            };
            Filter {
                patterns: split("SCALING_FILTER"),
                skip: split("SCALING_SKIP"),
                exact: std::env::var_os("SCALING_EXACT").is_some(),
                list: std::env::var_os("SCALING_LIST").is_some(),
            }
        }

        /// Both, with the command line winning where it says anything.
        ///
        /// Field by field rather than all or nothing, so that
        /// `SCALING_SKIP=slow cargo bench -- --filter sort` means what it
        /// appears to.
        pub fn from_env_and_args() -> Filter {
            Filter::from_args().or(Filter::from_env())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_filter_with_nothing_in_it_keeps_everything() {
        let f = Filter::everything();
        assert!(f.matches("anything"));
        assert!(f.matches(""));
    }

    #[test]
    fn a_pattern_matches_any_part_of_a_name() {
        let f = Filter::everything().matching("sort");
        assert!(f.matches("mymod::sort_std"));
        assert!(f.matches("sorting@reversed"));
        assert!(!f.matches("hashing@short"));
    }

    #[test]
    fn several_patterns_are_an_or() {
        let f = Filter::everything().matching("sort").matching("hash");
        assert!(f.matches("mymod::sort_std"));
        assert!(f.matches("hashing@short"));
        assert!(!f.matches("mymod::fib"));
    }

    #[test]
    fn exact_matches_the_whole_name() {
        let f = Filter::everything().matching("sort").exact(true);
        assert!(f.matches("sort"));
        assert!(
            !f.matches("mymod::sort_std"),
            "a substring is not the whole name",
        );
    }

    /// A skip is applied after the patterns, so it can carve a hole in a
    /// broad filter - which is the useful way round.
    #[test]
    fn a_skip_narrows_a_filter_that_already_matched() {
        let f = Filter::everything().matching("sort").skipping("slow");
        assert!(f.matches("sorting@fast"));
        assert!(!f.matches("sorting@slow"));
    }

    /// And a filter carrying only a skip does what it looks like: everything
    /// except that.
    #[test]
    fn a_skip_on_its_own_keeps_everything_else() {
        let f = Filter::everything().skipping("slow");
        assert!(f.matches("anything"));
        assert!(!f.matches("very_slow_one"));
    }
}
