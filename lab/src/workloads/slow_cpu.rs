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

/// Multiply-adds per call. About 1.7 GHz and roughly one per cycle, so the
/// default is a bit over 2 ms - comfortably into the regime where a sample
/// takes several ticks and cannot be made shorter.
fn interior() -> u64 {
    std::env::var("LAB_SLOW_ITERS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3_500_000)
}

impl Workload {
    pub fn slow_cpu() -> Self {
        let n = interior();
        // Carried across calls like the canaries do, so the chain cannot be
        // constant-folded and each call genuinely depends on the last.
        let x = Cell::new(0x243F6A8885A308D3u64);
        Workload::simple("slow_cpu", Kind::Payload, move || {
            let mut v = x.get();
            for _ in 0..n {
                v = v
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
            }
            x.set(v);
            v
        })
    }
}
