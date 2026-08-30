/*!
Detecting and honouring a quiesced benchmarking environment.

Benchmark timings are only as stable as the machine underneath them. The
`quiet-bench` binary shipped with this crate reserves one or more CPUs for
benchmarking - moving other processes and interrupts off them, pinning the
clock frequency, and so on - and then advertises the reservation in the
environment:

```none
sudo quiet-bench reserve 2      # set the machine up, reserving CPU 2
quiet-bench run cargo test --release
sudo quiet-bench restore        # put everything back
```

`quiet-bench run` sets [`CPUS_VAR`] for the command it launches. Every
benchmark in this crate checks that variable and, if it is set, **pins
itself to those CPUs automatically** - so a program built against `scaling`
lands on the reserved CPU without needing a `taskset` wrapper, and without
any code change. Set [`NO_PIN_VAR`] to `1` to suppress that.

Pinning affects only the thread running the benchmark, and happens at most
once per thread. Nothing here does anything at all on non-Linux platforms,
or when [`CPUS_VAR`] is unset - which is the normal case for an ordinary
`cargo bench` on a developer machine.

Use [`status`] to find out what happened:

```
match scaling::quiet::status() {
    scaling::quiet::Status::Pinned { cpus } => {
        println!("running on reserved CPU(s) {cpus}");
    }
    other => println!("not pinned: {other}"),
}
```
*/

use std::fmt::{self, Display, Formatter};

/// Environment variable naming the reserved CPU list, e.g. `"2"` or
/// `"2,5-7"`. Set by `quiet-bench run`; read by every benchmark in this
/// crate.
pub const CPUS_VAR: &str = "SCALING_BENCH_CPUS";

/// Set this to `1` to stop `scaling` pinning itself even when [`CPUS_VAR`]
/// is set.
pub const NO_PIN_VAR: &str = "SCALING_NO_PIN";

/// Where `quiet-bench` records the reserved CPUs. On `/run`, which is a
/// tmpfs, so the record cannot survive a reboot and go stale.
pub const CPUS_PATH: &str = "/run/quiet-bench.cpus";

/// What `scaling` found when it looked for a quiesced environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// We are pinned to the reserved CPUs: measurements are as good as this
    /// machine can make them.
    Pinned {
        /// The reserved CPU list we are pinned to.
        cpus: String,
    },
    /// CPUs are reserved, but this thread is not confined to them - pinning
    /// was suppressed, or failed.
    Reserved {
        /// The reserved CPU list.
        cpus: String,
        /// What this thread is actually allowed to run on.
        affinity: String,
    },
    /// No reservation is in effect. Timings will include whatever else the
    /// machine is doing.
    NotQuiesced,
    /// Not Linux: this crate has no way to reserve or detect CPUs here.
    Unsupported,
}

impl Display for Status {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        match self {
            Status::Pinned { cpus } => write!(f, "pinned to reserved CPU(s) {cpus}"),
            Status::Reserved { cpus, affinity } => write!(
                f,
                "CPU(s) {cpus} are reserved but this thread may run on {affinity}"
            ),
            Status::NotQuiesced => write!(
                f,
                "machine is not quiesced (see `quiet-bench reserve`); \
                 timings will include whatever else it is doing"
            ),
            Status::Unsupported => write!(f, "CPU reservation is only supported on Linux"),
        }
    }
}

/// The reserved CPU list, if there is one.
///
/// Prefers [`CPUS_VAR`], which `quiet-bench run` sets for its child, and
/// falls back to [`CPUS_PATH`], so a benchmark launched some other way
/// still notices a machine-wide reservation.
pub fn reserved_cpus() -> Option<String> {
    if let Ok(v) = std::env::var(CPUS_VAR) {
        let v = v.trim().to_string();
        if !v.is_empty() {
            return Some(v);
        }
    }
    std::fs::read_to_string(CPUS_PATH)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// The CPUs this thread is currently allowed to run on, as a Linux CPU
/// list (e.g. `"0-1,3-15"`).
///
/// Asks the kernel with `sched_getaffinity`, the exact counterpart of the
/// `sched_setaffinity` in [`pin_current_thread`] - so it reports the same
/// thread that pinning affects. Reading `/proc/self/status` instead would
/// answer for the thread-*group leader*, which is a different thread
/// whenever a benchmark runs anywhere but the main thread - as it does in
/// every `#[test]`, since the test harness gives each test its own thread.
#[cfg(target_os = "linux")]
pub fn current_affinity() -> Option<String> {
    let mut set: libc::cpu_set_t = unsafe { std::mem::zeroed() };
    let rc = unsafe {
        libc::sched_getaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &mut set)
    };
    if rc != 0 {
        return None;
    }
    let cpus: Vec<usize> = (0..libc::CPU_SETSIZE as usize)
        .filter(|&c| unsafe { libc::CPU_ISSET(c, &set) })
        .collect();
    Some(format_cpu_list(&cpus))
}

/// The CPUs this thread is currently allowed to run on. Always `None` on
/// non-Linux platforms, which have no reservation to report against.
#[cfg(not(target_os = "linux"))]
pub fn current_affinity() -> Option<String> {
    None
}

/// Expand a Linux CPU list like `"1,3-5"` into `[1, 3, 4, 5]`.
///
/// Returns `Err` with a human-readable reason if the list is malformed.
pub fn parse_cpu_list(list: &str) -> Result<Vec<usize>, String> {
    let mut out = Vec::new();
    for part in list.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        match part.split_once('-') {
            Some((lo, hi)) => {
                let lo: usize = lo
                    .trim()
                    .parse()
                    .map_err(|_| format!("bad CPU number {:?}", lo.trim()))?;
                let hi: usize = hi
                    .trim()
                    .parse()
                    .map_err(|_| format!("bad CPU number {:?}", hi.trim()))?;
                if hi < lo {
                    return Err(format!("descending CPU range {part:?}"));
                }
                out.extend(lo..=hi);
            }
            None => out.push(
                part.parse()
                    .map_err(|_| format!("bad CPU number {part:?}"))?,
            ),
        }
    }
    if out.is_empty() {
        return Err("empty CPU list".to_string());
    }
    out.sort_unstable();
    out.dedup();
    Ok(out)
}

/// Render a set of CPU numbers as a Linux CPU list, collapsing runs
/// (`[1, 3, 4, 5]` becomes `"1,3-5"`).
pub fn format_cpu_list(cpus: &[usize]) -> String {
    let mut sorted = cpus.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    let mut parts: Vec<String> = Vec::new();
    let mut i = 0;
    while i < sorted.len() {
        let start = sorted[i];
        let mut end = start;
        while i + 1 < sorted.len() && sorted[i + 1] == end + 1 {
            i += 1;
            end = sorted[i];
        }
        if start == end {
            parts.push(start.to_string());
        } else {
            parts.push(format!("{start}-{end}"));
        }
        i += 1;
    }
    parts.join(",")
}

/// Pin the calling thread to `cpus`.
///
/// Only the calling thread is affected; other threads, and the process as a
/// whole, are left alone.
#[cfg(target_os = "linux")]
pub fn pin_current_thread(cpus: &[usize]) -> Result<(), std::io::Error> {
    unsafe {
        let mut set: libc::cpu_set_t = std::mem::zeroed();
        libc::CPU_ZERO(&mut set);
        for &c in cpus {
            if c < libc::CPU_SETSIZE as usize {
                libc::CPU_SET(c, &mut set);
            }
        }
        // A pid of 0 means "the calling thread".
        if libc::sched_setaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &set) != 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

/// Pin the calling thread to `cpus`. Always fails on non-Linux platforms.
#[cfg(not(target_os = "linux"))]
pub fn pin_current_thread(_cpus: &[usize]) -> Result<(), std::io::Error> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "CPU pinning is only supported on Linux",
    ))
}

/// What `scaling` found when it looked for a quiesced environment. See the
/// module docs.
pub fn status() -> Status {
    if !cfg!(target_os = "linux") {
        return Status::Unsupported;
    }
    let cpus = match reserved_cpus() {
        Some(c) => c,
        None => return Status::NotQuiesced,
    };
    let affinity = match current_affinity() {
        Some(a) => a,
        None => return Status::NotQuiesced,
    };
    // Compare as sets rather than as strings, since "2" and "2-2" name the
    // same CPU and the kernel is free to format its answer either way.
    match (parse_cpu_list(&cpus), parse_cpu_list(&affinity)) {
        (Ok(want), Ok(have)) if want == have => Status::Pinned { cpus },
        _ => Status::Reserved { cpus, affinity },
    }
}

/// Pin this thread to the reserved CPUs, if a reservation is advertised and
/// pinning has not been suppressed.
///
/// Called automatically by every benchmark in this crate, at most once per
/// thread. Doing it more than once is harmless but pointless, so the result
/// is remembered.
pub(crate) fn pin_if_requested() {
    thread_local! {
        static DONE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }
    DONE.with(|done| {
        if done.get() {
            return;
        }
        done.set(true);
        if !cfg!(target_os = "linux") {
            return;
        }
        if std::env::var(NO_PIN_VAR).map(|v| v == "1").unwrap_or(false) {
            return;
        }
        // Only an explicit request in the environment triggers pinning. The
        // file at CPUS_PATH says a reservation exists, but not that *this*
        // program was meant to have it; silently seizing a reserved CPU
        // because one happens to be set aside would be an unpleasant
        // surprise for an unrelated process.
        let cpus = match std::env::var(CPUS_VAR) {
            Ok(c) => c,
            Err(_) => return,
        };
        if let Ok(cpus) = parse_cpu_list(&cpus) {
            // A failure here is not worth aborting a benchmark over: the
            // measurement is still valid, just noisier. `status()` reports
            // it as `Reserved` rather than `Pinned`.
            let _ = pin_current_thread(&cpus);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_lists_round_trip() {
        assert_eq!(parse_cpu_list("2").unwrap(), vec![2]);
        assert_eq!(parse_cpu_list("2,3").unwrap(), vec![2, 3]);
        assert_eq!(parse_cpu_list("1,3-5").unwrap(), vec![1, 3, 4, 5]);
        assert_eq!(parse_cpu_list(" 0-1 , 3 ").unwrap(), vec![0, 1, 3]);
        // Duplicates and disorder are tolerated and normalised, since these
        // lists come from sysfs and from humans.
        assert_eq!(parse_cpu_list("3,1,3").unwrap(), vec![1, 3]);

        assert_eq!(format_cpu_list(&[2]), "2");
        assert_eq!(format_cpu_list(&[1, 3, 4, 5]), "1,3-5");
        assert_eq!(format_cpu_list(&[0, 1, 3, 4, 5, 9]), "0-1,3-5,9");
        assert_eq!(format_cpu_list(&[5, 1, 4, 3]), "1,3-5");
    }

    #[test]
    fn bad_cpu_lists_are_rejected() {
        assert!(parse_cpu_list("").is_err());
        assert!(parse_cpu_list("  ").is_err());
        assert!(parse_cpu_list("five").is_err());
        assert!(parse_cpu_list("5-1").is_err());
        assert!(parse_cpu_list("1-").is_err());
    }

    #[test]
    fn status_is_consistent_with_the_environment() {
        // Whatever this machine's state, `status()` must agree with the
        // pieces it is derived from rather than panicking or contradicting
        // itself.
        match status() {
            // Reaching this on Linux would mean `status()` contradicted its
            // own platform check.
            #[cfg(target_os = "linux")]
            Status::Unsupported => panic!("Linux should never report Unsupported"),
            #[cfg(not(target_os = "linux"))]
            Status::Unsupported => {}
            Status::NotQuiesced => {}
            Status::Pinned { cpus } | Status::Reserved { cpus, .. } => {
                assert_eq!(Some(&cpus), reserved_cpus().as_ref());
                assert!(parse_cpu_list(&cpus).is_ok());
            }
        }
    }
}
