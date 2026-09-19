//! Did the algorithm deliver the accuracy it was asked for, and how long did
//! it take?
//!
//! Accuracy is a **gate, not a score**. An algorithm that lands inside the
//! requested error has done its job; being more accurate than asked has
//! bought nothing and cost time. So the differentiator among algorithms that
//! pass is time, and nothing else.
//!
//! This replaces scoring by run-to-run spread, which was wrong twice over.
//! It is blind to systematic error - every run carries the same bias, so a
//! biased algorithm looks consistent - and even where it works it rewards
//! precision beyond the request, which is waste.
//!
//! Everything here is measured against an independently estimated **truth**,
//! which is what makes bias visible at all. Truth is a higher-effort
//! two-rung measurement with composition varied across repeats; its own
//! spread is reported, and where that spread is comparable to an accuracy
//! goal the goal is declared unscorable rather than scored badly.
//!
//! **The circularity is real and is not hidden:** truth is produced by the
//! two-rung method, so a two-rung algorithm is partly being checked against
//! its own assumptions. A one-rung algorithm's disagreement with it is
//! meaningful; a two-rung algorithm's agreement is partly tautological.

use std::collections::{BTreeMap, BTreeSet};

/// Floors on the coverage curve, and what a well-behaved algorithm gives.
///
/// **The goal is a one-sigma standard error**, so the answer landing inside
/// it is expected about 68% of the time - not 90%, which would demand a bar
/// conservative by 1.6x. Requiring 90% here was a straight mistake: it asked
/// the algorithm to be better than the request.
///
/// So rather than one threshold, report the curve - inside 1x, 2x and beyond
/// 4x the goal - against the Gaussian expectations 68%, 95% and 0.006%. The
/// shape says more than any single point: an algorithm can look fine at 1x
/// and have a tail that a user would notice, and the 4x rate is where that
/// shows.
///
/// Two measured effects move these and are printed alongside, so the floors
/// can be set from data rather than guessed. Stopping *overshoots* - asked
/// 1%, stopped claiming 0.31%, because the rule only checks between rounds -
/// which widens the goal to several sigma and raises every rate. The bar's
/// *optimism* works the other way.
const PASS_WITHIN_1X: f64 = 0.50;
/// Within twice the goal: ~95% if Gaussian and honest.
const PASS_WITHIN_2X: f64 = 0.90;
/// The claimed one-sigma bar should cover about this often. Far below means
/// the bar is optimistic; far above means it is padded.
const EXPECT_COVERAGE: f64 = 0.68;
/// Coverage under this is reported as a failure rather than a wobble.
const COVERAGE_FLOOR: f64 = 0.50;
/// A miss by more than this multiple of the goal is a blow-up, not noise.
const BLOWUP: f64 = 4.0;
/// Blow-ups above this rate fail the gate.
const BLOWUP_MAX: f64 = 0.01;
/// Truth must be this much better determined than the goal to score it.
const TRUTH_MARGIN: f64 = 3.0;

struct Row {
    target: f64,
    cell: String,
    workload: String,
    estimate: f64,
    se: f64,
    secs: f64,
    /// Ran out of budget before the error bar reached the target. The
    /// crate calls this `(limit)`: not wrong, just less precise than asked,
    /// and the reported bar says how much less. Its *time* is censored - all
    /// we know is that it exceeded the cap.
    capped: bool,
}

fn parse(path: &str, with_target: bool) -> Vec<Row> {
    let Ok(text) = std::fs::read_to_string(path) else {
        eprintln!("could not read {path}");
        return Vec::new();
    };
    let mut head: Vec<String> = Vec::new();
    let mut out = Vec::new();
    for line in text.lines() {
        let f: Vec<&str> = line.split(',').collect();
        if head.is_empty() {
            head = f.iter().map(|s| s.to_string()).collect();
            continue;
        }
        let get = |n: &str| head.iter().position(|h| h == n).and_then(|i| f.get(i).copied());
        let (Some(c), Some(w), Some(e)) = (get("cell"), get("workload"), get("estimate")) else {
            continue;
        };
        let (Ok(e), Ok(se), Ok(t)) = (
            e.parse::<f64>(),
            get("se_naive").unwrap_or("nan").parse::<f64>(),
            get("total_s").unwrap_or("nan").parse::<f64>(),
        ) else {
            continue;
        };
        if !e.is_finite() || e <= 0.0 {
            continue;
        }
        let target = if with_target {
            get("target").and_then(|x| x.parse().ok()).unwrap_or(f64::NAN)
        } else {
            f64::NAN
        };
        let budget: f64 = get("budget_s").and_then(|x| x.parse().ok()).unwrap_or(f64::INFINITY);
        out.push(Row {
            target,
            cell: c.into(),
            workload: w.into(),
            estimate: e,
            se,
            secs: t,
            capped: t >= 0.98 * budget,
        });
    }
    out
}

fn median(v: &mut Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    if v.is_empty() { f64::NAN } else { v[v.len() / 2] }
}

fn rel_sd(v: &[f64]) -> f64 {
    if v.len() < 3 {
        return f64::NAN;
    }
    let m = v.iter().sum::<f64>() / v.len() as f64;
    let var = v.iter().map(|x| (x - m) * (x - m)).sum::<f64>() / v.len() as f64;
    if m > 0.0 { var.sqrt() / m } else { f64::NAN }
}

pub fn report(truth_path: &str, eval_path: &str) {
    // --- truth, per workload: value and how well it is pinned -------------
    let t = parse(truth_path, false);
    let mut truth: BTreeMap<String, (f64, f64, usize)> = BTreeMap::new();
    for w in t.iter().map(|r| r.workload.clone()).collect::<BTreeSet<_>>() {
        let v: Vec<f64> = t.iter().filter(|r| r.workload == w).map(|r| r.estimate).collect();
        if v.len() < 5 {
            continue;
        }
        let mut s = v.clone();
        // The spread of the mean, not of one run: truth is the average of
        // these, so its uncertainty shrinks with how many there were.
        let unc = rel_sd(&v) / (v.len() as f64).sqrt();
        truth.insert(w, (median(&mut s), unc, v.len()));
    }
    // Truth is scaffolding, not the point. Printed only when asked, or
    // when something fails and the numbers are suddenly interesting.
    let verbose = std::env::var("LAB_VERBOSE").is_ok();
    if verbose {
        println!("ground truth (two rungs, 30 s per run, composition varied)\n");
        println!("{:>13} {:>16} {:>12} {:>7}", "workload", "value", "uncertainty", "runs");
        for (w, (m, u, n)) in &truth {
            println!("{w:>13} {m:>13.4} ns {:>11.4}% {n:>7}", 100.0 * u);
        }
        println!();
    }

    // --- the gate ----------------------------------------------------------
    //
    // This is meant to read like a test run, not a report. An algorithm that
    // delivers the accuracy asked for should produce one line saying so and
    // nothing else; the numbers are only interesting when something fails and
    // we are working out why. Attention belongs on the timings.
    let e = parse(eval_path, true);
    let mut targets: Vec<f64> = e.iter().map(|r| r.target).filter(|x| x.is_finite()).collect();
    targets.sort_by(|a, b| b.partial_cmp(a).unwrap());
    targets.dedup();
    let cells: Vec<String> =
        e.iter().map(|r| r.cell.clone()).collect::<BTreeSet<_>>().into_iter().collect();

    let mut times: BTreeMap<(String, String), Vec<f64>> = BTreeMap::new();
    let mut passed: BTreeMap<(String, String), BTreeSet<String>> = BTreeMap::new();
    let mut any_fail = false;

    for goal in &targets {
        let gk = format!("{:.3}", goal);
        for c in &cells {
            let mut ok_count = 0usize;
            let mut total = 0usize;
            let mut fails: Vec<String> = Vec::new();
            for (w, (tv, tu, _)) in &truth {
                if tu * TRUTH_MARGIN > *goal {
                    continue; // truth too loose to judge this goal
                }
                let v: Vec<&Row> = e
                    .iter()
                    .filter(|r| r.workload == *w && r.cell == *c && (r.target - goal).abs() < 1e-12)
                    .collect();
                if v.len() < 20 {
                    continue;
                }
                total += 1;
                let n = v.len() as f64;
                // Judge every run against the bar *it reported*, not against
                // the target it was asked for. A run that hit the limit and
                // said "+/- 1.2%" is honest if it really is 1.2% - it is
                // slow, not wrong, and conflating the two would fail an
                // algorithm for running out of time.
                let off = |r: &&Row| (r.estimate - tv).abs();
                let bar = |r: &&Row| if r.se.is_finite() && r.se > 0.0 { r.se } else { f64::INFINITY };
                let w1 = v.iter().filter(|r| off(r) <= bar(r)).count() as f64 / n;
                let w2 = v.iter().filter(|r| off(r) <= 2.0 * bar(r)).count() as f64 / n;
                let nblow = v.iter().filter(|r| off(r) > BLOWUP * bar(r)).count();
                // Reported separately: how often it actually delivered the
                // precision requested, rather than stopping at the cap.
                let reached = v.iter().filter(|r| !r.capped).count() as f64 / n;
                let secs: f64 = v.iter().map(|r| r.secs).sum::<f64>() / n;

                // Blow-ups are counted, not rated: the Gaussian expectation
                // is 6 in 100000, and a couple of hundred runs cannot tell
                // that from zero. One event in 200 reads as 0.5%, which looks
                // like a rate and is one event.
                let why = if w1 < PASS_WITHIN_1X {
                    Some(format!("inside its own bar {:.0}% (floor {:.0}%)", 100.0 * w1, 100.0 * PASS_WITHIN_1X))
                } else if w2 < PASS_WITHIN_2X {
                    Some(format!("inside 2x its bar {:.0}% (floor {:.0}%)", 100.0 * w2, 100.0 * PASS_WITHIN_2X))
                } else if (nblow as f64) / n > BLOWUP_MAX {
                    Some(format!("{nblow} of {} runs off by >{:.0}x its own bar", v.len(), BLOWUP))
                } else {
                    None
                };
                match why {
                    None => {
                        ok_count += 1;
                        times.entry((c.clone(), gk.clone())).or_default().push(secs);
                        passed.entry((c.clone(), gk.clone())).or_default().insert(w.clone());
                    }
                    Some(reason) => fails.push(format!(
                        "      {w:<14} {reason}{}",
                        if reached < 0.99 {
                            format!("  [reached target in {:.0}% of runs]", 100.0 * reached)
                        } else {
                            String::new()
                        }
                    )),
                }
            }
            if total == 0 {
                println!("  {c:<10} {:>5.1}%  SKIP  (truth too loose to judge)", 100.0 * goal);
                continue;
            }
            if fails.is_empty() {
                println!("  {c:<10} {:>5.1}%  PASS  ({ok_count}/{total} workloads)", 100.0 * goal);
            } else {
                any_fail = true;
                println!("  {c:<10} {:>5.1}%  FAIL  ({ok_count}/{total} workloads)", 100.0 * goal);
                for f in &fails {
                    println!("{f}");
                }
            }
        }
    }

    // --- the point: time, among algorithms that delivered ------------------
    println!("\ntime to deliver, median over workloads where EVERY algorithm passed:");
    for goal in &targets {
        let gk = format!("{:.3}", goal);
        let common: Option<BTreeSet<String>> = cells
            .iter()
            .map(|c| passed.get(&(c.clone(), gk.clone())).cloned())
            .reduce(|a, b| match (a, b) {
                (Some(x), Some(y)) => Some(x.intersection(&y).cloned().collect()),
                _ => None,
            })
            .flatten();
        let common = common.unwrap_or_default();
        // Two views, because they answer different questions and the gap
        // between them is itself information.
        //
        // `common` is like-for-like: only workloads every algorithm got
        // right, so nobody banks credit for being quick on something a rival
        // could not do at all. `own` is each algorithm on everything it
        // passed, which is what a user would actually experience.
        //
        // If an algorithm's own-set time is much better than its common-set
        // time, it is fast on exactly the workloads it can handle and slow on
        // the hard ones - worth seeing rather than averaging away.
        // Times are right-censored: a run that hit the budget cap tells us
        // only that it needed *more* than the cap, not how much more. A
        // median of such data is honest while fewer than half are censored;
        // past that the only truthful statement is "> cap", so say that
        // rather than quoting the cap as though it were a measurement.
        let time_over = |c: &String, set: &BTreeSet<String>| -> Option<String> {
            let v: Vec<&Row> = e
                .iter()
                .filter(|r| {
                    r.cell == *c && (r.target - goal).abs() < 1e-12 && set.contains(&r.workload)
                })
                .collect();
            if v.is_empty() {
                return None;
            }
            let censored = v.iter().filter(|r| r.capped).count() as f64 / v.len() as f64;
            let cap = v.iter().map(|r| r.secs).fold(0.0, f64::max);
            if censored > 0.5 {
                return Some(format!("> {:.0} ms ({:.0}% hit the cap)", 1000.0 * cap, 100.0 * censored));
            }
            let mut t: Vec<f64> = v.iter().map(|r| r.secs).collect();
            let m = median(&mut t);
            Some(if censored > 0.0 {
                format!("{:.1} ms ({:.0}% capped)", 1000.0 * m, 100.0 * censored)
            } else {
                format!("{:.1} ms", 1000.0 * m)
            })
        };
        let empty = BTreeSet::new();
        print!("  {:>5.1}%  each on what it passed: ", 100.0 * goal);
        let mut parts = Vec::new();
        for c in &cells {
            let own = passed.get(&(c.clone(), gk.clone())).unwrap_or(&empty);
            match time_over(c, own) {
                Some(t) => parts.push(format!("{c} {t} [{} wl]", own.len())),
                None => parts.push(format!("{c} - [0 wl]")),
            }
        }
        println!("{}", parts.join("   "));

        if common.len() < 5 {
            println!(
                "          like-for-like: only {} workload(s) all algorithms passed, \
                 too few to compare",
                common.len()
            );
            continue;
        }
        print!("          like-for-like over {}: ", common.len());
        let mut parts = Vec::new();
        for c in &cells {
            if let Some(t) = time_over(c, &common) {
                parts.push(format!("{c} {t}"));
            }
        }
        println!("{}", parts.join("   "));
    }
    if any_fail && !verbose {
        println!("\n(LAB_VERBOSE=1 for ground-truth values)");
    }
}
