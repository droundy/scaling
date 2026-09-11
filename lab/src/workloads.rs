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
//! payload("sort_1k", |s| Input::Ints(shuffled(1024, s)), |_, i| {
//!     let v = ints(i);
//!     v.sort_unstable();
//!     v[0]
//! }),
//! ```
//!
//! The first closure makes one input and runs **before the timer starts**;
//! the second is the thing being measured. Keep everything you do not want
//! in the number - allocation, filling a buffer, restoring order a previous
//! iteration destroyed - in the generator.
//!
//! Both are plain `fn` pointers. Non-capturing closures coerce to those, so
//! the one-liner above works, but a closure that captures a variable will
//! not compile. Put the constant in the closure body, as `1024` is here.
//!
//! If your benchmark needs an input shape [`Input`] does not have, add a
//! variant and an accessor beside [`ints`]. That is the only place in this
//! program where adding a benchmark costs more than one line.

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

/// One prepared input, in whatever shape the benchmark wants.
///
/// An enum rather than a generic or a trait object, so that a heterogeneous
/// list of benchmarks stays a plain `Vec` of a concrete type.
pub enum Input {
    Ints(Vec<u64>),
    Text(String),
    /// Canaries generate nothing - their input is the machine - so they
    /// carry the batch size instead, and run the whole batch in one call.
    Batch { iters: u64, seed: u64 },
}

/// Get at an `Ints` input. Panics loudly on a mismatch, which in a table
/// this small is a typo you want to hear about immediately.
pub fn ints(i: &mut Input) -> &mut Vec<u64> {
    match i {
        Input::Ints(v) => v,
        _ => panic!("this benchmark's generator does not make Input::Ints"),
    }
}

/// Get at a `Text` input.
pub fn text(i: &mut Input) -> &mut String {
    match i {
        Input::Text(s) => s,
        _ => panic!("this benchmark's generator does not make Input::Text"),
    }
}

pub struct Workload {
    pub name: &'static str,
    pub kind: Kind,
    /// Build one input. Runs before the timer starts.
    gen: fn(u64) -> Input,
    /// The thing being measured. `Ctx` is here for the canaries; payloads
    /// ignore it.
    f: fn(&Ctx, &mut Input) -> u64,
    inputs: Vec<Input>,
}

impl Workload {
    /// Build this batch's inputs. Not timed.
    pub fn prepare(&mut self, iters: u64, seed: u64) {
        // Clearing here rather than after `run` is deliberate: dropping the
        // previous batch's inputs is real work, and it happens outside the
        // timed region on this side of the call instead of inside it on the
        // other.
        self.inputs.clear();
        match self.kind {
            Kind::Payload => {
                self.inputs.reserve(iters as usize);
                for i in 0..iters {
                    self.inputs.push((self.gen)(seed.wrapping_add(i)));
                }
            }
            // One input describing the whole batch: a canary loops
            // internally rather than being called `iters` times, because
            // the call overhead would be a large part of what it measures.
            _ => self.inputs.push(Input::Batch { iters, seed }),
        }
    }

    /// Run over what `prepare` built. Timed.
    pub fn run(&mut self, ctx: &Ctx) -> u64 {
        let mut acc = 0u64;
        // Iterate rather than drain: the inputs must outlive the timed
        // region, or their destructors land inside it.
        for x in self.inputs.iter_mut() {
            acc ^= (self.f)(ctx, x);
        }
        acc
    }
}

fn payload(name: &'static str, gen: fn(u64) -> Input, f: fn(&Ctx, &mut Input) -> u64) -> Workload {
    Workload { name, kind: Kind::Payload, gen, f, inputs: Vec::new() }
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

/// A canary generates nothing, so this is never called for one. It exists
/// only to fill the field.
fn no_input(_seed: u64) -> Input {
    Input::Batch { iters: 0, seed: 0 }
}

fn batch(i: &Input) -> (u64, u64) {
    match i {
        Input::Batch { iters, seed } => (*iters, *seed),
        _ => panic!("a canary was given a generated input"),
    }
}

/// CPU canary: one dependent multiply-add chain, entirely in registers.
///
/// Minimal instruction-level parallelism on purpose, so its cost tracks the
/// core clock and almost nothing else. Measured at 0.02% coefficient of
/// variation on a quiesced machine - a 200 ppm ruler.
fn cpu_canary(_c: &Ctx, i: &mut Input) -> u64 {
    let (iters, _) = batch(i);
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
fn mem_canary(c: &Ctx, i: &mut Input) -> u64 {
    let (iters, seed) = batch(i);
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
pub fn all() -> Vec<Workload> {
    vec![
        Workload {
            name: "cpu_canary",
            kind: Kind::CpuCanary,
            gen: no_input,
            f: cpu_canary,
            inputs: Vec::new(),
        },
        Workload {
            name: "mem_canary",
            kind: Kind::MemCanary,
            gen: no_input,
            f: mem_canary,
            inputs: Vec::new(),
        },
        // Shuffling belongs in the generator: sorting an already-sorted
        // vector measures something else entirely.
        payload("sort_1k", |s| Input::Ints(shuffled(1024, s)), |_, i| {
            let v = ints(i);
            v.sort_unstable();
            v[0] ^ v[v.len() - 1]
        }),
        payload("hashmap_256", |s| Input::Ints(shuffled(256, s)), |_, i| {
            use std::collections::HashMap;
            let keys = ints(i);
            let m: HashMap<u64, u64> = keys.iter().map(|&k| (k, k ^ 1)).collect();
            *m.get(&keys[0]).unwrap_or(&0)
        }),
        payload("sum_64k", |s| Input::Ints(shuffled(8192, s)), |_, i| {
            ints(i).iter().fold(0u64, |a, &b| a.wrapping_add(b))
        }),
        payload("parse_int", |s| Input::Text(format!("{s}")), |_, i| {
            text(i).parse::<u64>().unwrap_or(0)
        }),
    ]
}

/// A vector of `n` scrambled values. Cheap, deterministic in `seed`, and
/// deliberately not sorted.
pub fn shuffled(n: usize, seed: u64) -> Vec<u64> {
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
