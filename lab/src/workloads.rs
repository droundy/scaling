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

/// L1-resident walk: should track the CPU canary almost exactly.
pub fn walk_l1(c: &Ctx, iters: u64, _seed: u64) -> u64 {
    walk(&c.small, iters)
}

/// L2-resident walk: tracks neither canary especially well, which is the
/// known weak spot rather than a bug.
pub fn walk_l2(c: &Ctx, iters: u64, _seed: u64) -> u64 {
    walk(&c.medium, iters)
}

/// DRAM-bound walk: should track the memory canary almost exactly.
pub fn walk_dram(c: &Ctx, iters: u64, seed: u64) -> u64 {
    mem_canary(c, iters, seed.rotate_left(17))
}

/// Deliberately part CPU bound and part memory bound, in a ratio set by
/// `cpu_per`. Useful for checking that a two-canary estimator recovers the
/// mixture it was built with.
fn mixed(c: &Ctx, iters: u64, seed: u64, cpu_per: u64) -> u64 {
    let mut p = (seed as usize) & (c.chase.len() - 1);
    let mut x = 0x243F6A8885A308D3u64;
    for _ in 0..iters {
        for _ in 0..cpu_per {
            x = x
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
        }
        p = c.chase[p ^ (x as usize & 1)] as usize;
    }
    p as u64 ^ x
}

pub fn mix_mostly_cpu(c: &Ctx, iters: u64, seed: u64) -> u64 {
    mixed(c, iters, seed, 4200)
}

pub fn mix_even(c: &Ctx, iters: u64, seed: u64) -> u64 {
    mixed(c, iters, seed, 470)
}

fn walk(buf: &[u64], iters: u64) -> u64 {
    let mask = buf.len() - 1;
    let mut s = 0u64;
    let mut i = 0usize;
    for k in 0..iters {
        s = s.wrapping_add(buf[i]);
        i = (i + 9 + (k as usize & 7)) & mask;
    }
    s
}

// ------------------------------------------------------------------- table

/// Every workload, in one place. Add yours here.
///
/// The canaries must come first: nothing depends on the order, but a reader
/// scanning the output wants them together at the top.
pub fn all() -> Vec<Workload> {
    use Kind::*;
    vec![
        Workload { name: "cpu_canary", kind: CpuCanary, run: cpu_canary },
        Workload { name: "mem_canary", kind: MemCanary, run: mem_canary },
        Workload { name: "walk_l1", kind: Payload, run: walk_l1 },
        Workload { name: "walk_l2", kind: Payload, run: walk_l2 },
        Workload { name: "walk_dram", kind: Payload, run: walk_dram },
        Workload { name: "mix_mostly_cpu", kind: Payload, run: mix_mostly_cpu },
        Workload { name: "mix_even", kind: Payload, run: mix_even },
    ]
}
