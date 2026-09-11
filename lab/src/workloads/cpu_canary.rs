//! CPU canary: a dependent multiply-add chain, entirely in registers.
//!
//! Minimal instruction-level parallelism on purpose, so its cost tracks the
//! core clock and almost nothing else. Measured at 0.02% coefficient of
//! variation on a quiesced machine - a 200 ppm ruler.
//!
//! It holds no state and allocates nothing, so it goes through the same
//! generate-then-run path as any payload with nothing in the generator.

use super::{Input, Kind, Workload};

/// Links of chain per call.
///
/// Large enough that the call and the loop around it are a rounding error -
/// at roughly 0.3 ns a link this is some 300 ns of work against a couple of
/// nanoseconds of overhead - and small enough that calibration can still
/// land near its target batch duration rather than overshooting it.
const CHUNK: u64 = 1024;

fn gen(seed: u64) -> Input {
    Input::Seed(seed)
}

fn run(i: &mut Input) -> u64 {
    let mut x = i.seed() | 1;
    for _ in 0..CHUNK {
        x = x
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
    }
    x
}

pub fn workload() -> Workload {
    Workload::new("cpu_canary", Kind::CpuCanary, gen, run)
}
