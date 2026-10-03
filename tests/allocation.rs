//! Counting allocations, with `Allocator` as this binary's allocator.
//!
//! Its own test binary because a global allocator is the whole program's:
//! installing it in the library's unit tests would slow every test there.
//!
//! Driven through [`Config::run`] for the reason `tests/macros.rs`
//! gives.

use scaling::{Allocator, Config, Metrics};
use std::time::Duration;

#[global_allocator]
static ALLOC: Allocator = Allocator::new();

// ---- what the counts are, for allocations of known shape ----
//
// Each case is a candidate that allocates in a way whose counts can be worked
// out by hand, and a metrics function that shows all four.

fn everything() -> Metrics {
    Metrics::new()
        .allocation_count()
        .peak_allocated_bytes()
        .total_allocated_bytes()
        .net_allocated_bytes()
}

/// One allocation, which is also what is kept.
#[scaling::bench(group = "once")]
fn once() -> Vec<u8> {
    vec![0u8; 10_000]
}

#[scaling::metrics(group = "once", allocation)]
fn count_once(_: Vec<u8>) -> Metrics {
    everything()
}

/// Memory freed before the end still counts towards the peak.
#[scaling::bench(group = "churn")]
fn churn() {
    let a = vec![0u8; 4_000];
    let b = vec![0u8; 6_000];
    drop(a);
    drop(b);
    let c = vec![0u8; 3_000];
    drop(c);
}

#[scaling::metrics(group = "churn", allocation)]
fn count_churn(_: ()) -> Metrics {
    everything()
}

/// What is kept is counted, and what was freed is not.
#[scaling::bench(group = "kept_some")]
fn kept_some() -> Vec<u8> {
    let scratch = vec![0u8; 4_000];
    drop(scratch);
    vec![0u8; 100]
}

#[scaling::metrics(group = "kept_some", allocation)]
fn count_kept_some(_: Vec<u8>) -> Metrics {
    everything()
}

/// Freeing what the candidate was handed is negative, and allocates nothing.
#[scaling::input(group = "release")]
fn big() -> Vec<u8> {
    vec![0u8; 50_000]
}

#[scaling::bench(group = "release")]
fn release(v: &mut Vec<u8>) {
    *v = Vec::new();
}

#[scaling::metrics(group = "release", allocation)]
fn count_release(_: ()) -> Metrics {
    everything()
}

/// The input was already held, so only what the candidate allocates counts.
#[scaling::input(group = "held")]
fn held_input() -> Vec<u8> {
    vec![0u8; 50_000]
}

#[scaling::bench(group = "held")]
fn small(_: &mut Vec<u8>) {
    let s = vec![0u8; 100];
    drop(s);
}

#[scaling::metrics(group = "held", allocation)]
fn count_held(_: ()) -> Metrics {
    everything()
}

/// Growing an allocation counts the growth, as a second request.
#[scaling::bench(group = "grow")]
fn grow() -> Vec<u8> {
    let mut v: Vec<u8> = Vec::with_capacity(100);
    // Room for 900 in all, so 800 more than it has.
    v.reserve_exact(900);
    v
}

#[scaling::metrics(group = "grow", allocation)]
fn count_grow(_: Vec<u8>) -> Metrics {
    everything()
}

/// Another thread's allocations are not this thread's.
#[scaling::bench(group = "elsewhere")]
fn elsewhere() {
    std::thread::spawn(|| drop(vec![0u8; 100_000]))
        .join()
        .expect("the thread finishes");
}

#[scaling::metrics(group = "elsewhere", allocation)]
fn count_elsewhere(_: ()) -> Metrics {
    everything()
}

/// The four counts of one candidate, by its name.
fn counts(report: &scaling::Report, entry: &str) -> (f64, f64, f64, f64) {
    let m = report
        .metrics(entry)
        .unwrap_or_else(|error| panic!("the candidate {entry}: {error}"));
    let get = |name: &str| {
        m.get(name)
            .unwrap_or_else(|| panic!("{name} of {entry}"))
            .as_f64()
    };
    (
        get("alloc count"),
        get("alloc peak"),
        get("alloc total"),
        get("alloc net"),
    )
}

#[test]
fn the_allocator_is_in_use() {
    drop(vec![0u8; 1]);
    assert!(Metrics::allocator_installed());
}

#[test]
fn counts_are_of_the_allocations_a_candidate_makes() {
    let report = report();
    // (count, peak, total, net)
    assert_eq!(
        counts(&report, "once:once"),
        (1.0, 10_000.0, 10_000.0, 10_000.0)
    );
    // All of it was freed again.
    assert_eq!(
        counts(&report, "churn:churn"),
        (3.0, 10_000.0, 13_000.0, 0.0)
    );
    // The 100 it began with, and the 800 it grew by.
    assert_eq!(counts(&report, "grow:grow"), (2.0, 900.0, 900.0, 900.0));

    let (count, peak, total, net) = counts(&report, "kept_some:kept_some");
    assert_eq!((count, total, net), (2.0, 4_100.0, 100.0));
    assert!(peak >= 4_000.0, "{peak}");

    // Freeing what it was handed is negative and allocates nothing.
    assert_eq!(
        counts(&report, "release:release@big"),
        (0.0, 0.0, 0.0, -50_000.0)
    );

    // The 50_000 bytes of input were held already; only the 100 are its own.
    let (count, peak, total, net) = counts(&report, "held:small@held_input");
    assert_eq!((count, peak, total, net), (1.0, 100.0, 100.0, 0.0));

    // Spawning a thread allocates a little here; the 100_000 are the thread's.
    let (_, peak, _, _) = counts(&report, "elsewhere:elsewhere");
    assert!(peak < 100_000.0, "{peak}");
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
        .peak_allocated_bytes()
        .allocation_count()
        .total_allocated_bytes()
        .net_allocated_bytes()
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
    let held = Metrics::allocations()
        .expect("the run was counted")
        .net_allocated_bytes;
    Metrics::new().ratio("held per byte", held as f64 / out.len() as f64)
}

// Not marked `allocation`, so its run is not counted and there are no counts.
#[scaling::bench(group = "uncounted")]
fn lone() -> Vec<u8> {
    vec![0u8; 10]
}

#[scaling::metrics(group = "uncounted")]
fn peeks(_out: Vec<u8>) -> Metrics {
    Metrics::new().count("had counts", Metrics::allocations().is_some() as u8)
}

fn report() -> scaling::Report {
    let cfg = Config::relative(0.1).with_max_time(Duration::from_millis(50));
    cfg.run().expect("the registrations compose")
}

#[test]
fn a_metrics_function_shows_the_counts_of_its_candidates_run() {
    let report = report();
    let results = report.comparison("build").expect("the build comparison");
    let candidates: Vec<&str> = results.names().collect();
    assert_eq!(candidates, ["at_once", "in_pieces"]);
    let metrics = results.metrics();
    let names: Vec<&str> = metrics[0].iter().map(|(name, _)| name).collect();
    assert_eq!(
        names,
        [
            "size",
            "alloc peak",
            "alloc count",
            "alloc total",
            "alloc net"
        ]
    );
    let value = |candidate: usize, metric: &str| metrics[candidate].get(metric).unwrap().as_f64();

    // One allocation of exactly the size asked for.
    assert_eq!(value(0, "alloc peak"), 10_000.0);
    assert_eq!(value(0, "alloc count"), 1.0);
    assert_eq!(value(0, "alloc total"), 10_000.0);
    // And all of it is still held, as the output.
    assert_eq!(value(0, "alloc net"), 10_000.0);

    // Grown a piece at a time: several requests, the peak at least the
    // final size, and more asked for in all than ever held at once.
    assert!(value(1, "alloc count") > 1.0);
    assert!(value(1, "alloc peak") >= 10_000.0);
    assert!(value(1, "alloc total") >= value(1, "alloc peak"));
    // What is kept is the output, with whatever room it grew to.
    assert!(value(1, "alloc net") >= 10_000.0);
    // Not the million bytes the metrics function itself allocated.
    assert!(value(1, "alloc peak") < 100_000.0);
}

#[test]
fn a_metrics_function_can_read_the_counts_to_build_its_own_metric() {
    let report = report();
    let results = report.comparison("kept").expect("the kept comparison");
    let candidates: Vec<&str> = results.names().collect();
    assert_eq!(candidates, ["exact", "padded"]);
    let held = |candidate: usize| {
        results.metrics()[candidate]
            .get("held per byte")
            .map(|v| v.as_f64())
    };
    // 8000 bytes held for 8000 bytes returned, and 32000 held for 8000.
    assert_eq!(held(0), Some(1.0));
    assert_eq!(held(1), Some(4.0));
}

#[test]
fn a_function_not_marked_allocation_has_no_counts() {
    let report = report();
    let metrics = report
        .metrics("uncounted:lone")
        .expect("the lone candidate");
    assert_eq!(metrics.get("had counts").map(|v| v.as_f64()), Some(0.0));
}
