//! The filters on [`Config`]: measuring only some of what is registered.

use crate::assemble::{Lane, Plan};
use crate::Config;
use std::fmt;
use std::sync::Arc;

type Keep = Arc<dyn Fn(&str) -> bool + Send + Sync + 'static>;

/// One question about names for each kind of thing. Asking again joins the new
/// question to the old by `and`.
#[derive(Clone)]
pub(crate) struct Filters {
    groups: Keep,
    candidates: Keep,
    inputs: Keep,
}

impl Default for Filters {
    fn default() -> Self {
        Filters {
            groups: Arc::new(|_| true),
            candidates: Arc::new(|_| true),
            inputs: Arc::new(|_| true),
        }
    }
}

impl fmt::Debug for Filters {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Filters").finish_non_exhaustive()
    }
}

fn and(filter: &mut Keep, keep: Keep) {
    let before = Arc::clone(filter);
    *filter = Arc::new(move |name| before(name) && keep(name));
}

impl Filters {
    /// Take out of `plan` what the filters do not keep. A benchmark that
    /// stands alone is a group of one candidate on the unnamed implicit input,
    /// so all three are asked of it.
    pub(crate) fn apply(&self, plan: &mut Plan) {
        plan.flat
            .retain(|r| (self.groups)(&r.name) && (self.candidates)(&r.name) && (self.inputs)(""));
        plan.lanes.retain_mut(|lane| self.narrow(lane));
    }

    /// Narrow a lane to what is kept, and say whether anything is left. The
    /// baseline stays whenever another candidate does, since it is what the
    /// others are compared with.
    fn narrow(&self, lane: &mut Lane) -> bool {
        if !(self.groups)(lane.group) {
            return false;
        }
        lane.inputs.retain(|input| (self.inputs)(&input.name));
        if lane.inputs.is_empty() {
            return false;
        }
        let mut kept: Vec<bool> = lane
            .candidates
            .iter()
            .map(|c| (self.candidates)(&c.name))
            .collect();
        if !kept.contains(&true) {
            return false;
        }
        kept[0] = true;
        retain_where(&mut lane.candidates, &kept);
        // Empty when no candidate has a metrics function.
        if !lane.metrics.is_empty() {
            retain_where(&mut lane.metrics, &kept);
        }
        true
    }
}

fn retain_where<T>(items: &mut Vec<T>, kept: &[bool]) {
    let mut places = kept.iter();
    items.retain(|_| *places.next().expect("one answer for each item"));
}

impl Config {
    /// Measure only the groups `keep` says yes to, keeping every other
    /// setting.
    ///
    /// `keep` is given a group's name, as the report shows it. A benchmark
    /// that stands alone, and a scaling benchmark, is a group of its own, as
    /// [`Report::groups`] has it, and is given its own name. Asking again
    /// narrows further: a group has to be kept by every filter of its kind.
    ///
    /// ```no_run
    /// use scaling::Config;
    ///
    /// fn main() -> Result<(), scaling::RegistrationError> {
    ///     // Only the groups whose names start with `sort`.
    ///     Config::default()
    ///         .filter_groups(|group| group.starts_with("sort"))
    ///         .run_and_print()
    /// }
    /// ```
    ///
    /// # What a filtered run is
    ///
    /// The comparisons that are left are measured as they would have been,
    /// and named as they would have been. What changes is that a run costs
    /// what its benchmarks cost, and that the multiple-comparison correction
    /// is for the comparisons the run holds: five comparisons are five chances
    /// at a false positive and one is one, so a comparison filtered down to on
    /// its own is judged more leniently than among the others, and a filtered
    /// run and a full one are not quite asking the same question.
    ///
    /// Registrations are checked before anything is filtered, so a mistake in
    /// one that is left out of this run is still an error.
    ///
    /// [`Report::groups`]: crate::Report::groups
    pub fn filter_groups<F>(mut self, keep: F) -> Self
    where
        F: Fn(&str) -> bool + Send + Sync + 'static,
    {
        and(&mut self.filters.groups, Arc::new(keep));
        self
    }

    /// Measure only the candidates `keep` says yes to, keeping every other
    /// setting.
    ///
    /// `keep` is given a candidate's name, as the report shows it (that is
    /// `candidate`, without its group or input). A group with no candidate
    /// kept is not measured. A benchmark that stands alone is the one
    /// candidate of a group of its own, and is given its own name; see
    /// [`Config::filter_groups`].
    ///
    /// A group's baseline is what the other candidates are compared with, so
    /// it is measured whenever any candidate of its group is kept, whatever
    /// `keep` says of it: a filter chooses what is measured, never what it is
    /// measured against. A filter that keeps only the baseline measures it
    /// alone. A group left with one candidate is reported as that candidate,
    /// as a lone candidate always is.
    ///
    /// Asking again narrows further, and everything said at
    /// [`Config::filter_groups`] about a filtered run holds here.
    pub fn filter_candidates<F>(mut self, keep: F) -> Self
    where
        F: Fn(&str) -> bool + Send + Sync + 'static,
    {
        and(&mut self.filters.candidates, Arc::new(keep));
        self
    }

    /// Measure only on the inputs `keep` says yes to, keeping every other
    /// setting.
    ///
    /// `keep` is given an input's name, as the report shows it: `small`, or
    /// `sets@10` for the input a `sizes(..)` made at that size. A group that
    /// declares no input has the one implicit input, whose name is empty, and
    /// `keep` is given `""` for it. So is a benchmark that stands alone, which
    /// has that input too; see [`Config::filter_groups`]. A group with no
    /// input kept is not measured.
    ///
    /// Asking again narrows further, and everything said at
    /// [`Config::filter_groups`] about a filtered run holds here.
    pub fn filter_inputs<F>(mut self, keep: F) -> Self
    where
        F: Fn(&str) -> bool + Send + Sync + 'static,
    {
        and(&mut self.filters.inputs, Arc::new(keep));
        self
    }
}
