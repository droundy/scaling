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
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::hint::black_box;
use std::io::Read;
use std::sync::mpsc;
use std::thread;
use std::time::Instant;

impl Workload {
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

    /// 6. Pointer Chasing: `BTreeMap::get`, always missing
    pub fn btree_miss() -> Self {
        // A million entries, built once in the constructor. Doing it lazily
        // would put the construction inside whichever sample touched it
        // first, since `simple` runs its closure wholly inside the timed
        // region.
        let mut map: BTreeMap<u64, u64> = (0..1_000_000).map(|i| (i, i)).collect();
        const MISS: u64 = 500_123;
        map.remove(&MISS);
        Workload::simple("btree_miss", Kind::Payload, move || map.get(&MISS).copied())
    }

    /// 7. Macro-Memory Bandwidth: 256 MiB copy
    ///
    /// *Costs ~25 ms an iteration and half a gigabyte of resident memory.*
    /// See the note on `all` about what that does to a round.
    pub fn copy_256mb() -> Self {
        let size = 256 << 20;
        let source: Vec<u8> = vec![0x42; size];
        let dest = RefCell::new(vec![0u8; size]);
        Workload::simple("copy_256mb", Kind::Payload, move || {
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
pub fn all() -> HashMap<String, Workload> {
    [
        Workload::instant_now(),
        Workload::f64_sin(),
        Workload::mpsc_send(),
        Workload::urandom_read(),
        Workload::thread_spawn(),
        Workload::btree_miss(),
        Workload::copy_256mb(),
        Workload::str_find(),
        Workload::slice_sort(),
        Workload::parse_u64(),
    ]
    .into_iter()
    .map(|w| (w.name.to_string(), w))
    .collect()
}
