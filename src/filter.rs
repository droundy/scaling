//! Running only some of a suite's benchmarks.
//!
//! A benchmark suite can contain many entries, and filtering lets a run focus
//! on one function or subset without changing the benchmark definitions.
//!
//! Build one by hand and pass it through [`crate::runner::Options::filter`].
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
/// ```
/// use scaling::Filter;
///
/// // Substring, by default - matches the module-qualified name too.
/// let by_fn_name = Filter::everything().matching("fib_200");
/// assert!(by_fn_name.matches("mymod::fib_200"));
/// assert!(!by_fn_name.matches("mymod::fib_500"));
///
/// // `@` and the module path are just more of the string to match on.
/// let by_matrix = Filter::everything().matching("sorting@reversed");
/// assert!(by_matrix.matches("sorting@reversed"));
/// assert!(!by_matrix.matches("sorting@sorted"));
///
/// // `exact` requires the whole name, module path included.
/// let exact = Filter::everything().matching("fib_200").exact(true);
/// assert!(!exact.matches("mymod::fib_200"), "the module prefix is part of the name");
/// assert!(exact.matches("fib_200"), "matches only a name with nothing else in it");
///
/// // A skip narrows what a broader filter already matched.
/// let with_skip = Filter::everything()
///     .matching("mymod::")
///     .skipping("slow");
/// assert!(with_skip.matches("mymod::fib_200"));
/// assert!(!with_skip.matches("mymod::slow_fib"));
/// ```
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
/// Running a suite takes the Bonferroni limit from the number of comparisons
/// the suite actually holds. Run five and you have five chances at a false
/// positive; run one and you have one. So a comparison filtered down to on
/// its own is judged more leniently than the same comparison among others,
/// and a difference can be called a change alone that was not a change in
/// the suite. That is correct rather than surprising - the correction is
/// *for* the size of the family - but it does mean a filtered run and a full
/// one are not quite asking the same question.
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
    /// crate's business: ask a suite for its names and print those.
    ///
    /// It earns its place because registered benchmarks are named after the
    /// module and function they were written in, and nobody wrote those names
    /// down anywhere - so "what is there?" has no other answer.
    pub fn is_listing(&self) -> bool {
        self.list
    }

    /// Ask to be told what would run rather than running it.
    pub fn listing(mut self, list: bool) -> Self {
        self.list = list;
        self
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
