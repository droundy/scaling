//! Scoring measurement protocols against each other, without lying.
//!
//! This module exists because the obvious analysis is a trap, and I fell in
//! it twice in one day.
//!
//! The obvious analysis is: run each protocol many times, report the spread
//! of its answers, and prefer the tighter one. That is *reproducibility*, and
//! reproducibility is **structurally blind to systematic error** - every run
//! carries the same bias, so a biased protocol looks perfectly consistent.
//! Worse, it is anti-correlated with what we care about: a protocol that
//! removes a bias pays variance to do it, so it scores *worse* on the very
//! table meant to judge it. Ranking protocols by spread will reliably pick
//! the most biased one.
//!
//! No caveat printed beside the table fixes this - a warning repeated on
//! every table is one I will skim. So the table is not printed.
//!
//! What can be said without ground truth:
//!
//! - **Scatter**: how much one protocol's answer moves between runs. Real,
//!   but only half the error.
//! - **Disagreement**: how far the protocols are from each other. Nobody
//!   knows which is right, but if they differ by X then *somebody* is wrong
//!   by at least X/2. It is a lower bound on a bias, attributable to no one.
//!
//! And the rule that follows: **when disagreement exceeds scatter, the
//! systematic term dominates and no ranking by scatter is meaningful.** The
//! report refuses to rank in that case rather than printing a number I would
//! misread.

use std::collections::{BTreeMap, BTreeSet};

/// Workloads whose answers have proven unstable for reasons that are not
/// about the protocol, and which therefore must not move an aggregate.
///
/// `mpsc_send`'s cost lives partly on another thread draining its channel,
/// so it depends on a queue depth no protocol controls; it has failed every
/// reliability test this lab has run. `mem_canary` reads ~20% differently
/// between processes because its chase table lands at a different address
/// each time. Both are kept because they are informative about *failure*,
/// and both are excluded from every summary, because a median over eleven
/// workloads of which two have no stable answer is not a median of anything.
const UNRELIABLE: [&str; 2] = ["mpsc_send", "mem_canary"];

/// Fewest usable cases before a summary statistic is worth printing.
const MIN_FOR_SUMMARY: usize = 5;

struct Row {
    cell: String,
    budget: f64,
    workload: String,
    estimate: f64,
}

fn load(paths: &[String]) -> Vec<Row> {
    let mut out = Vec::new();
    for p in paths {
        let Ok(text) = std::fs::read_to_string(p) else {
            eprintln!("could not read {p}");
            continue;
        };
        let mut head: Vec<String> = Vec::new();
        for line in text.lines() {
            let f: Vec<&str> = line.split(',').collect();
            if head.is_empty() {
                head = f.iter().map(|s| s.to_string()).collect();
                continue;
            }
            let get = |name: &str| -> Option<&str> {
                head.iter()
                    .position(|h| h == name)
                    .and_then(|i| f.get(i).copied())
            };
            let (Some(c), Some(b), Some(w), Some(e)) = (
                get("cell"),
                get("budget_s"),
                get("workload"),
                get("estimate"),
            ) else {
                continue;
            };
            let (Ok(b), Ok(e)) = (b.parse::<f64>(), e.parse::<f64>()) else {
                continue;
            };
            if !e.is_finite() || e <= 0.0 {
                continue;
            }
            out.push(Row {
                cell: c.to_string(),
                budget: b,
                workload: w.to_string(),
                estimate: e,
            });
        }
    }
    out
}

/// Budgets are keys as well as numbers, so they have to render identically
/// everywhere they are used.
fn fmt_budget(b: f64) -> String {
    let s = format!("{b}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

fn median(v: &mut [f64]) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    if v.is_empty() {
        f64::NAN
    } else {
        v[v.len() / 2]
    }
}

fn rel_sd(v: &[f64]) -> f64 {
    if v.len() < 3 {
        return f64::NAN;
    }
    let m = v.iter().sum::<f64>() / v.len() as f64;
    let var = v.iter().map(|x| (x - m) * (x - m)).sum::<f64>() / v.len() as f64;
    if m > 0.0 {
        var.sqrt() / m
    } else {
        f64::NAN
    }
}

pub fn report(paths: &[String]) {
    let rows = load(paths);
    if rows.is_empty() {
        eprintln!("no usable rows");
        return;
    }
    let cells: Vec<String> = rows
        .iter()
        .map(|r| r.cell.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let budgets: Vec<f64> = {
        let mut b: Vec<f64> = rows.iter().map(|r| r.budget).collect();
        b.sort_by(|x, y| x.partial_cmp(y).unwrap());
        b.dedup();
        b
    };
    let workloads: Vec<String> = rows
        .iter()
        .map(|r| r.workload.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();

    // (budget, workload, cell) -> estimates
    let mut by: BTreeMap<(String, String, String), Vec<f64>> = BTreeMap::new();
    for r in &rows {
        by.entry((fmt_budget(r.budget), r.workload.clone(), r.cell.clone()))
            .or_default()
            .push(r.estimate);
    }

    println!(
        "protocols compared: {}\n\
         scatter  = sd/mean of one protocol's answers over repeated runs, as a percent of its answer\n\
         disagree = spread of the protocols' median answers, as a percent of the middle one\n",
        cells.join(", ")
    );

    for b in &budgets {
        let bs = fmt_budget(*b);
        println!("===== budget {bs} s =====");
        println!(
            "{:>13} {:>10} {:>10} {:>28}",
            "workload", "disagree", "scatter", "what can be concluded"
        );
        let mut rankable: Vec<(String, Vec<(String, f64)>)> = Vec::new();
        for w in &workloads {
            let mut meds = Vec::new();
            let mut scats = Vec::new();
            for c in &cells {
                let Some(v) = by.get(&(bs.clone(), w.clone(), c.clone())) else {
                    continue;
                };
                if v.len() < 3 {
                    continue;
                }
                let mut t = v.clone();
                meds.push((c.clone(), median(&mut t)));
                scats.push((c.clone(), 100.0 * rel_sd(v)));
            }
            if meds.len() < 2 {
                continue;
            }
            let lo = meds.iter().map(|x| x.1).fold(f64::INFINITY, f64::min);
            let hi = meds.iter().map(|x| x.1).fold(f64::NEG_INFINITY, f64::max);
            let mid = (lo + hi) / 2.0;
            let disagree = 100.0 * (hi - lo) / mid;
            let worst_scatter = scats.iter().map(|x| x.1).fold(0.0, f64::max);

            // The refusal. If the protocols differ from each other by more
            // than any of them wobbles, the difference between them is
            // systematic, and systematic error is exactly what scatter
            // cannot see. Ranking on scatter here would reward whichever
            // protocol is most consistently wrong.
            let verdict = if !disagree.is_finite() || !worst_scatter.is_finite() {
                "insufficient runs"
            } else if disagree > worst_scatter {
                "NO RANKING: systematic"
            } else {
                "comparable; scatter usable"
            };
            let unreliable = UNRELIABLE.contains(&w.as_str());
            println!(
                "{:>13} {:>9.3}% {:>9.3}% {:>28}{}",
                w,
                disagree,
                worst_scatter,
                verdict,
                if unreliable { "  [unreliable]" } else { "" }
            );
            if verdict.starts_with("comparable") && !unreliable {
                rankable.push((w.clone(), scats));
            }
        }

        // Summaries only where ranking is defensible, and only with enough
        // cases to be a summary rather than an anecdote.
        if rankable.len() < MIN_FOR_SUMMARY {
            println!(
                "\n  no aggregate: only {} workload(s) can be ranked at this budget \
                 (need {}).\n  Where protocols disagree systematically, the difference \
                 between them is\n  a bias, and no amount of repetition reveals which \
                 one carries it.\n",
                rankable.len(),
                MIN_FOR_SUMMARY
            );
            continue;
        }
        println!(
            "\n  median scatter over the {} rankable workloads:",
            rankable.len()
        );
        for c in &cells {
            let mut v: Vec<f64> = rankable
                .iter()
                .filter_map(|(_, s)| s.iter().find(|x| &x.0 == c).map(|x| x.1))
                .collect();
            if v.len() == rankable.len() {
                println!("    {:>12} {:>8.3}%", c, median(&mut v));
            }
        }
        println!();
    }
}
