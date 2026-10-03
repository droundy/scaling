//! A benchmark binary, entire.
//!
//! Every benchmark below says what it is where it is written, and the `main`
//! at the end is the whole program: one call, with no `Suite`, no `add` per
//! benchmark and no `println!`. All of that is `Config::run_and_print`'s; build
//! a `scaling::Config` for a tighter accuracy target or time budget.

fn work(n: usize) -> u64 {
    (0..n as u64).fold(0u64, |a, x| a.wrapping_mul(31).wrapping_add(x))
}

// ---- benchmarks that stand alone ----

#[scaling::bench]
fn hashing() -> u64 {
    work(200)
}

#[scaling::bench(make_input = || (0..256u64).rev().collect::<Vec<u64>>())]
fn sorting_a_fresh_vec(v: &mut Vec<u64>) {
    v.sort();
}

#[scaling::bench_scaling(nmin = 32)]
fn hashing_scales(n: usize) -> u64 {
    work(n)
}

// ---- a comparison: one shared input, three ways of using it ----

#[scaling::input(group = "summing")]
fn summing_data() -> Vec<u64> {
    (0..256u64).collect()
}

#[scaling::bench(group = "summing", baseline)]
fn by_loop(v: &mut Vec<u64>) -> u64 {
    let mut total = 0u64;
    for x in v.iter() {
        total = total.wrapping_add(*x);
    }
    total
}

#[scaling::bench(group = "summing")]
fn by_fold(v: &mut Vec<u64>) -> u64 {
    v.iter().fold(0u64, |a, x| a.wrapping_add(*x))
}

#[scaling::bench(group = "summing")]
fn by_sum(v: &mut Vec<u64>) -> u64 {
    v.iter().copied().sum()
}

// ---- a matrix: two implementations against three inputs ----
//
// Nothing here lists a pairing. Six cells come from five declarations, and a
// fourth input would make eight from six.

#[scaling::bench(group = "sorting", baseline)]
fn stable(v: &mut Vec<u64>) {
    v.sort();
}

#[scaling::bench(group = "sorting")]
fn unstable(v: &mut Vec<u64>) {
    v.sort_unstable();
}

#[scaling::input(group = "sorting", name = "sorted")]
fn already_sorted() -> Vec<u64> {
    (0..400u64).collect()
}

#[scaling::input(group = "sorting", name = "reversed")]
fn reversed() -> Vec<u64> {
    (0..400u64).rev().collect()
}

#[scaling::input(group = "sorting", name = "sawtooth")]
fn sawtooth() -> Vec<u64> {
    (0..400u64).map(|i| (i * 7) % 64).collect()
}

// ---- extra numbers beside the time: what each candidate wrote ----

#[scaling::input(group = "encoding")]
fn text() -> String {
    "abcd".repeat(100)
}

#[scaling::bench(group = "encoding", baseline)]
fn plain(s: &mut String) -> Vec<u8> {
    s.clone().into_bytes()
}

#[scaling::bench(group = "encoding")]
fn doubled(s: &mut String) -> Vec<u8> {
    let mut bytes = s.clone().into_bytes();
    bytes.extend_from_slice(s.as_bytes());
    bytes
}

#[scaling::metrics(group = "encoding")]
fn sizes(out: Vec<u8>) -> scaling::Metrics {
    scaling::Metrics::new().bytes("size", out.len())
}

fn main() -> Result<(), scaling::RegistrationError> {
    scaling::Config::default().run_and_print()
}
