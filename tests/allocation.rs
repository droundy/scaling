//! Counting allocations, with `CountingAlloc` as this binary's allocator.
//!
//! Its own test binary because a global allocator is the whole program's:
//! installing it in the library's unit tests would slow every test there.
//!
//! Driven through [`scaling::runner::measure`] for the reason `tests/macros.rs`
//! gives.

use scaling::alloc::{self, CountingAlloc};
use scaling::{Config, Metrics};
use std::time::Duration;

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc::new();

// ---- counting by hand ----

#[test]
fn measure_reports_what_a_closure_allocated() {
    let (_kept, stats) = alloc::measure(|| vec![0u8; 10_000]);
    assert!(alloc::installed());
    assert_eq!(stats.peak_bytes, 10_000);
    assert_eq!(stats.allocations, 1);
    assert_eq!(stats.allocated_bytes, 10_000);
    assert_eq!(stats.retained_bytes, 10_000);
}

#[test]
fn memory_freed_before_the_end_still_counts_towards_the_peak() {
    let ((), stats) = alloc::measure(|| {
        let a = vec![0u8; 4_000];
        let b = vec![0u8; 6_000];
        drop(a);
        drop(b);
        let c = vec![0u8; 3_000];
        drop(c);
    });
    assert_eq!(stats.peak_bytes, 10_000);
    assert_eq!(stats.allocations, 3);
    assert_eq!(stats.allocated_bytes, 13_000);
    // All of it was freed again.
    assert_eq!(stats.retained_bytes, 0);
}

#[test]
fn what_is_kept_is_counted_and_what_was_freed_is_not() {
    let (_kept, stats) = alloc::measure(|| {
        let scratch = vec![0u8; 4_000];
        drop(scratch);
        vec![0u8; 100]
    });
    assert_eq!(stats.retained_bytes, 100);
    assert!(stats.peak_bytes >= 4_000);
}

#[test]
fn freeing_what_was_held_before_is_negative() {
    let held = vec![0u8; 50_000];
    let ((), stats) = alloc::measure(|| drop(held));
    assert_eq!(stats.retained_bytes, -50_000);
    assert_eq!(stats.peak_bytes, 0);
}

#[test]
fn only_what_the_closure_allocated_is_counted() {
    let held = vec![0u8; 50_000];
    let ((), stats) = alloc::measure(|| {
        let small = vec![0u8; 100];
        drop(small);
    });
    assert_eq!(stats.peak_bytes, 100);
    drop(held);
}

#[test]
fn growing_an_allocation_counts_the_growth() {
    let (_kept, stats) = alloc::measure(|| {
        let mut v: Vec<u8> = Vec::with_capacity(100);
        // Room for 900 in all, so 800 more than it has.
        v.reserve_exact(900);
        v
    });
    assert_eq!(stats.peak_bytes, 900);
    assert_eq!(stats.allocations, 2);
    // The 100 it began with, and the 800 it grew by.
    assert_eq!(stats.allocated_bytes, 900);
}

#[test]
fn another_threads_allocations_are_not_counted() {
    let ((), stats) = alloc::measure(|| {
        std::thread::spawn(|| drop(vec![0u8; 100_000]))
            .join()
            .expect("the thread finishes");
    });
    assert!(stats.peak_bytes < 100_000, "{stats:?}");
}

// ---- a metrics function that asks for the counts ----

#[scaling::bench(group = "build", baseline)]
fn at_once() -> Vec<u8> {
    vec![0u8; 10_000]
}

#[scaling::bench(group = "build")]
fn in_pieces() -> Vec<u8> {
    let mut v = Vec::new();
    for _ in 0..10 {
        v.extend_from_slice(&[0u8; 1_000]);
    }
    v
}

// What the function does to the output is not counted against the
// candidate: it allocates a good deal here.
#[scaling::metrics(group = "build", allocation)]
fn counted(out: Vec<u8>) -> Metrics {
    let _scratch = vec![0u8; 1_000_000];
    Metrics::new()
        .bytes("size", out.len())
        .peak_bytes()
        .allocations()
        .allocated_bytes()
        .retained_bytes()
}

// ---- a function that builds a metric of its own from the counts ----

#[scaling::bench(group = "kept", baseline)]
fn exact() -> Vec<u8> {
    vec![0u8; 8_000]
}

#[scaling::bench(group = "kept")]
fn padded() -> Vec<u8> {
    let mut v = Vec::with_capacity(32_000);
    v.extend_from_slice(&[0u8; 8_000]);
    v
}

#[scaling::metrics(group = "kept", allocation)]
fn waste(out: Vec<u8>) -> Metrics {
    let held = alloc::counts().expect("the run was counted").retained_bytes;
    Metrics::new().ratio("held per byte", held as f64 / out.len() as f64)
}

// Not marked `allocation`, so its run is not counted and there are no counts.
#[scaling::bench(group = "uncounted")]
fn lone() -> Vec<u8> {
    vec![0u8; 10]
}

#[scaling::metrics(group = "uncounted")]
fn peeks(_out: Vec<u8>) -> Metrics {
    Metrics::new().count("had counts", alloc::counts().is_some() as u8)
}

fn report() -> scaling::Report {
    let cfg = Config::relative(0.1).with_max_time(Duration::from_millis(50));
    scaling::runner::measure(&cfg).expect("the registrations compose")
}

#[test]
fn a_metrics_function_shows_the_counts_of_its_candidates_run() {
    let report = report();
    let (_, group) = report
        .groups()
        .find(|(name, _)| *name == "build")
        .expect("the build group");
    assert_eq!(group.candidates, ["at_once", "in_pieces"]);
    let names: Vec<&str> = group.metrics.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(names, ["size", "peak", "allocs", "allocated", "retained"]);
    let value = |metric: usize, candidate: usize| group.metrics[metric].values[candidate][0];

    // One allocation of exactly the size asked for.
    assert_eq!(value(1, 0), Some(10_000.0));
    assert_eq!(value(2, 0), Some(1.0));
    assert_eq!(value(3, 0), Some(10_000.0));
    // And all of it is still held, as the output.
    assert_eq!(value(4, 0), Some(10_000.0));

    // Grown a piece at a time: several requests, the peak at least the
    // final size, and more asked for in all than ever held at once.
    assert!(value(2, 1).unwrap() > 1.0, "{:?}", value(2, 1));
    assert!(value(1, 1).unwrap() >= 10_000.0, "{:?}", value(1, 1));
    assert!(value(3, 1).unwrap() >= value(1, 1).unwrap());
    // What is kept is the output, with whatever room it grew to.
    assert!(value(4, 1).unwrap() >= 10_000.0, "{:?}", value(4, 1));
    // Not the million bytes the metrics function itself allocated.
    assert!(value(1, 1).unwrap() < 100_000.0);
}

#[test]
fn a_metrics_function_can_read_the_counts_to_build_its_own_metric() {
    let report = report();
    let (_, group) = report
        .groups()
        .find(|(name, _)| *name == "kept")
        .expect("the kept group");
    assert_eq!(group.candidates, ["exact", "padded"]);
    assert_eq!(group.metrics[0].name, "held per byte");
    // 8000 bytes held for 8000 bytes returned, and 32000 held for 8000.
    assert_eq!(group.metrics[0].values, [[Some(1.0)], [Some(4.0)]]);
}

#[test]
fn a_function_not_marked_allocation_has_no_counts() {
    let report = report();
    let (_, group) = report
        .groups()
        .find(|(name, _)| *name == "uncounted")
        .expect("the uncounted group");
    assert_eq!(group.metrics[0].values, [[Some(0.0)]]);
}
