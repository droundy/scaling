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

use std::{
    hint::black_box,
    rc::Rc,
    sync::{Arc, Mutex},
    time::Instant,
};

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

pub struct Workload {
    pub name: &'static str,
    pub kind: Kind,
    /// Prepare to time a batch of the workload with batch size `usize`.
    f: Box<dyn Fn(usize) -> Box<dyn FnOnce() -> f64>>,
}

impl Workload {
    pub fn new<T: 'static, O: 'static>(
        name: &'static str,
        kind: Kind,
        gen: fn() -> T,
        f: fn(&mut T) -> O,
    ) -> Workload {
        let data = Arc::new(Mutex::new(Vec::new()));
        Workload {
            name,
            kind,
            f: Box::new(move |count| {
                {
                    let mut data = data.lock().unwrap();
                    data.clear();
                    data.extend(std::iter::repeat_with(gen).take(count));
                }
                let data = data.clone();
                Box::new(move || {
                    let mut data = data.lock().unwrap();
                    let start = Instant::now();
                    for x in data.iter_mut() {
                        black_box(f(x));
                    }
                    start.elapsed().as_secs_f64() * 1e9
                })
            }),
        }
    }

    /// A workload with no input to generate: one call is one iteration, and
    /// the batch is a loop around it.
    ///
    /// Takes an `impl Fn` rather than a `fn` pointer so a workload can carry
    /// its own state - a table, a counter - by capturing it. A plain `fn`
    /// still coerces, so the stateless case is unchanged.
    ///
    /// The `Rc` is what lets a `Fn` outer closure hand a fresh `FnOnce` to
    /// each batch: the inner closure has to *own* what it calls, and you
    /// cannot move out of a captured variable more than once. One refcount
    /// bump per batch, and it happens before the clock starts.
    pub fn simple<O: 'static>(
        name: &'static str,
        kind: Kind,
        f: impl Fn() -> O + 'static,
    ) -> Workload {
        let f = Rc::new(f);
        Workload {
            name,
            kind,
            f: Box::new(move |count| {
                let f = Rc::clone(&f);
                Box::new(move || {
                    let start = Instant::now();
                    // Monomorphic: `f` is a concrete type here, so this call
                    // inlines. The only `dyn` is the box around this closure,
                    // entered once per batch.
                    for _ in 0..count {
                        black_box(f());
                    }
                    start.elapsed().as_secs_f64() * 1e9
                })
            }),
        }
    }

    pub fn time_batch(&self, count: usize) -> Box<dyn FnOnce() -> f64> {
        (self.f)(count)
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
    selected(&std::env::var("LAB_PAYLOADS").unwrap_or_default())
}

/// The canaries, plus the payloads named in `names` - a comma-separated
/// list, or empty for all of them.
///
/// Selecting a subset is worth having because a round costs what its members
/// cost: six workloads at 100 us each is a 600 us round, and a payload you
/// are not studying is 100 us of drift between the two you are.
///
/// **The canaries are never optional.** Every ratio estimator divides by one
/// of them, so a run without both has nothing to compare against.
///
/// Sorted by name before returning, because [`payloads::all`] is a `HashMap`
/// and its iteration order varies between processes. Two runs that listed
/// their workloads in different orders would still be *correct* - the
/// recording is keyed by name and the round order is reshuffled anyway - but
/// the reports would be gratuitously hard to read side by side.
pub fn selected(names: &str) -> Vec<Workload> {
    let mut pool = payloads::all();
    let mut chosen: Vec<Workload> = if names.trim().is_empty() {
        pool.into_values().collect()
    } else {
        names
            .split(',')
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .map(|n| {
                pool.remove(n).unwrap_or_else(|| {
                    let mut known: Vec<String> = payloads::all().into_keys().collect();
                    known.sort();
                    panic!("no payload named {n:?}; known payloads are {known:?}")
                })
            })
            .collect()
    };
    chosen.sort_by_key(|w| w.name);

    let mut ws = vec![Workload::cpu_canary(), Workload::mem_canary()];
    ws.extend(chosen);
    ws
}
