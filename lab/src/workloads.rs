//! The things we time.
//!
//! Two of these are canaries, whose *true* cost never changes, so anything
//! their timings do is the machine. The rest are payloads: ordinary code we
//! want an honest number for.
//!
//! # Adding a payload
//!
//! One line in [`all`]:
//!
//! ```ignore
//! generated("sort_1k", |seed| shuffled(1024, seed), |v| { v.sort_unstable(); v[0] })
//! ```
//!
//! The first closure makes one input and runs **before the timer starts**;
//! the second is the thing being measured. Keep everything you do not want
//! in the number - allocation, filling a buffer, restoring order a previous
//! iteration destroyed - in the generator.

use std::rc::Rc;

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

/// One thing to time, in two phases.
///
/// The split is the point: [`Bench::prepare`] is called outside the timed
/// region and [`Bench::run`] inside it, so a benchmark measures the work and
/// not the scaffolding around it.
pub trait Bench {
    fn name(&self) -> &'static str;
    fn kind(&self) -> Kind;
    /// Build `iters` inputs. Not timed.
    fn prepare(&mut self, iters: u64, seed: u64);
    /// Run over what `prepare` built. Timed.
    fn run(&mut self) -> u64;
}

// ------------------------------------------------------------------ payloads

/// A payload: a generator, and the function under test.
struct Generated<I, G, F> {
    name: &'static str,
    gen: G,
    f: F,
    inputs: Vec<I>,
}

impl<I, G, F> Bench for Generated<I, G, F>
where
    G: FnMut(u64) -> I,
    F: FnMut(&mut I) -> u64,
{
    fn name(&self) -> &'static str {
        self.name
    }
    fn kind(&self) -> Kind {
        Kind::Payload
    }
    fn prepare(&mut self, iters: u64, seed: u64) {
        // Clearing here rather than after `run` is deliberate: dropping the
        // previous batch's inputs is real work, and it happens outside the
        // timed region on this side of the call instead of inside it on the
        // other.
        self.inputs.clear();
        self.inputs.reserve(iters as usize);
        for i in 0..iters {
            self.inputs.push((self.gen)(seed.wrapping_add(i)));
        }
    }
    fn run(&mut self) -> u64 {
        let mut acc = 0u64;
        // Iterate rather than drain: the inputs must outlive the timed
        // region, or their destructors land inside it.
        for x in self.inputs.iter_mut() {
            acc ^= (self.f)(x);
        }
        acc
    }
}

/// Build a payload. This is what you call from [`all`].
pub fn generated<I: 'static>(
    name: &'static str,
    gen: impl FnMut(u64) -> I + 'static,
    f: impl FnMut(&mut I) -> u64 + 'static,
) -> Box<dyn Bench> {
    Box::new(Generated { name, gen, f, inputs: Vec::new() })
}

// ------------------------------------------------------------------ canaries

/// Buffers the canaries need. **Canaries only** - a payload that reaches in
/// here is sharing state with the instrument that is supposed to be
/// measuring it independently.
pub struct Ctx {
    /// Next-index array for the pointer chase, far larger than L3 so a
    /// scattered read really does miss.
    pub chase: Vec<u64>,
}

impl Ctx {
    pub fn new() -> Ctx {
        // Size against this machine's L3 rather than to a constant: L3 runs
        // from ~4 MiB on a laptop to hundreds of MiB on a server, and an
        // array that fits inside L3 is not a memory canary at all - it
        // quietly becomes an L3 canary and reports the machine as far
        // quieter than it is.
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
        Ctx { chase }
    }
}

/// A canary has nothing to generate: its input is the machine.
struct CanaryBench {
    name: &'static str,
    kind: Kind,
    ctx: Rc<Ctx>,
    f: fn(&Ctx, u64, u64) -> u64,
    iters: u64,
    seed: u64,
}

impl Bench for CanaryBench {
    fn name(&self) -> &'static str {
        self.name
    }
    fn kind(&self) -> Kind {
        self.kind
    }
    fn prepare(&mut self, iters: u64, seed: u64) {
        self.iters = iters;
        self.seed = seed;
    }
    fn run(&mut self) -> u64 {
        (self.f)(&self.ctx, self.iters, self.seed)
    }
}

/// CPU canary: one dependent multiply-add chain, entirely in registers.
///
/// Minimal instruction-level parallelism on purpose, so its cost tracks the
/// core clock and almost nothing else. Measured at 0.02% coefficient of
/// variation on a quiesced machine - a 200 ppm ruler.
fn cpu_canary(_c: &Ctx, iters: u64, _seed: u64) -> u64 {
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
fn mem_canary(c: &Ctx, iters: u64, seed: u64) -> u64 {
    let mut p = (seed as usize) & (c.chase.len() - 1);
    for _ in 0..iters {
        p = c.chase[p] as usize;
    }
    p as u64
}

// --------------------------------------------------------------------- table

/// Every workload, in one place. Add yours here.
///
/// **Exactly two canaries, and they come first.** Everything else is a
/// payload. Resist adding a third canary-shaped thing as a "payload": a
/// synthetic workload built out of the same primitives as a canary tracks it
/// perfectly by construction, so the estimators score far better on it than
/// on anything real, and the table quietly stops meaning what it says. An
/// earlier version of this file had a payload that literally called
/// `mem_canary`, which is how that mistake looks from the inside.
///
/// The payloads below are examples of the shape, not a considered selection.
pub fn all(ctx: Rc<Ctx>) -> Vec<Box<dyn Bench>> {
    let canary = |name, kind, f: fn(&Ctx, u64, u64) -> u64| -> Box<dyn Bench> {
        Box::new(CanaryBench { name, kind, ctx: Rc::clone(&ctx), f, iters: 0, seed: 0 })
    };
    vec![
        canary("cpu_canary", Kind::CpuCanary, cpu_canary),
        canary("mem_canary", Kind::MemCanary, mem_canary),
        // Shuffling belongs in the generator: sorting an already-sorted
        // vector measures something else entirely.
        generated("sort_1k", |seed| shuffled(1024, seed), |v| {
            v.sort_unstable();
            v[0] ^ v[v.len() - 1]
        }),
        generated("hashmap_1k", |seed| shuffled(256, seed), |keys| {
            use std::collections::HashMap;
            let m: HashMap<u64, u64> = keys.iter().map(|&k| (k, k ^ 1)).collect();
            *m.get(&keys[0]).unwrap_or(&0)
        }),
        generated("format_int", |seed| seed, |n| format!("{n}").len() as u64),
        generated("sum_64k", |seed| shuffled(8192, seed), |v| {
            v.iter().fold(0u64, |a, &b| a.wrapping_add(b))
        }),
    ]
}

/// A vector of `n` scrambled values. Cheap, deterministic in `seed`, and
/// deliberately not sorted.
fn shuffled(n: usize, seed: u64) -> Vec<u64> {
    let mut x = seed | 1;
    (0..n)
        .map(|_| {
            x = x
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            x >> 11
        })
        .collect()
}
