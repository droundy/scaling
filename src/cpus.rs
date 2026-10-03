//! The small pieces of the quiesced-machine machinery that the library and the
//! `quiet-bench` binary both need: where the reservation is recorded, reading
//! and writing a Linux CPU list, and pinning a thread.
//!
//! Compiled into both rather than published: the binary is the only
//! outsider that needs them, and making them public to reach it would make
//! them part of this crate's API. The binary includes this file by path.

/// Where `quiet-bench` records the reserved CPUs. On `/run`, which is a
/// tmpfs, so the record cannot survive a reboot and go stale.
pub(crate) const CPUS_PATH: &str = "/run/quiet-bench.cpus";

/// Expand a Linux CPU list like `"1,3-5"` into `[1, 3, 4, 5]`.
///
/// Returns `Err` with a human-readable reason if the list is malformed.
pub(crate) fn parse_cpu_list(list: &str) -> Result<Vec<usize>, String> {
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
pub(crate) fn format_cpu_list(cpus: &[usize]) -> String {
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

/// Pin the thread with kernel task id `tid` to `cpus`. A `tid` of 0 means
/// the calling thread.
///
/// The one place the affinity mask is built, because getting it wrong is a
/// memory-safety question rather than a behavioural one: `CPU_SET` writes
/// into a fixed-size bitmap, so a cpu id past its end has to be dropped
/// here rather than written past the end of the set.
#[cfg(target_os = "linux")]
pub(crate) fn pin_thread(tid: i32, cpus: &[usize]) -> Result<(), std::io::Error> {
    unsafe {
        let mut set: libc::cpu_set_t = std::mem::zeroed();
        libc::CPU_ZERO(&mut set);
        for &c in cpus {
            if c < libc::CPU_SETSIZE as usize {
                libc::CPU_SET(c, &mut set);
            }
        }
        if libc::sched_setaffinity(tid, std::mem::size_of::<libc::cpu_set_t>(), &set) != 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

/// Pin the thread with kernel task id `tid` to `cpus`. Always fails on
/// non-Linux platforms.
#[cfg(not(target_os = "linux"))]
pub(crate) fn pin_thread(_tid: i32, _cpus: &[usize]) -> Result<(), std::io::Error> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "CPU pinning is only supported on Linux",
    ))
}
