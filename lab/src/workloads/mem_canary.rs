//! Memory canary: a pointer chase starting somewhere new every call.
//!
//! The moving start is load-bearing. A 100 us chase touches only tens of
//! KiB, so walking the *same* path every call leaves it cache resident and
//! it reads 23 ns per access instead of 142 ns - still perfectly steady, and
//! measuring the wrong thing. A fresh start per call sweeps a window through
//! the table, so every access genuinely misses.
//!
//! The table lives here, privately, behind a `LazyLock`. Nothing outside
//! this module can reach it, which is the point: a payload sharing state
//! with the instrument meant to measure it independently is the same mistake
//! as a payload that calls the canary outright, one level down.

use super::{index, Input, Kind, Workload};
use std::sync::LazyLock;

/// Chase steps per call. Each is a dependent load that misses, so at ~142 ns
/// apiece this is a couple of microseconds of work - call overhead is
/// nothing against it, and calibration still has room to aim.
const STEPS: u64 = 16;

static CHASE: LazyLock<Vec<u64>> = LazyLock::new(build);

/// Built on first use, which is inside `prepare` and therefore never inside
/// a timed region.
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

fn gen(seed: u64) -> Input {
    Input::Index(seed as usize & (CHASE.len() - 1))
}

fn run(i: &mut Input) -> u64 {
    let mut p = index(i);
    for _ in 0..STEPS {
        p = CHASE[p] as usize;
    }
    p as u64
}

pub fn workload() -> Workload {
    Workload::new("mem_canary", Kind::MemCanary, gen, run)
}
