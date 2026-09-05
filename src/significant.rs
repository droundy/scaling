/// Calculates the conservative two-tailed Bonferroni Z-limit.
///
/// * n - Total number of measurements being tested
/// * fwer - Target Family-Wise Error Rate (e.g., 0.05 for 95% confidence)
fn bonferroni_z_limit(n: u64, fwer: f64) -> f64 {
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

pub(crate) static NUM_MEASUREMENTS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
pub(crate) static NUM_MEASUREMENTS_PLANNED: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

pub(crate) fn is_significant(difference: f64, std_error: f64, error_rate: f64) -> bool {
    let measurements = NUM_MEASUREMENTS_PLANNED.load(std::sync::atomic::Ordering::Relaxed);
    let count = NUM_MEASUREMENTS.load(std::sync::atomic::Ordering::Relaxed);
    if count > measurements {
        println!("You should plan for at least {count} comparisons!");
    }
    (difference / std_error).abs() > bonferroni_z_limit(measurements, error_rate)
}
