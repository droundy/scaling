//! Example payloads: ordinary `std` work.
//!
//! These are examples of the shape, not a considered selection - replace
//! them with whatever you actually want to measure. Anything that wants more
//! than a few lines, or private state, deserves its own module beside
//! [`super::cpu_canary`] instead of a line here.

use super::{ints, shuffled, text, Input, Kind, Workload};

pub fn all() -> Vec<Workload> {
    vec![
        // Shuffling belongs in the generator: sorting an already-sorted
        // vector measures something else entirely.
        Workload::new("sort_1k", Kind::Payload, gen_sort, run_sort),
        Workload::new("hashmap_256", Kind::Payload, gen_map, run_map),
        Workload::new("sum_64k", Kind::Payload, gen_sum, run_sum),
        Workload::new("parse_int", Kind::Payload, gen_parse, run_parse),
    ]
}

fn gen_sort(seed: u64) -> Input {
    Input::Ints(shuffled(1024, seed))
}
fn run_sort(i: &mut Input) -> u64 {
    let v = ints(i);
    v.sort_unstable();
    v[0] ^ v[v.len() - 1]
}

fn gen_map(seed: u64) -> Input {
    Input::Ints(shuffled(256, seed))
}
fn run_map(i: &mut Input) -> u64 {
    use std::collections::HashMap;
    let keys = ints(i);
    let m: HashMap<u64, u64> = keys.iter().map(|&k| (k, k ^ 1)).collect();
    *m.get(&keys[0]).unwrap_or(&0)
}

fn gen_sum(seed: u64) -> Input {
    Input::Ints(shuffled(8192, seed))
}
fn run_sum(i: &mut Input) -> u64 {
    ints(i).iter().fold(0u64, |a, &b| a.wrapping_add(b))
}

fn gen_parse(seed: u64) -> Input {
    Input::Text(format!("{seed}"))
}
fn run_parse(i: &mut Input) -> u64 {
    text(i).parse::<u64>().unwrap_or(0)
}
