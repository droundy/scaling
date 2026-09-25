//! Memory canary: a pointer chase over a table far larger than L3.
//!
//! Like the CPU canary it has no input to generate, so it is a
//! [`Workload::simple`] and the batch size is the number of chase steps. But
//! unlike the CPU canary it must *not* do identical work every batch: the
//! start moves, and that is load-bearing.
//!
//! A 100 us chase is only some 700 dependent loads, touching ~45 KiB of
//! cache lines. Start from the same place every batch and that window is
//! resident in L2 by the second one, so it reads ~23 ns a step instead of
//! ~142 ns - perfectly steady, and measuring the wrong level of the
//! hierarchy. Moving the start sweeps the window through the table so every
//! access genuinely misses. This has been got wrong twice; the failure is
//! silent and looks like a quieter machine.
//!
//! The table and the cursor are captured by the closure, so they are
//! unreachable from anywhere else. That is the point: a payload sharing
//! state with the instrument meant to measure it independently is the same
//! mistake as a payload that calls the canary outright, one level down.

use super::{Kind, Workload};
use std::cell::Cell;

fn build() -> Vec<u64> {
    // Size against this machine's L3 rather than to a constant: L3 runs from
    // ~4 MiB on a laptop to hundreds of MiB on a server, and a table that
    // fits inside L3 is not a memory canary at all - it quietly becomes an
    // L3 canary and reports the machine as far quieter than it is.
    let l3 = std::fs::read_to_string("/sys/devices/system/cpu/cpu0/cache/index3/size")
        .ok()
        .and_then(|s| s.trim().trim_end_matches('K').parse::<usize>().ok())
        .map(|k| k * 1024)
        .unwrap_or(12 << 20);
    let bytes = (4 * l3).clamp(64 << 20, 512 << 20);
    let n = (bytes / 8).next_power_of_two();
    eprintln!("mem_canary table {} MiB", n * 8 / (1 << 20));

    // A full-period LCG permutation, written *sequentially*. Shuffling
    // instead (Sattolo) would be correct and take seconds, because every
    // swap is a scattered write into an array far bigger than cache.
    // Traversal is still unpredictable, because the next index comes out of
    // the loaded value.
    let mask = n - 1;
    let mut chase = Vec::with_capacity(n);
    for i in 0..n {
        chase.push((i.wrapping_mul(6364136223846793005).wrapping_add(1) & mask) as u64);
    }
    chase
}

impl Workload {
    /// 2. Memory canary: pointer chase, table far beyond L3
    pub fn mem_canary() -> Self {
        // Built here rather than lazily. `Workload::simple` runs its function
        // wholly inside the timed region, so a table first touched in there
        // would put ~50 ms of one-time sequential writes into a sample.
        let table = build();
        let mask = table.len() - 1;
        // The chase simply continues where the last batch left it, which is
        // better than restarting anywhere: the whole run walks one long path
        // through the table, so nothing is revisited until it has wrapped
        // millions of steps later and been evicted many times over. No
        // cursor to scatter, and no way to accidentally sit still.
        let p = Cell::new(0usize);
        Workload::simple("mem_canary", Kind::MemCanary, move || {
            let next = table[p.get() & mask] as usize;
            p.set(next);
            next
        })
    }
}
