//! The things we time.
//!
//! Two of these are canaries, whose *true* cost never changes, so anything
//! their timings do is the machine. The rest are payloads: ordinary code we
//! want an honest number for.
//!
//! # Adding one
//!
//! Write a `fn(&Ctx, u64, u64) -> u64` that does `iters` iterations and
//! returns something derived from the work (so the optimiser cannot delete
//! it), then add a line to [`all`]. That is the whole procedure.

/// What a workload is mostly limited by. Used to pick which canary a
/// payload should be divided by; see `estimate::ratio_auto`.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Kind {
    /// A canary: constant true cost, limited by core clock.
    CpuCanary,
    /// A canary: constant true cost, limited by memory latency.
    MemCanary,
    /// Something we actually want to measure.
    Payload,
}

pub struct Workload {
    pub name: &'static str,
    pub kind: Kind,
    pub run: fn(&Ctx, u64, u64) -> u64,
}

/// Shared buffers, built once. Passed to every workload so nothing has to
/// allocate on the timed path unless it means to.
pub struct Ctx {
    /// Next-index array for the pointer chase, far larger than L3 so a
    /// scattered read really does miss.
    pub chase: Vec<u64>,
    /// 8 KiB: comfortably L1 resident.
    pub small: Vec<u64>,
    /// 512 KiB: L2 resident, out of L1.
    pub medium: Vec<u64>,
}

impl Ctx {
    pub fn new() -> Ctx {
        // Size the chase array against this machine's L3 rather than to a
        // constant: L3 runs from ~4 MiB on a laptop to hundreds of MiB on a
        // server, and an array that fits inside L3 is not a memory canary at
        // all - it quietly becomes an L3 canary and reports the machine as
        // far quieter than it is.
        let l3 = std::fs::read_to_string("/sys/devices/system/cpu/cpu0/cache/index3/size")
            .ok()
            .and_then(|s| s.trim().trim_end_matches('K').parse::<usize>().ok())
            .map(|k| k * 1024)
            .unwrap_or(12 << 20);
        let bytes = (4 * l3).clamp(64 << 20, 512 << 20);
        let n = (bytes / 8).next_power_of_two();

        // A full-period LCG permutation, written *sequentially*. Shuffling
        // instead (Sattolo) would be correct and take seconds, because every
        // swap is a scattered write into an array far bigger than cache.
        // Traversal is still unpredictable, because the next index comes out
        // of the loaded value.
        let mask = n - 1;
        let mut chase = Vec::with_capacity(n);
        for i in 0..n {
            chase.push((i.wrapping_mul(6364136223846793005).wrapping_add(1) & mask) as u64);
        }
        Ctx {
            chase,
            small: (0..(8 << 10) / 8u64).collect(),
            medium: (0..(512 << 10) / 8u64).collect(),
        }
    }
}

// ---------------------------------------------------------------- canaries

/// CPU canary: one dependent multiply-add chain, entirely in registers.
///
/// Minimal instruction-level parallelism on purpose, so its cost tracks the
/// core clock and almost nothing else. Measured at 0.02% coefficient of
/// variation on a quiesced machine - a 200 ppm ruler.
pub fn cpu_canary(_c: &Ctx, iters: u64, _seed: u64) -> u64 {
    let mut x = 0x243F6A8885A308D3u64;
    for _ in 0..iters {
        x = x
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
    }
    x
}

/// Memory canary: a pointer chase starting somewhere new every call.
///
/// The moving start is load-bearing. A 100 us chase touches only tens of
/// KiB, so walking the *same* path every call leaves it cache resident and
/// it reads 23 ns per access instead of 142 ns - still perfectly steady,
/// and measuring the wrong thing.
pub fn mem_canary(c: &Ctx, iters: u64, seed: u64) -> u64 {
    let mut p = (seed as usize) & (c.chase.len() - 1);
    for _ in 0..iters {
        p = c.chase[p] as usize;
    }
    p as u64
}

// ---------------------------------------------------------------- payloads

// These are examples of the shape, not a considered selection. Replace them
// with whatever you actually want to measure.
//
// Note that each one rebuilds its input every iteration, because sorting an
// already-sorted vector measures something else entirely. That allocation is
// part of what gets timed. For a lab that is fine and honest - it is what
// the caller would pay too - but it does mean these say as much about the
// allocator as about the algorithm.

/// `sort` on a kilobyte of already-shuffled `u64`.
pub fn sort_1k(c: &Ctx, iters: u64, _seed: u64) -> u64 {
    let mut acc = 0u64;
    for _ in 0..iters {
        let mut v: Vec<u64> = c.small[..128].to_vec();
        v.sort_unstable();
        acc ^= v[0] ^ v[127];
    }
    acc
}

/// Building a `HashMap` and reading it back: hashing, plus scattered access
/// over a table too small to escape cache.
pub fn hashmap_1k(_c: &Ctx, iters: u64, _seed: u64) -> u64 {
    use std::collections::HashMap;
    let mut acc = 0u64;
    for _ in 0..iters {
        let mut m: HashMap<u64, u64> = HashMap::with_capacity(256);
        for i in 0..256u64 {
            m.insert(i.wrapping_mul(2654435761), i);
        }
        acc ^= *m.get(&(7u64.wrapping_mul(2654435761))).unwrap_or(&0);
    }
    acc
}

/// Formatting an integer into a fresh `String`.
pub fn format_int(_c: &Ctx, iters: u64, seed: u64) -> u64 {
    let mut acc = 0u64;
    for i in 0..iters {
        let s = format!("{}", seed.wrapping_add(i));
        acc ^= s.len() as u64;
    }
    acc
}

/// Summing a 512 KiB slice: streaming, prefetch-friendly, L2 resident.
pub fn sum_512k(c: &Ctx, iters: u64, _seed: u64) -> u64 {
    let mut acc = 0u64;
    for _ in 0..iters {
        acc = acc.wrapping_add(c.medium.iter().fold(0u64, |a, &b| a.wrapping_add(b)));
    }
    acc
}

// ------------------------------------------------------------------- table

/// Every workload, in one place. Add yours here.
///
/// **Exactly two canaries, and they come first.** Everything else is a
/// payload. Resist adding a third canary-shaped thing as a "payload": a
/// synthetic workload built out of the same primitives as a canary tracks it
/// perfectly by construction, so the estimators score far better on it than
/// on anything real, and the table quietly stops meaning what it says. An
/// earlier version of this file had a payload that literally called
/// `mem_canary`, which is how that mistake looks from the inside.
pub fn all() -> Vec<Workload> {
    use Kind::*;
    vec![
        Workload { name: "cpu_canary", kind: CpuCanary, run: cpu_canary },
        Workload { name: "mem_canary", kind: MemCanary, run: mem_canary },
        Workload { name: "sort_1k", kind: Payload, run: sort_1k },
        Workload { name: "hashmap_1k", kind: Payload, run: hashmap_1k },
        Workload { name: "format_int", kind: Payload, run: format_int },
        Workload { name: "sum_512k", kind: Payload, run: sum_512k },
    ]
}
