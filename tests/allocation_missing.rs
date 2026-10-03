//! A metrics function that counts allocations, in a program that does not
//! use the allocator that counts them: reported before anything runs,
//! rather than shown as zeros.

use scaling::Metrics;

#[scaling::bench(group = "build", baseline)]
fn at_once() -> Vec<u8> {
    vec![0u8; 100]
}

#[scaling::bench(group = "build")]
fn twice() -> Vec<u8> {
    vec![0u8; 200]
}

#[scaling::metrics(group = "build", allocation)]
fn counted(out: Vec<u8>) -> Metrics {
    Metrics::new()
        .bytes("size", out.len())
        .peak_allocated_bytes()
}

#[test]
fn asking_for_counts_without_the_allocator_is_a_fatal_diagnostic() {
    let problems = match scaling::runner::measure(&scaling::Config::default()) {
        Ok(_) => panic!("it should not have run"),
        Err(problems) => problems,
    };
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].is_fatal());
    let said = problems[0].to_string();
    assert!(
        said.contains("counted") && said.contains("global_allocator"),
        "{said}"
    );
}
