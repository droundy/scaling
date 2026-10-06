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

/// The two-tailed Bonferroni limit for a family of `n` comparisons, each
/// judged by Student's t with `df` degrees of freedom.
///
/// The normal limit, [`bonferroni_z_limit`], assumes the standard error is
/// known exactly. A standard error estimated from a handful of rounds is not,
/// and at the tail probabilities a large family needs the difference is
/// large: for 1,500 comparisons the normal limit is 4.17, but from 8 rounds
/// (7 degrees of freedom) it is 9.36. Using the normal limit there would
/// make false positives several times likelier than the family-wise rate
/// promises.
pub(super) fn bonferroni_t_limit(n: u64, fwer: f64, df: f64) -> f64 {
    // `df` is NaN when there was nothing to estimate from, which this also
    // refuses.
    if n == 0 || df.is_nan() || df <= 0.0 {
        return f64::NAN;
    }
    let alpha = fwer / n as f64;
    // P(|T| > t) falls as t grows; bisect for the t where it equals alpha.
    let (mut lo, mut hi) = (0.0f64, 1e4f64);
    for _ in 0..200 {
        let mid = 0.5 * (lo + hi);
        if two_sided_t_tail(mid, df) > alpha {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    0.5 * (lo + hi)
}

/// P(|T| > t) for Student's t with `df` degrees of freedom.
fn two_sided_t_tail(t: f64, df: f64) -> f64 {
    regularized_incomplete_beta(df / 2.0, 0.5, df / (df + t * t))
}

/// The regularized incomplete beta function I_x(a, b), by its continued
/// fraction (Numerical Recipes' `betai`).
fn regularized_incomplete_beta(a: f64, b: f64, x: f64) -> f64 {
    if x <= 0.0 {
        return 0.0;
    }
    if x >= 1.0 {
        return 1.0;
    }
    let ln_front = ln_gamma(a + b) - ln_gamma(a) - ln_gamma(b) + a * x.ln() + b * (1.0 - x).ln();
    if x < (a + 1.0) / (a + b + 2.0) {
        ln_front.exp() * beta_continued_fraction(a, b, x) / a
    } else {
        1.0 - ln_front.exp() * beta_continued_fraction(b, a, 1.0 - x) / b
    }
}

fn beta_continued_fraction(a: f64, b: f64, x: f64) -> f64 {
    const TINY: f64 = 1e-300;
    let (qab, qap, qam) = (a + b, a + 1.0, a - 1.0);
    let mut c = 1.0;
    let mut d = 1.0 - qab * x / qap;
    if d.abs() < TINY {
        d = TINY;
    }
    d = 1.0 / d;
    let mut h = d;
    for m in 1..300 {
        let m = m as f64;
        let m2 = 2.0 * m;
        for aa in [
            m * (b - m) * x / ((qam + m2) * (a + m2)),
            -(a + m) * (qab + m) * x / ((a + m2) * (qap + m2)),
        ] {
            d = 1.0 + aa * d;
            if d.abs() < TINY {
                d = TINY;
            }
            c = 1.0 + aa / c;
            if c.abs() < TINY {
                c = TINY;
            }
            d = 1.0 / d;
            h *= d * c;
        }
        if (d * c - 1.0).abs() < 1e-14 {
            break;
        }
    }
    h
}

/// ln Γ(x) for x > 0, by Lanczos' approximation.
fn ln_gamma(x: f64) -> f64 {
    const G: [f64; 9] = [
        0.999_999_999_999_809_9,
        676.520_368_121_885_1,
        -1_259.139_216_722_402_8,
        771.323_428_777_653_1,
        -176.615_029_162_140_6,
        12.507_343_278_686_905,
        -0.138_571_095_265_720_12,
        9.984_369_578_019_572e-6,
        1.505_632_735_149_311_6e-7,
    ];
    if x < 0.5 {
        // Reflection, so the approximation is only ever used where it is good.
        return (std::f64::consts::PI / (std::f64::consts::PI * x).sin()).ln() - ln_gamma(1.0 - x);
    }
    let x = x - 1.0;
    let mut a = G[0];
    let t = x + 7.5;
    for (i, g) in G.iter().enumerate().skip(1) {
        a += g / (x + i as f64);
    }
    0.5 * (2.0 * std::f64::consts::PI).ln() + (x + 0.5) * t.ln() - t + a.ln()
}

/// Is `difference` big enough, against `std_error`, to call a change?
///
/// Takes the limit rather than computing it, so that the sampling loop and
/// [`crate::Timing::is_changed`] are guaranteed to be asking the same
/// question: one is this predicate applied to the observed difference, the
/// other is it applied to the smallest difference worth detecting.
///
/// `z_alpha` is `NaN` when no comparisons were planned, and every
/// comparison against `NaN` is false - so nothing is ever reported as
/// changed until a plan is set.
pub(super) fn is_significant(difference: f64, std_error: f64, z_alpha: f64) -> bool {
    (difference / std_error).abs() > z_alpha
}
