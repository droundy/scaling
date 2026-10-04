//! A CPU workload whose duration is a knob.
//!
//! Every other workload here costs what it costs. This one does `INTERIOR`
//! dependent multiply-adds per call, so one call can be made to take a
//! microsecond or a hundred milliseconds without changing anything else about
//! it - which is what lets a single workload be walked across the ~1 ms
//! boundary where the scheduler tick stops being a rare contaminant and
//! becomes a constant tax.
//!
//! **Why CPU and not another `copy_64mb`.** The slow regime could not be
//! studied with the memory workload we had, because `copy_64mb` is noisy in
//! its own right: its ~1.24% batch-to-batch variation is bandwidth wobble and
//! it swamps the 0.1-0.2% the tick contributes, so it answers nothing about
//! the tick. A dependent ALU chain has almost no intrinsic variance - the
//! cpu canary runs at 0.03% when read with a cycle counter - so whatever
//! noise shows up at these durations is the machine, not the workload.
//!
//! It is also the regime where sample size stops being a free parameter: one
//! call already costs more than any sample we would choose, so `n = 1` is
//! forced, there is nothing to calibrate, and the fixed per-measurement cost
//! is a rounding error. A benchmark harness should notice that and stop
//! trying to be clever, and this workload is how we check that it does.

use super::{Kind, Workload};
use std::cell::Cell;

/// Multiply-adds per call. Each is a canary link, four cycles, so at 1.7 GHz
/// the default is about 8 ms - comfortably into the regime where a sample
/// takes several ticks and cannot be made shorter.
fn interior() -> u64 {
    std::env::var("LAB_SLOW_ITERS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3_500_000)
}

impl Workload {
    pub fn slow_cpu() -> Self {
        Self::slow_cpu_named("slow_cpu", interior())
    }

    /// A second chain of its own length (`LAB_SLOW2_ITERS`), for putting
    /// two different call lengths in one round: whether a call of one
    /// length runs at the clock of a call of another is the question a
    /// canary matched in duration has to answer.
    pub fn slow_cpu2() -> Self {
        let n = std::env::var("LAB_SLOW2_ITERS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(1_000_000);
        Self::slow_cpu_named("slow_cpu2", n)
    }

    /// More chains of their own lengths (`LAB_SLOW3_ITERS`, `LAB_SLOW4_ITERS`,
    /// `LAB_SLOW5_ITERS`), so that a fast known-answer pair and a slow one
    /// can share a suite.
    pub fn slow_cpu_n(k: u8) -> Self {
        let (name, var) = match k {
            3 => ("slow_cpu3", "LAB_SLOW3_ITERS"),
            4 => ("slow_cpu4", "LAB_SLOW4_ITERS"),
            _ => ("slow_cpu5", "LAB_SLOW5_ITERS"),
        };
        let n = std::env::var(var).ok().and_then(|s| s.parse().ok()).unwrap_or(1_000_000);
        Self::slow_cpu_named(name, n)
    }

    fn slow_cpu_named(name: &'static str, n: u64) -> Self {
        // Carried across calls like the canaries do, so the chain cannot be
        // constant-folded and each call genuinely depends on the last.
        let x = Cell::new(0x243F6A8885A308D3u64);
        // Each link goes through `black_box`, exactly as the canary's batch
        // loop does. Without it the compiler unrolls the chain and folds
        // eight links into one multiply-add with precomputed constants: a
        // call of 250,000 links took 28us where the canary's chain predicts
        // 228us. With it the loop is the canary's own, link for link, so
        // this workload's true ratio to the canary is exactly
        // `LAB_SLOW_ITERS` - a known answer for the slow regime, whatever
        // the clock is doing.
        if std::env::var("LAB_SLOW_FOLDED").is_ok() {
            // The old loop, which the compiler folds eight links at a time:
            // kept as a switch for telling its effects on neighbours apart
            // from the `black_box` version's.
            return Workload::simple(name, Kind::Payload, move || {
                let mut v = x.get();
                for _ in 0..n {
                    v = v
                        .wrapping_mul(6364136223846793005)
                        .wrapping_add(1442695040888963407);
                }
                x.set(v);
                v
            });
        }
        Workload::simple(name, Kind::Payload, move || {
            for _ in 0..n {
                let v = x
                    .get()
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                x.set(v);
                std::hint::black_box(v);
            }
            x.get()
        })
    }
}
