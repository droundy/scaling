//! CPU canary: a dependent multiply-add chain, entirely in registers.
//!
//! Minimal instruction-level parallelism on purpose, so its cost tracks the
//! core clock and almost nothing else. Measured at 0.02% coefficient of
//! variation on a quiesced machine - a 200 ppm ruler.
//!
//! It has no input to generate and no state to keep, so it is a
//! [`Workload::simple`]: the batch size is the number of links, and the
//! whole batch is one loop with nothing between the iterations. That makes
//! its `ns/iter` a genuine per-link figure rather than per chunk of some
//! arbitrary size.

use super::{Kind, Workload};
use std::cell::Cell;

impl Workload {
    /// 1. CPU Canary: dependent multiply-add chain
    pub fn cpu_canary() -> Self {
        // The chain has to survive between calls, so it lives in a `Cell`
        // rather than a local. Started from a constant, never from anything
        // varying: every batch must do identical work or this stops being a
        // ruler.
        let x = Cell::new(0x243F6A8885A308D3u64);
        Workload::simple("cpu_canary", Kind::CpuCanary, move || {
            let v = x
                .get()
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            x.set(v);
            v
        })
    }
}
