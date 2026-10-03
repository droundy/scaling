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
fn asking_for_counts_without_the_allocator_is_an_error() {
    let said = match scaling::Config::default().run() {
        Ok(_) => panic!("it should not have run"),
        Err(error) => error.to_string(),
    };
    let problems = said.lines().filter(|l| l.starts_with("  - ")).count();
    assert_eq!(problems, 1, "{said}");
    assert!(
        said.contains("counted") && said.contains("global_allocator"),
        "{said}"
    );
}
