use super::*;
use std::fmt::{self, Display, Formatter};
use std::sync::atomic::Ordering::Relaxed;
use std::time::{Duration, Instant};

const MIN_SAMPLES: usize = 6;

const SAMPLE_TIME: Duration = Duration::from_micros(100);

const MAX_SAMPLES: usize = 1_000_000;

#[derive(Debug, PartialEq, Clone)]
pub struct Comparison {
    pub old: Stats,
    pub new: Stats,
}

impl Comparison {
    pub fn difference_ns(&self) -> f64 {
        self.new.ns_per_iter - self.old.ns_per_iter
    }
    pub fn std_error(&self) -> f64 {
        (self.new.std_error.powi(2) + self.old.std_error.powi(2)).sqrt()
    }
    pub fn is_changed(&self) -> bool {
        crate::significant::is_significant(self.difference_ns(), self.std_error(), 0.05)
    }
}

impl Display for Comparison {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        if self.is_changed() {
            let percent_change = self.difference_ns() / self.old.ns_per_iter * 100.0;
            let rel_error = self.std_error() / self.old.ns_per_iter * 100.0;
            write!(f, "{percent_change:+.1}% +/- {rel_error:.1}%")
        } else {
            write!(f, "(unchanged)")
        }
    }
}

impl Config {
    pub fn compare<OLD, NEW, O>(&self, mut f_old: OLD, mut f_new: NEW) -> Comparison
    where
        OLD: FnMut() -> O,
        NEW: FnMut() -> O,
    {
        self.compare_env((), |_| f_old(), |_| f_new())
    }

    pub fn compare_env<OLD, NEW, I, O>(&self, env: I, f_old: OLD, f_new: NEW) -> Comparison
    where
        OLD: FnMut(&mut I) -> O,
        NEW: FnMut(&mut I) -> O,
        I: Clone,
    {
        self.compare_gen_env(move || env.clone(), f_old, f_new)
    }

    pub fn compare_gen_env<G, OLD, NEW, I, O>(
        &self,
        mut gen_env: G,
        mut f_old: OLD,
        mut f_new: NEW,
    ) -> Comparison
    where
        G: FnMut() -> I,
        OLD: FnMut(&mut I) -> O,
        NEW: FnMut(&mut I) -> O,
    {
        crate::significant::NUM_MEASUREMENTS_PLANNED
            .fetch_max(self.num_comparisons_planned, Relaxed);
        crate::significant::NUM_MEASUREMENTS.fetch_add(1, Relaxed);
        quiet::pin_if_requested();
        let start = Instant::now();
        let mut xs: Vec<I> = Vec::new();
        let (unit, old_ns, new_ns, probed) =
            calibrate(&mut gen_env, &mut f_old, &mut f_new, &mut xs, self, start);
        if start.elapsed() > self.max_time {
            return Comparison {
                old: Stats {
                    ns_per_iter: old_ns / unit as f64,
                    std_error: f64::NAN,
                    iterations: probed,
                    samples: 1,
                    hit_limit: true,
                    untrustworthy: true,
                },
                new: Stats {
                    ns_per_iter: new_ns / unit as f64,
                    std_error: f64::NAN,
                    iterations: probed,
                    samples: 1,
                    hit_limit: true,
                    untrustworthy: true,
                },
            };
        }

        let mut old_samples = Running::default();
        let mut new_samples = Running::default();
        loop {
            let (_, old_t) = time_batch(&mut gen_env, &mut f_old, &mut xs, unit);
            let (_, new_t) = time_batch(&mut gen_env, &mut f_new, &mut xs, unit);
            old_samples.push(old_t / unit as f64);
            new_samples.push(new_t / unit as f64);

            let (old_mean, old_std_error) = old_samples.mean_and_stderr();
            let (new_mean, new_std_error) = new_samples.mean_and_stderr();

            let out_of_budget = old_samples.count >= MAX_SAMPLES || start.elapsed() > self.max_time;
            let std_error = (old_std_error.powi(2) + new_std_error.powi(2)).sqrt();
            let precise_enough = old_samples.count >= MIN_SAMPLES
                && self.accuracy_met(old_mean.min(new_mean), std_error);
            if precise_enough || out_of_budget {
                return Comparison {
                    old: Stats {
                        ns_per_iter: old_mean,
                        std_error: old_std_error,
                        iterations: probed + old_samples.count as u64 * unit as u64,
                        samples: old_samples.count,
                        hit_limit: !precise_enough,
                        untrustworthy: old_samples.count < MIN_SAMPLES,
                    },
                    new: Stats {
                        ns_per_iter: new_mean,
                        std_error: new_std_error,
                        iterations: probed + new_samples.count as u64 * unit as u64,
                        samples: new_samples.count,
                        hit_limit: !precise_enough,
                        untrustworthy: new_samples.count < MIN_SAMPLES,
                    },
                };
            }
        }
    }
}

fn calibrate<G, OLD, NEW, I, O>(
    gen_env: &mut G,
    f_old: &mut OLD,
    f_new: &mut NEW,
    xs: &mut Vec<I>,
    cfg: &Config,
    start: Instant,
) -> (usize, f64, f64, u64)
where
    G: FnMut() -> I,
    OLD: FnMut(&mut I) -> O,
    NEW: FnMut(&mut I) -> O,
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
        let (old_setup_ns, old_t) = time_batch(gen_env, f_old, xs, unit);
        let (new_setup_ns, new_t) = time_batch(gen_env, f_new, xs, unit);
        probed += unit as u64;
        let total_ns = old_setup_ns + new_setup_ns + old_t + new_t;
        if old_t + new_t >= target
            || total_ns >= probe_ceiling_ns
            || unit >= unit_cap
            || start.elapsed() > cfg.max_time
        {
            return (unit, old_t, new_t, probed);
        }
        let factor_time = (target / (old_t + new_t).max(1.0)).clamp(2.0, 100.0);
        let factor_safety = (probe_ceiling_ns / total_ns.max(1.0)).max(1.0);
        let factor = factor_time.min(factor_safety);
        unit = ((unit as f64 * factor).ceil() as usize)
            .max(unit + 1)
            .min(unit_cap);
    }
}
