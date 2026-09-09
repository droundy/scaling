use super::*;
use std::fmt::{self, Display, Formatter};

#[derive(Debug, Clone)]
pub struct Comparison {
    pub baseline: Stats,
    pub candidate: Stats,
    /// The Bonferroni limit this comparison was judged against, carried from
    /// the family of comparisons it was measured in. See
    /// [`Config::z_alpha_for`].
    z_alpha: f64,
    /// Standard error of the difference, taken from the per-round
    /// differences rather than by combining the two halves. `NaN` when
    /// fewer than two rounds were run.
    paired_std_error: f64,
}

impl Comparison {
    pub fn difference_ns(&self) -> f64 {
        self.candidate.ns_per_iter - self.baseline.ns_per_iter
    }
    /// Standard error of [`Comparison::difference_ns`].
    ///
    /// Taken from the per-round differences rather than by combining the two
    /// halves. The halves are timed back to back under nearly identical
    /// conditions, so whatever the machine does slowly - a drifting clock, a
    /// warming package - moves both together and cancels out of each round's
    /// difference. Adding their variances as though they were independent
    /// counts that common movement twice.
    ///
    /// It shows on a workload with real spread. Comparing such a function
    /// against itself, where the true difference is zero so the spread of
    /// the reported difference is exactly what the `±` should describe:
    ///
    /// ```none
    ///                observed spread   claimed
    ///   combined         0.123ns       0.165ns
    ///   paired           0.123ns       0.126ns
    /// ```
    ///
    /// A third narrower, and honest rather than merely cautious.
    ///
    /// Falls back to the combined form when there were fewer than two rounds
    /// to difference - the budget-blown path, where no paired estimate
    /// exists.
    pub fn std_error(&self) -> f64 {
        if self.paired_std_error.is_nan() {
            return (self.candidate.std_error.powi(2) + self.baseline.std_error.powi(2)).sqrt();
        }
        self.paired_std_error
    }
    pub fn is_changed(&self) -> bool {
        crate::significant::is_significant(self.difference_ns(), self.std_error(), self.z_alpha)
    }

    /// Assemble one from parts measured elsewhere.
    ///
    /// For [`crate::ComparisonSet`], which times more than two alternatives
    /// against each other and then reports each against the baseline. The
    /// verdict, the sensitivity and the formatting are the same questions
    /// there as here, so they are asked of the same type.
    pub(crate) fn from_parts(
        baseline: Stats,
        candidate: Stats,
        z_alpha: f64,
        paired_std_error: f64,
    ) -> Self {
        Comparison {
            baseline,
            candidate,
            z_alpha,
            paired_std_error,
        }
    }

    /// The smallest difference this comparison could have called a change,
    /// in nanoseconds.
    ///
    /// Sampling aims to bring this down to
    /// [`Config::target_rel_error`] of the baseline, but a comparison that
    /// ran out of [`Config::max_time`] stops wherever it got to - so on a
    /// result that is not changed, this is what "not changed" is worth.
    ///
    /// `NaN` when fewer than two samples were collected, leaving
    /// [`Stats::std_error`] itself `NaN`. That prints as something other than
    /// a plain result, so check this before formatting it yourself.
    pub fn min_detectable_difference(&self) -> f64 {
        self.z_alpha * self.std_error()
    }

    /// [`Comparison::min_detectable_difference`] as a fraction of the
    /// baseline (`0.01` = 1%).
    ///
    /// `NaN` wherever [`Comparison::min_detectable_difference`] is, and
    /// infinite when the baseline measured as zero, where a relative figure
    /// is undefined.
    pub fn min_detectable_rel(&self) -> f64 {
        self.min_detectable_difference() / self.baseline.ns_per_iter
    }
}

impl Display for Comparison {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        // Both halves are filled in together, so either would answer for the
        // pair - but combine them rather than depending on that. Reaching the
        // sensitivity goal costs several times what a plain accuracy target
        // does, so a comparison running out of budget is ordinary rather than
        // exotic, and a truncated answer has to say so.
        let limit = match (
            self.baseline.hit_limit || self.candidate.hit_limit,
            self.baseline.untrustworthy || self.candidate.untrustworthy,
        ) {
            (true, true) => " (limit, untrusted)",
            (true, false) => " (limit)",
            (false, true) => " (untrusted)",
            (false, false) => "",
        };
        if self.is_changed() {
            let percent_change = self.difference_ns() / self.baseline.ns_per_iter * 100.0;
            let rel_error = self.std_error() / self.baseline.ns_per_iter * 100.0;
            write!(f, "{percent_change:+.1}% ± {rel_error:.1}%{limit}")
        } else {
            let detectable = self.min_detectable_rel() * 100.0;
            if detectable.is_finite() {
                write!(f, "(unchanged, would detect {detectable:.1}%){limit}")
            } else {
                write!(f, "(unchanged){limit}")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;

    fn stats(ns: f64, std_error: f64) -> Stats {
        Stats {
            ns_per_iter: ns,
            std_error,
            iterations: 1000,
            samples: 100,
            hit_limit: false,
            untrustworthy: false,
        }
    }

    fn comparison(baseline: f64, candidate: f64, se_each: f64, planned: u64) -> Comparison {
        Comparison {
            baseline: stats(baseline, se_each),
            candidate: stats(candidate, se_each),
            z_alpha: crate::significant::bonferroni_z_limit(planned, crate::significant::FWER),
            // These are hand-built, so there are no per-round differences to
            // take: the tests using them ask about the combined form.
            paired_std_error: f64::NAN,
        }
    }

    /// The stopping rule and the verdict must be the same question asked of
    /// two different differences. If they ever drift apart, a comparison
    /// could stop at a precision that cannot decide the thing it stopped for.
    #[test]
    fn stopping_asks_exactly_what_is_changed_asks() {
        for planned in [1u64, 3, 10, 100] {
            let cfg = Config::default();
            let z_alpha = Config::z_alpha_for(planned);
            for baseline in [1.0, 71.0, 2.5e6] {
                let goal = cfg.comparison_goal_ns(baseline);
                for scale in [0.5, 0.9, 0.99, 1.01, 1.1, 2.0] {
                    // Pick a standard error, then ask both questions of it.
                    let se = goal / z_alpha * scale;
                    let stopped = cfg.comparison_accuracy_met(baseline, se, z_alpha);
                    // A comparison whose difference is exactly the goal.
                    let c = comparison(baseline, baseline + goal, se / 2.0f64.sqrt(), planned);
                    assert_eq!(
                        stopped,
                        c.is_changed(),
                        "planned {planned}, baseline {baseline}, scale {scale}"
                    );
                }
            }
            // This test never runs a comparison - it only asks the two
            // predicates about hand-built numbers - so there is no count for
            // `Drop` to check against the plan.
        }
    }

    /// A difference the size of the goal sits exactly on the threshold, so
    /// `min_detectable_difference` should come back as the goal itself.
    #[test]
    fn min_detectable_difference_is_the_goal_once_sampling_stops() {
        let cfg = Config::default();
        let baseline = 500.0;
        let goal = cfg.comparison_goal_ns(baseline);
        // The standard error the stopping rule is aiming for.
        let se = goal / Config::z_alpha_for(4);
        let c = comparison(baseline, baseline, se / 2.0f64.sqrt(), 4);
        assert!(
            (c.min_detectable_difference() - goal).abs() < 1e-9,
            "expected {goal}, got {}",
            c.min_detectable_difference()
        );
        assert!((c.min_detectable_rel() - cfg.target_rel_error).abs() < 1e-12);
    }

    /// A run cut off by the budget must never be mistaken for a clean "no
    /// change".
    #[test]
    fn display_marks_a_truncated_run() {
        // Planned and precise: the sensitivity is the whole story.
        let clean = format!("{}", comparison(100.0, 100.0, 0.1, 4));
        assert!(clean.starts_with("(unchanged, would detect"), "{clean}");
        assert!(!clean.contains("limit"), "{clean}");

        // The budget ran out before the goal was reached.
        let mut truncated = comparison(100.0, 100.0, 0.1, 4);
        truncated.baseline.hit_limit = true;
        truncated.candidate.hit_limit = true;
        let truncated = format!("{truncated}");
        assert!(truncated.ends_with(" (limit)"), "{truncated}");

        // Too few samples for the error bar itself to be worth reading.
        let mut untrusted = comparison(100.0, 100.0, 0.1, 4);
        untrusted.baseline.untrustworthy = true;
        untrusted.candidate.untrustworthy = true;
        let untrusted = format!("{untrusted}");
        assert!(untrusted.ends_with(" (untrusted)"), "{untrusted}");

        // A change that is real, but measured on a truncated run.
        let mut changed = comparison(100.0, 130.0, 0.1, 4);
        changed.baseline.hit_limit = true;
        changed.candidate.hit_limit = true;
        // Pinned whole, so the `\u{b1}` cannot quietly become an ASCII `+/-` and
        // drift from what `Stats` prints.
        assert_eq!("+30.0% \u{b1} 0.1% (limit)", format!("{changed}"));
    }

    /// Each entry point corrects for the family it can see, with nothing
    /// promised in advance.
    ///
    /// This is what replaced `Plan`: `Config` used to carry a promised count
    /// and a `Drop` that asserted the promise was kept. The count was only
    /// ever needed because a threshold was computed somewhere other than
    /// where the comparisons were made - so now each computes its own, and
    /// there is no promise left to break.
    #[test]
    fn each_family_gets_its_own_threshold() {
        // More comparisons in the family means a stricter threshold.
        let one = Config::z_alpha_for(1);
        let ten = Config::z_alpha_for(10);
        assert!(one.is_finite() && ten.is_finite());
        assert!(ten > one, "ten comparisons must be judged harder than one");

        // A `Config` is now plain data: no shared state, so cloning shares
        // nothing and dropping asserts nothing. Clippy enforces the second
        // half - `drop(cfg)` here would warn that there is no `Drop` to run.
        let cfg = Config::relative(0.02);
        let clone = cfg.clone();
        assert_eq!(clone.target_rel_error, cfg.target_rel_error);
    }

    /// Consecutive standalone comparisons must not draw the same sequence of
    /// orders, or a loop of them correlates with itself.
    ///
    /// The plan's `made` counter did this as a side effect of counting.
    /// Removing it would have quietly left every standalone comparison on the
    /// same seed, which no test then in the suite would have caught - the
    /// ones that run comparisons in a loop are gated on a quiesced machine
    /// and skip on most.
    #[test]
    fn consecutive_comparisons_get_different_seeds() {
        let a = Config::next_comparison_seed();
        let b = Config::next_comparison_seed();
        let c = Config::next_comparison_seed();
        assert_ne!(a, b);
        assert_ne!(b, c);
    }

    /// A workload whose cost is drawn at random, so the spread is real
    /// rather than machine noise.
    fn variable_cost(seed: u64, iterations: usize) -> impl FnMut() -> u64 {
        let mut rng = XorShift(seed | 1);
        move || {
            let n = 1 + (rng.next() as usize % iterations);
            let mut acc = 0u64;
            for i in 0..n {
                acc = acc.wrapping_mul(31).wrapping_add(i as u64);
            }
            acc
        }
    }

    /// The headline promise: a difference twice the goal is caught nearly
    /// always, while one exactly at the goal is a coin flip.
    ///
    /// Two alternatives is a family of one, so this is judged at the same
    /// threshold the deleted `Config::compare` used to apply, and asks the
    /// same question of the same numbers - only through the k-way loop,
    /// which is the one sampling loop left.
    #[test]
    fn twice_the_goal_is_caught_and_the_goal_itself_is_a_coin_flip() {
        println!();
        if !quiesced() {
            println!("SKIPPED: machine is not quiesced (see `quiet-bench reserve`)");
            return;
        }
        const REPEATS: u64 = 20;
        for (name, multiple, low, high) in
            [("1x goal", 1.0, 0.15, 0.85), ("2x goal", 2.0, 0.70, 1.0)]
        {
            let cfg = Config::relative(0.05).with_max_time(Duration::from_secs(4));
            let mut changed = 0u64;
            for r in 0..REPEATS {
                // `candidate` does `multiple * 5%` more work than `baseline`.
                let base_iters = 2000;
                let cand_iters = (base_iters as f64 * (1.0 + 0.05 * multiple)) as usize;
                let seed = 0x9e37_79b9_7f4a_7c15u64.wrapping_mul(r + 1);
                let c = cfg
                    .comparison()
                    .add("baseline", variable_cost(seed, base_iters))
                    .add("candidate", variable_cost(seed, cand_iters))
                    .run();
                if c.any_changed() {
                    changed += 1;
                }
            }
            let rate = changed as f64 / REPEATS as f64;
            println!(
                "{name}: detected {changed}/{REPEATS} = {:.0}%",
                rate * 100.0
            );
            assert!(
                rate >= low && rate <= high,
                "{name}: detection rate {rate:.2} outside [{low}, {high}]"
            );
        }
    }
}
