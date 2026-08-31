//! Measuring how a benchmark's cost grows with the size of its input.
//!
//! Two stages, described in the crate docs: [`discover_sizes`] finds the
//! smallest size worth timing and how fast cost grows from there, and
//! [`choose_sizes`] and [`measure_scaling`] turn that into a ladder of
//! sizes measured until the answer is precise. The fitting below is what
//! reads an answer out of those measurements.

use super::*;
use std::fmt::{self, Display, Formatter};
use std::hint::black_box;
use std::time::{Duration, Instant};

impl Config {
    /// Benchmark the power-law scaling of a function.
    ///
    /// See [`bench_scaling`] for the default-accuracy version.
    ///
    /// The accuracy applies to [`Scaling::ns_per_scale`], the constant in
    /// front of the fitted law, and only once the law itself has been
    /// identified - see [`ScalingStats::rel_std_error`] for why those are
    /// two different questions.
    ///
    /// `target_rel_error` is the one that makes sense here.
    /// `target_abs_error` is accepted and well defined, but its units are
    /// nanoseconds per `Nᴾ`, which makes it confusing.
    pub fn bench_scaling<F, O>(&self, f: F, nmin: usize) -> ScalingStats
    where
        F: Fn(usize) -> O,
    {
        bench_scaling_with(self, f, nmin)
    }

    /// Benchmark the power-law scaling of a function with a generated input.
    ///
    /// See [`bench_scaling_gen`] for the default-accuracy version, and
    /// [`Config::bench_scaling`] for what the accuracy applies to.
    pub fn bench_scaling_gen<G, F, I, O>(&self, gen_env: G, f: F, nmin: usize) -> ScalingStats
    where
        G: FnMut(usize) -> I,
        F: Fn(&mut I) -> O,
    {
        bench_scaling_gen_with(self, gen_env, f, nmin)
    }
}

/// Statistics for a benchmark run determining the scaling of a function.
#[derive(Debug, PartialEq, Clone)]
pub struct ScalingStats {
    pub scaling: Scaling,
    /// Relative standard error of [`Scaling::ns_per_scale`], as a fraction
    /// (0.01 = 1%).
    ///
    /// **This is conditional on the reported scaling law being the right
    /// one.** It says how well the constant is known *given* that the
    /// function really is `O(Nᴾ)` for the reported `P`; it says
    /// nothing about whether that law was chosen correctly, because it is
    /// computed after the choice and cannot see the alternatives that were
    /// rejected. `goodness_of_fit` is the signal for that half - it is set
    /// to zero when the fit could not distinguish between candidate laws -
    /// so read the two together, and treat a tight error bar next to a zero
    /// `goodness_of_fit` as "precise about a shape I could not pin down".
    ///
    /// `NaN` before three samples, where a standard error cannot exist.
    pub rel_std_error: f64,
    pub goodness_of_fit: f64,
    /// How many times the benchmarked code was actually run.
    pub iterations: u64,
    /// How many samples were taken (ie. how many times we allocated the
    /// environment and measured the time).
    pub samples: usize,
    /// `true` if the benchmark ran out of time before reaching its
    /// `accuracy` target, or gave up without identifying a scaling law.
    pub hit_limit: bool,
}

impl ScalingStats {
    /// The standard error of [`Scaling::ns_per_scale`], in the same units
    /// as it - the absolute counterpart of
    /// [`ScalingStats::rel_std_error`], and what `Display` shows after the
    /// `±`. See that field for what the figure does and does not cover.
    pub fn std_error(&self) -> f64 {
        // `abs` because a fit is free to land on a negative coefficient -
        // a term the data does not really support can come out either side
        // of zero - and an error bar is a width, which has no sign.
        self.scaling.ns_per_scale.abs() * self.rel_std_error
    }
}
/// The timing and scaling results (without statistics) for a benchmark.
#[derive(Debug, PartialEq, Clone)]
pub struct Scaling {
    /// The scaling power.
    ///
    /// If this is 2, for instance, you have an `O(N²)` algorithm. Only
    /// power laws are fitted: a cost that is not one is reported as the
    /// power it most behaves like over the range measured, with
    /// [`ScalingStats::goodness_of_fit`] zeroed to say so.
    pub power: usize,
    /// The time, in nanoseconds, per scaled size of the problem. If
    /// the problem scales as O(N²) for instance, this is the number
    /// of nanoseconds per N².
    pub ns_per_scale: f64,
}

impl Display for ScalingStats {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        // Same rules as `Stats`: value and error in one unit, the error to
        // two significant figures, and the value to exactly the precision
        // the error justifies. The unit is written once, outside the
        // parentheses, since it applies to both.
        let suffix = self.scaling.scale_suffix();
        let (div, unit) = unit_for(self.scaling.ns_per_scale);
        let value = self.scaling.ns_per_scale / div;
        let limit = if self.hit_limit { " (limit)" } else { "" };
        // R² stays, unlike on `Stats`, because here it is not a stand-in
        // for precision - it is the only signal about whether the right
        // *shape* was found, which no error bar on the constant can give.
        // Zero means the fit could not tell the candidate laws apart.
        if self.std_error().is_nan() {
            // Two different things bring us here, and "take more samples" is
            // the right advice for neither. `rel_std_error` is NaN when no
            // scaling law was identified at all, which more sampling of the
            // same shape will not fix; otherwise the fitted coefficient
            // landed on exactly zero, which makes the relative error
            // infinite and the absolute one `0 * inf`. A scaling sweep
            // always takes at least two rounds across every size, so it is
            // never the sample count that is missing.
            let why = if self.rel_std_error.is_nan() {
                "no scaling law identified"
            } else {
                "the fitted coefficient is zero"
            };
            write!(
                f,
                "{value:>8.2}{unit}{suffix} (± unknown, {why} after {} samples){limit} (R²={:.3})",
                self.samples, self.goodness_of_fit
            )
        } else {
            let scaled = self.std_error() / div;
            let decimals = error_decimals(scaled);
            let error = if scaled > 0.0 && scaled < 1e-4 {
                format!("{scaled:.1e}")
            } else {
                format!("{scaled:.*}", decimals)
            };
            let shown = format!("({:.*} ± {}){}{}", decimals, value, error, unit, suffix);
            write!(f, "{shown:>22}{limit} (R²={:.3})", self.goodness_of_fit)
        }
    }
}
impl Scaling {
    /// The `/N²`-style suffix naming what `ns_per_scale` is measured per,
    /// without the number in front of it. Split out so that
    /// [`ScalingStats`] can print `(0.51 ± 0.02)ns/N` - the error belongs
    /// inside the parentheses with the value it qualifies, and the unit
    /// only wants saying once.
    fn scale_suffix(&self) -> String {
        match self.power {
            0 => "/iter".to_string(),
            1 => "/N".to_string(),
            2 => "/N²".to_string(),
            3 => "/N³".to_string(),
            4 => "/N⁴".to_string(),
            5 => "/N⁵".to_string(),
            6 => "/N⁶".to_string(),
            7 => "/N⁷".to_string(),
            8 => "/N⁸".to_string(),
            9 => "/N⁹".to_string(),
            p => format!("/N^{p}"),
        }
    }
}

impl Display for Scaling {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        // The same unit rule as [`ScalingStats`], via the same helper.
        // Formatting through `Duration`'s `Debug` instead - which is what
        // this did - reintroduces exactly the problem `unit_for` exists to
        // avoid, and left the two impls disagreeing about the unit for one
        // and the same magnitude.
        //
        // Three decimals, fixed, because there is no error bar here to set
        // the precision: this type is the answer without the statistics.
        let (div, unit) = unit_for(self.ns_per_scale);
        let value = format!("{:.3}{}", self.ns_per_scale / div, unit);
        write!(f, "{:>8}{}", value, self.scale_suffix())
    }
}

/// Benchmark the power-law scaling of the function.
///
/// Uses the default accuracy (see [`Config`]).
///
/// Reports the integer power ᴾ in 𝑶(𝑁ᴾ) and the constant in front of it,
/// with a standard error. Sizes are chosen by a first stage that climbs
/// until a single call is long enough to time; each is then measured
/// repeatedly, so the fit is judged against error bars that were measured
/// rather than assumed. A cost that is not a power law is rejected -
/// `goodness_of_fit` zeroed and `hit_limit` set - rather than fitted anyway.
/// Takes around 10s by default; see [`Config::max_time`].
///
/// # Choosing `nmin`
///
/// Sizes are measured in multiples of `nmin`, and nothing smaller is tried.
/// That matters more than it looks, because many costs simply do not behave
/// the same way at small sizes: a vector that fits in cache is a different
/// machine from one that does not, and measuring across the boundary fits a
/// curve to two regimes at once.
///
/// Summing a vector shows this plainly. Run from `nmin` of 1 it is not a
/// power law at all - it is rejected on every run, its constant moves by
/// tens of percent between runs, and the reported `±` understates that by a
/// factor of fifty. Raising `nmin` until the smallest size is already out
/// of cache fixes it:
///
/// ```none
/// nmin           reported   spread over 8 runs   claimed +-
///       1     0.303 ns/N          39.5%              0.74%
///   1_000     0.371 ns/N           8.9%              0.60%
/// 100_000     0.423 ns/N           2.8%              0.73%
/// 1_000_000   0.686 ns/N           0.7%              0.58%
/// ```
///
/// Note that the answer *changes*, and is not converging on a mistake being
/// corrected: per-element cost really is higher once the vector no longer
/// fits in cache. There is no single true number here, only one per regime,
/// and `nmin` is how you say which regime you meant. If a scaling result
/// comes back flagged, an `nmin` above wherever your workload changes
/// character is the first thing to try.
///
/// See [`Config::bench_scaling`] to choose your own accuracy.
pub fn bench_scaling<F, O>(f: F, nmin: usize) -> ScalingStats
where
    F: Fn(usize) -> O,
{
    Config::default().bench_scaling(f, nmin)
}

fn bench_scaling_with<F, O>(cfg: &Config, f: F, nmin: usize) -> ScalingStats
where
    F: Fn(usize) -> O,
{
    quiet::pin_if_requested();
    scaling_sweep(cfg, nmin, |n| {
        // `black_box` on the size as well as the result: without it the
        // optimiser can see a literal `n` and lift the whole call out, the
        // job the old code did by running over a `vec![n; iters]`.
        let n = black_box(n);
        let start = Instant::now();
        black_box(f(n));
        start.elapsed().as_secs_f64() * 1e9
    })
}

/// Run both stages and turn the result into a [`ScalingStats`].
///
/// `measure(n)` performs one call at size `n` and returns its cost in
/// nanoseconds; the two entry points differ only in how they build it.
///
/// The budget splits between the stages rather than being shared: stage one
/// gets [`DISCOVERY_SHARE`] to find the sizes and stage two gets what is
/// left to measure them properly. Keeping them separate means a slow
/// discovery cannot starve the measurement it exists to set up.
fn scaling_sweep(
    cfg: &Config,
    nmin: usize,
    mut measure: impl FnMut(usize) -> f64,
) -> ScalingStats {
    let mut calls = 0u64;
    let mut counted = |n: usize| {
        calls += 1;
        measure(n)
    };

    let range = discover_sizes(nmin, cfg.max_time.mul_f64(DISCOVERY_SHARE), &mut counted);
    let sizes = choose_sizes(range, nmin);

    // Each degree costs a size, and a fit needs more sizes than terms.
    let max_degree = MAX_DEGREE.min(sizes.len().saturating_sub(2));
    let measured = measure_scaling(
        &sizes,
        cfg,
        cfg.max_time.mul_f64(1.0 - DISCOVERY_SHARE),
        max_degree,
        &mut counted,
    );

    // One call per sample, so these two counts are the same number. Both
    // are kept because they answer different questions for a caller, and
    // because the plain-`bench` side of the crate still distinguishes them.
    let samples = calls as usize;
    let Some(fit) = measured.fit else {
        // No degree cleared its own error bar - the cost did not measurably
        // grow, and did not measurably do anything else either.
        return ScalingStats {
            scaling: Scaling {
                power: 0,
                    ns_per_scale: 0.0,
            },
            rel_std_error: f64::NAN,
            goodness_of_fit: 0.0,
            iterations: calls,
            samples,
            hit_limit: true,
        };
    };
    // A polynomial that misses by far more than the error bars allow has
    // not identified anything, whatever its coefficients came out as, so it
    // says so in the way callers already understand: a zeroed
    // `goodness_of_fit`, and the limit flag set.
    // No special case for a rejected fit: the power came from the log-log
    // slope either way, which is the exponent the cost behaves like over
    // the range measured whether or not a polynomial describes it exactly.
    // Rejection changes what we claim, not what we measured.
    let rejected = !(fit.chi2_per_dof <= CHI2_REJECT);
    ScalingStats {
        scaling: Scaling {
            power: fit.power,
            ns_per_scale: fit.ns_per_scale,
        },
        rel_std_error: fit.std_error / fit.ns_per_scale.abs(),
        // R² is the share of the spread a model accounts for, and a
        // constant model accounts for none of it by construction - the fit
        // *is* the mean, so R² is identically zero however well it
        // describes the data. Reporting that zero would claim the shape was
        // unidentified when it was in fact settled, so a constant that
        // survived the chi-squared test is reported as the perfect fit it is.
        goodness_of_fit: if rejected {
            0.0
        } else if fit.power == 0 {
            1.0
        } else {
            fit.r2.max(0.0)
        },
        iterations: calls,
        samples,
        hit_limit: measured.hit_limit || rejected,
    }
}


/// Benchmark the power-law scaling of the function with generated input
///
/// This function is like [`bench_scaling`], but uses a generating function
/// to construct the input to your benchmarked function.
///
/// Reports the integer power ᴾ in 𝑶(𝑁ᴾ) and the constant in front of it,
/// with a standard error. Sizes are chosen by a first stage that climbs
/// until a single call is long enough to time; each is then measured
/// repeatedly, so the fit is judged against error bars that were measured
/// rather than assumed. A cost that is not a power law is rejected -
/// `goodness_of_fit` zeroed and `hit_limit` set - rather than fitted anyway.
/// Takes around 10s by default; see [`Config::max_time`].
///
/// # Choosing `nmin`
///
/// Sizes are measured in multiples of `nmin`, and nothing smaller is tried.
/// That matters more than it looks, because many costs simply do not behave
/// the same way at small sizes: a vector that fits in cache is a different
/// machine from one that does not, and measuring across the boundary fits a
/// curve to two regimes at once.
///
/// Summing a vector shows this plainly. Run from `nmin` of 1 it is not a
/// power law at all - it is rejected on every run, its constant moves by
/// tens of percent between runs, and the reported `±` understates that by a
/// factor of fifty. Raising `nmin` until the smallest size is already out
/// of cache fixes it:
///
/// ```none
/// nmin           reported   spread over 8 runs   claimed +-
///       1     0.303 ns/N          39.5%              0.74%
///   1_000     0.371 ns/N           8.9%              0.60%
/// 100_000     0.423 ns/N           2.8%              0.73%
/// 1_000_000   0.686 ns/N           0.7%              0.58%
/// ```
///
/// Note that the answer *changes*, and is not converging on a mistake being
/// corrected: per-element cost really is higher once the vector no longer
/// fits in cache. There is no single true number here, only one per regime,
/// and `nmin` is how you say which regime you meant. If a scaling result
/// comes back flagged, an `nmin` above wherever your workload changes
/// character is the first thing to try.
///
/// # Example
/// ```
/// use scaling::bench_scaling_gen;
///
/// let summation = bench_scaling_gen(|n| vec![3.0; n], |v| v.iter().cloned().sum::<f64>(),0);
/// println!("summation: {}", summation);
/// assert_eq!(1, summation.scaling.power); // summation must run in linear time.
/// ```
/// which gives output
/// ```none
/// summation:    (1.206 ± 0.011)ns/N (R²=0.999)
/// ```
///
/// See [`Config::bench_scaling_gen`] to choose your own accuracy.
pub fn bench_scaling_gen<G, F, I, O>(gen_env: G, f: F, nmin: usize) -> ScalingStats
where
    G: FnMut(usize) -> I,
    F: Fn(&mut I) -> O,
{
    Config::default().bench_scaling_gen(gen_env, f, nmin)
}

fn bench_scaling_gen_with<G, F, I, O>(
    cfg: &Config,
    mut gen_env: G,
    f: F,
    nmin: usize,
) -> ScalingStats
where
    G: FnMut(usize) -> I,
    F: Fn(&mut I) -> O,
{
    quiet::pin_if_requested();
    scaling_sweep(cfg, nmin, |n| {
        // Build the environment before the clock starts and drop it after
        // the clock stops, so neither generation nor drop lands in the
        // measurement.
        let mut x = gen_env(n);
        let start = Instant::now();
        black_box(f(&mut x));
        let elapsed = start.elapsed();
        drop(x);
        elapsed.as_secs_f64() * 1e9
    })
}


// The polynomial fit below is validated against synthetic data in
// `tests::fitting`, but does not yet drive `compute_scaling_gen`: wiring it
// in changes what gets reported for real workloads (notably `N log N`, which
// is not a polynomial at all), so it lands separately from the machinery.


/// A polynomial fit against sizes whose error bars were *measured* rather
/// than inferred.
#[derive(Debug, Clone, PartialEq)]
struct PolyFit {
    /// Coefficient of each power, `coefficients[k]` multiplying `Nᵏ`.
    coefficients: Vec<f64>,
    /// Standard error of each coefficient.
    ses: Vec<f64>,
    /// Chi-squared per degree of freedom: near 1 when this polynomial
    /// describes the measurements within their own error bars, large when
    /// it does not.
    chi2_per_dof: f64,
    /// Weighted R²: the share of the spread in the measurements that this
    /// polynomial accounts for.
    r2: f64,
}

/// Fit `t = c₀ + c₁N + ... + c_degree·N^degree` to sizes measured with
/// known standard errors.
///
/// Differs from a plain weighted least squares in where the coefficient
/// errors come from. When the point errors are only *assumed*, the usual
/// move is to scale the covariance by the residual variance - the fit
/// grades its own uncertainty from how well it happened to fit. Here each
/// `se` was measured by repeating the benchmark at that size, so the
/// covariance is `(XᵀWX)⁻¹` outright with `W = 1/se²`, and no rescaling is
/// wanted.
///
/// That separation is the point: the coefficient errors then say how well
/// the coefficients are known, and `chi2_per_dof` independently says
/// whether the model fits at all. Conflated, a bad model quietly inflates
/// the error bars instead of admitting it is the wrong shape.
fn weighted_poly_fit(ns: &[f64], means: &[f64], ses: &[f64], degree: usize) -> Option<PolyFit> {
    let terms = degree + 1;
    let n = ns.len();
    if n <= terms {
        return None;
    }
    let scale = ns.iter().cloned().fold(0.0, f64::max);
    if !scale.is_finite() || scale <= 0.0 {
        return None;
    }
    let mut ws = Vec::with_capacity(n);
    for &se in ses {
        if !se.is_finite() || se <= 0.0 {
            return None;
        }
        ws.push(1.0 / (se * se));
    }
    // Rescale sizes into (0, 1] before building the design: a raw
    // Vandermonde over sizes in the thousands is hopeless in f64.
    let rows: Vec<Vec<f64>> = ns
        .iter()
        .map(|&x| {
            let u = x / scale;
            (0..terms).map(|j| u.powi(j as i32)).collect()
        })
        .collect();
    let mut a = vec![vec![0.0; terms]; terms];
    let mut b = vec![0.0; terms];
    for i in 0..n {
        for j in 0..terms {
            b[j] += ws[i] * rows[i][j] * means[i];
            for k in 0..terms {
                a[j][k] += ws[i] * rows[i][j] * rows[i][k];
            }
        }
    }
    let inv = invert(&a)?;
    let coef: Vec<f64> = (0..terms)
        .map(|j| (0..terms).map(|k| inv[j][k] * b[k]).sum())
        .collect();
    let chi2: f64 = (0..n)
        .map(|i| {
            let pred: f64 = (0..terms).map(|j| coef[j] * rows[i][j]).sum();
            ws[i] * (means[i] - pred).powi(2)
        })
        .sum();
    // No residual-variance factor here - see the note above.
    //
    // A covariance diagonal is a variance, so it is positive whenever the
    // arithmetic held up; a zero or negative one means it did not, and the
    // matrix was too ill-conditioned for `invert`'s pivot test to catch.
    // Clamping such a value to zero would report the coefficient as known
    // exactly, which the refinement loop reads as "precise enough" and stops
    // on - fabricating certainty out of a numerical breakdown. An
    // unidentifiable fit says so instead, the same way `invert` does.
    let mut se = Vec::with_capacity(terms);
    for j in 0..terms {
        let var = inv[j][j];
        if !(var > 0.0) || !var.is_finite() {
            return None;
        }
        se.push(var.sqrt());
    }
    let unscale = |v: &[f64]| -> Vec<f64> {
        (0..terms).map(|j| v[j] / scale.powi(j as i32)).collect()
    };
    // Weighted R², about the weighted mean. Reported alongside chi-squared
    // rather than instead of it: R² says how much of the spread the fit
    // accounts for, which is flattering whenever the spread is large, while
    // chi-squared asks the sharper question of whether what is left over is
    // as small as the error bars say it should be.
    let wsum: f64 = ws.iter().sum();
    let wmean: f64 = (0..n).map(|i| ws[i] * means[i]).sum::<f64>() / wsum;
    let sstot: f64 = (0..n).map(|i| ws[i] * (means[i] - wmean).powi(2)).sum();
    Some(PolyFit {
        coefficients: unscale(&coef),
        ses: unscale(&se),
        chi2_per_dof: chi2 / (n - terms) as f64,
        r2: if sstot > 0.0 { 1.0 - chi2 / sstot } else { 0.0 },
    })
}

/// The highest power stage two will consider.
///
/// Cubic is already past what a benchmark can usually distinguish, and each
/// extra degree costs a size: [`weighted_poly_fit`] needs more sizes than
/// terms, so degree three needs five.
const MAX_DEGREE: usize = 3;

/// How much of the budget stage one may spend finding sizes.
///
/// Discovery is reconnaissance - every measurement it takes is a single
/// unreplicated call, useful for steering and not for the answer - so it
/// gets a minority share and stage two keeps the rest.
const DISCOVERY_SHARE: f64 = 0.25;

/// The most sizes stage one's climb will try before giving up on going
/// higher.
const MAX_CLIMB_STEPS: usize = 8;

/// How many sizes stage two measures.
///
/// More than two, because two points can be joined by a line of any shape
/// and say nothing about which; and enough that a cubic still has degrees
/// of freedom left over, so chi-squared has something to test.
const NUM_SIZES: usize = 6;

/// The least a single call may cost and still be worth measuring.
///
/// This is the floor stage one climbs to. `Instant` resolves to some tens
/// of nanoseconds, so at 20 microseconds the clock contributes on the order
/// of 0.1% - below the noise in anything worth benchmarking, and far below
/// the accuracy anyone asks for. Sizes cheaper than this do not measure the
/// function, they measure the clock.
const MIN_MEASURABLE: Duration = Duration::from_micros(20);

/// How much slower the largest size should be than the smallest.
///
/// The ladder is placed in *time*, not in size, and this is the whole of
/// the choice. Time is where the cost lives and where the accuracy comes
/// from, so it is the quantity worth being deliberate about; the size range
/// is then whatever delivers it, `TIME_SPAN^(1/p)`, which is four for a
/// linear cost, two for a quadratic one and 1.59 for a cubic. Choosing a
/// size span directly would mean choosing a different time span for every
/// benchmark without meaning to.
///
/// Four rather than two because it doubles the range for about half as much
/// again in cost: six rungs log-spaced over a time ratio `T` cost
/// `(T^(6/5) - 1)/(T^(1/5) - 1)` times the cheapest one, which is 8.7 at
/// `T = 2` and 13.4 at `T = 4`.
const TIME_SPAN: f64 = 4.0;

/// The most the sizes may span however slowly the cost grows.
///
/// A nearly flat cost would otherwise ask for an unbounded size range to
/// reach [`TIME_SPAN`], and there is a point past which a wider range
/// measures a different machine - one whose working set still fits in
/// cache - rather than a smaller version of the same one.
const MAX_SIZE_SPAN: f64 = 64.0;

/// Above this chi-squared per degree of freedom, the polynomial is not the
/// right shape and we decline to call it the scaling.
///
/// Chi-squared per degree of freedom sits near 1 when a model accounts for
/// the data to within its error bars, so the threshold belongs near 1 too:
/// with five degrees of freedom a correct model gives 1 ± 0.63, and three
/// is a comfortable three sigma above that.
///
/// It sat at ten for a while, on the reasoning that erring loose was cheap.
/// It is not cheap. Ten accepts a model wrong by an order of magnitude, and
/// what it let through was a *constant* accepted as the shape of a linear
/// cost whenever the measurements were noisy enough. That failure then
/// hides itself: the prefactor of a degree-zero fit is near enough the mean
/// of every measurement, so it looks precise immediately and the refinement
/// loop stops before collecting the data that would have shown the trend.
/// A wrong model never announces itself as imprecise, so the accuracy
/// target cannot catch it - it has to be rejected here or not at all.
///
/// Not 1 exactly, because these error bars are themselves measured from six
/// replicates and uncertain by about a third, and because timing noise is
/// not Gaussian. Three leaves room for that without leaving room for a
/// model that is simply wrong. Erring tight is also the safer direction:
/// rejecting a good fit costs a fallback to the log-log slope, which for a
/// true power law returns the same integer anyway.
const CHI2_REJECT: f64 = 3.0;

/// What stage one hands to stage two: the smallest size worth measuring,
/// what one call there costs, and how fast cost grows with size.
///
/// Deliberately a *lower* bound and a rate rather than a list of sizes.
/// Stage one takes single unreplicated measurements, which are fine for
/// steering and not fit to draw conclusions from, so it should decide as
/// little as possible - and the largest size is not its decision to make,
/// because it depends on a budget only stage two knows how it will spend.
#[derive(Debug, Clone, Copy, PartialEq)]
struct SizeRange {
    /// The floor of stage two's ladder: the *largest* size stage one tried.
    ///
    /// Largest, despite being a lower bound, because stage one climbs from
    /// below and stops at the first size worth measuring. Every size under
    /// it has been tried and found too cheap to time, so this is the only
    /// one stage one has evidence for - and being the last rung of a
    /// cautious climb, it is affordable by construction.
    lo: usize,
    /// Nanoseconds for one call at `lo`.
    ///
    /// The unit stage two budgets in: knowing what the cheapest rung costs,
    /// and how fast cost grows from it, is what turns a budget into a
    /// ladder. Stage one keeps it under a ceiling set from that budget, so
    /// a full opening round is affordable however the ladder comes out.
    lo_time: f64,
    /// Continuous exponent: cost grows about as `N^exponent`.
    ///
    /// Continuous, not rounded. Rounding here would throw away exactly the
    /// information [`choose_sizes`] needs, which is not what the answer is
    /// but how expensive a larger size will turn out to be.
    exponent: f64,
}

/// Stage one: find the smallest size worth measuring, and how fast cost
/// grows from there.
///
/// Climbs from `nmin` until one call clears [`MIN_MEASURABLE`], and stops
/// there. Every measurement here is a single call.
///
/// `budget` is a backstop against a pathological climb, not a target: the
/// climb stops at the floor, and what it costs is whatever getting there
/// cost. Nothing here is sized against the time available.
fn discover_sizes(
    nmin: usize,
    budget: Duration,
    mut measure: impl FnMut(usize) -> f64,
) -> SizeRange {
    let step = nmin.max(1);
    let budget_ns = budget.as_secs_f64() * 1e9;
    let floor_ns = MIN_MEASURABLE.as_secs_f64() * 1e9;

    let mut last_n = step;
    let mut last_t = measure(step);
    let mut spent = last_t;
    // The rung we will hand over: the largest size tried, which is the
    // first one to clear the floor. Until something clears it, the largest
    // we have is the best we can offer, however untrustworthy its timing.
    let mut lo = (last_n, last_t);
    let mut prev: Option<(f64, f64)> = None;
    let mut exponent = 1.0;
    // See `measure_scaling`: the calls are not the only thing that costs
    // time here either.
    let started = Instant::now();

    for _ in 0..MAX_CLIMB_STEPS {
        if started.elapsed().as_secs_f64() * 1e9 >= budget_ns {
            break;
        }
        if let Some((pn, pt)) = prev {
            if let Some(p) = two_point_exponent(pn, pt, last_n as f64, last_t) {
                exponent = p;
            }
        }
        // Measurable, and affordable: done climbing.
        if last_t >= floor_ns {
            lo = (last_n, last_t);
            break;
        }
        let left = Duration::from_secs_f64((budget_ns - spent).max(0.0) / 1e9);
        // Aim each step at the floor we are trying to clear; `next_size`
        // caps the growth and keeps the step affordable.
        let Some(next) = next_size(last_n as f64, last_t, exponent, MIN_MEASURABLE, left)
        else {
            break;
        };
        // Sizes are multiples of `nmin`, which is the caller's unit.
        let next = (next / step).max(1) * step;
        if next <= last_n {
            break;
        }
        let t = measure(next);
        spent += t;
        prev = Some((last_n as f64, last_t));
        last_n = next;
        last_t = t;
        lo = (last_n, last_t);
    }

    // A climb that never took a step has a floor but no growth rate, and
    // `exponent` is still the assumed 1.0. That assumption is not harmless:
    // `choose_sizes` budgets with it, so believing an N-cubed cost to be
    // linear plans a ladder whose top rung costs hundreds of times what was
    // predicted. One probe upward buys the real rate. It aims at the
    // ceiling rather than the floor, which is already behind us, and `lo`
    // does not move - the probe is reconnaissance, not a rung, and may well
    // land somewhere too expensive to be one.
    if prev.is_none() {
        let left = Duration::from_secs_f64((budget_ns - spent).max(0.0) / 1e9);
        // Aim at where the ladder's top will be - `TIME_SPAN` times the
        // cost we are at. The floor is behind us, so aiming there would ask
        // for a step backwards and get none; aiming at the top measures the
        // growth rate across exactly the range it will be used to describe.
        let aim = Duration::from_secs_f64(last_t * TIME_SPAN / 1e9);
        if let Some(next) = next_size(last_n as f64, last_t, exponent, aim, left) {
            let next = (next / step).max(1) * step;
            if next > last_n {
                let t = measure(next);
                if let Some(p) = two_point_exponent(last_n as f64, last_t, next as f64, t) {
                    exponent = p;
                }
            }
        }
    }

    SizeRange {
        lo: lo.0,
        lo_time: lo.1,
        exponent,
    }
}

/// Choose the sizes stage two will measure.
///
/// The bottom is stage one's: below [`SizeRange::lo`] we would be timing
/// the clock. The top is the only choice, and it is made in time - far
/// enough up that the largest size takes [`TIME_SPAN`] times as long as the
/// smallest, which stage one's growth rate converts into a size.
///
/// No budget is consulted. What makes a range good is that it separates the
/// powers while costing little, and both of those are settled by the
/// numbers stage one already measured; checking a budget as well would only
/// let a large one talk us into spending more than the answer needs. The
/// opening rounds come to about 80 times the cheapest call, so a benchmark
/// at the 20-microsecond floor is measured in under two milliseconds. A
/// noisy one costs more, but it costs more by *needing* more - the
/// refinement loop buys precision it has found it lacks, which is the only
/// good reason to spend longer.
///
/// Sizes come out log-spaced, the spacing that divides a range of powers
/// evenly, since equal ratios rather than equal differences are what make
/// each rung contribute comparably.
fn choose_sizes(range: SizeRange, nmin: usize) -> Vec<usize> {
    let step = nmin.max(1);
    // A cost that barely grows would need an unbounded size range to reach
    // `TIME_SPAN`; `MAX_SIZE_SPAN` is where we stop asking.
    let span = if range.exponent > 0.0 {
        TIME_SPAN.powf(1.0 / range.exponent).min(MAX_SIZE_SPAN)
    } else {
        MAX_SIZE_SPAN
    };
    let ratio = span.powf(1.0 / (NUM_SIZES - 1) as f64);

    let mut sizes: Vec<usize> = Vec::with_capacity(NUM_SIZES);
    for i in 0..NUM_SIZES {
        let x = range.lo as f64 * ratio.powi(i as i32);
        let n = ((x / step as f64).round().max(1.0) as usize).saturating_mul(step);
        // Rounding onto a multiple of `nmin` can land two rungs together;
        // keep the ladder strictly increasing rather than measuring a size
        // twice and calling the two measurements independent.
        if sizes.last() != Some(&n) {
            sizes.push(n);
        }
    }

    // Log spacing assumes there are enough distinct integers in the range
    // to land on, and near the bottom there are not: a cubic cost wants a
    // size span of only 1.59, which from `lo` of 1 rounds every rung onto
    // 1 or 2. Too few sizes to fit is the one failure with no answer at
    // all, so fall back to the densest packing there is - consecutive steps
    // up from `lo`, every one the smallest size still available.
    if sizes.len() < NUM_SIZES {
        sizes = (0..NUM_SIZES)
            .map(|i| range.lo.saturating_add(i.saturating_mul(step)))
            .collect();
    }
    sizes
}

/// How many times each size is measured before the first fit is attempted.
///
/// Every measurement here is a *single call* of the benchmarked function.
/// Nothing is batched: a benchmark worth asking about the scaling of is one
/// that gets slow as `N` grows, so where a batch would have been needed to
/// out-measure the clock, a larger `N` does the same job and tells us
/// something we wanted to know anyway. Stage one already picks sizes whose
/// single call is long enough to time, which is what makes this safe.
///
/// Not batching is also what keeps the fitted model honest. Timing a batch
/// and dividing by its length folds the fixed per-batch overhead into a
/// `c/N` term, which no polynomial in `N` can represent, so it comes out
/// smeared across every coefficient. One call per sample leaves that
/// overhead where it belongs: a constant, which the degree-zero term of
/// [`weighted_poly_fit`] represents exactly.
///
/// Six rather than three because these standard errors become the fit's
/// weights. An SE from three samples is itself uncertain by around half its
/// own size, and a badly mis-estimated weight does more damage than a
/// merely wide one; six brings that to about a third, and the loop below
/// improves it where it matters.
const INITIAL_REPEATS: usize = 6;

/// Measure the scaling of `measure` across `sizes`, refining until the
/// dominant coefficient is known to the configured relative accuracy.
///
/// `measure(n)` performs one call at size `n` and returns its cost in
/// nanoseconds. Each size is measured [`INITIAL_REPEATS`] times, giving a
/// mean and a standard error over those repeats; those feed
/// [`scaling_fit`], and if its answer is not precise enough every size is
/// measured once more. So the loop spends time only to buy precision it has
/// already found it lacks.
///
/// Only the *relative* accuracy target applies. The absolute one is a
/// `Duration`, and what stage two reports is not a duration - it is
/// nanoseconds per `N^power`, whose units change with the power that was
/// found. Comparing it against a time would be a units error that happens
/// to typecheck.
///
/// Returns the fit and whether the budget ran out first; a fit that ran out
/// of budget is still the best available answer, just not a precise one.
struct Measured {
    fit: Option<ScalingFit>,
    /// The budget ran out before the accuracy target was reached.
    hit_limit: bool,
}

fn measure_scaling(
    sizes: &[usize],
    cfg: &Config,
    budget: Duration,
    max_degree: usize,
    mut measure: impl FnMut(usize) -> f64,
) -> Measured {
    let ns: Vec<f64> = sizes.iter().map(|&n| n as f64).collect();
    let mut acc = vec![Running::default(); sizes.len()];
    let budget_ns = budget.as_secs_f64() * 1e9;
    let mut spent = 0.0;
    // Two clocks, because they measure different things and either can be
    // the binding one. `spent` adds up what the calls themselves cost,
    // which is what the accuracy is bought with; `started` is real time,
    // which also covers what `measure` does around the call - building and
    // dropping an environment, most of all, which for something like a
    // sort costs as much again as the sort does. Budgeting on `spent`
    // alone would overrun by whatever that setup costs, and would never
    // terminate at all for a benchmark whose calls measure as zero.
    let started = Instant::now();
    let over_budget =
        |spent: f64| spent >= budget_ns || started.elapsed().as_secs_f64() * 1e9 >= budget_ns;

    let mut round = |acc: &mut Vec<Running>, spent: &mut f64| {
        for (i, &n) in sizes.iter().enumerate() {
            let t = measure(n);
            acc[i].push(t);
            *spent += t;
        }
    };
    // Two rounds unconditionally, because a standard error needs two
    // points to exist at all and without one there is nothing to fit. After
    // that the budget has a say: a ladder that turned out more expensive
    // than predicted should return a poor answer on time rather than a good
    // one long after the caller stopped waiting.
    for i in 0..INITIAL_REPEATS {
        if i >= 2 && over_budget(spent) {
            break;
        }
        round(&mut acc, &mut spent);
    }

    loop {
        let mut means = Vec::with_capacity(acc.len());
        let mut ses = Vec::with_capacity(acc.len());
        for a in &acc {
            let (m, se) = a.mean_and_stderr();
            means.push(m);
            ses.push(se);
        }
        let fit = scaling_fit(&ns, &means, &ses, max_degree);
        // `map_or` rather than `is_some_and`, which needs a newer compiler
        // than the `rust-version` in `Cargo.toml` promises.
        let precise = fit.as_ref().map_or(false, |f| {
            f.std_error < cfg.target_rel_error * f.ns_per_scale.abs()
        });
        // Check the budget only after a fit that was not good enough, so a
        // benchmark that is already precise enough never reports having hit
        // a limit it did not need.
        if precise || over_budget(spent) {
            return Measured {
                fit,
                hit_limit: !precise,
            };
        }
        round(&mut acc, &mut spent);
    }
}

/// The answer stage two is working towards: which integer power dominates,
/// how big its coefficient is, and how well either is known.
#[derive(Debug, Clone, Copy, PartialEq)]
struct ScalingFit {
    /// The dominant integer power: 1 for `O(N)`, 2 for `O(N²)`.
    power: usize,
    /// Nanoseconds per `N^power`.
    ns_per_scale: f64,
    /// Standard error of `ns_per_scale`.
    std_error: f64,
    /// Chi-squared per degree of freedom for the chosen polynomial.
    chi2_per_dof: f64,
    /// Weighted R² for the chosen polynomial.
    r2: f64,
}

/// Choose the integer power and report its coefficient.
///
/// The power comes from the log-log slope over the measured sizes: if cost
/// goes as `Nᵖ` then `log t` is linear in `log N` with gradient `p`, and
/// rounding that gradient gives the integer power. The coefficient then
/// comes from a polynomial refit at that degree, so it means what its units
/// say - `ns` per `N^power`, with the lower-order terms carried alongside
/// rather than folded in.
///
/// Chi-squared reports on the fit; it does not choose it. That separation
/// is the point. It used to choose - walk the degrees, keep the first the
/// threshold accepted - and the answer then swung with the threshold in
/// both directions. Loose, and a *constant* was accepted as the shape of a
/// linear cost whenever the measurements were noisy. Tight, and summing
/// integers came out `O(N²)`: it is not exactly linear, because per-element
/// cost changes as the vector outgrows cache, so at high precision a
/// straight line is genuinely rejected and a parabola genuinely fits
/// better. Both answers were defensible from the residuals and both were
/// wrong, because "which model survives a threshold" is not the question
/// anyone asked.
///
/// The slope answers the question that was asked - how fast does this grow
/// - and it does not care that a parabola could be drawn through the
/// points. It also needs no separate rule for a term too small to matter: a
/// cubic contributing a billionth of the runtime moves the slope by a
/// billionth, so it is ignored by arithmetic rather than by a threshold.
///
/// What chi-squared still does, and only it can do, is say whether *any*
/// polynomial describes the data. That is reported, not acted on.
fn scaling_fit(
    ns: &[f64],
    means: &[f64],
    ses: &[f64],
    max_degree: usize,
) -> Option<ScalingFit> {
    let slope = power_fit(ns, means, ses)?.exponent;
    if !slope.is_finite() {
        return None;
    }
    let power = slope.round().clamp(0.0, max_degree as f64) as usize;
    let fit = weighted_poly_fit(ns, means, ses, power)?;
    Some(ScalingFit {
        power,
        ns_per_scale: fit.coefficients[power],
        std_error: fit.ses[power],
        chi2_per_dof: fit.chi2_per_dof,
        r2: fit.r2,
    })
}

/// Invert a small symmetric positive-definite matrix by Gauss-Jordan with
/// partial pivoting. `None` if it is singular to working precision, which
/// is how an unidentifiable fit reports itself.
fn invert(a: &[Vec<f64>]) -> Option<Vec<Vec<f64>>> {
    let n = a.len();
    let mut m: Vec<Vec<f64>> = a
        .iter()
        .enumerate()
        .map(|(i, row)| {
            let mut r = row.clone();
            r.extend((0..n).map(|j| if i == j { 1.0 } else { 0.0 }));
            r
        })
        .collect();
    for col in 0..n {
        let pivot = (col..n).max_by(|&i, &j| m[i][col].abs().total_cmp(&m[j][col].abs()))?;
        if m[pivot][col].abs() < 1e-300 {
            return None;
        }
        m.swap(col, pivot);
        let d = m[col][col];
        for v in m[col].iter_mut() {
            *v /= d;
        }
        for row in 0..n {
            if row != col {
                let f = m[row][col];
                if f != 0.0 {
                    let (pivot_row, target) = if row < col {
                        let (a, b) = m.split_at_mut(col);
                        (&b[0], &mut a[row])
                    } else {
                        let (a, b) = m.split_at_mut(row);
                        (&a[col], &mut b[0])
                    };
                    for (t, p) in target.iter_mut().zip(pivot_row.iter()) {
                        *t -= f * p;
                    }
                }
            }
        }
    }
    Some(m.into_iter().map(|r| r[n..].to_vec()).collect())
}



/// A power-law fit of measured times against problem size: `t ≈ c·Nᵖ`.
///
/// Every field is a statement about the *measurements*, not about the
/// algorithm. `exponent` is continuous on purpose - `N log N` has no
/// integer exponent, and a cost that is linear over part of the range and
/// quadratic over the rest has no single one either. Rounding to an integer
/// is a presentation decision, taken later and only once `chi2_per_dof`
/// says a power law is a fair description at all.
#[derive(Debug, Clone, Copy, PartialEq)]
struct PowerFit {
    /// The exponent `p`.
    exponent: f64,
    /// Standard error of `exponent`.
    exponent_se: f64,
    /// The prefactor `c`, in nanoseconds, so that `t ≈ c·Nᵖ`.
    prefactor: f64,
    /// Chi-squared per degree of freedom.
    ///
    /// Near 1 when a single power law describes the data within the
    /// measured error bars; large when it does not. This is a real
    /// goodness-of-fit test rather than a heuristic, and it is only
    /// available because each point carries an error bar that was
    /// *measured* rather than assumed - see [`power_fit`].
    ///
    /// Measured on synthetic sweeps: 0.8 for `O(N)`, 1.4 for `O(N²)`, 1.6
    /// for `O(N³)`, against 32 for `N log N`, 758 for `5N + 0.05N²` and
    /// 6924 for `2ᴺ`. The separation is not subtle.
    chi2_per_dof: f64,
}

/// Weighted least squares of `log t` against `log N`.
///
/// Taking logs turns `t = c·Nᵖ` into a straight line whose slope is the
/// exponent, so one ordinary linear fit answers the whole question - no
/// candidate basis, no model selection, and nothing that has to be
/// orthogonalised.
///
/// Each point must arrive with its own standard error, from having been
/// measured independently rather than inferred from the spread about the
/// line. That is what makes this the textbook weighted case with *known*
/// variances: `se(log t) = se(t)/t`, so the weight is `(t/se)²`, and
/// `chi2_per_dof` becomes a genuine goodness-of-fit rather than a
/// restatement of the residuals. Every earlier attempt here assumed a noise
/// shape instead of measuring one, and was optimistic by about twofold for
/// its trouble.
fn power_fit(ns: &[f64], ts: &[f64], ses: &[f64]) -> Option<PowerFit> {
    if ns.len() < 3 {
        return None;
    }
    let mut x = Vec::with_capacity(ns.len());
    let mut y = Vec::with_capacity(ns.len());
    let mut w = Vec::with_capacity(ns.len());
    for ((&n, &t), &se) in ns.iter().zip(ts).zip(ses) {
        if !(n > 0.0 && t > 0.0 && se > 0.0) || !se.is_finite() {
            return None;
        }
        x.push(n.ln());
        y.push(t.ln());
        w.push((t / se).powi(2));
    }
    let sw: f64 = w.iter().sum();
    if !sw.is_finite() || sw <= 0.0 {
        return None;
    }
    let xbar: f64 = x.iter().zip(&w).map(|(xi, wi)| wi * xi).sum::<f64>() / sw;
    let ybar: f64 = y.iter().zip(&w).map(|(yi, wi)| wi * yi).sum::<f64>() / sw;
    let sxx: f64 = x
        .iter()
        .zip(&w)
        .map(|(xi, wi)| wi * (xi - xbar).powi(2))
        .sum();
    if !sxx.is_finite() || sxx <= 0.0 {
        // Every N identical: no leverage on the exponent at all.
        return None;
    }
    let sxy: f64 = x
        .iter()
        .zip(&y)
        .zip(&w)
        .map(|((xi, yi), wi)| wi * (xi - xbar) * (yi - ybar))
        .sum();
    let exponent = sxy / sxx;
    let intercept = ybar - exponent * xbar;
    let chi2: f64 = x
        .iter()
        .zip(&y)
        .zip(&w)
        .map(|((xi, yi), wi)| wi * (yi - (intercept + exponent * xi)).powi(2))
        .sum();
    Some(PowerFit {
        exponent,
        // With known variances the exponent's variance is 1/Sxx outright -
        // no residual scale enters, because the weights are already in the
        // right units.
        exponent_se: (1.0 / sxx).sqrt(),
        prefactor: intercept.exp(),
        chi2_per_dof: chi2 / (ns.len() - 2) as f64,
    })
}

/// The exponent implied by two measurements alone: `log(t₂/t₁) / log(N₂/N₁)`.
///
/// Cheap enough to drive size selection, where the question is only "how
/// fast is this growing, roughly" - enough to predict what the next size
/// will cost and so to choose one that is large enough to be informative
/// without being too slow to afford.
///
/// Read across adjacent pairs it also says something the global fit cannot:
/// a *drifting* local exponent means no single power law holds, and which
/// way it drifts says why. Rising towards a limit is a mixed cost
/// approaching its asymptotic term; rising without bound is faster than any
/// polynomial.
fn two_point_exponent(n1: f64, t1: f64, n2: f64, t2: f64) -> Option<f64> {
    if !(n1 > 0.0 && n2 > 0.0 && t1 > 0.0 && t2 > 0.0) || n1 == n2 {
        return None;
    }
    Some((t2 / t1).ln() / (n2 / n1).ln())
}

/// Never let one step multiply the problem size by more than this.
///
/// The extrapolation below is only as good as an exponent estimated from
/// two noisy points, and its error enters as an exponent - so a modest
/// mistake in `p` is a large mistake in predicted cost. Capping the step
/// bounds what a wrong guess can cost: worst case we take an extra
/// measurement or two, rather than launching a single evaluation that eats
/// the entire budget.
const MAX_SIZE_GROWTH: f64 = 8.0;

/// The most of the remaining budget one measurement may be predicted to
/// consume.
///
/// Overshooting is far worse than undershooting. A size that turns out too
/// small costs one cheap measurement and is immediately corrected; a size
/// that turns out too large can spend the whole budget on a single point
/// and leave nothing to fit. So the prediction has to fit several times
/// over into what is left.
const BUDGET_SHARE_PER_SIZE: f64 = 0.25;

/// Choose the next problem size to measure during size discovery.
///
/// Extrapolates from `exponent`: if cost grows as `Nᵖ`, then reaching
/// `target` from `(last_n, last_t)` wants `last_n · (target/last_t)^(1/p)`.
///
/// Deliberately timid, in three separate ways, because the prediction is
/// built on an exponent estimated from two noisy measurements and enters
/// the answer as a reciprocal exponent:
///
/// * `p` is floored at 1. A sublinear estimate - which noise alone can
///   produce at small sizes, where fixed overheads still dominate and cost
///   barely moves - would give `1/p > 1` and demand an enormous jump.
/// * The step is capped at [`MAX_SIZE_GROWTH`] regardless.
/// * The predicted cost must fit [`BUDGET_SHARE_PER_SIZE`] of what remains,
///   so no single measurement can consume the budget even if the estimate
///   is badly wrong.
///
/// Returns `None` when even the smallest useful step - one more than the
/// last size - is predicted to overrun the budget, which is how discovery
/// learns it has reached the largest size it can afford.
fn next_size(
    last_n: f64,
    last_t: f64,
    exponent: f64,
    target: Duration,
    budget_left: Duration,
) -> Option<usize> {
    if !(last_n >= 1.0 && last_t > 0.0) || !exponent.is_finite() {
        return None;
    }
    let target_ns = target.as_secs_f64() * 1e9;
    let affordable_ns = budget_left.as_secs_f64() * 1e9 * BUDGET_SHARE_PER_SIZE;
    if affordable_ns <= 0.0 {
        return None;
    }
    // Never plan a measurement we cannot pay for, whatever the target says.
    let aim_ns = target_ns.min(affordable_ns);
    if aim_ns <= last_t {
        // The size we have already measured costs as much as we can afford
        // to spend on the next one; there is no room to grow.
        return None;
    }
    let p = exponent.max(1.0);
    // Three ceilings, whichever is lowest:
    //
    // * the step that reaches the target under the measured exponent;
    // * the step that stays affordable under a *pessimistic* exponent, one
    //   whole power above the measured one;
    // * the flat cap.
    //
    // The pessimistic term deserves its place because the estimate comes
    // from two noisy points and enters the cost as an exponent, so being
    // wrong about it is expensive in one direction only: too small a size
    // wastes one cheap measurement and corrects itself next step, while too
    // large a size can spend the entire budget on a single point and leave
    // nothing to fit. Budgeting as though the cost climbs a power faster
    // than measured makes the step affordable *by construction* rather than
    // proposing one and hoping.
    let to_target = (aim_ns / last_t).powf(1.0 / p);
    let affordable_growth = (affordable_ns / last_t).powf(1.0 / (p + 1.0));
    let growth = to_target.min(affordable_growth).min(MAX_SIZE_GROWTH);
    // `is_finite` first so the comparison never has to reason about NaN.
    if !growth.is_finite() || growth <= 1.0 {
        return None;
    }
    // Round down, not up: rounding up can push a step that was affordable
    // by a hair over the line, and undershooting is the cheap mistake.
    let next = (last_n * growth).floor().max(last_n + 1.0);
    if !next.is_finite() || next > usize::MAX as f64 {
        return None;
    }
    // A last check in the currency the budget is paid in, which also covers
    // the rounding-up above.
    let predicted = last_t * (next / last_n).powf(p + 1.0);
    (predicted <= affordable_ns).then_some(next as usize)
}





#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn scales_o_one() {
        println!();
        let stats = bench_scaling(|_| thread::sleep(Duration::from_millis(10)), 1);
        println!("O(N): {}", stats);
        assert_eq!(stats.scaling.power, 0);
        println!("   error: {:e}", stats.scaling.ns_per_scale - 1e7);
        assert!((stats.scaling.ns_per_scale - 1e7).abs() < 1e6);
        // A constant used to be the case the fit could say least about:
        // with only an R² to go on, nothing distinguished "flat" from
        // "could not tell", so it reported itself clueless. Measured error
        // bars settle it - a degree-zero fit that lands inside them has
        // identified the shape as surely as any other - so a constant is
        // now a proper answer, reached and stood behind.
        //
        // Only on a quiet machine, though. Contention makes timing noise
        // heavy-tailed rather than merely large: a stray slow sample sits
        // far outside error bars estimated from the samples around it, and
        // chi-squared rightly rejects a flat model that the data no longer
        // supports. The power above survives that; believing the *shape*
        // does not, and should not.
        if quiesced() {
            assert_eq!(1.0, stats.goodness_of_fit);
            assert!(!stats.hit_limit);
        }
        let shown = format!("{stats}");
        assert!(shown.contains('±'), "{shown}");
        assert!(shown.contains("R²"), "{shown}");
    }

    #[test]
    fn scales_o_n() {
        println!();
        let stats = bench_scaling(|n| thread::sleep(Duration::from_millis(10 * n as u64)), 1);
        println!("O(N): {}", stats);
        assert_eq!(stats.scaling.power, 1);
        println!("   error: {:e}", stats.scaling.ns_per_scale - 1e7);
        assert!((stats.scaling.ns_per_scale - 1e7).abs() < 1e5);

        // The sleep above is immune to a busy machine; this is not.
        // Summing a vector is memory-bound, so its per-element cost is set
        // by what else is touching the cache, and on a contended machine
        // the measured growth is that neighbour's rather than the sum's.
        // The reservation exists for exactly this - see `quiet-bench`.
        if !quiesced() {
            println!("SKIPPED (not quiesced): cannot measure a memory-bound cost here");
            return;
        }
        println!("Summing integers");
        let stats = bench_scaling_gen(
            |n| (0..n as u64).collect::<Vec<_>>(),
            |v| v.iter().cloned().sum::<u64>(),
            1,
        );
        println!("O(N): {}", stats);
        println!("   error: {:e}", stats.scaling.ns_per_scale - 1e7);
        assert_eq!(stats.scaling.power, 1);
    }

    #[test]
    fn scales_o_n_log_n_looks_like_n() {
        // Memory-bound, like the sum above: on a contended machine this
        // measures the neighbours rather than the sort.
        if !quiesced() {
            println!("SKIPPED (not quiesced): cannot measure a memory-bound cost here");
            return;
        }
        println!("Sorting integers");
        let stats = bench_scaling_gen(
            |n| {
                (0..n as u64)
                    .map(|i| (i * 13 + 5) % 137)
                    .collect::<Vec<_>>()
            },
            |v| v.sort(),
            1,
        );
        println!("O(N log N): {}", stats);
        println!("   error: {:e}", stats.scaling.ns_per_scale - 1e7);
        assert_eq!(stats.scaling.power, 1);
    }


    #[test]
    fn scales_o_n_square() {
        println!();
        let stats = bench_scaling(
            |n| thread::sleep(Duration::from_millis(10 * (n * n) as u64)),
            1,
        );
        println!("O(N): {}", stats);
        assert_eq!(stats.scaling.power, 2);
        println!("   error: {:e}", stats.scaling.ns_per_scale - 1e7);
        assert!((stats.scaling.ns_per_scale - 1e7).abs() < 1e5);
    }

    /// Synthetic data with a known answer, which is the only way to check
    /// that the fit reports the *asymptotically dominant* term rather than
    /// whichever single power happens to fit best.
    mod fitting {
        use super::*;






        /// Geometrically spaced sizes spanning a wide range, as size
        /// selection is meant to produce.
        fn wide_sizes() -> Vec<f64> {
            let mut v: Vec<f64> = (6..20).map(|k| (1.6f64).powi(k).round()).collect();
            v.dedup();
            v
        }

        /// What stage two hands the fit: each size measured independently,
        /// so every point arrives with its own error bar rather than one
        /// inferred from the spread about the line.
        fn measured(
            f: impl Fn(f64) -> f64,
            ns: &[f64],
            rel: f64,
            seed: u64,
        ) -> (Vec<f64>, Vec<f64>) {
            let mut rng = XorShift(seed | 1);
            let mut ts = Vec::new();
            let mut ses = Vec::new();
            for &n in ns {
                let truth = f(n);
                let u = |r: &mut XorShift| (r.next() >> 11) as f64 / (1u64 << 53) as f64;
                let jitter = (u(&mut rng) + u(&mut rng) - 1.0) * rel;
                ts.push(truth * (1.0 + jitter));
                ses.push(truth * rel);
            }
            (ts, ses)
        }

        const HUGE: Duration = Duration::from_secs(3600);

        #[test]
        fn the_next_size_aims_at_the_target_using_the_measured_power() {
            // Quadratic: to go from 1ms to 16ms, a 16x in time, wants a 4x
            // in size - inside the cap, so the power is what decides.
            let n = next_size(100.0, 1e6, 2.0, Duration::from_millis(16), HUGE).unwrap();
            assert!((390..=410).contains(&n), "quadratic step gave {n}");
            // Cubic: the same 16x in time is only a 2.5x in size, because
            // the cost climbs faster with each unit of N.
            let n = next_size(100.0, 1e6, 3.0, Duration::from_millis(16), HUGE).unwrap();
            assert!((245..=260).contains(&n), "cubic step gave {n}");
            // Linear: a 100x in time would want a 100x in size, so here the
            // cap is what decides instead.
            let n = next_size(100.0, 1e6, 1.0, Duration::from_millis(100), HUGE).unwrap();
            assert_eq!(800, n, "linear step should be capped at 8x");
        }

        #[test]
        fn a_wild_exponent_cannot_provoke_a_wild_step() {
            // A sublinear estimate is what noise produces at small sizes,
            // where fixed overheads still dominate and cost barely moves.
            // Taken literally it asks for an enormous jump; the floor at
            // p = 1 and the growth cap between them refuse.
            for exponent in [0.01, 0.2, 0.5, 1.0] {
                let n = next_size(100.0, 1e3, exponent, HUGE, HUGE).unwrap();
                assert!(n <= 800, "exponent {exponent} gave {n}");
            }
            // Nonsense in, nothing out - rather than a nonsensical size.
            assert_eq!(None, next_size(100.0, 1e6, f64::NAN, HUGE, HUGE));
            assert_eq!(None, next_size(100.0, 0.0, 2.0, HUGE, HUGE));

            // With the budget rather than the cap deciding, the floor at
            // p = 1 is what keeps the step honest. A measured exponent of
            // 0.01 says "cost barely moves with N", so budgeting on it
            // would license nearly doubling the size; treating the cost as
            // at least linear - and charging a power above that - keeps the
            // step to something a wrong guess can survive.
            let budget = Duration::from_nanos(8_000_000);
            let n = next_size(100.0, 1e6, 0.01, HUGE, budget).unwrap();
            assert!(
                n <= 150,
                "a near-zero exponent should not license a {n}-sized step"
            );
        }

        #[test]
        fn one_measurement_cannot_eat_the_whole_budget() {
            // The last measurement took 1s and only 2s remain. A quarter of
            // what is left is 500ms, less than the point we already have,
            // so there is no affordable step and discovery stops.
            assert_eq!(
                None,
                next_size(100.0, 1e9, 2.0, HUGE, Duration::from_secs(2))
            );
            // With plenty of budget the same call proceeds, and what it
            // picks is predicted to cost well under the whole of it.
            let budget = Duration::from_secs(600);
            let n = next_size(100.0, 1e9, 2.0, HUGE, budget).unwrap();
            let predicted = 1e9 * ((n as f64) / 100.0).powi(3);
            assert!(
                predicted <= budget.as_secs_f64() * 1e9 * 0.25 + 1.0,
                "picked {n}, predicted {predicted}ns of a {budget:?} budget"
            );
        }

        #[test]
        fn size_discovery_always_makes_progress_or_stops() {
            // Never returns the size it was given: either a strictly larger
            // one, or `None`. A loop built on this cannot spin.
            let mut n = 1.0f64;
            let mut steps = 0;
            while let Some(next) = next_size(n, 1e3 * n * n, 2.0, HUGE, HUGE) {
                assert!(next as f64 > n, "{next} did not advance past {n}");
                n = next as f64;
                steps += 1;
                if steps > 200 {
                    break;
                }
            }
            assert!(steps > 0, "should have taken at least one step");
        }

        /// What stage two collects: a handful of repeats at each size,
        /// summarised as a mean and a standard error over those repeats.
        fn replicated(
            f: impl Fn(f64) -> f64,
            ns: &[f64],
            rel: f64,
            repeats: usize,
            seed: u64,
        ) -> (Vec<f64>, Vec<f64>) {
            let mut rng = XorShift(seed | 1);
            let mut means = Vec::new();
            let mut ses = Vec::new();
            for &n in ns {
                let truth = f(n);
                let mut acc = Running::default();
                for _ in 0..repeats {
                    let u = |r: &mut XorShift| (r.next() >> 11) as f64 / (1u64 << 53) as f64;
                    let jitter = (u(&mut rng) + u(&mut rng) - 1.0) * rel;
                    acc.push(truth * (1.0 + jitter));
                }
                let (mean, se) = acc.mean_and_stderr();
                means.push(mean);
                ses.push(se);
            }
            (means, ses)
        }

        #[test]
        fn integer_powers_come_from_measured_error_bars() {
            let ns = wide_sizes();
            for (name, power, f) in [
                ("N", 1, Box::new(|n: f64| 3.0 * n) as Box<dyn Fn(f64) -> f64>),
                ("N^2", 2, Box::new(|n: f64| 0.02 * n * n)),
                ("N^3", 3, Box::new(|n: f64| 1e-4 * n * n * n)),
            ] {
                for seed in [1u64, 3, 5, 7] {
                    let (means, ses) = replicated(&f, &ns, 0.02, 6, seed);
                    let fit = scaling_fit(&ns, &means, &ses, 3).unwrap();
                    assert_eq!(power, fit.power, "{name} seed {seed}");
                    assert!(
                        fit.chi2_per_dof < 5.0,
                        "{name} seed {seed}: chi2/dof {} should be near 1",
                        fit.chi2_per_dof
                    );
                }
            }
        }

        #[test]
        fn a_mixed_cost_reports_the_power_it_ends_up_at() {
            // The case a single-term search cannot get right: linear
            // dominates over most of the range, but the cost is quadratic.
            let ns = wide_sizes();
            for seed in [1u64, 3, 5, 7] {
                let (means, ses) =
                    replicated(|n| 5.0 * n + 0.05 * n * n, &ns, 0.02, 6, seed);
                let fit = scaling_fit(&ns, &means, &ses, 3).unwrap();
                assert_eq!(2, fit.power, "seed {seed}");
                assert!(
                    (fit.ns_per_scale - 0.05).abs() < 5.0 * fit.std_error,
                    "seed {seed}: {} +- {} should bracket 0.05",
                    fit.ns_per_scale,
                    fit.std_error
                );
            }
        }

        #[test]
        fn more_repeats_tighten_the_answer() {
            // The lever stage two pulls when the fit is not precise enough:
            // measure each size more times. Nothing else changes.
            let ns = wide_sizes();
            let few = replicated(|n| 0.02 * n * n, &ns, 0.05, 3, 1);
            let many = replicated(|n| 0.02 * n * n, &ns, 0.05, 48, 1);
            let a = scaling_fit(&ns, &few.0, &few.1, 3).unwrap();
            let b = scaling_fit(&ns, &many.0, &many.1, 3).unwrap();
            assert!(
                b.std_error < a.std_error,
                "48 repeats ({}) should beat 3 ({})",
                b.std_error,
                a.std_error
            );
        }

        /// A synthetic single call: the true cost at `n`, jittered.
        fn one_call(f: impl Fn(f64) -> f64, rel: f64, seed: u64) -> impl FnMut(usize) -> f64 {
            let mut rng = XorShift(seed | 1);
            move |n| {
                let u = |r: &mut XorShift| (r.next() >> 11) as f64 / (1u64 << 53) as f64;
                let jitter = (u(&mut rng) + u(&mut rng) - 1.0) * rel;
                f(n as f64) * (1.0 + jitter)
            }
        }

        fn counted<'a>(
            calls: &'a std::cell::Cell<usize>,
            mut inner: impl FnMut(usize) -> f64 + 'a,
        ) -> impl FnMut(usize) -> f64 + 'a {
            move |n| {
                calls.set(calls.get() + 1);
                inner(n)
            }
        }

        fn precise() -> Config {
            Config {
                target_rel_error: 0.01,
                target_abs_error: Duration::ZERO,
                max_time: Duration::from_secs(1),
            }
        }

        #[test]
        fn measuring_refines_until_the_answer_is_precise_enough() {
            let sizes = [64usize, 128, 256, 512, 1024];
            for seed in [1u64, 3, 5, 7] {
                let cfg = precise();
                let m = measure_scaling(
                    &sizes,
                    &cfg,
                    Duration::from_secs(3600),
                    3,
                    one_call(|n| 0.02 * n * n, 0.10, seed),
                );
                let (fit, limited) = (m.fit.unwrap(), m.hit_limit);
                assert!(!limited, "seed {seed}: should not have hit the budget");
                assert_eq!(2, fit.power, "seed {seed}");
                assert!(
                    fit.std_error < cfg.target_rel_error * fit.ns_per_scale,
                    "seed {seed}: {} +- {} misses the 1% target",
                    fit.ns_per_scale,
                    fit.std_error
                );
                assert!(
                    (fit.ns_per_scale - 0.02).abs() < 5.0 * fit.std_error,
                    "seed {seed}: {} +- {} should bracket 0.02",
                    fit.ns_per_scale,
                    fit.std_error
                );
            }
        }

        /// Replicates that straddle the truth exactly, so the mean of a
        /// full round-set lands on it and the fit has nothing to explain.
        ///
        /// Random jitter cannot be used here: chi-squared per degree of
        /// freedom is scale-free, near 1 whatever the noise level, so with
        /// six replicates it wanders either side of the threshold according
        /// to the seed. This test is about what the loop does once the
        /// model *is* accepted, so the data has to make that certain.
        fn balanced(f: impl Fn(f64) -> f64, rel: f64) -> impl FnMut(usize) -> f64 {
            let mut seen: std::collections::HashMap<usize, usize> = Default::default();
            move |n| {
                let k = seen.entry(n).or_insert(0);
                let sign = if *k % 2 == 0 { 1.0 } else { -1.0 };
                *k += 1;
                f(n as f64) * (1.0 + sign * rel)
            }
        }

        #[test]
        fn a_target_already_met_costs_nothing_extra() {
            // The loop buys precision it has found it lacks, and not
            // otherwise: a model that fits and a target already met must be
            // paid for exactly once.
            let sizes = [64usize, 128, 256, 512, 1024];
            let calls = std::cell::Cell::new(0);
            let cfg = Config {
                target_rel_error: 0.5,
                ..precise()
            };
            let m = measure_scaling(
                &sizes,
                &cfg,
                Duration::from_secs(3600),
                3,
                counted(&calls, balanced(|n| 0.02 * n * n, 0.02)),
            );
            let fit = m.fit.expect("exact quadratic data must fit");
            assert!(!m.hit_limit);
            assert_eq!(2, fit.power);
            assert!(
                fit.chi2_per_dof <= CHI2_REJECT,
                "premise: the model must be accepted, chi2/dof was {}",
                fit.chi2_per_dof
            );
            assert_eq!(INITIAL_REPEATS * sizes.len(), calls.get());
        }

        #[test]
        fn a_tighter_target_is_paid_for_with_more_calls() {
            let sizes = [64usize, 128, 256, 512, 1024];
            let mut counts = Vec::new();
            for target in [0.05, 0.005] {
                let calls = std::cell::Cell::new(0);
                let cfg = Config {
                    target_rel_error: target,
                    ..precise()
                };
                measure_scaling(
                    &sizes,
                    &cfg,
                    Duration::from_secs(3600),
                    3,
                    counted(&calls, one_call(|n| 0.02 * n * n, 0.10, 1)),
                );
                counts.push(calls.get());
            }
            assert!(
                counts[1] > counts[0],
                "0.5% took {} calls, 5% took {}",
                counts[1],
                counts[0]
            );
        }

        #[test]
        fn running_out_of_budget_still_answers_and_says_so() {
            // Better a wide answer flagged as wide than no answer: the
            // caller can tell the difference, which is the whole point of
            // returning the flag alongside the fit.
            let sizes = [64usize, 128, 256, 512, 1024];
            let cfg = Config {
                target_rel_error: 1e-9,
                ..precise()
            };
            let m = measure_scaling(
                &sizes,
                &cfg,
                Duration::from_millis(50),
                3,
                one_call(|n| 0.02 * n * n, 0.10, 1),
            );
            assert!(m.hit_limit, "an unreachable target must report the limit");
            assert_eq!(2, m.fit.unwrap().power);
        }

        #[test]
        fn a_benchmark_that_measures_as_free_still_terminates() {
            // A call the optimiser removed entirely can time as zero, and
            // a budget spent in units of measured cost would then never be
            // spent at all. The wall clock is what stops it.
            let sizes = [64usize, 128, 256, 512, 1024];
            let cfg = Config {
                target_rel_error: 1e-12,
                ..precise()
            };
            let m = measure_scaling(&sizes, &cfg, Duration::from_millis(20), 3, |_| 0.0);
            assert!(m.hit_limit);
            assert!(m.fit.is_none(), "nothing measurable, so nothing to report");
        }

        #[test]
        fn setup_around_the_call_counts_against_the_budget() {
            // `bench_scaling_gen` builds and drops an environment outside
            // the timed region, so real time can run out long before the
            // measured cost does. Simulated here by a `measure` that
            // sleeps far longer than the time it reports.
            let sizes = [64usize, 128];
            let cfg = Config {
                target_rel_error: 1e-12,
                ..precise()
            };
            let started = Instant::now();
            let m = measure_scaling(&sizes, &cfg, Duration::from_millis(100), 0, |_| {
                thread::sleep(Duration::from_millis(10));
                1.0
            });
            let elapsed = started.elapsed();
            assert!(m.hit_limit);
            assert!(
                elapsed < Duration::from_millis(600),
                "budget of 100ms overran to {elapsed:?}"
            );
        }

        #[test]
        fn an_exhausted_budget_still_buys_an_error_bar() {
            // The budget is already gone after the first round here. Two
            // rounds happen anyway, because a standard error needs two
            // points to exist and a fit needs standard errors: stopping on
            // the budget alone would return nothing at all rather than
            // something wide, which is the worse of the two failures.
            let sizes = [64usize, 128, 256];
            let cfg = Config {
                target_rel_error: 1e-12,
                ..precise()
            };
            let m = measure_scaling(
                &sizes,
                &cfg,
                Duration::from_micros(1),
                1,
                one_call(|n| 3.0 * n, 0.05, 1),
            );
            assert!(m.hit_limit);
            let fit = m.fit.expect("two rounds are enough to fit");
            assert!(fit.std_error.is_finite() && fit.std_error > 0.0);
        }

        /// Records what sizes a stage-one climb asked for.
        fn climb(
            nmin: usize,
            cost: impl Fn(f64) -> f64,
            budget: Duration,
        ) -> (SizeRange, Vec<usize>) {
            let tried = std::cell::RefCell::new(Vec::new());
            let range = discover_sizes(nmin, budget, |n| {
                tried.borrow_mut().push(n);
                cost(n as f64)
            });
            let tried = tried.into_inner();
            (range, tried)
        }

        #[test]
        fn a_measurable_first_size_still_yields_a_growth_rate() {
            // A cost that is already worth timing at `nmin` gives the climb
            // nothing to do, and it would hand over the assumed exponent of
            // 1 - which `choose_sizes` would then use to budget, planning a
            // ladder whose top rung costs a hundredfold what it predicted.
            for (name, p, cost) in [
                ("N", 1.0, Box::new(|n: f64| 1e7 * n) as Box<dyn Fn(f64) -> f64>),
                ("N^2", 2.0, Box::new(|n: f64| 1e7 * n * n)),
                ("N^3", 3.0, Box::new(|n: f64| 1e7 * n * n * n)),
            ] {
                let (range, tried) = climb(1, &cost, Duration::from_secs(3600));
                assert_eq!(1, range.lo, "{name}: the floor should not move");
                assert!(
                    tried.len() > 1,
                    "{name}: must probe upward to learn the growth rate"
                );
                assert!(
                    (range.exponent - p).abs() < 0.2,
                    "{name}: exponent came out {}",
                    range.exponent
                );
            }
        }

        #[test]
        fn the_ladder_always_has_enough_rungs_to_fit() {
            // Log spacing assumes enough distinct integers to land on, and
            // near the bottom there are not: from `lo` of 1 across a span
            // of 2, six log-spaced rungs round onto two. A fit needs more
            // sizes than terms, so a collapsed ladder cannot find the
            // scaling the sweep was set up to look for - the cubic case
            // was reduced to fitting a constant.
            for lo in [1usize, 2, 7, 100, 20_000] {
                for p in [0.5, 1.0, 2.0, 3.0] {
                    for secs in [1u64, 8, 60] {
                        let range = SizeRange {
                            lo,
                            lo_time: 1e7,
                            exponent: p,
                        };
                        let sizes = choose_sizes(range, 1);
                        assert_eq!(
                            NUM_SIZES,
                            sizes.len(),
                            "lo={lo} p={p} {secs}s gave {sizes:?}"
                        );
                        assert!(
                            sizes.windows(2).all(|w| w[0] < w[1]),
                            "lo={lo} p={p} {secs}s not increasing: {sizes:?}"
                        );
                        assert!(sizes[0] >= lo, "lo={lo}: {sizes:?} starts below the floor");
                    }
                }
            }
        }

        #[test]
        fn a_ladder_keeps_the_caller_unit() {
            // `nmin` is the caller's unit - sizes are counts of it - so
            // every rung has to be a multiple of it, densest packing
            // included.
            for nmin in [3usize, 64, 1000] {
                for p in [1.0, 3.0] {
                    let range = SizeRange {
                        lo: nmin,
                        lo_time: 1e7,
                        exponent: p,
                    };
                    let sizes = choose_sizes(range, nmin);
                    assert!(
                        sizes.iter().all(|n| n % nmin == 0),
                        "nmin={nmin} p={p}: {sizes:?}"
                    );
                }
            }
        }

        #[test]
        fn the_ladder_spans_a_fixed_amount_of_time_not_of_size() {
            // The whole of the choice. A fixed size span would mean a
            // different time span for every benchmark - four times the size
            // is four times the work for a linear cost and sixty-four times
            // for a cubic one - so the span is set in time and the sizes
            // follow from the growth rate stage one measured.
            //
            // Placed high enough up that log spacing has integers to land
            // on; down at the bottom the densest packing takes over and the
            // time span is whatever consecutive sizes happen to give.
            let mut tops = Vec::new();
            for p in [1.0, 2.0, 3.0] {
                let range = SizeRange {
                    lo: 20_000,
                    lo_time: 1e7,
                    exponent: p,
                };
                let sizes = choose_sizes(range, 1);
                assert_eq!(NUM_SIZES, sizes.len(), "p={p}: {sizes:?} collapsed");
                let time_span = (*sizes.last().unwrap() as f64 / 20_000.0).powf(p);
                assert!(
                    (time_span - TIME_SPAN).abs() < 0.1 * TIME_SPAN,
                    "p={p}: {sizes:?} spans {time_span}x in time, wanted {TIME_SPAN}x"
                );
                tops.push(*sizes.last().unwrap());
            }
            assert!(
                tops[0] > tops[1] && tops[1] > tops[2],
                "faster growth must reach the same time in fewer sizes, got {tops:?}"
            );
        }

        #[test]
        fn the_opening_rounds_cost_what_the_arithmetic_says() {
            // Six rungs log-spaced over a time ratio T cost
            // (T^(6/5) - 1)/(T^(1/5) - 1) times the cheapest one, which is
            // 13.4 at T = 4; six opening rounds make it about 80. At the
            // 20-microsecond floor that is under two milliseconds, and it
            // is the figure that says this does not need a time budget to
            // keep it honest.
            let lo_time = MIN_MEASURABLE.as_secs_f64() * 1e9;
            let range = SizeRange {
                lo: 1000,
                lo_time,
                exponent: 1.0,
            };
            let sizes = choose_sizes(range, 1);
            let round: f64 = sizes
                .iter()
                .map(|&n| lo_time * n as f64 / 1000.0)
                .sum();
            let opening = round * INITIAL_REPEATS as f64;
            assert!(
                opening < 2e6,
                "opening rounds came to {opening}ns, expected under 2ms"
            );
        }

        #[test]
        fn a_forced_ladder_is_the_cheapest_one_that_fits() {
            // Where affordability has to give way, it gives way as little
            // as possible: every rung is the smallest size still available,
            // so no ladder with this many rungs costs less. What the budget
            // then buys is fewer rounds, which `measure_scaling` handles by
            // stopping early and saying so - a wide answer on time, rather
            // than no answer at all.
            let range = SizeRange {
                lo: 1,
                lo_time: 1e7,
                exponent: 3.0,
            };
            let sizes = choose_sizes(range, 1);
            assert_eq!(vec![1, 2, 3, 4, 5, 6], sizes);
        }

        #[test]
        fn a_resolved_but_negligible_term_is_not_the_scaling() {
            // Measured on real hardware: summing integers produced a cubic
            // coefficient of about -5e-10 against a standard error of
            // 3e-16. Distinguishable from zero by seven orders of
            // magnitude, and worth about a billionth of the runtime. Being
            // sure a term is not zero says nothing about it being the
            // scaling, and only the second question has a useful answer.
            //
            // The error bars are given directly rather than simulated,
            // because that is the situation: a benchmark repeatable enough
            // to resolve a term far below the one that carries the cost.
            let ns = wide_sizes();
            let top = ns.iter().cloned().fold(0.0, f64::max);
            // Sized so the cubic is a billionth of the runtime at the top.
            let tiny = 1e-9 * 3.0 / top.powi(2);
            let means: Vec<f64> = ns.iter().map(|&n| 3.0 * n + tiny * n * n * n).collect();
            let ses: Vec<f64> = means.iter().map(|&m| 1e-12 * m).collect();

            // The term really is resolved - many sigma from zero - so
            // this is not a case that would be dismissed for being noise.
            // That is the whole point: being sure a term is not zero says
            // nothing about it being the scaling.
            let cubic = weighted_poly_fit(&ns, &means, &ses, 3).unwrap();
            let sigmas = cubic.coefficients[3].abs() / cubic.ses[3];
            assert!(sigmas > 6.0, "the cubic should be far from zero, at {sigmas}");

            let fit = scaling_fit(&ns, &means, &ses, 3).unwrap();
            assert_eq!(1, fit.power, "a billionth of the runtime is not the scaling");
        }

        #[test]
        fn a_term_that_carries_the_cost_is_still_found() {
            // The other side of the same rule: a leading term that really
            // does account for the runtime must survive it, including when
            // a large lower-order term keeps its share well under half.
            let ns = wide_sizes();
            for (name, power, f) in [
                ("N^2", 2, Box::new(|n: f64| 0.02 * n * n) as Box<dyn Fn(f64) -> f64>),
                ("5N + 0.05N^2", 2, Box::new(|n: f64| 5.0 * n + 0.05 * n * n)),
                ("N^3", 3, Box::new(|n: f64| 1e-4 * n * n * n)),
            ] {
                let (means, ses) = replicated(&f, &ns, 0.002, 12, 1);
                let fit = scaling_fit(&ns, &means, &ses, 3).unwrap();
                assert_eq!(power, fit.power, "{name}");
            }
        }

        #[test]
        fn a_wide_error_bar_does_not_make_a_constant_the_shape_of_a_trend() {
            // The failure the rejection threshold exists to stop, taken
            // from the ladder `choose_sizes` actually builds: a linear cost
            // spanning four times in time, measured to 20%. A constant
            // misses that by chi-squared per degree of freedom of about 5 -
            // plainly the wrong shape, and yet under a threshold of ten it
            // was accepted, because ten tolerates a model wrong by an order
            // of magnitude.
            //
            // The damage is not just the wrong power. A degree-zero
            // prefactor is near enough the mean of every measurement, so it
            // looks precise at once and the refinement loop stops - the
            // accuracy target cannot save us, because a wrong model does
            // not present as an imprecise one.
            let ns = [1000.0, 1320.0, 1741.0, 2297.0, 3031.0, 4000.0];
            let means: Vec<f64> = ns.iter().map(|&n| 10.0 * n).collect();
            let ses: Vec<f64> = means.iter().map(|&m| 0.20 * m).collect();

            let flat = weighted_poly_fit(&ns, &means, &ses, 0).unwrap();
            assert!(
                flat.chi2_per_dof > CHI2_REJECT,
                "a constant must be rejected here, chi2/dof was {}",
                flat.chi2_per_dof
            );
            // And it is the threshold doing the work, not a hopeless fit:
            // this is the size of miss a loose threshold waves through.
            assert!(
                flat.chi2_per_dof < 10.0,
                "premise: the miss is moderate, chi2/dof was {}",
                flat.chi2_per_dof
            );

            let fit = scaling_fit(&ns, &means, &ses, 3).unwrap();
            assert_eq!(1, fit.power);
            assert!((fit.ns_per_scale - 10.0).abs() < 1e-6, "{}", fit.ns_per_scale);
        }

        #[test]
        fn a_whole_sweep_says_whether_it_believes_itself() {
            // Both stages end to end, on costs of known shape, driven by a
            // synthetic clock. Chi-squared no longer picks the model, so
            // reporting is the whole of its remaining job: saying whether
            // any polynomial actually described what was measured.
            let cfg = Config {
                target_rel_error: 0.02,
                target_abs_error: Duration::ZERO,
                max_time: Duration::from_secs(60),
            };

            // A real power law: identified, believed, and not flagged.
            let mut clock = one_call(|n| 50.0 * n * n, 0.02, 1);
            let stats = scaling_sweep(&cfg, 1, |n| clock(n));
            assert_eq!(2, stats.scaling.power);
            assert!(stats.goodness_of_fit > 0.9, "{}", stats.goodness_of_fit);
            assert!(!stats.hit_limit);
            assert!(
                (stats.scaling.ns_per_scale - 50.0).abs() < 0.1 * 50.0,
                "{}",
                stats.scaling.ns_per_scale
            );

            // A cost no polynomial describes. It still reports the power it
            // behaves like - the slope does not need a polynomial to exist
            // - but says it could not vouch for the shape.
            let mut clock = one_call(|n| 40.0 * n * (n + 1.0).ln(), 0.0005, 1);
            let stats = scaling_sweep(&cfg, 1, |n| clock(n));
            assert_eq!(
                0.0, stats.goodness_of_fit,
                "an N log N cost is not a polynomial and should say so"
            );
            assert!(stats.hit_limit);
        }

        #[test]
        fn a_bad_fit_does_not_widen_the_coefficient_error_bars() {
            // The whole reason for measuring the error bars rather than
            // assuming them: how precisely a coefficient is known depends on
            // the sizes and how well each was measured, and on nothing else.
            // `(XtWX)^-1` never sees the timings. So two datasets that share
            // sizes and error bars get identical coefficient errors even when
            // one is the right shape and the other is nowhere close - the
            // misfit shows up in chi-squared, where it belongs, instead of
            // being laundered into error bars wide enough to cover it.
            let ns = wide_sizes();
            let (fits, ses) = replicated(|n| 0.02 * n * n, &ns, 0.01, 8, 1);
            let misfits: Vec<f64> = ns.iter().map(|&n| 2.0 * n * n.ln()).collect();

            let good = weighted_poly_fit(&ns, &fits, &ses, 2).unwrap();
            let bad = weighted_poly_fit(&ns, &misfits, &ses, 2).unwrap();

            for (j, (g, b)) in good.ses.iter().zip(bad.ses.iter()).enumerate() {
                assert!(
                    (g - b).abs() <= 1e-9 * g.abs(),
                    "term {j}: error bars {g} and {b} should not depend on the timings"
                );
            }
            assert!(good.chi2_per_dof < 5.0, "{}", good.chi2_per_dof);
            assert!(bad.chi2_per_dof > 100.0, "{}", bad.chi2_per_dof);
        }

        #[test]
        fn the_reported_coefficient_is_refit_at_the_chosen_degree() {
            // Our terms are not orthogonal, so a coefficient read off a wider
            // fit is a partial thing: it describes what N^p contributes once
            // higher terms have taken their share, which is not the question
            // asked. Refitting at the chosen degree both re-centres it and
            // stops it paying for degrees of freedom the data did not need.
            let ns = wide_sizes();
            for seed in [1u64, 3, 5, 7] {
                let (means, ses) = replicated(|n| 3.0 * n, &ns, 0.02, 6, seed);
                let fit = scaling_fit(&ns, &means, &ses, 3).unwrap();
                assert_eq!(1, fit.power, "seed {seed}");

                let refit = weighted_poly_fit(&ns, &means, &ses, fit.power).unwrap();
                assert_eq!(refit.coefficients[fit.power], fit.ns_per_scale, "seed {seed}");
                assert_eq!(refit.ses[fit.power], fit.std_error, "seed {seed}");

                let wide = weighted_poly_fit(&ns, &means, &ses, 3).unwrap();
                assert!(
                    fit.std_error < 0.8 * wide.ses[fit.power],
                    "seed {seed}: refitting should tighten {} below {}",
                    fit.std_error,
                    wide.ses[fit.power]
                );
            }
        }

        #[test]
        fn a_shape_that_does_not_fit_is_visible_in_chi_squared() {
            // Coefficient errors say how well the coefficients are known;
            // chi-squared says whether the model is the right shape. Keeping
            // them apart is the point of using measured error bars, and it
            // is what lets a bad shape be caught rather than absorbed into
            // wider error bars.
            let ns = wide_sizes();
            let (means, ses) = replicated(|n| 2.0 * n * n.ln(), &ns, 0.005, 12, 1);
            let fit = scaling_fit(&ns, &means, &ses, 3).unwrap();
            assert!(
                fit.chi2_per_dof > 10.0,
                "N log N is not a polynomial here; chi2/dof was {}",
                fit.chi2_per_dof
            );
        }

        #[test]
        fn a_power_law_gives_back_its_own_exponent() {
            let ns = wide_sizes();
            for (name, p, f) in [
                ("N", 1.0, Box::new(|n: f64| 3.0 * n) as Box<dyn Fn(f64) -> f64>),
                ("N^2", 2.0, Box::new(|n: f64| 0.02 * n * n)),
                ("N^3", 3.0, Box::new(|n: f64| 1e-4 * n * n * n)),
            ] {
                for seed in 1..5 {
                    let (ts, ses) = measured(&f, &ns, 0.01, seed);
                    let fit = power_fit(&ns, &ts, &ses).unwrap();
                    assert!(
                        (fit.exponent - p).abs() < 0.02,
                        "{name} seed {seed}: exponent {} should be about {p}",
                        fit.exponent
                    );
                    // And it says so confidently: a single power law really
                    // does describe this, so chi-squared per degree of
                    // freedom sits near one.
                    assert!(
                        fit.chi2_per_dof < 5.0,
                        "{name} seed {seed}: chi2/dof {} should be near 1",
                        fit.chi2_per_dof
                    );
                }
            }
        }

        #[test]
        fn the_prefactor_comes_back_too() {
            let ns = wide_sizes();
            let (ts, ses) = measured(|n| 0.02 * n * n, &ns, 0.01, 1);
            let fit = power_fit(&ns, &ts, &ses).unwrap();
            assert!(
                (fit.prefactor - 0.02).abs() < 0.02 * 0.1,
                "prefactor {} should be about 0.02",
                fit.prefactor
            );
        }

        #[test]
        fn a_cost_that_is_not_one_power_law_says_so() {
            // Chi-squared per degree of freedom is the signal, and it is
            // not a close call: pure powers sit near 1 (asserted above),
            // while these are tens to hundreds.
            let ns = wide_sizes();
            for (name, f) in [
                (
                    "N log N",
                    Box::new(|n: f64| 2.0 * n * n.ln()) as Box<dyn Fn(f64) -> f64>,
                ),
                ("5N + 0.05N^2", Box::new(|n: f64| 5.0 * n + 0.05 * n * n)),
            ] {
                let (ts, ses) = measured(&f, &ns, 0.01, 1);
                let fit = power_fit(&ns, &ts, &ses).unwrap();
                assert!(
                    fit.chi2_per_dof > 10.0,
                    "{name}: chi2/dof {} should be large",
                    fit.chi2_per_dof
                );
            }
        }

        #[test]
        fn a_drifting_local_exponent_shows_a_mixed_cost_approaching_its_limit() {
            // 5N + 0.05N^2 is asymptotically quadratic, but only becomes so
            // as N grows. The local exponent climbs towards 2, and *that*
            // is the asymptotic answer - a single global fit averages the
            // whole transition and lands uselessly in between.
            let ns = wide_sizes();
            let (ts, _) = measured(|n| 5.0 * n + 0.05 * n * n, &ns, 0.001, 1);
            let local: Vec<f64> = ns
                .windows(2)
                .zip(ts.windows(2))
                .filter_map(|(n, t)| two_point_exponent(n[0], t[0], n[1], t[1]))
                .collect();
            let first = local.first().copied().unwrap();
            let last = local.last().copied().unwrap();
            assert!(first < 1.4, "starts near linear, got {first}");
            assert!(last > 1.8, "ends near quadratic, got {last}");

            // A genuine power law does not drift.
            let (ts, _) = measured(|n| 0.02 * n * n, &ns, 0.001, 1);
            let local: Vec<f64> = ns
                .windows(2)
                .zip(ts.windows(2))
                .filter_map(|(n, t)| two_point_exponent(n[0], t[0], n[1], t[1]))
                .collect();
            for p in &local {
                assert!((p - 2.0).abs() < 0.15, "steady quadratic, got {p}");
            }
        }

        #[test]
        fn two_points_are_enough_for_a_rough_exponent() {
            // Which is all size selection needs: enough to predict what the
            // next size will cost.
            assert!((two_point_exponent(10.0, 100.0, 100.0, 10000.0).unwrap() - 2.0).abs() < 1e-9);
            assert!((two_point_exponent(10.0, 10.0, 100.0, 100.0).unwrap() - 1.0).abs() < 1e-9);
            assert_eq!(None, two_point_exponent(10.0, 1.0, 10.0, 2.0));
            assert_eq!(None, two_point_exponent(10.0, 0.0, 20.0, 1.0));
        }













    }

    /// A fixed amount of arithmetic, touching no memory beyond a register.
    ///
    /// Deliberately not a vector workload: the point here is to measure the
    /// estimator, and a memory-bound cost measures the cache instead.
    fn spin(rounds: u64) -> u64 {
        let rounds = std::hint::black_box(rounds);
        let mut x = std::hint::black_box(1u64);
        for i in 0..rounds {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(i);
        }
        x
    }

    /// Does the reported `±` describe the spread you actually get?
    ///
    /// The only way to know is to run the whole thing repeatedly and
    /// compare, which no amount of reading the estimator can substitute
    /// for. Checked on a cost that really is a power law, because
    /// `rel_std_error` is documented as conditional on the law being right
    /// and it would be idle to hold it to a promise it does not make.
    /// `a_flagged_fit_does_not_pretend_to_a_trustworthy_error_bar` covers
    /// what happens when the law is not right.
    #[test]
    fn scaling_error_bar_is_honest() {
        println!();
        if !quiesced() {
            println!("SKIPPED: machine is not quiesced (see `quiet-bench reserve`)");
            return;
        }
        const REPEATS: usize = 12;
        let runs: Vec<ScalingStats> = (0..REPEATS)
            .map(|_| bench_scaling(|n| spin(n as u64), 1))
            .collect();

        // Only runs whose law was identified, and only those that agreed on
        // it: `ns_per_scale` is measured per `Nᴾ`, so runs with different P
        // report different quantities in different units and pooling them
        // would compare nanoseconds-per-N with nanoseconds-per-N².
        let good: Vec<&ScalingStats> = runs
            .iter()
            .filter(|s| s.goodness_of_fit > 0.0 && s.scaling.power == 1)
            .collect();
        assert!(
            good.len() * 2 > REPEATS,
            "only {} of {REPEATS} runs identified this linear cost",
            good.len()
        );

        let (_, observed) =
            mean_and_spread(&good.iter().map(|s| s.scaling.ns_per_scale).collect::<Vec<_>>());
        let claimed = good.iter().map(|s| s.rel_std_error).sum::<f64>() / good.len() as f64;
        let ratio = observed / claimed;
        println!(
            "claimed {:.3}%, observed {:.3}%, ratio {ratio:.2}x",
            100.0 * claimed,
            100.0 * observed
        );
        // Generous, like its flat-benchmark counterpart: a spread estimated
        // from a dozen runs is itself noisy, and run-to-run drift the
        // estimator cannot see - cache state, frequency - inflates the
        // observed side without any dishonesty on the claimed side.
        assert!(
            ratio < 4.0,
            "claimed {:.3}% but observed spread was {:.3}% ({ratio:.1}x overconfident)",
            100.0 * claimed,
            100.0 * observed
        );
    }

    /// The other half of the promise: when the cost is *not* a power law,
    /// the error bar is not to be trusted - and the library has to say so
    /// rather than leave it to be discovered.
    ///
    /// Summing a vector is the case in point. It looks linear and is not:
    /// per-element cost climbs as the vector outgrows cache, so the fitted
    /// constant depends on which sizes a run happened to choose, and the
    /// spread across runs comes out an order of magnitude wider than any
    /// single run's `±`. That gap is real and cannot be closed from inside
    /// one run. What can be done is to refuse to vouch for it, which is
    /// what a zeroed `goodness_of_fit` and the `(limit)` mark are for.
    ///
    /// The caller's remedy is `nmin`: start above the size where the
    /// workload changes character and it becomes a power law again, with an
    /// honest error bar. See [`bench_scaling`] for the measurements.
    #[test]
    fn a_flagged_fit_does_not_pretend_to_a_trustworthy_error_bar() {
        println!();
        if !quiesced() {
            println!("SKIPPED: machine is not quiesced (see `quiet-bench reserve`)");
            return;
        }
        const REPEATS: usize = 8;
        let runs: Vec<ScalingStats> = (0..REPEATS)
            .map(|_| {
                bench_scaling_gen(
                    |n| (0..n as u64).collect::<Vec<_>>(),
                    |v| v.iter().cloned().sum::<u64>(),
                    1,
                )
            })
            .collect();

        let (_, observed) =
            mean_and_spread(&runs.iter().map(|s| s.scaling.ns_per_scale).collect::<Vec<_>>());
        let claimed = runs.iter().map(|s| s.rel_std_error).sum::<f64>() / runs.len() as f64;
        println!(
            "claimed {:.3}%, observed {:.3}%",
            100.0 * claimed,
            100.0 * observed
        );

        // The premise: this really is the dishonest-looking case.
        assert!(
            observed > 4.0 * claimed,
            "expected the spread to outrun the error bar here, {observed} vs {claimed}"
        );
        // The promise: every such run says so, and none is left looking
        // trustworthy.
        for s in &runs {
            assert_eq!(
                0.0, s.goodness_of_fit,
                "a cost no power law describes must report itself unidentified: {s}"
            );
            assert!(s.hit_limit, "and must carry the limit mark: {s}");
        }
    }


}


