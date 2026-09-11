//! The things we time.
//!
//! Two of these are canaries, whose *true* cost never changes, so anything
//! their timings do is the machine. The rest are payloads: ordinary code we
//! want an honest number for. **The runner cannot tell them apart** - a
//! canary is a `Workload` like any other, holds its own state privately, and
//! goes through the same prepare-then-run path. `Kind` is a label for the
//! report, not a branch in the machinery.
//!
//! # Adding a benchmark
//!
//! Give it a module beside [`cpu_canary`], or a line in [`payloads`] if it
//! is small. A module needs three things: a generator, the function under
//! test, and a `workload()` that names them.
//!
//! ```ignore
//! fn gen(seed: u64) -> Input { Input::shuffled(1024, seed) }
//!
//! fn run(i: &mut Input) -> u64 {
//!     let v = i.ints();
//!     v.sort_unstable();
//!     v[0]
//! }
//!
//! pub fn workload() -> Workload { Workload::new("sort_1k", Kind::Payload, gen, run) }
//! ```
//!
//! `gen` runs **before the timer starts** and `run` inside it, so everything
//! you do not want in the number - allocating, filling a buffer, restoring
//! order a previous iteration destroyed - belongs in `gen`.
//!
//! Then add `module::workload()` to [`all`].

mod cpu_canary;
mod mem_canary;
mod payloads;

/// What a workload is mostly limited by. Used to label the report and to
/// pick which canary a payload should be divided by; see
/// `estimate::ratio_auto`.
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
/// list of benchmarks stays a plain `Vec` of a concrete type. If you need a
/// shape that is not here, add a variant and an accessor beside [`Input::ints`].
pub enum Input {
    Ints(Vec<u64>),
    Text(String),
    /// An index into a table the benchmark keeps privately.
    Index(usize),
    /// A seed, for a benchmark that allocates nothing.
    Seed(u64),
}

impl Input {
    /// Get at an `Ints` input. Panics loudly on a mismatch, which is a typo
    /// you want to hear about immediately rather than a case to handle: a
    /// benchmark's generator and its function are written together and
    /// always agree, or neither is doing what its author meant.
    pub fn ints(&mut self) -> &mut Vec<u64> {
        match self {
            Input::Ints(v) => v,
            _ => panic!("this benchmark's generator does not make Input::Ints"),
        }
    }

    /// Get at a `Text` input.
    pub fn text(&mut self) -> &mut String {
        match self {
            Input::Text(s) => s,
            _ => panic!("this benchmark's generator does not make Input::Text"),
        }
    }

    /// Get at an `Index` input.
    pub fn index(&self) -> usize {
        match self {
            Input::Index(n) => *n,
            _ => panic!("this benchmark's generator does not make Input::Index"),
        }
    }

    /// Get at a `Seed` input.
    pub fn seed(&self) -> u64 {
        match self {
            Input::Seed(n) => *n,
            _ => panic!("this benchmark's generator does not make Input::Seed"),
        }
    }

    /// A vector of `n` scrambled values. Cheap, deterministic in `seed`, and
    /// deliberately not sorted.
    fn shuffled(n: usize, seed: u64) -> Self {
        let mut x = seed | 1;
        Input::Ints(
            (0..n)
                .map(|_| {
                    x = x
                        .wrapping_mul(6364136223846793005)
                        .wrapping_add(1442695040888963407);
                    x >> 11
                })
                .collect(),
        )
    }
}

pub struct Workload {
    pub name: &'static str,
    pub kind: Kind,
    /// Build one input. Runs before the timer starts.
    gen: fn(u64) -> Input,
    /// The thing being measured.
    f: fn(&mut Input) -> u64,
    inputs: Vec<Input>,
}

impl Workload {
    pub fn new(
        name: &'static str,
        kind: Kind,
        gen: fn(u64) -> Input,
        f: fn(&mut Input) -> u64,
    ) -> Workload {
        Workload {
            name,
            kind,
            gen,
            f,
            inputs: Vec::new(),
        }
    }

    /// Build this batch's inputs. Not timed.
    pub fn prepare(&mut self, iters: u64, seed: u64) {
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

    /// Run over what `prepare` built. Timed.
    pub fn run(&mut self) -> u64 {
        let mut acc = 0u64;
        // Iterate rather than drain: the inputs must outlive the timed
        // region, or their destructors land inside it.
        for x in self.inputs.iter_mut() {
            acc ^= (self.f)(x);
        }
        acc
    }
}

/// Every workload, in one place. Add yours here.
///
/// **Exactly two canaries, and they come first.** Everything else is a
/// payload. Resist adding a third canary-shaped thing as a "payload": a
/// synthetic workload built out of the same primitives as a canary tracks it
/// perfectly by construction, so the estimators score far better on it than
/// on anything real, and the table quietly stops meaning what it says. An
/// earlier version of this program had a payload that literally called the
/// memory canary, which is how that mistake looks from the inside.
pub fn all() -> Vec<Workload> {
    let mut ws = vec![cpu_canary::workload(), mem_canary::workload()];
    ws.extend(payloads::all());
    ws
}
