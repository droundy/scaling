//! From per-round times to an answer, its error bar, and when to stop.
//!
//! A round gives each alternative of a group one time per iteration. What
//! this module does with a series of those, one per round, is deliberately
//! simple:
//!
//! - An alternative's time is a **trimmed mean** of its rounds, and a
//!   comparison with the baseline is a trimmed mean of the **log of their
//!   ratio in each round**. Taking the ratio within a round is what lets a
//!   machine that speeds up or slows down cancel out of it: both alternatives
//!   met the same machine.
//! - Its error bar is the **standard error over rounds** (see [`trimmed`]).
//!   Rounds of one group are far apart in time compared with how long a
//!   sample takes, and measured to be uncorrelated, so no allowance for
//!   correlation between them is made.
//! - A comparison is called a change by **Student's t** at the Bonferroni
//!   level of its family, because a standard error estimated from a handful
//!   of rounds is itself uncertain, and at the tail probabilities a large
//!   family needs that is a large difference ([`limit`]).
//!
//! Everything here is a function of slices of `f64`, with no knowledge of
//! how they were measured.

use super::*;

/// The fraction trimmed from each end of the per-round values.
///
/// Something occasionally interrupts a sample - a scheduler tick, another
/// process, the thread moving core - and it always makes the sample slower.
/// A trimmed mean ignores those few without having to recognise them. A
/// quarter from each end was as good as any of the amounts tried (none, a
/// tenth, a quarter, two fifths) and still averages over half the rounds.
const TRIM: f64 = 0.25;

/// An estimate from a series of values: a trimmed mean, and how well it is
/// known.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Estimate {
    pub(crate) mean: f64,
    /// The standard error of `mean`. `NaN` with fewer than two values.
    pub(crate) std_error: f64,
    /// The degrees of freedom to judge `std_error` by: how many values the
    /// mean was taken over, less one. Zero with fewer than two values.
    pub(crate) df: f64,
}

/// The trimmed mean of `v`, and its standard error.
///
/// The standard error is the winsorised one (Tukey and McLaughlin): the
/// values trimmed off each end are replaced by the nearest value kept, so
/// that they still count as being far out without being allowed to say how
/// far. It is then the spread of that, over the root of the number of values,
/// scaled up for the fraction that was cut.
pub(crate) fn trimmed(v: &[f64]) -> Estimate {
    let n = v.len();
    if n == 0 {
        return Estimate {
            mean: f64::NAN,
            std_error: f64::NAN,
            df: 0.0,
        };
    }
    let mut s = v.to_vec();
    s.sort_by(f64::total_cmp);
    let cut = ((TRIM * n as f64) as usize).min((n - 1) / 2);
    let kept = &s[cut..n - cut];
    let mean = kept.iter().sum::<f64>() / kept.len() as f64;
    if n < 2 {
        return Estimate {
            mean,
            std_error: f64::NAN,
            df: 0.0,
        };
    }
    let (lo, hi) = (s[cut], s[n - cut - 1]);
    let winsorised_mean = s.iter().map(|x| x.clamp(lo, hi)).sum::<f64>() / n as f64;
    let winsorised_variance = s
        .iter()
        .map(|x| (x.clamp(lo, hi) - winsorised_mean).powi(2))
        .sum::<f64>()
        / (n - 1) as f64;
    let kept_fraction = kept.len() as f64 / n as f64;
    Estimate {
        mean,
        std_error: winsorised_variance.max(0.0).sqrt() / (kept_fraction * (n as f64).sqrt()),
        df: (kept.len() - 1) as f64,
    }
}

/// The log of `a`'s time over `b`'s, round by round, estimated as by
/// [`trimmed`].
///
/// A round's time is the long lap less the short one, so it is not positive
/// when something holds up the short lap for longer than the long lap's extra
/// calls take - a preemption of several milliseconds - and for a benchmark
/// whose work the optimiser deleted, whose times hover around zero. A round in
/// which only one side is not positive is an outlier by construction, and
/// counts as one: its log ratio is minus infinity if `a` is the one, plus
/// infinity if `b` is, for the trim to cut off and the winsorising to keep
/// from saying how far out it was. A round in which neither is positive says
/// nothing, and is left out.
fn log_ratio(a: &[f64], b: &[f64]) -> Estimate {
    let v: Vec<f64> = a
        .iter()
        .zip(b)
        .filter_map(|(&x, &y)| match (x > 0.0, y > 0.0) {
            (true, true) => Some((x / y).ln()),
            (false, true) => Some(f64::NEG_INFINITY),
            (true, false) => Some(f64::INFINITY),
            (false, false) => None,
        })
        .collect();
    trimmed(&v)
}

/// A candidate compared with the baseline, round by round.
pub(crate) enum Paired {
    /// The log of the ratio of their times.
    Log(Estimate),
    /// The difference of their times in nanoseconds, for times so near zero -
    /// a benchmark the optimiser deleted - that a ratio means nothing. Only
    /// its standard error and degrees of freedom are of use.
    Linear(Estimate),
}

/// Compare `candidate` with `baseline`, which are each one time per round.
///
/// By the ratio of their times, unless there are more rounds without a
/// positive time than the trim cuts off ([`log_ratio`] says what those are).
/// Then the times hover around zero, as for a benchmark whose work the
/// optimiser deleted, a ratio of them means nothing, and the whole comparison
/// is by difference instead.
pub(crate) fn paired(candidate: &[f64], baseline: &[f64]) -> Paired {
    let ratio = log_ratio(candidate, baseline);
    if ratio.df >= 1.0 && ratio.mean.is_finite() && ratio.std_error.is_finite() {
        return Paired::Log(ratio);
    }
    let difference: Vec<f64> = candidate.iter().zip(baseline).map(|(a, b)| a - b).collect();
    Paired::Linear(trimmed(&difference))
}

/// The limit a change must exceed, in standard errors, to be called one in a
/// family of `family` comparisons: Bonferroni's, by Student's t at `df`
/// degrees of freedom, and by the normal distribution when `df` says nothing
/// is known.
pub(crate) fn limit(family: u64, df: f64) -> f64 {
    let t = significant::bonferroni_t_limit(family, significant::FWER, df);
    if t.is_nan() {
        significant::bonferroni_z_limit(family, significant::FWER)
    } else {
        t
    }
}

/// One sample's time per iteration, from its laps' times in nanoseconds.
///
/// With a long lap, the long lap less the short one: each lap carries the
/// same fixed cost of being timed, and the difference has none of it. With
/// only a short lap (see [`laps::LONG_LAP_UNITS`]), that lap alone.
pub(crate) fn per_iteration(timed: [f64; 3], laps: [usize; 3]) -> f64 {
    if laps[2] > laps[1] {
        (timed[2] - timed[1]) / (laps[2] - laps[1]) as f64
    } else {
        timed[1] / laps[1] as f64
    }
}

/// Whether every number the report stands behind is known well enough, from
/// `times`, each alternative's time per iteration in every round so far.
///
/// A lone alternative: its own time, to the goal. Otherwise every comparison
/// with the baseline (`times[0]`). For one that is of interest: would a
/// change the size of the goal be detected, at the family's Bonferroni level,
/// by Student's t? That is the question [`Difference::is_changed`] asks of
/// the change actually measured, put before it is known. For one that is
/// `rough`, which is not tested for a change: is the ratio known to within
/// [`Config::with_rough_error`], one standard error either way?
pub(crate) fn all_precise(cfg: &Config, times: &[Vec<f64>], family: u64, rough: &[bool]) -> bool {
    let baseline = trimmed(&times[0]);
    if times.len() == 1 {
        return cfg.accuracy_met(baseline.mean, baseline.std_error);
    }
    let goal = if baseline.mean > 0.0 {
        (cfg.comparison_goal_ns(baseline.mean) / baseline.mean).ln_1p()
    } else {
        cfg.target_rel_error.ln_1p()
    };
    let rough_goal = cfg.target_rough_error.ln_1p();
    times[1..]
        .iter()
        .zip(&rough[1..])
        .all(
            |(candidate, &rough)| match (paired(candidate, &times[0]), rough) {
                (Paired::Log(e), true) => e.std_error <= rough_goal,
                (Paired::Log(e), false) => {
                    e.std_error == 0.0 || limit(family, e.df) * e.std_error <= goal
                }
                (Paired::Linear(e), true) => {
                    e.std_error <= cfg.target_rough_error * baseline.mean.abs()
                }
                (Paired::Linear(e), false) => {
                    cfg.comparison_accuracy_met(baseline.mean, e.std_error, limit(family, e.df))
                }
            },
        )
}
