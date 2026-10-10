//! What a measurement comes back as, [`Timing`], and the timed inner loop
//! every benchmark is measured with.
//!
//! The sampling loop that decides when a measurement is good enough is
//! [`InputGroup::run`](crate::InputGroup), which every non-scaling benchmark
//! goes through, alone or in a group; see [`Config`] for the accuracy target
//! it stops at and [`Timing`] for what comes back.

use super::*;
use std::fmt::{self, Formatter};
use std::hint::black_box;
use std::time::Instant;

/// A benchmark's measured timing.
#[derive(Debug, PartialEq, Clone, Copy)]
#[non_exhaustive]
pub struct Timing {
    /// The time, in nanoseconds, per iteration.
    pub ns_per_iter: f64,
    /// Standard error of `ns_per_iter`, in nanoseconds - the figure shown
    /// after the `±`.
    ///
    /// This is the standard error *of sampling* seen within this call: the
    /// benchmark's own run-to-run variability plus timer noise. It is *not*
    /// a bound on systematic differences between separate process runs —
    /// CPU frequency state, code layout, and cache/allocator state shift
    /// between runs, and no statistic computed inside one call can see
    /// them. A very small `std_error` on a deterministic benchmark means
    /// sampling is no longer the limit, not that the number is accurate to
    /// that many digits. `NaN` when fewer than 2 samples were collected
    /// (see `hit_limit`), where a standard error cannot exist.
    ///
    /// [`Timing::rel_std_error`] gives the same figure as a fraction.
    pub std_error: f64,
    /// How many times the benchmarked code was actually run, including the
    /// calibration probes whose timings were discarded.
    ///
    /// `u64` rather than `usize` because this is a count rather than
    /// anything memory-sized: a nanosecond-scale benchmark can legitimately
    /// run past 4.3 billion iterations within its time budget, which a
    /// 32-bit `usize` could not hold.
    pub iterations: u64,
    /// How many samples were taken: one for each round of measuring, which
    /// times many iterations in laps. [`Timing::iterations`] says how many
    /// ran in all.
    pub samples: usize,
    /// `true` if the benchmark ran out of time before reaching its accuracy
    /// target: the answer is real, just less precise than you asked for.
    ///
    /// Look at `std_error` to see how much less. Distinct from
    /// [`Timing::untrustworthy`], which is about whether to believe
    /// `std_error` in the first place.
    pub hit_limit: bool,
    /// `true` if too few samples were collected for the standard error
    /// itself to be worth believing.
    ///
    /// A standard error estimated from two or three samples is so noisy
    /// that it may look small purely by luck, so this says "the `±` on this
    /// line is not evidence of anything", regardless of how tight it
    /// appears. It is a different question from `hit_limit`: a slow
    /// function on a short budget sets both, but a fast noisy one that
    /// simply needed longer sets only `hit_limit`, and its error bar is
    /// perfectly believable - just wider than requested.
    pub untrustworthy: bool,
    /// Read through [`Timing::difference`].
    pub(crate) difference: Option<Difference>,
}

impl Timing {
    /// Standard error as a fraction of the measurement (0.01 = 1%).
    ///
    /// `NaN` when [`Timing::std_error`] is, and also when `ns_per_iter` is
    /// zero, where a relative error is undefined.
    pub fn rel_std_error(&self) -> f64 {
        self.std_error / self.ns_per_iter
    }

    /// The difference from the baseline, if this is not the baseline itself.
    pub fn difference(&self) -> Option<&Difference> {
        self.difference.as_ref()
    }

    /// Whether this timing differs significantly from its baseline.
    pub fn is_changed(&self) -> bool {
        self.difference.as_ref().is_some_and(Difference::is_changed)
    }
}

impl Timing {
    pub(crate) fn write_measurement(&self, f: &mut Formatter) -> fmt::Result {
        // Report the error bar in the *same* unit as the measurement, even
        // when that means leading zeroes. The point of an error bar is to
        // let a reader tell at a glance whether two results differ by more
        // than their uncertainty, and that is a direct digit-for-digit
        // comparison when the units match - whereas "100.2673ms ± 20.05µs"
        // makes them do a unit conversion in their head first, and
        // "± 0.02%" makes them do arithmetic.
        let (div, unit) = unit_for(self.ns_per_iter);
        // Two separate things can be wrong with a line, so they get two
        // separate marks: `(limit)` means the answer is less precise than
        // requested, `(untrusted)` means the `±` itself is not worth
        // reading. A slow function on a short budget earns both.
        let limit = match (self.hit_limit, self.untrustworthy) {
            (true, true) => " (limit, untrusted)",
            (true, false) => " (limit)",
            (false, true) => " (untrusted)",
            (false, false) => "",
        };
        if self.std_error.is_nan() {
            // `Running::mean_and_stderr` gives NaN for exactly one reason:
            // fewer than two samples to estimate a standard error from,
            // only possible via the single-sample "blew the whole time
            // budget already" path.
            //
            // With no error bar there is nothing to set the precision, so
            // fall back to a fixed four decimals. A precision asked for by
            // the formatter is extra digits beyond what the error justifies,
            // so it has nothing to add to here.
            let value = format!("{:.4}{}", self.ns_per_iter / div, unit);
            write!(
                f,
                "{value} (± unknown, only {} sample{}){limit}",
                self.samples,
                if self.samples == 1 { "" } else { "s" }
            )
        } else {
            let (value, error) =
                value_and_error(self.ns_per_iter / div, self.std_error / div, f.precision());
            let value = format!("{value}{unit}");
            let error = format!("{error}{unit}");
            // Deliberately no iteration or sample count. Those were worth
            // showing when the only quality signal was an R², which says
            // nothing about how well the answer is known; now that the `±`
            // states the precision outright they are just noise on a line
            // meant to be scanned in a column. Both remain on [`Timing`] for
            // anyone who wants them.
            write!(f, "{value} ± {error}{limit}")
        }
    }
}

/// Call `f` `n` times over `xs`, and say how long that took in nanoseconds.
///
/// Only the calls are timed. [`crate::InputGroup`] prepares one batch of
/// inputs and then hands the same batch - cloned - to each alternative in
/// turn, so generating the inputs and timing the calls happen in different
/// places, and the inputs are dropped only after the clock has stopped.
///
/// With at least `n` inputs each call gets one of its own. With fewer, the
/// calls go round them again from the start - which is what an alternative
/// that leaves its input as it found it wants, and no other does. See
/// [`run_calls`].
pub(crate) fn time_loop<F, I, O>(f: &mut F, xs: &mut [I], n: usize) -> f64
where
    F: FnMut(&mut I) -> O,
{
    let start = Instant::now();
    run_calls(f, xs, n);
    start.elapsed().as_secs_f64() * 1e9
}

/// Call `f` `n` times over `xs`, in order, going back to the start of `xs`
/// whenever it runs out.
///
/// We iterate over `&mut *xs` rather than draining it, because we don't
/// want to drop the input values until after the clock has stopped. The
/// wrap is a loop around loops, not a remainder in the call: asking each call
/// which input is next would be a cost the function being timed does not have.
fn run_calls<F, I, O>(f: &mut F, xs: &mut [I], n: usize)
where
    F: FnMut(&mut I) -> O,
{
    let mut left = n;
    while left > 0 {
        let take = left.min(xs.len());
        assert!(take > 0, "calls to make, and no inputs to make them on");
        for x in &mut xs[..take] {
            black_box(f(x));
        }
        left -= take;
    }
}

/// Run `f` over `xs` as consecutive laps of `laps[j]` calls each, and say
/// how long each lap took in nanoseconds.
///
/// The clock is read between laps and nothing else happens there: the
/// readings are kept as [`Instant`]s and turned into durations only after
/// the last lap. Whatever a reading costs, or sets off, is a fixed cost
/// added to each lap, which is why the sampling loop subtracts a short lap
/// from a long one rather than trusting either alone. A lap of zero calls
/// is allowed and comes back as (nearly) zero.
///
/// With at least as many inputs in `xs` as the laps use in all, each call has
/// one of its own, and a lap picks up where the one before left off. With
/// fewer, every lap starts again at the first input and goes round `xs` as
/// often as it needs: the inputs are a pool, which only a function that
/// leaves its input as it found it can be timed on.
pub(crate) fn time_laps<F, I, O>(f: &mut F, xs: &mut [I], laps: [usize; 3]) -> [f64; 3]
where
    F: FnMut(&mut I) -> O,
{
    let own = xs.len() >= laps.iter().sum::<usize>();
    let mut marks = [Instant::now(); 4];
    let mut at = 0;
    for (j, &n) in laps.iter().enumerate() {
        if own {
            for x in &mut xs[at..at + n] {
                black_box(f(x));
            }
            at += n;
        } else {
            run_calls(f, xs, n);
        }
        marks[j + 1] = Instant::now();
    }
    let lap = |j: usize| (marks[j + 1] - marks[j]).as_secs_f64() * 1e9;
    [lap(0), lap(1), lap(2)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;
    use std::thread;
    use std::time::Duration;

    /// Measure `f` the way every benchmark is measured: as one in a suite,
    /// alone.
    fn bench<O>(cfg: &Config, f: impl FnMut() -> O + 'static) -> Timing {
        let mut suite = cfg.suite();
        suite.add("bench", f);
        suite.run().timing("bench").expect("it was measured")
    }

    // Cheap deterministic PRNG so a failure reproduces from its seed.

    /// A benchmark with real, injected variance (coefficient of variation
    /// around 50%): a randomized amount of trivial work. Deterministic
    /// functions aren't a good fit for testing `rel_std_error`'s honesty -
    /// their spread is machine noise, not something the estimator controls -
    /// so these accuracy tests need randomized cost instead.
    fn variable_cost(seed: u64) -> impl FnMut() -> u64 {
        let mut rng = XorShift(seed | 1);
        move || {
            let n = 1 + (rng.next() % 2000) as usize;
            let mut acc = 0u64;
            for i in 0..n {
                acc = acc.wrapping_mul(31).wrapping_add(i as u64);
            }
            acc
        }
    }

    /// Distinct, well-spread seeds so repeats are independent. Reusing one
    /// seed across repeats would replay the same random sequence and
    /// understate the very spread these tests measure.
    fn seed_for(repeat: usize) -> u64 {
        0x9e37_79b9_7f4a_7c15u64.wrapping_mul(repeat as u64 + 1) | 1
    }

    /// Whether this machine is quiesced enough for a calibration check to
    /// mean anything.
    ///
    /// The tests that compare a claimed error bar against the spread
    /// actually observed across repeated runs are measuring *between-run*
    /// variation, which is precisely what no within-run statistic can see -
    /// it is the documented limit of `std_error`. On a machine that is
    /// merely idle rather than reserved, that variation swamps what is
    /// being tested: measured 12.2x on the flat check and 5.2x on the
    /// scaling one, against 1.25x when pinned. So these skip rather than
    /// fail, and are exercised by running the suite under
    /// `quiet-bench run` - which is what `quiet-bench` is for.

    #[test]
    fn accuracy_matches_the_request() {
        println!();
        if !quiesced() {
            println!("SKIPPED: machine is not quiesced (see `quiet-bench reserve`)");
            return;
        }
        const REPEATS: usize = 15;
        for &target in &[0.05, 0.01] {
            let cfg = Config::relative(target);
            let estimates: Vec<f64> = (0..REPEATS)
                .map(|r| bench(&cfg, variable_cost(seed_for(r))).ns_per_iter)
                .collect();
            let (_, observed) = mean_and_spread(&estimates);
            println!(
                "target {:.1}% -> observed run-to-run spread {:.2}%",
                100.0 * target,
                100.0 * observed
            );
            // A standard deviation estimated from only REPEATS runs is
            // itself noisy, so this leaves generous room - the point is to
            // catch a stopping rule that has become decorative (as the old
            // R²-based one was), not to pin down the constant precisely.
            assert!(
                observed < target * 4.0,
                "asked for {:.2}% accuracy but observed spread was {:.2}%",
                100.0 * target,
                100.0 * observed
            );
        }
    }

    #[test]
    fn tighter_target_costs_more_and_is_more_precise() {
        println!();
        if !quiesced() {
            println!("SKIPPED: machine is not quiesced (see `quiet-bench reserve`)");
            return;
        }
        const REPEATS: usize = 10;
        // A larger gap keeps the test meaningful: a small gap often stops at the
        // same floor for both targets.
        let loose: Vec<Timing> = (0..REPEATS)
            .map(|r| bench(&Config::relative(0.05), variable_cost(seed_for(r))))
            .collect();
        let tight: Vec<Timing> = (0..REPEATS)
            .map(|r| bench(&Config::relative(0.003), variable_cost(seed_for(r))))
            .collect();
        let iters = |v: &[Timing]| v.iter().map(|s| s.iterations).sum::<u64>();
        let (loose_iters, tight_iters) = (iters(&loose), iters(&tight));
        println!("loose iterations {loose_iters}, tight iterations {tight_iters}");
        assert!(tight_iters > 2 * loose_iters);

        let spread =
            |v: &[Timing]| mean_and_spread(&v.iter().map(|s| s.ns_per_iter).collect::<Vec<_>>()).1;
        let (loose_spread, tight_spread) = (spread(&loose), spread(&tight));
        println!(
            "loose spread {:.2}%, tight spread {:.2}%",
            100.0 * loose_spread,
            100.0 * tight_spread
        );
        assert!(tight_spread < loose_spread);
    }

    #[test]
    fn a_zero_standard_error_meets_any_target() {
        // A benchmark optimised away entirely measures identically every
        // time, so its standard error is exactly zero and no further
        // sampling can improve it. Both variants have to accept that.
        // Deciding this by dividing the error by the (also zero) mean gave
        // NaN, which compares false against everything, so such a benchmark
        // could never stop voluntarily and burned its whole budget on every
        // run - under `Relative` just as much as `Absolute`.
        assert!(Config::relative(0.01).accuracy_met(0.0, 0.0));
        assert!(Config::absolute(Duration::from_nanos(50)).accuracy_met(0.0, 0.0));

        // A real error still has to clear the bar, either way round.
        assert!(!Config::relative(0.01).accuracy_met(100.0, 5.0));
        assert!(Config::relative(0.01).accuracy_met(100.0, 0.5));
        assert!(!Config::absolute(Duration::from_nanos(1))
            .with_relative_error(0.0)
            .accuracy_met(100.0, 5.0));
        assert!(Config::absolute(Duration::from_nanos(10))
            .with_relative_error(0.0)
            .accuracy_met(100.0, 5.0));

        // The two goals are independent, and the coarser one wins: a 1%
        // goal on a 100ns measurement wants the error under 1ns, but a
        // 5ns absolute floor says 5ns is close enough, so it stops.
        assert!(Config::relative(0.01)
            .with_absolute_error(Duration::from_nanos(5))
            .accuracy_met(100.0, 4.0));
    }

    #[test]
    fn display_reports_an_absolute_error_in_the_value_s_own_unit() {
        let shown = |ns: f64, rel: f64| {
            format!(
                "{}",
                Timing {
                    ns_per_iter: ns,
                    std_error: ns * rel,
                    iterations: 10,
                    samples: 6,
                    hit_limit: false,
                    untrustworthy: false,
                    difference: None,
                }
            )
            .trim_start()
            .to_string()
        };

        // A sub-nanosecond error bar has to survive: it is the ordinary case
        // for a fast function, and formatting via `Duration` (which has
        // nanosecond resolution) would round it away to `0ns`. The value is
        // shown to the same two decimals as the error, not to more: `71.0000`
        // would be claiming four digits the measurement does not support.
        assert_eq!(shown(71.0, 0.0017), "71.00ns ± 0.12ns");

        // Both sides in the same unit, so two results can be compared digit
        // for digit without a unit conversion in the reader's head - and to
        // the same precision, so every digit printed is one the measurement
        // actually justifies.
        assert_eq!(shown(100_267_300.0, 0.0002), "100.27ms ± 0.02ms");

        // One significant digit is all an error bar deserves, whatever its
        // magnitude relative to the value; the error is rounded to the
        // nearest digit, but is written with a second one only when its first
        // digit would otherwise be a 1.
        assert_eq!(shown(2_500.0, 0.032), "2.50µs ± 0.08µs");

        // Even an error far below the value's own unit keeps its digit
        // rather than collapsing to zero - and here that does mean three
        // decimals on the value, because the error genuinely reaches them.
        assert_eq!(shown(0.4523, 0.02), "0.452ns ± 0.009ns");

        // An error large enough to need no decimals says `± 25ns`, not
        // `± 25.0ns`, which would be a digit the measurement cannot support.
        assert_eq!(shown(500.0, 0.05), "500ns ± 25ns");
    }

    #[test]
    fn an_absolute_accuracy_target_is_honoured() {
        println!();
        // Only the absolute goal: the relative one is disabled, since
        // sampling stops at whichever goal is coarser and the 1% default
        // would otherwise govern for a workload of this size.
        let only_absolute =
            |ns| Config::absolute(Duration::from_nanos(ns)).with_relative_error(0.0);
        // 25ns, not the 5ns this used to ask for. `variable_cost` has a
        // coefficient of variation around 50%, so the standard error falls
        // as the square root of the sample count and the last factor of two
        // costs four times what the one before it did. Swept on one machine
        // against the default budget:
        //
        //   target      se reached   iterations
        //        5ns      6.975ns      912848 (limit)
        //       10ns     10.000ns      419017
        //       25ns     24.995ns       62929
        //      100ns     99.886ns        4873
        //      500ns    499.284ns         169
        //
        // 5ns was not reachable there at all, and was only ever reached on
        // a machine fast enough to buy it - so the test passed or failed on
        // the hardware rather than on the library, which is what it went on
        // doing, about half the time, on CI. 25ns costs a fourteenth of the
        // iterations the budget demonstrably supports, so what is being
        // tested is that the target governs sampling, not that this
        // particular machine is quick.
        let stats = bench(&only_absolute(25), variable_cost(7));
        println!("absolute 25ns: {stats}");
        assert!(!stats.hit_limit, "should have reached +-25ns in the budget");
        assert!(
            stats.std_error < 25.0,
            "asked for +-25ns, got +-{:.2}ns",
            stats.std_error
        );

        // A tighter target should cost more; the floor means the 25ns case is not
        // comparable to an even larger threshold that stops at the same floor.
        let dear = bench(&only_absolute(5), variable_cost(7));
        println!("absolute 5ns: {dear}");
        assert!(
            dear.iterations > stats.iterations,
            "tight target used {} iterations, looser used {}",
            dear.iterations,
            stats.iterations
        );
    }

    #[test]
    fn a_slow_function_on_a_short_budget_still_gets_an_error_bar() {
        println!();
        // Short budgets can stop before the minimum sample count; the error bar
        // should still be reported rather than turning into NaN. A 100ms call
        // costs two calls to calibrate (one of them untimed) and one a round,
        // a call that long needing no warm-up, so this budget buys a few
        // rounds and not the eight a stop would need.
        let cfg = Config::default().with_max_time(Duration::from_millis(500));
        let stats = bench(&cfg, || thread::sleep(Duration::from_millis(100)));
        println!("{stats}");
        assert!(
            stats.samples >= 2 && stats.samples < crate::input_group::MIN_SAMPLES,
            "expected to stop short of MIN_SAMPLES, got {} samples",
            stats.samples
        );
        assert!(
            !stats.std_error.is_nan(),
            "a standard error exists from {} samples and should be reported",
            stats.samples
        );
        // The run hit the budget and the sample count is too small to trust the
        // bar, but the reported error is still honest.
        assert!(stats.hit_limit);
        assert!(stats.untrustworthy);
        assert!(stats.ns_per_iter > 99.0e6);
    }

    #[test]
    fn unreachable_target_is_flagged() {
        println!();
        // An impossible target plus a short budget should be reported as a short
        // run, not a confident result. Short, but long enough for a good number
        // of rounds: each one times eleven units of laps.
        let cfg = Config::relative(1e-9).with_max_time(Duration::from_millis(300));
        let stats = bench(&cfg, variable_cost(1));
        println!("{stats}");
        assert!(stats.hit_limit);
        assert!(!cfg.accuracy_met(stats.ns_per_iter, stats.std_error));
        // Plenty of samples were collected; the budget simply ran out before the
        // target was reached.
        assert!(!stats.untrustworthy);
    }

    #[test]
    fn reported_error_is_honest() {
        println!();
        if !quiesced() {
            println!("SKIPPED: machine is not quiesced (see `quiet-bench reserve`)");
            return;
        }
        const REPEATS: usize = 40;
        for &target in &[0.05, 0.02, 0.01] {
            let cfg = Config::relative(target);
            let stats: Vec<Timing> = (0..REPEATS)
                .map(|r| bench(&cfg, variable_cost(seed_for(r))))
                .collect();
            let claimed = stats.iter().map(|s| s.rel_std_error()).sum::<f64>() / REPEATS as f64;
            let (_, observed) =
                mean_and_spread(&stats.iter().map(|s| s.ns_per_iter).collect::<Vec<_>>());
            let ratio = observed / claimed;
            println!(
                "target {:.1}%: claimed {:.2}%, observed {:.2}%, ratio {:.2}x",
                100.0 * target,
                100.0 * claimed,
                100.0 * observed,
                ratio
            );
            // The reported error should track the observed spread rather than just
            // shrinking to the requested target.
            assert!(
                ratio < 3.0,
                "claimed {:.2}% but observed spread was {:.2}% ({:.1}x overconfident)",
                100.0 * claimed,
                100.0 * observed,
                ratio
            );
        }
    }
}
