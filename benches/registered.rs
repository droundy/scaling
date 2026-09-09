//! A benchmark binary with Design A gone all the way in.
//!
//! Compare it with `benches/filtered.rs`, which is the same idea assembled by
//! hand: a `Config`, a `Suite`, an `add` per benchmark, a decision about what
//! to do when `--list` was asked for, and a `println!`. None of that is here.
//! Every benchmark below says what it is where it is written, and the last
//! line is the whole program.
//!
//! ```none
//! cargo bench --features registry --bench registered -- --list
//! cargo bench --features registry --bench registered -- --filter sorting
//! cargo bench --features registry --bench registered -- --max-time 200ms
//! cargo bench --features registry --bench registered -- --format json
//! ```
//!
//! Stage 7 of `REGISTRATION.md`.

fn work(n: usize) -> u64 {
    (0..n as u64).fold(0u64, |a, x| a.wrapping_mul(31).wrapping_add(x))
}

// ---- benchmarks that stand alone ----

#[scaling::bench]
fn hashing() -> u64 {
    work(200)
}

#[scaling::bench(gen_input = || (0..256u64).rev().collect::<Vec<u64>>())]
fn sorting_a_fresh_vec(v: &mut Vec<u64>) {
    v.sort();
}

#[scaling::bench_scaling(nmin = 32)]
fn hashing_scales(n: usize) -> u64 {
    work(n)
}

// ---- a comparison: one shared input, three ways of using it ----

#[scaling::gen_input(group = "summing")]
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

#[scaling::candidate(matrix = "sorting", baseline)]
fn stable(v: &mut Vec<u64>) {
    v.sort();
}

#[scaling::candidate(matrix = "sorting")]
fn unstable(v: &mut Vec<u64>) {
    v.sort_unstable();
}

#[scaling::input(matrix = "sorting", name = "sorted")]
fn already_sorted() -> Vec<u64> {
    (0..400u64).collect()
}

#[scaling::input(matrix = "sorting", name = "reversed")]
fn reversed() -> Vec<u64> {
    (0..400u64).rev().collect()
}

#[scaling::input(matrix = "sorting", name = "sawtooth")]
fn sawtooth() -> Vec<u64> {
    (0..400u64).map(|i| (i * 7) % 64).collect()
}

scaling::main!();
