/// Calculates the conservative two-tailed Bonferroni Z-limit.
///
/// * n - Total number of measurements being tested
/// * fwer - Target Family-Wise Error Rate (e.g., 0.05 for 95% confidence)
fn bonferroni_z_limit(n: u64, fwer: f64) -> f64 {
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

pub(crate) fn is_significant(
    difference: f64,
    std_error: f64,
    error_rate: f64,
    num_comparisons: u64,
) -> bool {
    (difference / std_error).abs() > bonferroni_z_limit(num_comparisons, error_rate)
}
