use super::*;
use std::fmt::{self, Display, Formatter};
use std::time::Duration;

const MIN_SAMPLES: usize = 6;

const SAMPLE_TIME: Duration = Duration::from_micros(100);

const MAX_SAMPLES: usize = 1_000_000;

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

impl Config {
    /// Compare two functions `f_baseline` and `f_candidate` to see if performance has changed.
    ///
    /// This is judged at the Bonferroni limit for a family of one.
    ///
    /// # This does not correct across calls
    ///
    /// Calling this `n` times judges each result as though it were the only
    /// comparison being made, so the chance of *some* false positive among
    /// them grows with `n` - which is the thing a multiple-comparison
    /// correction exists to stop. A `Config` cannot know how many times it is
    /// about to be called, and this corrects for what it can see.
    ///
    /// `Config` used to carry a promised count and a `Drop` that checked it,
    /// which made the caller declare that total; removing that machinery
    /// removed the guarantee with it. Use a [`Suite`] for comparisons that
    /// belong together: it collects them all before running any, so it knows
    /// the size of the family and corrects for it. That is the only path here
    /// that gets this right, and these functions are expected to give way to
    /// it.
    ///
    /// See [`Config::compare_gen_input`] for algorithm details.
    pub fn compare<B, C, O>(&self, mut f_baseline: B, mut f_candidate: C) -> Comparison
    where
        B: FnMut() -> O,
        C: FnMut() -> O,
    {
        self.compare_input((), |_| f_baseline(), |_| f_candidate())
    }

    /// Compare two functions `f_baseline` and `f_candidate` that need mutable state to run.
    ///
    /// Every iteration of both functions gets its own freshly-cloned copy of `input`, so neither
    /// function ever sees state the other left behind, and neither sees its own from a previous
    /// iteration. The cloning happens outside the timed region and is not measured.
    ///
    /// Note that a whole batch of copies exists at once - possibly many thousands - so `input` is
    /// best kept small. [`Config::compare_gen_input`] takes a closure instead, for state that is
    /// too expensive to clone or is not [`Clone`] at all.
    ///
    /// As with every `compare_*` function, this corrects for a family of one
    /// and does not correct across calls; see [`Config::compare`]. See
    /// [`Config::compare_gen_input`] for algorithm details.
    pub fn compare_input<B, C, I, O>(&self, input: I, f_baseline: B, f_candidate: C) -> Comparison
    where
        B: FnMut(&mut I) -> O,
        C: FnMut(&mut I) -> O,
        I: Clone,
    {
        self.compare_gen_input(move || input.clone(), f_baseline, f_candidate)
    }

    /// Compare two functions whose mutable state is built fresh rather than cloned.
    ///
    /// `gen_input` is called once per iteration of each function, so `f_baseline` and
    /// `f_candidate` always get their own inputs and neither can leave anything behind for the
    /// other. Building them is not timed. Use this in preference to [`Config::compare_input`]
    /// when the state is expensive to clone, or is not [`Clone`] at all.
    ///
    /// As with every `compare_*` function, this corrects for a family of one
    /// and does not correct across calls; see [`Config::compare`].
    ///
    /// ## Overhead
    ///
    /// Every iteration performs a lookup into a big vector to reach its input, exactly as
    /// [`Config::bench_gen_input`] does, and the same worst-case cache-miss caveat applies.
    /// Here both functions pay it alike, so most of it cancels out of the difference.
    ///
    /// # Algorithm
    ///
    /// 1. **Calibrate.** Find the smallest batch size `unit` for which one batch of each
    ///    function *together* reach `SAMPLE_TIME`, extrapolating multiplicatively from the last
    ///    probe as [`Config::bench_gen_input`] does. Both functions are measured at the same
    ///    `unit`, so it is the pair of batches, not either one alone, that costs a sample time.
    /// 2. **Sample.** Each round times a batch of `unit` iterations of `f_baseline` and then a
    ///    batch of `unit` iterations of `f_candidate`, recording the per-iteration time of
    ///    each. Timing them back to back means slow drift - a core changing frequency, a
    ///    neighbour waking up - lands on both nearly equally, so it largely cancels out of the
    ///    difference between them. It still widens both error bars, but that only makes
    ///    [`Comparison::is_changed`] harder to satisfy; drift that fell on one function alone
    ///    would instead look exactly like a real change.
    /// 3. **Stop** once there are at least `MIN_SAMPLES` rounds *and* a difference the size of
    ///    the goal would be detected - that is, once [`Comparison::is_changed`] would fire if
    ///    the difference were exactly `target_rel_error` of the baseline (or
    ///    `target_abs_error`, whichever is coarser). The standard error being driven down is
    ///    that of the *difference*, `sqrt(se_baseline² + se_candidate²)`. If `max_time` runs
    ///    out first, stop anyway and set `hit_limit` on both halves.
    ///
    /// Step 3 asks the very question the result will later be judged by, so what the goal buys
    /// you is a floor on sensitivity rather than on precision. It is a 50% floor, deliberately: a
    /// difference exactly the size of your goal is caught about half the time, and one twice
    /// that size essentially always - 97.5% at a single comparison, higher as more are planned.
    /// Ask for 1% and you should expect 1% regressions to slip through regularly and 2% ones
    /// not to. [`Comparison::min_detectable_difference`] reports where a given run actually
    /// landed, which matters most when the budget ran out before the goal was reached.
    ///
    /// Reaching that floor costs `z²` times the sampling the plain accuracy target would need -
    /// roughly 4x at one planned comparison, 8x at ten, 12x at a hundred - so comparison suites
    /// usually want an explicit [`Config::with_max_time`].
    ///
    /// One caveat on reading the output: a difference detected right at the threshold is
    /// overstated by around 40% on average, because it only clears the bar on the runs where
    /// noise pushed it up. That is true of any significance threshold, not special to this one.
    ///
    /// See [`Config::bench_gen_input`] for why batching does not bias the per-iteration figures
    /// that come out of step 2.
    pub fn compare_gen_input<G, B, C, I, O>(
        &self,
        gen_input: G,
        f_baseline: B,
        f_candidate: C,
    ) -> Comparison
    where
        G: FnMut() -> I,
        B: FnMut(&mut I) -> O,
        C: FnMut(&mut I) -> O,
    {
        let _machine = Machine::claim();
        // Twice the budget, because a comparison produces two `Stats`: at the
        // single budget each side would get half the wall clock a lone
        // `bench` call is allowed, for the same target.
        let clock = Clock::new(self.max_time * 2);
        block_on(
            &clock,
            // A family of one: this call makes exactly one comparison, and
            // nothing else shares its threshold.
            self.compare_gen_input_async(
                &clock,
                Config::z_alpha_for(1),
                Config::next_comparison_seed(),
                gen_input,
                f_baseline,
                f_candidate,
            ),
        )
    }

    /// The comparison's sampling loop, which yields to the scheduler between
    /// rounds.
    ///
    /// A *round* - baseline and candidate, back to back - is the unit, and it
    /// is deliberately atomic. The paired error bar this reports comes from
    /// the per-round differences, and that only cancels the machine's slow
    /// movement because the two halves are timed under near-identical
    /// conditions. Yielding between them would let a whole suite run in the
    /// gap and put the drift back into every difference.
    ///
    /// `clock` must be built with twice [`Config::max_time`], for the reason
    /// given at the call site above.
    ///
    /// Neither pinning nor the exclusive guard is taken here; the caller owns
    /// them, so a suite claims the machine once for the whole session.
    /// `z_alpha` is the Bonferroni limit for the family this comparison
    /// belongs to, and `seed` distinguishes its random stream from that of
    /// any sibling. Both come from the caller because both are facts about
    /// the family rather than about this one comparison.
    pub(crate) async fn compare_gen_input_async<G, B, C, I, O>(
        &self,
        clock: &Clock,
        z_alpha: f64,
        seed: u64,
        mut gen_input: G,
        mut f_baseline: B,
        mut f_candidate: C,
    ) -> Comparison
    where
        G: FnMut() -> I,
        B: FnMut(&mut I) -> O,
        C: FnMut(&mut I) -> O,
    {
        let mut xs: Vec<I> = Vec::new();
        let (unit, base_ns, cand_ns, probed) = calibrate(
            &mut gen_input,
            &mut f_baseline,
            &mut f_candidate,
            &mut xs,
            clock,
        )
        .await;
        if clock.exhausted() {
            return Comparison {
                baseline: Stats {
                    ns_per_iter: base_ns / unit as f64,
                    std_error: f64::NAN,
                    iterations: probed,
                    samples: 1,
                    hit_limit: true,
                    untrustworthy: true,
                },
                candidate: Stats {
                    ns_per_iter: cand_ns / unit as f64,
                    std_error: f64::NAN,
                    iterations: probed,
                    samples: 1,
                    hit_limit: true,
                    untrustworthy: true,
                },
                z_alpha,
                paired_std_error: f64::NAN,
            };
        }

        // Time spent *running* the two functions, not wall-clock time: an
        // input that is slow to build would otherwise satisfy the floor by
        // being built, which is not evidence about either function.
        let mut measured_ns = 0.0;
        let mut base_samples = Running::default();
        let mut cand_samples = Running::default();
        let mut diff_samples = Running::default();

        // One batch of one function, with everything shared passed in rather
        // than captured, so the two closures borrow disjointly.
        let mut run_baseline =
            |gen: &mut G, xs: &mut Vec<I>, unit: usize| time_batch(gen, &mut f_baseline, xs, unit);
        let mut run_candidate =
            |gen: &mut G, xs: &mut Vec<I>, unit: usize| time_batch(gen, &mut f_candidate, xs, unit);

        // Which function is timed first is chosen per round, so neither
        // occupies a fixed position. Timing them in a fixed order leaves the
        // two sampling fixed and *different* phases of anything periodic in
        // the machine, and there is something periodic in it: the scheduler
        // tick, a 1000Hz line in the spectrum of a fixed-cadence sample.
        //
        // The choice is made behind a `&mut dyn` over the whole batch, not
        // over the function inside it. Writing it as an `if`/`else` around
        // two `time_batch` calls duplicates the timing loop, and the copies
        // are not equally fast, so each function ends up measured by a
        // *mixture* of two of them - which showed up as a 3-5% difference
        // between a function and itself. Erasing the batch instead leaves
        // each function one consistently-compiled loop, and costs one
        // indirect call per batch rather than per iteration.
        let mut flip: u64 = 0x9E37_79B9_7F4A_7C15 ^ seed.wrapping_mul(0x2545_F491_4F6C_DD1D);
        loop {
            flip ^= flip << 13;
            flip ^= flip >> 7;
            flip ^= flip << 17;
            let candidate_first = flip & 1 == 1;
            type Batch<'a, G, I> = &'a mut dyn FnMut(&mut G, &mut Vec<I>, usize) -> (f64, f64);
            let (first, second): (Batch<G, I>, Batch<G, I>) = if candidate_first {
                (&mut run_candidate, &mut run_baseline)
            } else {
                (&mut run_baseline, &mut run_candidate)
            };
            let (_, t_first) = first(&mut gen_input, &mut xs, unit);
            let (_, t_second) = second(&mut gen_input, &mut xs, unit);
            let (base_t, cand_t) = if candidate_first {
                (t_second, t_first)
            } else {
                (t_first, t_second)
            };
            measured_ns += base_t + cand_t;
            base_samples.push(base_t / unit as f64);
            cand_samples.push(cand_t / unit as f64);
            diff_samples.push((cand_t - base_t) / unit as f64);

            let (base_mean, base_std_error) = base_samples.mean_and_stderr();
            let (cand_mean, cand_std_error) = cand_samples.mean_and_stderr();
            let (_, paired_std_error) = diff_samples.mean_and_stderr();

            let out_of_budget = base_samples.count >= MAX_SAMPLES || clock.exhausted();
            // The same estimate `Comparison::std_error` reports, so what
            // sampling drives down is exactly what the verdict is made on.
            // Stopping on the combined form instead measured no better -
            // both hold the error bar to the observed spread - but it would
            // leave the rule and the verdict asking different questions.
            let std_error = if paired_std_error.is_nan() {
                (base_std_error.powi(2) + cand_std_error.powi(2)).sqrt()
            } else {
                paired_std_error
            };
            // Twice [`MIN_SAMPLE_TIME`], because a round here buys evidence
            // about *two* functions: at the single floor each side would get
            // half of what a lone `bench` call gets, and a comparison is only
            // as good as the weaker of its two halves.
            let precise_enough = base_samples.count >= MIN_SAMPLES
                && measured_ns >= 2.0 * MIN_SAMPLE_TIME.as_secs_f64() * 1e9
                && self.comparison_accuracy_met(base_mean, std_error, z_alpha);
            if precise_enough || out_of_budget {
                return Comparison {
                    baseline: Stats {
                        ns_per_iter: base_mean,
                        std_error: base_std_error,
                        iterations: probed + base_samples.count as u64 * unit as u64,
                        samples: base_samples.count,
                        hit_limit: !precise_enough,
                        untrustworthy: base_samples.count < MIN_SAMPLES,
                    },
                    candidate: Stats {
                        ns_per_iter: cand_mean,
                        std_error: cand_std_error,
                        iterations: probed + cand_samples.count as u64 * unit as u64,
                        samples: cand_samples.count,
                        hit_limit: !precise_enough,
                        untrustworthy: cand_samples.count < MIN_SAMPLES,
                    },
                    z_alpha,
                    paired_std_error,
                };
            }
            // One *round* per poll, never half of one. See the note on this
            // function about why the pair may not be split.
            clock.yield_now().await;
        }
    }
}

async fn calibrate<G, B, C, I, O>(
    gen_input: &mut G,
    f_base: &mut B,
    f_cand: &mut C,
    xs: &mut Vec<I>,
    clock: &Clock,
) -> (usize, f64, f64, u64)
where
    G: FnMut() -> I,
    B: FnMut(&mut I) -> O,
    C: FnMut(&mut I) -> O,
{
    let probe_ceiling_ns = (clock.budget() / 100)
        .max(Duration::from_millis(5))
        .as_secs_f64()
        * 1e9;
    const MAX_CALIBRATION_UNIT: usize = 2_000_000;
    const MAX_CALIBRATION_BYTES: usize = 64 * 1024 * 1024;
    let unit_cap =
        MAX_CALIBRATION_UNIT.min(MAX_CALIBRATION_BYTES / std::mem::size_of::<I>().max(1));
    let target = SAMPLE_TIME.as_secs_f64() * 1e9;
    let mut unit = 1usize;
    let mut probed = 0u64;
    loop {
        let (base_setup_ns, base_t) = time_batch(gen_input, f_base, xs, unit);
        let (cand_setup_ns, cand_t) = time_batch(gen_input, f_cand, xs, unit);
        probed += unit as u64;
        let total_ns = base_setup_ns + cand_setup_ns + base_t + cand_t;
        if base_t + cand_t >= target
            || total_ns >= probe_ceiling_ns
            || unit >= unit_cap
            || clock.exhausted()
        {
            return (unit, base_t, cand_t, probed);
        }
        // Before the extrapolation, so every return reports a `unit` and the
        // times measured at it.
        if !clock.yield_now().await {
            return (unit, base_t, cand_t, probed);
        }
        let factor_time = (target / (base_t + cand_t).max(1.0)).clamp(2.0, 100.0);
        let factor_safety = (probe_ceiling_ns / total_ns.max(1.0)).max(1.0);
        let factor = factor_time.min(factor_safety);
        unit = ((unit as f64 * factor).ceil() as usize)
            .max(unit + 1)
            .min(unit_cap);
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
                let c = cfg.compare(
                    variable_cost(0x9e37_79b9_7f4a_7c15u64.wrapping_mul(r + 1), base_iters),
                    variable_cost(0x9e37_79b9_7f4a_7c15u64.wrapping_mul(r + 1), cand_iters),
                );
                if c.is_changed() {
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
