use super::*;
use std::fmt::{self, Display, Formatter};

/// The measured difference between a timing and its baseline.
#[derive(Debug, PartialEq, Clone, Copy)]
pub struct Difference {
    /// How much slower the candidate measured than the baseline, in
    /// nanoseconds. Negative means the candidate was faster.
    pub ns: f64,
    /// Standard error of `ns`, in nanoseconds.
    pub std_error: f64,
    /// The baseline's nanoseconds per iteration, used for relative changes.
    pub baseline_ns_per_iter: f64,
    baseline_std_error: f64,
    /// How many standard errors the difference must exceed to count as a
    /// change: the limit for the comparison's family. `NaN` for a rough one,
    /// which is not tested.
    limit: f64,
    /// The candidate's time over the baseline's as a natural log, and its
    /// standard error, when the comparison was measured as a ratio within
    /// each round. Significance is then judged on this scale, which is the
    /// one the stopping rule used.
    log_ratio: Option<(f64, f64)>,
    /// Whether this comparison was only asked how big the difference is, not
    /// whether there is one. It is then never called a change, and is shown
    /// as a size.
    rough: bool,
}

impl Difference {
    /// How large the difference is relative to the baseline (0.01 = 1%).
    pub fn relative(&self) -> f64 {
        self.ns / self.baseline_ns_per_iter
    }

    /// How large the difference is relative to the baseline, as a percentage.
    pub fn percent(&self) -> f64 {
        self.relative() * 100.0
    }

    /// Whether the difference is large enough to count as a real change.
    ///
    /// Never for a comparison that was only asked for a rough size.
    pub fn is_changed(&self) -> bool {
        if self.rough {
            return false;
        }
        match self.log_ratio {
            Some((ln, se)) => crate::significant::is_significant(ln, se, self.limit),
            None => crate::significant::is_significant(self.ns, self.std_error, self.limit),
        }
    }

    /// Whether this comparison was asked only how big the difference is, and
    /// not whether there is one. See `uninteresting` in [`bench`](macro@bench).
    pub fn is_rough(&self) -> bool {
        self.rough
    }

    /// The smallest difference this result could have called a change.
    ///
    /// `NaN` for a rough comparison (see [`Difference::is_rough`]), which
    /// could have called nothing a change.
    pub fn min_detectable_difference(&self) -> f64 {
        self.limit * self.std_error
    }

    /// The smallest detectable difference as a fraction of the baseline.
    pub fn min_detectable_rel(&self) -> f64 {
        self.min_detectable_difference() / self.baseline_ns_per_iter
    }

    #[cfg(test)]
    pub(crate) fn combined_std_error(&self, candidate_std_error: f64) -> f64 {
        (candidate_std_error.powi(2) + self.baseline_std_error.powi(2)).sqrt()
    }

    pub(crate) fn from_parts(
        baseline: &Timing,
        candidate: &Timing,
        limit: f64,
        paired_std_error: f64,
    ) -> Self {
        let std_error = if paired_std_error.is_nan() {
            (candidate.std_error.powi(2) + baseline.std_error.powi(2)).sqrt()
        } else {
            paired_std_error
        };
        Difference {
            ns: candidate.ns_per_iter - baseline.ns_per_iter,
            std_error,
            baseline_ns_per_iter: baseline.ns_per_iter,
            baseline_std_error: baseline.std_error,
            limit,
            log_ratio: None,
            rough: false,
        }
    }

    /// A difference measured as a ratio within each round: `ln_ratio` is the
    /// log of the candidate's time over the baseline's and `ln_std_error` its
    /// standard error. `limit` is the threshold `is_changed` applies to their
    /// quotient, Student's t for the family at the estimate's degrees of
    /// freedom. The nanosecond figures are derived from the baseline's time.
    pub(crate) fn from_log_ratio(
        baseline: &Timing,
        ln_ratio: f64,
        ln_std_error: f64,
        limit: f64,
    ) -> Self {
        let ratio = ln_ratio.exp();
        Difference {
            ns: baseline.ns_per_iter * (ratio - 1.0),
            std_error: baseline.ns_per_iter * ratio * ln_std_error,
            baseline_ns_per_iter: baseline.ns_per_iter,
            baseline_std_error: baseline.std_error,
            limit,
            log_ratio: Some((ln_ratio, ln_std_error)),
            rough: false,
        }
    }

    /// The same difference, for a comparison that is only asked how big it is:
    /// it has no limit to exceed, and is never called a change.
    pub(crate) fn rough(self) -> Self {
        Difference {
            limit: f64::NAN,
            rough: true,
            ..self
        }
    }
}

impl Display for Timing {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let Some(difference) = &self.difference else {
            return self.write_measurement(f);
        };
        let limit = match (self.hit_limit, self.untrustworthy) {
            (true, true) => " (limit, untrusted)",
            (true, false) => " (limit)",
            (false, true) => " (untrusted)",
            (false, false) => "",
        };
        // A rough comparison says how big, whether or not it is also a change:
        // it was never asked that.
        if difference.is_changed() || difference.rough {
            let percent_error = difference.std_error / difference.baseline_ns_per_iter * 100.0;
            let (value, error) =
                value_and_error(difference.percent(), percent_error, f.precision());
            // The sign is always shown, so a speedup and a slowdown of the
            // same size cannot be mistaken for each other.
            let sign = if value.starts_with('-') { "" } else { "+" };
            write!(f, "{sign}{value}% ± {error}%{limit}")
        } else {
            let detectable = difference.min_detectable_rel() * 100.0;
            if detectable.is_finite() {
                write!(f, "(< {detectable:.1}%){limit}")
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

    fn timing(ns: f64, std_error: f64) -> Timing {
        Timing {
            ns_per_iter: ns,
            std_error,
            iterations: 1000,
            samples: 100,
            hit_limit: false,
            untrustworthy: false,
            difference: None,
        }
    }

    fn comparison(baseline: f64, candidate: f64, se_each: f64, planned: u64) -> Timing {
        let baseline = timing(baseline, se_each);
        let mut candidate_timing = timing(candidate, se_each);
        candidate_timing.difference = Some(Difference::from_parts(
            &baseline,
            &candidate_timing,
            crate::significant::bonferroni_z_limit(planned, crate::significant::FWER),
            f64::NAN,
        ));
        candidate_timing
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
        let d = c.difference().expect("it was compared");
        assert!(
            (d.min_detectable_difference() - goal).abs() < 1e-9,
            "expected {goal}, got {}",
            d.min_detectable_difference()
        );
        assert!((d.min_detectable_rel() - cfg.target_rel_error).abs() < 1e-12);
    }

    /// A run cut off by the budget must never be mistaken for a clean "no
    /// change".
    #[test]
    fn display_marks_a_truncated_run() {
        // Planned and precise: the sensitivity is the whole story.
        let clean = format!("{}", comparison(100.0, 100.0, 0.1, 4));
        assert!(clean.starts_with("(< "), "{clean}");
        assert!(!clean.contains("limit"), "{clean}");

        // The budget ran out before the goal was reached.
        let mut truncated = comparison(100.0, 100.0, 0.1, 4);
        truncated.hit_limit = true;
        let truncated = format!("{truncated}");
        assert!(truncated.ends_with(" (limit)"), "{truncated}");

        // Too few samples for the error bar itself to be worth reading.
        let mut untrusted = comparison(100.0, 100.0, 0.1, 4);
        untrusted.untrustworthy = true;
        let untrusted = format!("{untrusted}");
        assert!(untrusted.ends_with(" (untrusted)"), "{untrusted}");

        // A change that is real, but measured on a truncated run.
        let mut changed = comparison(100.0, 130.0, 0.1, 4);
        changed.hit_limit = true;
        // Pinned whole, so the `\u{b1}` cannot quietly become an ASCII `+/-` and
        // drift from what a raw Timing prints.
        assert_eq!("+30.00% \u{b1} 0.14% (limit)", format!("{changed}"));
    }

    /// Each entry point corrects for the family it can see, with no shared
    /// promise to keep.
    ///
    /// The threshold is computed where the comparisons are assembled, so each
    /// family makes its own correction from the comparisons it actually
    /// contains.
    #[test]
    fn each_family_gets_its_own_threshold() {
        // More comparisons in the family means a stricter threshold.
        let one = Config::z_alpha_for(1);
        let ten = Config::z_alpha_for(10);
        assert!(one.is_finite() && ten.is_finite());
        assert!(ten > one, "ten comparisons must be judged harder than one");

        let cfg = Config::relative(0.02);
        let clone = cfg.clone();
        assert_eq!(clone.target_rel_error, cfg.target_rel_error);
    }

    /// Consecutive standalone comparisons must not draw the same sequence of
    /// orders, or a loop of them correlates with itself.
    ///
    /// Each comparison advances the seed so standalone runs do not reuse a
    /// sequence that would be correlated with the previous one.
    #[test]
    fn consecutive_comparisons_get_different_seeds() {
        let a = Config::next_comparison_seed();
        let b = Config::next_comparison_seed();
        let c = Config::next_comparison_seed();
        assert_ne!(a, b);
        assert_ne!(b, c);
    }

    /// A workload whose cost is drawn at random, so the spread is real
    /// rather than machine noise: it waits for a time drawn uniformly from
    /// nothing to twice `mean_ns`, so that its mean is `mean_ns` and it varies
    /// as much as its mean. Time, not work, so that a debug build and a
    /// release build cost the same.
    fn variable_cost(seed: u64, mean_ns: u64) -> impl FnMut() -> u64 {
        let mut rng = XorShift(seed | 1);
        move || {
            let wait = Duration::from_nanos(rng.next() % (2 * mean_ns));
            let start = std::time::Instant::now();
            while start.elapsed() < wait {
                std::hint::spin_loop();
            }
            wait.as_nanos() as u64
        }
    }

    /// The headline promise: a difference twice the goal is caught nearly
    /// always, while one exactly at the goal is a coin flip.
    ///
    /// A two-way comparison is still a one-family comparison, so it is judged at
    /// the same decision threshold as any other family but through the shared
    /// input group loop.
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
                // `candidate` takes `multiple * 5%` longer than `baseline`.
                // Calls of half a millisecond, each varying as much as its
                // mean: a round averages only a few dozen of them, so a 5%
                // goal takes tens of rounds to reach and not the floor of
                // eight. Where the floor decides, a difference the size of
                // the goal is found far more often than half the time.
                let base_ns = 500_000;
                let cand_ns = (base_ns as f64 * (1.0 + 0.05 * multiple)) as u64;
                let seed = 0x9e37_79b9_7f4a_7c15u64.wrapping_mul(r + 1);
                let c = cfg
                    .input_group()
                    .add("baseline", variable_cost(seed, base_ns))
                    .add("candidate", variable_cost(seed, cand_ns))
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
