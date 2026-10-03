//! Metrics: extra numbers computed from what the candidates returned.
//!
//! Nothing below pairs a metrics function with a candidate. `sizes` names a
//! group and takes a `Vec<u8>`, and every candidate of that group that
//! returns one gets it, while those that return something else, or something
//! that cannot be named, are measured as they always were and simply have
//! none.
//!
//! Driven through [`Config::run`] for the reason `tests/macros.rs`
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

// ---- one function for several groups at once ----

#[scaling::input(group("left", "right"))]
fn shared_text() -> String {
    "xyz".repeat(5)
}

#[scaling::bench(group("left", "right"), baseline)]
fn first(s: &mut String) -> Vec<u8> {
    s.clone().into_bytes()
}

#[scaling::bench(group("left", "right"))]
fn second(s: &mut String) -> Vec<u8> {
    let mut bytes = s.clone().into_bytes();
    bytes.truncate(3);
    bytes
}

// Names a group nothing else is in as well, which is not a mistake.
#[scaling::metrics(group("left", "right", "unused"))]
fn lengths(out: Vec<u8>) -> Metrics {
    Metrics::new().count("length", out.len())
}

fn report() -> scaling::Report {
    let cfg = Config::relative(0.1).with_max_time(Duration::from_millis(50));
    cfg.run().expect("the registrations compose")
}

/// One input's results, by the name a comparison goes by: the group and the
/// input.
fn comparison(report: &scaling::Report, name: &str) -> scaling::Timings {
    report
        .comparison(name)
        .unwrap_or_else(|| panic!("the comparison {name}"))
}

#[test]
fn a_group_wide_function_gives_metrics_to_every_candidate_it_fits() {
    let report = report();
    for (input, sizes) in [("long", [400.0, 800.0]), ("short", [20.0, 40.0])] {
        let results = comparison(&report, &format!("encode@{input}"));
        let candidates: Vec<&str> = results.names().collect();
        assert_eq!(candidates, ["plain", "doubled", "lazy", "length_only"]);
        let size = |candidate: usize| results.metrics()[candidate].get("size");
        assert_eq!(size(0), Some(sizes[0]), "{input}");
        assert_eq!(size(1), Some(sizes[1]), "{input}");
        // These return other types, so the function does not apply.
        assert_eq!(size(2), None, "{input}");
        assert_eq!(size(3), None, "{input}");
    }
}

#[test]
fn a_candidate_whose_output_cannot_be_named_is_still_measured() {
    let report = report();
    let results = comparison(&report, "encode@long");
    let lazy = results.names().position(|c| c == "lazy").unwrap();
    assert!(results.timings()[lazy].ns_per_iter > 0.0);
}

#[test]
fn a_function_that_reads_the_input_sees_it_as_it_began() {
    let report = report();
    let results = comparison(&report, "expansion@words");
    let candidates: Vec<&str> = results.names().collect();
    assert_eq!(candidates, ["same", "twice"]);
    let growth = |candidate: usize| results.metrics()[candidate].get("growth");
    assert_eq!(growth(0), Some(1.0));
    assert_eq!(growth(1), Some(2.0));
}

#[test]
fn one_function_can_serve_several_groups() {
    let report = report();
    for name in ["left", "right"] {
        let results = comparison(&report, &format!("{name}@shared_text"));
        let candidates: Vec<&str> = results.names().collect();
        assert_eq!(candidates, ["first", "second"], "{name}");
        let length = |candidate: usize| results.metrics()[candidate].get("length");
        assert_eq!(length(0), Some(15.0), "{name}");
        assert_eq!(length(1), Some(3.0), "{name}");
    }
}
