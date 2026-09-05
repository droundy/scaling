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
    pub fn compare<BASELINE, CANDIDATE, O>(
        &self,
        mut f_baseline: BASELINE,
        mut f_candidate: CANDIDATE,
    ) -> Comparison
    where
        BASELINE: FnMut() -> O,
        CANDIDATE: FnMut() -> O,
    {
        self.compare_env((), |_| f_baseline(), |_| f_candidate())
    }

    pub fn compare_env<BASELINE, CANDIDATE, I, O>(
        &self,
        env: I,
        f_baseline: BASELINE,
        f_candidate: CANDIDATE,
    ) -> Comparison
    where
        BASELINE: FnMut(&mut I) -> O,
        CANDIDATE: FnMut(&mut I) -> O,
        I: Clone,
    {
        self.compare_gen_env(move || env.clone(), f_baseline, f_candidate)
    }

    pub fn compare_gen_env<G, BASELINE, CANDIDATE, I, O>(
        &self,
        mut gen_env: G,
        mut f_baseline: BASELINE,
        mut f_candidate: CANDIDATE,
    ) -> Comparison
    where
        G: FnMut() -> I,
        BASELINE: FnMut(&mut I) -> O,
        CANDIDATE: FnMut(&mut I) -> O,
    {
        self.num_comparisons_made.fetch_add(1, Release);
        quiet::pin_if_requested();
        let start = Instant::now();
        let mut xs: Vec<I> = Vec::new();
        let (unit, baseline_ns, candidate_ns, probed) = calibrate(
            &mut gen_env,
            &mut f_baseline,
            &mut f_candidate,
            &mut xs,
            self,
            start,
        );
        if start.elapsed() > self.max_time {
            return Comparison {
                baseline: Stats {
                    ns_per_iter: baseline_ns / unit as f64,
                    std_error: f64::NAN,
                    iterations: probed,
                    samples: 1,
                    hit_limit: true,
                    untrustworthy: true,
                },
                candidate: Stats {
                    ns_per_iter: candidate_ns / unit as f64,
                    std_error: f64::NAN,
                    iterations: probed,
                    samples: 1,
                    hit_limit: true,
                    untrustworthy: true,
                },
                num_comparisons_planned: self.num_comparisons_planned,
            };
        }

        let mut baseline_samples = Running::default();
        let mut candidate_samples = Running::default();
        loop {
            let (_, baseline_t) = time_batch(&mut gen_env, &mut f_baseline, &mut xs, unit);
            let (_, candidate_t) = time_batch(&mut gen_env, &mut f_candidate, &mut xs, unit);
            baseline_samples.push(baseline_t / unit as f64);
            candidate_samples.push(candidate_t / unit as f64);

            let (baseline_mean, baseline_std_error) = baseline_samples.mean_and_stderr();
            let (candidate_mean, candidate_std_error) = candidate_samples.mean_and_stderr();

            let out_of_budget =
                baseline_samples.count >= MAX_SAMPLES || start.elapsed() > self.max_time;
            let std_error = (baseline_std_error.powi(2) + candidate_std_error.powi(2)).sqrt();
            let precise_enough = baseline_samples.count >= MIN_SAMPLES
                && self.accuracy_met(baseline_mean.min(candidate_mean), std_error);
            if precise_enough || out_of_budget {
                return Comparison {
                    baseline: Stats {
                        ns_per_iter: baseline_mean,
                        std_error: baseline_std_error,
                        iterations: probed + baseline_samples.count as u64 * unit as u64,
                        samples: baseline_samples.count,
                        hit_limit: !precise_enough,
                        untrustworthy: baseline_samples.count < MIN_SAMPLES,
                    },
                    candidate: Stats {
                        ns_per_iter: candidate_mean,
                        std_error: candidate_std_error,
                        iterations: probed + candidate_samples.count as u64 * unit as u64,
                        samples: candidate_samples.count,
                        hit_limit: !precise_enough,
                        untrustworthy: candidate_samples.count < MIN_SAMPLES,
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

fn calibrate<G, BASELINE, CANDIDATE, I, O>(
    gen_env: &mut G,
    f_baseline: &mut BASELINE,
    f_candidate: &mut CANDIDATE,
    xs: &mut Vec<I>,
    cfg: &Config,
    start: Instant,
) -> (usize, f64, f64, u64)
where
    G: FnMut() -> I,
    BASELINE: FnMut(&mut I) -> O,
    CANDIDATE: FnMut(&mut I) -> O,
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
        let (baseline_setup_ns, baseline_t) = time_batch(gen_env, f_baseline, xs, unit);
        let (candidate_setup_ns, candidate_t) = time_batch(gen_env, f_candidate, xs, unit);
        probed += unit as u64;
        let total_ns = baseline_setup_ns + candidate_setup_ns + baseline_t + candidate_t;
        if baseline_t + candidate_t >= target
            || total_ns >= probe_ceiling_ns
            || unit >= unit_cap
            || start.elapsed() > cfg.max_time
        {
            return (unit, baseline_t, candidate_t, probed);
        }
        let factor_time = (target / (baseline_t + candidate_t).max(1.0)).clamp(2.0, 100.0);
        let factor_safety = (probe_ceiling_ns / total_ns.max(1.0)).max(1.0);
        let factor = factor_time.min(factor_safety);
        unit = ((unit as f64 * factor).ceil() as usize)
            .max(unit + 1)
            .min(unit_cap);
    }
}
