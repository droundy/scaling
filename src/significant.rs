/// Calculates the conservative two-tailed Bonferroni Z-limit.
///
/// * n - Total number of measurements being tested
/// * fwer - Target Family-Wise Error Rate (e.g., 0.05 for 95% confidence)
pub(super) fn bonferroni_z_limit(n: u64, fwer: f64) -> f64 {
    if n == 0 {
        return f64::NAN; // no way to know if this is significant.
    }
    let alpha_adj = fwer / (n as f64);

    // For a two-tailed test, the upper tail probability is half the adjusted alpha
    let p = alpha_adj / 2.0;

    // Abramowitz and Stegun rational approximation for the inverse normal CDF upper tail.
    // t = sqrt(-2 * ln(p))
    let t = (-2.0 * p.ln()).sqrt();

    let c0 = 2.515517;
    let c1 = 0.802853;
    let c2 = 0.010328;

    let d1 = 1.432788;
    let d2 = 0.189269;
    let d3 = 0.001308;

    let numerator = c0 + (c1 * t) + (c2 * t * t);
    let denominator = 1.0 + (d1 * t) + (d2 * t * t) + (d3 * t * t * t);

    t - (numerator / denominator)
}

/// The family-wise error rate every comparison is judged against: a 5%
/// chance of *any* false positive across the whole planned suite.
pub(super) const FWER: f64 = 0.05;

/// Is `difference` big enough, against `std_error`, to call a change?
///
/// Takes the limit rather than computing it, so that the sampling loop and
/// [`crate::Comparison::is_changed`] are guaranteed to be asking the same
/// question: one is this predicate applied to the observed difference, the
/// other is it applied to the smallest difference worth detecting.
///
/// `z_alpha` is `NaN` when no comparisons were planned, and every
/// comparison against `NaN` is false - so nothing is ever reported as
/// changed until a plan is set.
pub(super) fn is_significant(difference: f64, std_error: f64, z_alpha: f64) -> bool {
    (difference / std_error).abs() > z_alpha
}
