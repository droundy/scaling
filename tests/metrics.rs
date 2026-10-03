//! Metrics: extra numbers computed from what the candidates returned.
//!
//! Nothing below pairs a metrics function with a candidate. `sizes` names a
//! group and takes a `Vec<u8>`, and every candidate of that group that
//! returns one gets it, while those that return something else, or something
//! that cannot be named, are measured as they always were and simply have
//! none.
//!
//! Driven through [`scaling::runner::measure`] for the reason `tests/macros.rs`
//! gives.

use scaling::{Config, Metrics};
use std::time::Duration;

// ---- one metrics function for a group, taking the output ----

#[scaling::input(group = "encode", name = "short")]
fn short_text() -> String {
    "ab".repeat(10)
}

#[scaling::input(group = "encode", name = "long")]
fn long_text() -> String {
    "abcd".repeat(100)
}

#[scaling::bench(group = "encode", baseline)]
fn plain(s: &mut String) -> Vec<u8> {
    s.clone().into_bytes()
}

#[scaling::bench(group = "encode")]
fn doubled(s: &mut String) -> Vec<u8> {
    let mut bytes = s.clone().into_bytes();
    bytes.extend_from_slice(s.as_bytes());
    bytes
}

// Returns another type, so the function below does not apply to it.
#[scaling::bench(group = "encode")]
fn length_only(s: &mut String) -> usize {
    s.len()
}

// Returns a type that cannot be named in a registration, so it cannot be
// handed on, but is measured all the same.
#[scaling::bench(group = "encode")]
fn lazy(s: &mut String) -> impl Iterator<Item = u8> {
    s.clone().into_bytes().into_iter()
}

#[scaling::metrics(group = "encode")]
fn sizes(out: Vec<u8>) -> Metrics {
    Metrics::new().bytes("size", out.len())
}

// ---- a function that also reads the input as it was ----

#[scaling::input(group = "expansion")]
fn words() -> String {
    "one two three".to_string()
}

#[scaling::bench(group = "expansion", baseline)]
fn same(s: &mut String) -> Vec<u8> {
    // Left changed, to show the metrics see the input as it began.
    let bytes = s.clone().into_bytes();
    s.clear();
    bytes
}

#[scaling::bench(group = "expansion")]
fn twice(s: &mut String) -> Vec<u8> {
    let mut bytes = s.clone().into_bytes();
    bytes.extend_from_slice(s.as_bytes());
    s.clear();
    bytes
}

#[scaling::metrics(group = "expansion")]
fn growth(input: &String, out: Vec<u8>) -> Metrics {
    Metrics::new().ratio("growth", out.len() as f64 / input.len() as f64)
}

fn report() -> scaling::Report {
    let cfg = Config::relative(0.1).with_max_time(Duration::from_millis(50));
    scaling::runner::measure(&cfg).expect("the registrations compose")
}

#[test]
fn a_group_wide_function_gives_metrics_to_every_candidate_it_fits() {
    let report = report();
    let (_, group) = report
        .groups()
        .find(|(name, _)| *name == "encode")
        .expect("the encode group");
    assert_eq!(
        group.candidates,
        ["plain", "doubled", "lazy", "length_only"]
    );
    let names: Vec<&str> = group.inputs.iter().map(|i| i.name.as_str()).collect();
    assert_eq!(names, ["long", "short"]);
    assert_eq!(group.metrics.len(), 1);
    assert_eq!(group.metrics[0].name, "size");
    assert_eq!(
        group.metrics[0].values,
        [
            [Some(400.0), Some(20.0)],
            [Some(800.0), Some(40.0)],
            [None, None],
            [None, None],
        ]
    );
}

#[test]
fn a_candidate_whose_output_cannot_be_named_is_still_measured() {
    let report = report();
    let (_, group) = report
        .groups()
        .find(|(name, _)| *name == "encode")
        .expect("the encode group");
    let lazy = group.candidates.iter().position(|c| c == "lazy").unwrap();
    assert!(group.measurements[lazy].iter().all(Option::is_some));
}

#[test]
fn a_function_that_reads_the_input_sees_it_as_it_began() {
    let report = report();
    let (_, group) = report
        .groups()
        .find(|(name, _)| *name == "expansion")
        .expect("the expansion group");
    assert_eq!(group.candidates, ["same", "twice"]);
    assert_eq!(group.metrics[0].name, "growth");
    assert_eq!(group.metrics[0].values, [[Some(1.0)], [Some(2.0)]]);
}
