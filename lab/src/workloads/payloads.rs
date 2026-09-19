//! Payloads: the things we actually want honest numbers for.
//!
//! Deliberately a spread of shapes rather than a tidy set - a syscall, a
//! cross-core wakeup, a thread spawn, a 256 MiB memcpy - because an
//! estimator that only works on well-behaved arithmetic is not worth having.
//!
//! Most of these are [`Workload::simple`]: they have no per-iteration input,
//! only state that has to live across calls, which the closure captures.
//! Only [`Workload::str_find`] builds something fresh each iteration, and so
//! is a [`Workload::new`] with a real generator.
//!
//! Where state has to be *mutated* per call, it goes in a `RefCell`: `simple`
//! takes an `impl Fn`, so interior mutability is the way in. The borrow is a
//! flag check against workloads that cost microseconds, so it does not
//! register.

use super::{Kind, Workload};
use rand::seq::SliceRandom;
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::hint::black_box;
use std::io::Read;
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::Instant;

// A catalogue: five of these are in `workloads::best()` and the rest are
// here to be promoted into it. Not dead by accident.
#[allow(dead_code)]
impl Workload {
    /// 0. The harness floor: a loop that does nothing.
    ///
    /// Here as a control, not as a benchmark. It has no working set, so
    /// nothing about it can be cold; whatever fixed cost a measurement of
    /// *this* carries is the harness itself - the two clock reads and the
    /// boxed call - and nothing else. Any fixed cost a real workload shows
    /// above this one has to come from the workload's own state.
    ///
    /// `black_box` inside [`Workload::simple`] is what stops the optimiser
    /// deleting the loop outright.
    pub fn nothing() -> Self {
        Workload::simple("nothing", Kind::Payload, || 0u8)
    }

    /// 1. The Meta-Test: `Instant::now`
    ///
    /// The clock timing itself. Whatever this reads is a floor under every
    /// other measurement in the program.
    pub fn instant_now() -> Self {
        Workload::simple("instant_now", Kind::Payload, Instant::now)
    }

    /// 2. Pure Scalar Math: `f64::sin`
    ///
    /// A fresh random angle each iteration, generated in `gen` where it is
    /// not timed - drawing the number costs a few nanoseconds against a `sin`
    /// of maybe twenty, so doing it inside the timed region would put a
    /// quarter of the measurement in the RNG. It also removes the need to
    /// `black_box` the input: a value that came from the generator is not a
    /// compile-time constant, so there is nothing to fold.
    ///
    /// Uniform over one period. A much wider range would be more interesting
    /// still, since large arguments take the expensive argument-reduction
    /// path and the timings go frankly bimodal - worth doing deliberately if
    /// you want a payload with a nasty distribution.
    pub fn f64_sin() -> Self {
        fn gen_angle() -> f64 {
            rand::random::<f64>() * std::f64::consts::TAU
        }
        fn run_sin(x: &mut f64) -> f64 {
            x.sin()
        }
        Workload::new("f64_sin", Kind::Payload, gen_angle, run_sin)
    }

    /// 3. Cross-Core Sync: `mpsc::Sender`
    pub fn mpsc_send() -> Self {
        let (tx, rx) = mpsc::channel::<u64>();
        // Drained on another thread, so what is measured is the send and the
        // wakeup rather than an unbounded queue growing.
        thread::spawn(move || while rx.recv().is_ok() {});
        Workload::simple("mpsc_send", Kind::Payload, move || {
            // `expect` rather than ignoring the result: if the receiver has
            // gone, every send fails instantly and this would quietly become
            // a benchmark of returning an error.
            tx.send(42).expect("mpsc receiver thread died")
        })
    }

    /// 4. Kernel Trap: `/dev/urandom` read
    pub fn urandom_read() -> Self {
        let state = RefCell::new((
            File::open("/dev/urandom").expect("open /dev/urandom"),
            [0u8; 4096],
        ));
        Workload::simple("urandom_read", Kind::Payload, move || {
            let mut s = state.borrow_mut();
            let (file, buf) = &mut *s;
            file.read_exact(buf).expect("read /dev/urandom")
        })
    }

    /// 5. Outlier Injection: `thread::spawn`
    ///
    /// Here to be badly behaved. Spawning and joining is scheduler-dependent
    /// and prone to occasional enormous samples, which is exactly the shape
    /// of noise a robust estimator is supposed to survive.
    pub fn thread_spawn() -> Self {
        Workload::simple("thread_spawn", Kind::Payload, || {
            thread::spawn(|| black_box(42u64))
                .join()
                .expect("spawned thread panicked")
        })
    }

    /// 6. Pointer Chasing: `BTreeMap::get`, always missing, never twice the
    ///    same way.
    ///
    /// **Every call looks up a different key, and that is the whole point.**
    /// This benchmark used to probe one fixed missing key a million times
    /// over, which made its cost depend entirely on whether that single tree
    /// path was still in cache - a property of whatever the harness had
    /// interleaved, not of `BTreeMap::get`. It measured 28.04 ns/iter in one
    /// round composition, 29.8 in another and 33.2 in a third: an 18% swing
    /// with the code under test unchanged. `cpu_canary` and `instant_now`,
    /// which have no such reusable state, did not move at all across the same
    /// compositions.
    ///
    /// With a fresh key each call nothing a previous call warmed can help the
    /// next one, so there is no warm-up transient to sit inside and no cached
    /// path for a neighbour to evict. That is the general rule this lab keeps
    /// rediscovering: a benchmark that reuses one input is measuring
    /// residency, and no amount of statistics repairs it. Real benchmarks
    /// want a prepared table of inputs, built far enough ahead to be cold.
    ///
    /// It also buys something we were short of - genuine per-call variability,
    /// since each probe walks a different path through 24 MiB.
    ///
    /// Generating the key inside the timed region is sloppy: a few ns of
    /// splitmix lands in every measurement. Against a tree walk that misses
    /// cache at every level it does not matter here, and keeping the
    /// generator in the closure avoids a table that would itself be a second
    /// cache-residency experiment.
    pub fn btree_miss() -> Self {
        // Repeatable: a fixed seed, so every process builds the same map and
        // probes the same sequence. Pseudorandom keys rather than 0..n so
        // that probes land all over the tree instead of walking one spine.
        fn splitmix(state: &mut u64) -> u64 {
            *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = *state;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        }
        // A million entries, built once in the constructor. Doing it lazily
        // would put the construction inside whichever sample touched it
        // first, since `simple` runs its closure wholly inside the timed
        // region.
        let mut build = 0x243F_6A88_85A3_08D3u64;
        let map: BTreeMap<u64, u64> = (0..1_000_000)
            .map(|_| {
                let k = splitmix(&mut build);
                (k, k)
            })
            .collect();
        // A probe stream disjoint from the keys: a random u64 is missing from
        // a million random u64s with overwhelming probability, so every
        // lookup still descends to a leaf and fails, as the name promises.
        let probe = Cell::new(0x853C_49E6_748F_EA9Bu64);
        Workload::simple("btree_miss", Kind::Payload, move || {
            let mut s = probe.get();
            let k = splitmix(&mut s);
            probe.set(s);
            map.get(&k).copied()
        })
    }

    /// 7. Macro-Memory Bandwidth: 64 MiB copy
    pub fn copy_64mb() -> Self {
        let size = 64 << 20;
        let source: Vec<u8> = vec![0x42; size];
        // Filled with something non-zero on purpose. `vec![0u8; n]` takes the
        // `alloc_zeroed` path, which for 64 MiB hands back fresh mmap'd zero
        // pages that are not faulted in until something writes to them - so
        // the first copy would pay 64 MiB of page faults inside the timer.
        // Measured once, that read as 47 ms against a true cost of 6.4 ms,
        // and since calibration is the first thing to run it is exactly the
        // measurement that got poisoned.
        //
        // Faulting the pages in here puts that cost where every other
        // workload's setup lives: outside the timed region.
        let dest = RefCell::new(vec![0xFFu8; size]);
        Workload::simple("copy_64mb", Kind::Payload, move || {
            let mut d = dest.borrow_mut();
            d.copy_from_slice(&source);
            (d.last().copied(), d.first().copied())
        })
    }

    /// 8. TLB Thrashing: `str::find` over 1 MiB
    ///
    /// The one payload with a genuine per-iteration input, so the only one
    /// that wants a generator. The needle sits at the very end, so the search
    /// always walks the whole haystack.
    pub fn str_find() -> Self {
        fn gen_haystack() -> String {
            let mut haystack = "ab".repeat(500_000);
            haystack.push_str("abc");
            haystack
        }
        fn run_find(haystack: &mut String) -> Option<usize> {
            haystack.find("abc")
        }
        Workload::new("str_find", Kind::Payload, gen_haystack, run_find)
    }

    /// 9. Branch Predictor & Allocation Isolation: `slice::sort`
    pub fn slice_sort() -> Self {
        fn gen_slice_sort() -> Vec<u64> {
            let mut out: Vec<u64> = (0..10_000).rev().collect();
            out.shuffle(&mut rand::rng());
            out
        }
        fn run_slice_sort(i: &mut Vec<u64>) -> u64 {
            i.sort_unstable();
            i[0]
        }
        Workload::new("slice_sort", Kind::Payload, gen_slice_sort, run_slice_sort)
    }

    /// 10. Parsing: `str::parse::<u64>`
    pub fn parse_u64() -> Self {
        fn gen_parse() -> String {
            rand::random::<u64>().to_string()
        }
        fn run_parse(i: &mut String) -> u64 {
            i.parse::<u64>().unwrap_or(0)
        }
        Workload::new("parse_u64", Kind::Payload, gen_parse, run_parse)
    }
}

/// Every payload, keyed by name, so a run can take any subset of them.
///
/// Nothing here depends on the order - the caller sorts - so adding a payload
/// is one line.
///
/// **Several of these cost far more than one sample is meant to.** A round
/// is only comparable if its members are measured close together, and
/// `copy_256mb` at ~25 ms an iteration is two orders of magnitude past the
/// 100 us a sample aims at. Selecting a subset with `LAB_PAYLOADS` is the
/// way to keep a round short; the heavyweight ones are worth having, but
/// worth having deliberately.
#[allow(dead_code)]
pub fn all() -> HashMap<String, Workload> {
    [
        Workload::nothing(),
        Workload::instant_now(),
        Workload::f64_sin(),
        Workload::mpsc_send(),
        Workload::urandom_read(),
        Workload::thread_spawn(),
        Workload::btree_miss(),
        Workload::copy_64mb(),
        Workload::str_find(),
        Workload::slice_sort(),
        Workload::parse_u64(),
    ]
    .into_iter()
    .map(|w| (w.name.to_string(), w))
    .collect()
}

impl Workload {
    /// The payloads worth sweeping by default.
    ///
    /// Chosen to span the ways a benchmark can be hard rather than to be
    /// representative, and deliberately without redundancy - each is here
    /// for something none of the others tests:
    ///
    /// | workload | why it is here |
    /// | --- | --- |
    /// | `instant_now` | the floor: the cheapest thing here, so harness overhead is the largest fraction of it |
    /// | `btree_miss` | the only memory-latency-bound payload, so the only one `mem_canary` can be right about |
    /// | `mpsc_send` | its cost lives partly on another core, which neither canary can observe |
    /// | `slice_sort` | branchy, allocating, input-sensitive: the closest thing to real code |
    /// | `copy_256mb` | pure streaming bandwidth, and far larger than a sample is meant to be |
    ///
    /// Built once and shared, so every subset measures the *same* workload
    /// rather than a fresh copy. `Arc` because a `Workload` holds a
    /// `Box<dyn Fn>` and so cannot be `Clone`, which `powerset` needs.
    ///
    /// Sharing buys more than the construction cost. State that persists
    /// inside a workload now carries across subsets instead of restarting,
    /// and the memory canary is the one that cares: its cursor keeps walking
    /// forward through the table, where thirty-one fresh copies would each
    /// have restarted at index zero and re-walked the same few megabytes -
    /// which by the third subset is L3-resident and no longer a memory
    /// canary at all.
    ///
    /// The canaries are not here. They are not optional, so `main` shares
    /// them separately and `run` puts them first.
    pub fn best() -> Vec<Arc<Workload>> {
        [
            Workload::instant_now(),
            Workload::btree_miss(),
            Workload::mpsc_send(),
            Workload::slice_sort(),
            Workload::copy_64mb(),
            // Workload::thread_spawn(),  // the pathological-tail case
            // Workload::f64_sin(),
            // Workload::urandom_read(),
            // Workload::str_find(),
            // Workload::parse_u64(),
        ]
        .into_iter()
        .map(Arc::new)
        .collect()
    }
}
