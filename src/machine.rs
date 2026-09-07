//! What the machine was doing around a sample.
//!
//! The rest of this crate infers a measurement's quality from the sample
//! timings themselves: the spread between batches becomes the `±`, and that
//! is the end of it. That inference has a blind spot it cannot close from
//! the inside. A run that was *uniformly* slow - the clock dropped for its
//! whole duration, or every batch shared the core with something else - has
//! every sample wrong by the same factor, so the spread between them stays
//! tight and [`Stats::std_error`](crate::Stats::std_error) reports a
//! confident error bar on a number that is off by a factor of two. Whole
//! runs at twice the cost have been seen on the machine this was developed
//! on, unquiesced.
//!
//! No statistic computed from the timings can see that, because the timings
//! are all it has and they agree with each other. The system, on the other
//! hand, knows. This module is where we ask it: what clock frequency was
//! the core at, which core was it, and (later) how much of the wall time did
//! this thread actually spend on a CPU. The answers are recorded beside the
//! measurement rather than used to correct it.
//!
//! Everything here is Linux-only and degrades to "we could not tell" rather
//! than to an error: on any other platform, and on a Linux without the
//! sysfs files, [`Probes`] reads nothing and reports nothing moved. Silence
//! from an instrument that is not there must not be mistaken for a clean
//! measurement, which is why the flags this feeds are named after what was
//! *seen* to move.

/// A monotonic clock for the measurements themselves.
///
/// On Linux this is `CLOCK_MONOTONIC_RAW` rather than the
/// `CLOCK_MONOTONIC` behind [`std::time::Instant`]. The two cost the same
/// (26.5 ns against 26.8 ns, measured), and both are read from the vDSO
/// without entering the kernel, but `CLOCK_MONOTONIC` is steered by NTP -
/// slewed by up to 500 parts per million, or 0.05%, to bring the system
/// clock into agreement with a time server. `CLOCK_MONOTONIC_RAW` is the
/// unsteered hardware counter.
///
/// This is a small effect and an honest one: 0.05% is well below the 1%
/// accuracy asked for by default, it cancels out of any comparison of two
/// benchmarks measured under the same slew, and it shows up only in
/// absolute figures. It is corrected here because it is free to correct,
/// not because it was ever the limiting error.
///
/// Everywhere else this is an `Instant`, which is the best that platform
/// offers.
#[derive(Clone, Copy)]
pub(crate) struct Timer(TimerRepr);

#[cfg(target_os = "linux")]
type TimerRepr = u64;
#[cfg(not(target_os = "linux"))]
type TimerRepr = std::time::Instant;

impl Timer {
    /// Start the clock.
    #[cfg(target_os = "linux")]
    pub(crate) fn start() -> Timer {
        Timer(monotonic_raw_ns())
    }

    /// Start the clock.
    #[cfg(not(target_os = "linux"))]
    pub(crate) fn start() -> Timer {
        Timer(std::time::Instant::now())
    }

    /// Nanoseconds since [`Timer::start`].
    #[cfg(target_os = "linux")]
    pub(crate) fn elapsed_ns(&self) -> f64 {
        monotonic_raw_ns().saturating_sub(self.0) as f64
    }

    /// Nanoseconds since [`Timer::start`].
    #[cfg(not(target_os = "linux"))]
    pub(crate) fn elapsed_ns(&self) -> f64 {
        self.0.elapsed().as_secs_f64() * 1e9
    }
}

/// `CLOCK_MONOTONIC_RAW` in nanoseconds.
///
/// The call cannot fail for a clock id the kernel knows, and a kernel old
/// enough not to know this one (pre-2.6.28) cannot run a current Rust
/// binary anyway - so a failure leaves the zeroed `timespec` alone and
/// reads as time standing still, which the callers here treat as a
/// zero-length interval rather than as a panic in the middle of a
/// measurement.
#[cfg(target_os = "linux")]
fn monotonic_raw_ns() -> u64 {
    clock_ns(libc::CLOCK_MONOTONIC_RAW)
}

#[cfg(target_os = "linux")]
fn clock_ns(clock: libc::clockid_t) -> u64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `ts` is a valid, initialised `timespec` we own.
    unsafe { libc::clock_gettime(clock, &mut ts) };
    (ts.tv_sec as u64)
        .wrapping_mul(1_000_000_000)
        .wrapping_add(ts.tv_nsec as u64)
}

/// What the system said about the conditions around one edge of a sample.
///
/// Cheap to take and cheap to copy: two of these bracket a batch, and the
/// difference between them is what gets recorded.
#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub(crate) struct Reading {
    /// Which CPU this thread was on, or `-1` where that is unknown.
    pub cpu: i32,
    /// The core's current clock frequency in kHz, or `0` where unknown.
    pub khz: u64,
}

/// The instruments we hold open for the duration of one benchmark run.
///
/// Held open, rather than opened per sample, because that is the whole
/// difference between affordable and not: opening, reading and closing
/// `scaling_cur_freq` costs 7.7 microseconds, which against a 100
/// microsecond batch is 7.7% of the sample - a bigger perturbation than
/// most of what we are trying to detect. Holding the descriptor and using
/// `pread` costs 0.5-0.9 microseconds depending on how fast the core is
/// running at the time, and two of those bracket a batch for well under 2%
/// of its wall time, none of it inside the timed region.
pub(crate) struct Probes {
    /// An open descriptor on the current CPU's `scaling_cur_freq`, and the
    /// CPU it belongs to. Reopened when this thread migrates, since the
    /// file is per-core.
    #[cfg(target_os = "linux")]
    freq: Option<(std::os::unix::io::RawFd, i32)>,
    /// The lowest and highest frequency seen so far, in kHz, and whether
    /// this thread has been seen on more than one CPU. `0` for `lo` means
    /// nothing has been read yet.
    lo_khz: u64,
    hi_khz: u64,
    first_cpu: Option<i32>,
    migrated: bool,
}

/// How far the reported clock frequency may wander before we call it a
/// change.
///
/// Not zero, because `scaling_cur_freq` does not hold still even when the
/// hardware does. Under `intel_pstate` it is an APERF/MPERF average, and
/// consecutive reads of a core pinned at a nominal 1.6 GHz come back as
/// 1599983, 1600016, 1600048 kHz - a wander of a few tens of kHz, or about
/// 0.005%. Exact comparison would flag essentially every run.
///
/// One percent, because that is where a frequency change starts to matter
/// against the accuracy this crate asks for by default: a 1% shift in clock
/// moves the answer by about 1%, which is the whole of the default error
/// budget, while the 0.005% the instrument invents on its own is two
/// hundred times below it. The failure this is really for - a run that
/// spent part of its time at half speed - clears the bar by a factor of
/// fifty.
const KHZ_TOLERANCE: f64 = 0.01;

/// Did the clock move between the lowest and highest readings of a run?
///
/// Split out from [`Probes`] so it can be tested without a machine that
/// will co-operate: the decision is arithmetic, and the hard part of
/// testing it is arranging for a real CPU to change frequency on cue.
fn khz_moved(lo_khz: u64, hi_khz: u64) -> bool {
    // Nothing was ever read: sysfs is absent, or this is not Linux. We saw
    // no movement because we were not looking, which is not the same as
    // there having been none - hence "moved", not "steady".
    if lo_khz == 0 {
        return false;
    }
    (hi_khz as f64) > (lo_khz as f64) * (1.0 + KHZ_TOLERANCE)
}

impl Probes {
    /// Open the instruments for one benchmark run.
    pub(crate) fn new() -> Probes {
        Probes {
            #[cfg(target_os = "linux")]
            freq: None,
            lo_khz: 0,
            hi_khz: 0,
            first_cpu: None,
            migrated: false,
        }
    }

    /// Throw away everything recorded so far, keeping the descriptors open.
    ///
    /// For the boundary between warmup and measurement: calibration runs
    /// the benchmark for real, so it is exactly when an idle core wakes up
    /// and climbs to its working frequency, and that climb is not something
    /// the run that follows should be marked for.
    pub(crate) fn forget(&mut self) {
        self.lo_khz = 0;
        self.hi_khz = 0;
        self.first_cpu = None;
        self.migrated = false;
    }

    /// Ask the system what it is doing right now, folding the answer into
    /// the run-level record as well as returning it.
    #[cfg(target_os = "linux")]
    pub(crate) fn read(&mut self) -> Reading {
        // SAFETY: `sched_getcpu` takes no arguments and cannot fail
        // destructively; it returns -1 on the platforms that lack it.
        let cpu = unsafe { libc::sched_getcpu() };
        match self.first_cpu {
            None => self.first_cpu = Some(cpu),
            Some(first) if first != cpu => self.migrated = true,
            Some(_) => {}
        }
        let khz = self.read_khz(cpu);
        if khz != 0 {
            if self.lo_khz == 0 || khz < self.lo_khz {
                self.lo_khz = khz;
            }
            if khz > self.hi_khz {
                self.hi_khz = khz;
            }
        }
        Reading { cpu, khz }
    }

    /// Ask the system what it is doing right now. Off Linux there is
    /// nothing to ask.
    #[cfg(not(target_os = "linux"))]
    pub(crate) fn read(&mut self) -> Reading {
        Reading { cpu: -1, khz: 0 }
    }

    /// The current core's clock frequency in kHz, or `0` if it cannot be
    /// read.
    ///
    /// Keeps one descriptor open and `pread`s it. The descriptor belongs to
    /// a particular core, so a migration means reopening - which costs the
    /// full 7.7 microseconds, and is why [`Probes::migrated`] exists: a
    /// migration invalidates the frequency comparison anyway, since the two
    /// readings are then of two different cores, which on a hybrid part may
    /// not even share a maximum frequency.
    #[cfg(target_os = "linux")]
    fn read_khz(&mut self, cpu: i32) -> u64 {
        if cpu < 0 {
            return 0;
        }
        match self.freq {
            Some((_, open_for)) if open_for == cpu => {}
            _ => {
                self.close_freq();
                let path = format!("/sys/devices/system/cpu/cpu{cpu}/cpufreq/scaling_cur_freq\0");
                // SAFETY: `path` is NUL-terminated and outlives the call.
                let fd = unsafe {
                    libc::open(
                        path.as_ptr() as *const libc::c_char,
                        libc::O_RDONLY | libc::O_CLOEXEC,
                    )
                };
                if fd < 0 {
                    // No cpufreq for this core: a VM, or a kernel built
                    // without it. Report "unknown" for the rest of the run
                    // rather than retrying every sample, which would cost a
                    // failed `open` per sample forever.
                    return 0;
                }
                self.freq = Some((fd, cpu));
            }
        }
        let (fd, _) = match self.freq {
            Some(f) => f,
            None => return 0,
        };
        // Twenty-odd bytes is ample: the file holds a decimal kHz count and
        // a newline, and the largest frequency anyone has is seven digits.
        let mut buf = [0u8; 24];
        // SAFETY: `buf` is a live, writable buffer of the length passed,
        // and `fd` is one we opened and have not closed.
        let n = unsafe { libc::pread(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len() - 1, 0) };
        if n <= 0 {
            return 0;
        }
        std::str::from_utf8(&buf[..n as usize])
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0)
    }

    #[cfg(target_os = "linux")]
    fn close_freq(&mut self) {
        if let Some((fd, _)) = self.freq.take() {
            // SAFETY: `fd` came from `open` above and is closed once.
            unsafe { libc::close(fd) };
        }
    }

    /// Did the clock move under this run?
    ///
    /// True when the core's reported frequency wandered by more than
    /// [`KHZ_TOLERANCE`] across the whole run, or when the thread was seen
    /// on more than one CPU - a migration invalidates the frequency
    /// comparison, and on a hybrid part moves the work between core types
    /// that are not even the same speed at the same frequency.
    ///
    /// Deliberately a statement about the *run* and not about any one
    /// sample. Under `intel_pstate`, `scaling_cur_freq` is an average over
    /// the driver's own sampling interval, which can be coarser than a 100
    /// microsecond batch - so two readings that bracket one sample may
    /// simply be the same stale average, and reading no change there is
    /// weak evidence. Over a run of thousands of samples the same
    /// instrument is entirely adequate, and the failure it is there to
    /// catch - a run that spent its time at a different speed than it
    /// looked - is a run-level failure.
    ///
    /// Never true where nothing could be read: off Linux, or without
    /// cpufreq. See the module docs on why that is a silence and not a
    /// clean bill of health.
    pub(crate) fn clock_moved(&self) -> bool {
        self.migrated || khz_moved(self.lo_khz, self.hi_khz)
    }
}

impl Drop for Probes {
    fn drop(&mut self) {
        #[cfg(target_os = "linux")]
        self.close_freq();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_frequency_tolerance_ignores_jitter_and_catches_throttling() {
        // Nothing read at all is not evidence of steadiness.
        assert!(!khz_moved(0, 0));
        // Bit-identical readings, the ordinary quiesced case.
        assert!(!khz_moved(1_600_000, 1_600_000));
        // The wander `intel_pstate` shows on a core pinned at 1.6 GHz:
        // measured 1599983 to 1600048, about 0.004%.
        assert!(!khz_moved(1_599_983, 1_600_048));
        // Half a percent: real, but below what the default accuracy target
        // would notice, and not worth a mark that would then appear on
        // every line.
        assert!(!khz_moved(1_600_000, 1_608_000));
        // Two percent, and the case that matters: a core that spent part of
        // the run at 900 MHz and the rest at 1.6 GHz.
        assert!(khz_moved(1_600_000, 1_632_000));
        assert!(khz_moved(925_779, 1_600_032));
    }

    #[test]
    fn the_timer_measures_forward() {
        // No timing assertion beyond monotonicity and finiteness: this is
        // checking that the clock is wired up, not how fast anything is.
        let t = Timer::start();
        let a = t.elapsed_ns();
        let b = t.elapsed_ns();
        assert!(a.is_finite() && a >= 0.0, "{a}");
        assert!(b >= a, "{b} < {a}");
    }

    #[test]
    fn probes_agree_with_themselves() {
        let mut probes = Probes::new();
        // Nothing has been read, so nothing can have moved.
        assert!(!probes.clock_moved());
        let first = probes.read();
        for _ in 0..100 {
            let r = probes.read();
            // Either the whole instrument is absent, or it keeps working:
            // a reading that comes back 0 kHz after one that did not would
            // mean the descriptor went bad mid-run.
            if first.khz != 0 {
                assert!(r.khz != 0, "frequency reading stopped working");
                // A plausible clock speed, in kHz. This is a sanity check
                // on the parse - a stray newline or a byte count off by one
                // would land far outside.
                assert!(
                    (100_000..20_000_000).contains(&r.khz),
                    "implausible frequency {} kHz",
                    r.khz
                );
            }
            if cfg!(target_os = "linux") {
                assert!(r.cpu >= 0, "sched_getcpu failed");
            }
        }
    }
}
