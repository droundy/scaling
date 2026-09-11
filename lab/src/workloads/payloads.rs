//! Example payloads: ordinary `std` work.
//!
//! These are examples of the shape, not a considered selection - replace
//! them with whatever you actually want to measure. Anything that wants more
//! than a few lines, or private state, deserves its own module beside
//! [`super::cpu_canary`] instead of a line here.

use super::{Input, Kind, Workload};
use std::collections::HashMap;

/// Every payload, keyed by name, so a run can take any subset of them.
///
/// A map rather than a list because selection is by name: see
/// [`super::selected`], which is what `LAB_PAYLOADS` drives. Nothing here
/// depends on the order - the caller sorts - so adding a payload is still
/// one line.
pub fn all() -> HashMap<String, Workload> {
    [
        // Shuffling belongs in the generator: sorting an already-sorted
        // vector measures something else entirely.
        Workload::new("sort_1k", Kind::Payload, gen_sort, run_sort),
        Workload::new("hashmap_256", Kind::Payload, gen_map, run_map),
        Workload::new("sum_64k", Kind::Payload, gen_sum, run_sum),
        Workload::new("parse_int", Kind::Payload, gen_parse, run_parse),
    ]
    .into_iter()
    .map(|w| (w.name.to_string(), w))
    .collect()
}

fn gen_sort(seed: u64) -> Input {
    Input::shuffled(1024, seed)
}
fn run_sort(i: &mut Input) -> u64 {
    let v = i.ints();
    v.sort_unstable();
    v[0] ^ v[v.len() - 1]
}

fn gen_map(seed: u64) -> Input {
    Input::shuffled(256, seed)
}
fn run_map(i: &mut Input) -> u64 {
    use std::collections::HashMap;
    let keys = i.ints();
    let m: HashMap<u64, u64> = keys.iter().map(|&k| (k, k ^ 1)).collect();
    *m.get(&keys[0]).unwrap_or(&0)
}

fn gen_sum(seed: u64) -> Input {
    Input::shuffled(8192, seed)
}
fn run_sum(i: &mut Input) -> u64 {
    i.ints().iter().fold(0u64, |a, &b| a.wrapping_add(b))
}

fn gen_parse(seed: u64) -> Input {
    Input::Text(format!("{seed}"))
}
fn run_parse(i: &mut Input) -> u64 {
    i.text().parse::<u64>().unwrap_or(0)
}
