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
    num_comparisons_planned: u64,
}

impl Comparison {
    pub fn difference_ns(&self) -> f64 {
        self.candidate.ns_per_iter - self.baseline.ns_per_iter
    }
    pub fn std_error(&self) -> f64 {
        (self.candidate.std_error.powi(2) + self.baseline.std_error.powi(2)).sqrt()
    }
    pub fn is_changed(&self) -> bool {
        crate::significant::is_significant(
            self.difference_ns(),
            self.std_error(),
            0.05,
            self.num_comparisons_planned,
        )
    }
}

impl Display for Comparison {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        if self.is_changed() {
            let percent_change = self.difference_ns() / self.baseline.ns_per_iter * 100.0;
            let rel_error = self.std_error() / self.baseline.ns_per_iter * 100.0;
            write!(f, "{percent_change:+.1}% +/- {rel_error:.1}%")
        } else {
            write!(f, "(unchanged)")
        }
    }
}

impl Config {
    pub fn compare<BASE, CAND, O>(&self, mut f_base: BASE, mut f_cand: CAND) -> Comparison
    where
        BASE: FnMut() -> O,
        CAND: FnMut() -> O,
    {
        self.compare_env((), |_| f_base(), |_| f_cand())
    }

    pub fn compare_env<BASE, CAND, I, O>(&self, env: I, f_base: BASE, f_cand: CAND) -> Comparison
    where
        BASE: FnMut(&mut I) -> O,
        CAND: FnMut(&mut I) -> O,
        I: Clone,
    {
        self.compare_gen_env(move || env.clone(), f_base, f_cand)
    }

    pub fn compare_gen_env<G, BASE, CAND, I, O>(
        &self,
        mut gen_env: G,
        mut f_base: BASE,
        mut f_cand: CAND,
    ) -> Comparison
    where
        G: FnMut() -> I,
        BASE: FnMut(&mut I) -> O,
        CAND: FnMut(&mut I) -> O,
    {
        self.num_comparisons_made.fetch_add(1, Release);
        quiet::pin_if_requested();
        let start = Instant::now();
        let mut xs: Vec<I> = Vec::new();
        let (unit, base_ns, cand_ns, probed) =
            calibrate(&mut gen_env, &mut f_base, &mut f_cand, &mut xs, self, start);
        if start.elapsed() > self.max_time {
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
                num_comparisons_planned: self.num_comparisons_planned,
            };
        }

        let mut base_samples = Running::default();
        let mut cand_samples = Running::default();
        loop {
            let (_, base_t) = time_batch(&mut gen_env, &mut f_base, &mut xs, unit);
            let (_, cand_t) = time_batch(&mut gen_env, &mut f_cand, &mut xs, unit);
            base_samples.push(base_t / unit as f64);
            cand_samples.push(cand_t / unit as f64);

            let (base_mean, base_std_error) = base_samples.mean_and_stderr();
            let (cand_mean, cand_std_error) = cand_samples.mean_and_stderr();

            let out_of_budget =
                base_samples.count >= MAX_SAMPLES || start.elapsed() > self.max_time;
            let std_error = (base_std_error.powi(2) + cand_std_error.powi(2)).sqrt();
            let precise_enough = base_samples.count >= MIN_SAMPLES
                && self.accuracy_met(base_mean.min(cand_mean), std_error);
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
                    num_comparisons_planned: self.num_comparisons_planned,
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

fn calibrate<G, BASE, CAND, I, O>(
    gen_env: &mut G,
    f_base: &mut BASE,
    f_cand: &mut CAND,
    xs: &mut Vec<I>,
    cfg: &Config,
    start: Instant,
) -> (usize, f64, f64, u64)
where
    G: FnMut() -> I,
    BASE: FnMut(&mut I) -> O,
    CAND: FnMut(&mut I) -> O,
{
    let probe_ceiling_ns = (cfg.max_time / 100)
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
        let (base_setup_ns, base_t) = time_batch(gen_env, f_base, xs, unit);
        let (cand_setup_ns, cand_t) = time_batch(gen_env, f_cand, xs, unit);
        probed += unit as u64;
        let total_ns = base_setup_ns + cand_setup_ns + base_t + cand_t;
        if base_t + cand_t >= target
            || total_ns >= probe_ceiling_ns
            || unit >= unit_cap
            || start.elapsed() > cfg.max_time
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
