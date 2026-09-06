use super::*;
use std::fmt::{self, Display, Formatter};
use std::sync::atomic::Ordering::{Acquire, Release};
use std::time::{Duration, Instant};

const MIN_SAMPLES: usize = 6;

const SAMPLE_TIME: Duration = Duration::from_micros(100);

const MAX_SAMPLES: usize = 1_000_000;

#[derive(Debug, Clone)]
pub struct Comparison {
    pub baseline: Stats,
    pub candidate: Stats,
    /// The Bonferroni limit this comparison was judged against, carried from
    /// the [`Config`] that made it.
    z_alpha: f64,
}

impl Comparison {
    pub fn difference_ns(&self) -> f64 {
        self.candidate.ns_per_iter - self.baseline.ns_per_iter
    }
    pub fn std_error(&self) -> f64 {
        (self.candidate.std_error.powi(2) + self.baseline.std_error.powi(2)).sqrt()
    }
    pub fn is_changed(&self) -> bool {
        crate::significant::is_significant(self.difference_ns(), self.std_error(), self.z_alpha)
    }

    /// The smallest difference this comparison could have called a change,
    /// in nanoseconds.
    ///
    /// Sampling aims to bring this down to
    /// [`Config::target_rel_error`] of the baseline, but a comparison that
    /// ran out of [`Config::max_time`] stops wherever it got to - so on a
    /// result that is not changed, this is what "not changed" is worth.
    ///
    /// `NaN` when there is no threshold to compare against, which is both of
    /// the cases where there is no verdict either: no plan was set (see
    /// [`Config::with_comparisons_planned`]), or fewer than two samples were
    /// collected, leaving [`Stats::std_error`] itself `NaN`. Both print as
    /// something other than a plain result, so check this before formatting
    /// it yourself.
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
        // Without a plan there is no threshold, so there is no verdict to
        // report. Saying "(unchanged)" here would read exactly like a
        // measured no-change result, and the `Drop` check that would
        // otherwise catch the mistake does not run if the process exits, or
        // if the `Config` is leaked or outlived by a clone.
        if self.z_alpha.is_nan() {
            return write!(f, "(no verdict: num_comparisons_planned is unset)");
        }
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
    /// Note that when using the `compare_*` family of functions, you *must* use
    /// `Config::with_comparisons_planned` to specify the number of comparisons that will be
    /// made.
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
    /// As with every `compare_*` function, you *must* use
    /// [`Config::with_comparisons_planned`] to specify the number of comparisons that will be
    /// made. See [`Config::compare_gen_input`] for algorithm details.
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
    /// As with every `compare_*` function, you *must* use
    /// [`Config::with_comparisons_planned`] to specify the number of comparisons that will be
    /// made.
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
        mut gen_input: G,
        mut f_baseline: B,
        mut f_candidate: C,
    ) -> Comparison
    where
        G: FnMut() -> I,
        B: FnMut(&mut I) -> O,
        C: FnMut(&mut I) -> O,
    {
        let made = self.num_comparisons_made.fetch_add(1, Release);
        // Twice the budget, because a comparison produces two `Stats`: at the
        // single budget each side would get half the wall clock a lone
        // `bench` call is allowed, for the same target.
        let budget = self.max_time * 2;
        quiet::pin_if_reserved();
        // Serialise while pinned: two benchmarks sharing one core measure
        // each other rather than themselves.
        let _exclusive = quiet::exclusive_if_pinned();
        let start = Instant::now();
        let mut xs: Vec<I> = Vec::new();
        let (unit, base_ns, cand_ns, probed) = calibrate(
            &mut gen_input,
            &mut f_baseline,
            &mut f_candidate,
            &mut xs,
            budget,
            start,
        );
        if start.elapsed() > budget {
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
                z_alpha: self.z_alpha,
            };
        }

        // Time spent *running* the two functions, not wall-clock time: an
        // input that is slow to build would otherwise satisfy the floor by
        // being built, which is not evidence about either function.
        let mut measured_ns = 0.0;
        let mut base_samples = Running::default();
        let mut cand_samples = Running::default();

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
        let mut flip: u64 = 0x9E37_79B9_7F4A_7C15 ^ made.wrapping_mul(0x2545_F491_4F6C_DD1D);
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

            let (base_mean, base_std_error) = base_samples.mean_and_stderr();
            let (cand_mean, cand_std_error) = cand_samples.mean_and_stderr();

            let out_of_budget = base_samples.count >= MAX_SAMPLES || start.elapsed() > budget;
            let std_error = (base_std_error.powi(2) + cand_std_error.powi(2)).sqrt();
            // Twice [`MIN_SAMPLE_TIME`], because a round here buys evidence
            // about *two* functions: at the single floor each side would get
            // half of what a lone `bench` call gets, and a comparison is only
            // as good as the weaker of its two halves.
            let precise_enough = base_samples.count >= MIN_SAMPLES
                && measured_ns >= 2.0 * MIN_SAMPLE_TIME.as_secs_f64() * 1e9
                && self.comparison_accuracy_met(base_mean, std_error);
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
                    z_alpha: self.z_alpha,
                };
            }
        }
    }
}

impl Drop for Config {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            if let Some(made) = Arc::get_mut(&mut self.num_comparisons_made) {
                // We now know that we are the *last* user of this Config, so we can get an
                // accurate count of how many comparisons were made.
                let made = made.load(Acquire);
                assert_eq!(
                    self.num_comparisons_planned, made,
                    "You need to set num_comparisons_planned to {made}."
                );
            }
        }
    }
}

fn calibrate<G, B, C, I, O>(
    gen_input: &mut G,
    f_base: &mut B,
    f_cand: &mut C,
    xs: &mut Vec<I>,
    budget: Duration,
    start: Instant,
) -> (usize, f64, f64, u64)
where
    G: FnMut() -> I,
    B: FnMut(&mut I) -> O,
    C: FnMut(&mut I) -> O,
{
    let probe_ceiling_ns = (budget / 100).max(Duration::from_millis(5)).as_secs_f64() * 1e9;
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
            || start.elapsed() > budget
        {
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
        }
    }

    /// The stopping rule and the verdict must be the same question asked of
    /// two different differences. If they ever drift apart, a comparison
    /// could stop at a precision that cannot decide the thing it stopped for.
    #[test]
    fn stopping_asks_exactly_what_is_changed_asks() {
        for planned in [1u64, 3, 10, 100] {
            let cfg = Config::default().with_comparisons_planned(planned);
            for baseline in [1.0, 71.0, 2.5e6] {
                let goal = cfg.comparison_goal_ns(baseline);
                for scale in [0.5, 0.9, 0.99, 1.01, 1.1, 2.0] {
                    // Pick a standard error, then ask both questions of it.
                    let se = goal / cfg.z_alpha * scale;
                    let stopped = cfg.comparison_accuracy_met(baseline, se);
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
            std::mem::forget(cfg);
        }
    }

    /// A difference the size of the goal sits exactly on the threshold, so
    /// `min_detectable_difference` should come back as the goal itself.
    #[test]
    fn min_detectable_difference_is_the_goal_once_sampling_stops() {
        let cfg = Config::default().with_comparisons_planned(4);
        let baseline = 500.0;
        let goal = cfg.comparison_goal_ns(baseline);
        // The standard error the stopping rule is aiming for.
        let se = goal / cfg.z_alpha;
        let c = comparison(baseline, baseline, se / 2.0f64.sqrt(), 4);
        assert!(
            (c.min_detectable_difference() - goal).abs() < 1e-9,
            "expected {goal}, got {}",
            c.min_detectable_difference()
        );
        assert!((c.min_detectable_rel() - cfg.target_rel_error).abs() < 1e-12);
        std::mem::forget(cfg); // no comparisons run; nothing for `Drop` to check
    }

    /// Two things a reader must never mistake for a clean "no change": a run
    /// that was cut off by the budget, and one that had no threshold at all.
    #[test]
    fn display_marks_a_truncated_run_and_an_unplanned_one() {
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

        // No plan: not a verdict, and it must not read like one.
        let unplanned = format!("{}", comparison(100.0, 100.0, 0.1, 0));
        assert!(unplanned.contains("no verdict"), "{unplanned}");
        assert!(!unplanned.contains("unchanged"), "{unplanned}");
    }

    /// With no plan set there is no threshold, so nothing is ever a change -
    /// but the loop must still terminate promptly rather than spending the
    /// whole budget discovering that.
    #[test]
    fn an_unplanned_comparison_terminates_and_reports_nothing() {
        let cfg = Config::default().with_max_time(Duration::from_secs(5));
        // Claim the CPU before starting the clock, so what is timed is the
        // comparison and not our wait for other tests to finish with it. The
        // claim is re-entrant, so the comparison's own costs nothing.
        let _held = crate::quiet::exclusive();
        let started = Instant::now();
        let c = cfg.compare(|| 1u64, || 1u64);
        let elapsed = started.elapsed();
        println!("unplanned: {c} in {elapsed:?}");
        assert!(!c.is_changed(), "no plan means no verdict");
        let shown = format!("{c}");
        assert!(shown.contains("no verdict"), "{shown}");
        assert!(
            !shown.contains("unchanged"),
            "must not read as a measured result: {shown}"
        );
        assert!(
            elapsed < Duration::from_secs(4),
            "should not burn the budget: took {elapsed:?}"
        );
        std::mem::forget(cfg); // Drop would rightly assert; not what this tests.
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
            let cfg = Config::relative(0.05)
                .with_max_time(Duration::from_secs(4))
                .with_comparisons_planned(REPEATS);
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
