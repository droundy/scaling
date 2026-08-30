//! What it costs to take a benchmark.
//!
//! Every other benchmark in this crate measures somebody else's code. This
//! one measures ours: how long `bench` takes to produce an answer, how
//! reproducible that answer is, and how much of the reported time is the
//! harness rather than the function.
//!
//! Run it with `cargo bench`, ideally under `quiet-bench run` so that the
//! wall-clock figures mean something.
//!
//! # Reading the output
//!
//! **wall** is the headline: real time from calling `bench` to getting a
//! `Stats` back. It is what you actually pay per line of a benchmark suite.
//!
//! **spread** is the accuracy that bought. Each benchmark is run several
//! times over, from scratch, and the spread is the standard deviation of
//! the answers across those independent runs. It needs no ground truth and
//! makes no assumption about the library being right, which is what lets
//! the same numbers be compared across versions. It is also the honest
//! check on any precision the library claims for itself: a stated error
//! much smaller than the spread is a stated error that is not true.
//!
//! Together they are the trade the library is making. Spending longer
//! should buy a smaller spread; spending longer and *not* buying one is a
//! bug worth knowing about.
//!
//! # `bench` measuring `bench`
//!
//! The tables above are timed with a plain `Instant` and a fixed number of
//! repeats, deliberately: measuring the harness with the harness would hide
//! a regression in exactly the case where it matters, since if `bench` went
//! wrong the numbers reporting on it would go wrong the same way and still
//! look fine.
//!
//! Which is precisely what makes it worth doing as well. The last section
//! turns `bench` on `bench` and holds the answer up against the stopwatch.
//! Alone it would prove nothing; against an independent measurement of the
//! same thing it is a real check, and the only self-consistency test the
//! library has. Two ways of measuring one quantity that agree are evidence;
//! the interesting day is the one where they stop agreeing.

use scaling::{bench, bench_env, bench_gen_env, bench_scaling, bench_scaling_gen};
use std::time::{Duration, Instant};

/// How many independent runs each row is built from.
///
/// Small, because the rows are seconds each and the point is a usable
/// summary rather than a precise one; the spread of five runs is rough but
/// it is measuring an effect that is either obvious or not worth acting on.
const REPEATS: usize = 5;

struct Row {
    name: &'static str,
    /// Mean of what the library reported, in nanoseconds.
    reported: f64,
    /// Standard deviation of that across independent runs, in nanoseconds.
    spread: f64,
    /// Mean wall-clock time of one run.
    wall: Duration,
    /// What each run concluded, when the number alone does not say. For
    /// scaling that is the power found: a spread in `ns_per_scale` cannot
    /// be read at all until you know whether the runs were even answering
    /// the same question.
    notes: Vec<String>,
}

impl Row {
    fn measure(name: &'static str, mut run: impl FnMut() -> f64) -> Row {
        Row::measure_noting(name, move || (run(), String::new()))
    }

    fn measure_noting(
        name: &'static str,
        mut run: impl FnMut() -> (f64, String),
    ) -> Row {
        let mut values = Vec::with_capacity(REPEATS);
        let mut notes = Vec::with_capacity(REPEATS);
        let mut total = Duration::ZERO;
        for _ in 0..REPEATS {
            let start = Instant::now();
            let (v, note) = run();
            total += start.elapsed();
            values.push(v);
            notes.push(note);
        }
        let n = values.len() as f64;
        let mean = values.iter().sum::<f64>() / n;
        let var = values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / (n - 1.0);
        Row {
            name,
            reported: mean,
            spread: var.sqrt(),
            wall: total / REPEATS as u32,
            notes,
        }
    }
}

/// Nanoseconds, in whatever unit reads most naturally.
fn ns(v: f64) -> String {
    if !v.is_finite() {
        return "     -".to_string();
    }
    let a = v.abs();
    if a >= 1e9 {
        format!("{:.3}s", v / 1e9)
    } else if a >= 1e6 {
        format!("{:.3}ms", v / 1e6)
    } else if a >= 1e3 {
        format!("{:.3}us", v / 1e3)
    } else if a >= 1.0 {
        format!("{v:.3}ns")
    } else {
        format!("{v:.4}ns")
    }
}

fn table(title: &str, unit: &str, rows: &[Row]) {
    println!("\n{title}");
    println!("{}", "-".repeat(title.len()));
    println!(
        "  {:<26} {:>12} {:>12} {:>10}",
        "", format!("reported{unit}"), "spread", "wall"
    );
    for r in rows {
        // Spread as a share of the value is the comparable form, but it is
        // meaningless when the value is near zero, so it is only shown when
        // the division is worth doing.
        let rel = if r.reported.abs() > 1e-12 {
            format!(" ({:.2}%)", 100.0 * r.spread / r.reported.abs())
        } else {
            String::new()
        };
        println!(
            "  {:<26} {:>12} {:>12} {:>10?}",
            r.name,
            ns(r.reported),
            format!("{}{}", ns(r.spread), rel),
            r.wall
        );
        // Only worth a line when the runs disagreed, or when there is
        // something to say at all.
        let mut seen: Vec<&String> = Vec::new();
        for n in r.notes.iter().filter(|n| !n.is_empty()) {
            if !seen.contains(&n) {
                seen.push(n);
            }
        }
        if !seen.is_empty() {
            let joined: Vec<&str> = seen.iter().map(|s| s.as_str()).collect();
            println!("  {:<26} {}", "", joined.join(", "));
        }
    }
}

/// Work the optimiser cannot remove, roughly proportional to `rounds`.
///
/// `rounds` goes through `black_box` first. Without that the call sites
/// below pass literals, the whole loop folds away at compile time, and
/// every row measures the same empty closure - which is exactly what the
/// first run of this benchmark did, reporting one multiply and a hundred
/// multiplies as costing the same to the fourth decimal place.
#[inline(never)]
fn spin(rounds: u64) -> u64 {
    let rounds = std::hint::black_box(rounds);
    let mut x = std::hint::black_box(1u64);
    for i in 0..rounds {
        x = x.wrapping_mul(6364136223846793005).wrapping_add(i);
    }
    x
}

fn main() {
    println!("Cost of taking a benchmark - {REPEATS} independent runs per row");
    println!("Wall time is real time per call to the library, not per iteration.");

    // ----------------------------------------------------------------
    // What the harness adds to each iteration.
    //
    // An empty closure costs nothing, so whatever `bench` reports for one
    // is the harness: the loop counter, the `black_box`, and for the `env`
    // forms the lookup into the environment vector. The crate documents
    // this as negligible; this is the measurement behind that claim.
    // ----------------------------------------------------------------
    let overhead = [
        Row::measure("bench", || bench(|| {}).ns_per_iter),
        Row::measure("bench_env", || {
            bench_env(vec![0u8; 16], |v| v.len()).ns_per_iter
        }),
        Row::measure("bench_gen_env", || {
            bench_gen_env(|| vec![0u8; 16], |v| v.len()).ns_per_iter
        }),
    ];
    table("Per-iteration harness overhead", "/iter", &overhead);

    // ----------------------------------------------------------------
    // The library measuring itself, checked against the stopwatch above.
    //
    // `bench(|| bench(...))` is a fair benchmark of an ordinary 9ms
    // function that happens to be `bench`. If the library is honest it must
    // land on the same number the stopwatch did - it is the same quantity,
    // measured two ways that share no code.
    // ----------------------------------------------------------------
    println!("\nbench measuring bench");
    println!("---------------------");
    let stopwatch = overhead[0].wall.as_secs_f64() * 1e9;
    let start = Instant::now();
    let self_measured = bench(|| {
        bench(|| {});
    })
    .ns_per_iter;
    let took = start.elapsed();
    println!("  {:<26} {:>12}", "by stopwatch", ns(stopwatch));
    println!("  {:<26} {:>12}", "by bench", ns(self_measured));
    println!(
        "  {:<26} {:>11.2}%   (took {:?})",
        "disagreement",
        100.0 * (self_measured - stopwatch) / stopwatch,
        took
    );

    // ----------------------------------------------------------------
    // Cost and accuracy across the range of function speeds. A benchmark
    // suite is mostly made of the cheap end, so that is where wall time
    // adds up; the slow end is where it is hardest to be accurate.
    // ----------------------------------------------------------------
    let by_cost = [
        Row::measure("1 round of spin", || bench(|| spin(1)).ns_per_iter),
        Row::measure("100 rounds", || bench(|| spin(100)).ns_per_iter),
        Row::measure("10k rounds", || bench(|| spin(10_000)).ns_per_iter),
        Row::measure("1ms   (sleep)", || {
            bench(|| std::thread::sleep(Duration::from_millis(1))).ns_per_iter
        }),
        Row::measure("10ms  (sleep)", || {
            bench(|| std::thread::sleep(Duration::from_millis(10))).ns_per_iter
        }),
    ];
    table("bench(), by cost of the function", "", &by_cost);

    // ----------------------------------------------------------------
    // The same for scaling, where the library has to settle a shape as
    // well as a constant and so has much more to pay for. `reported` here
    // is `ns_per_scale`, whose units depend on the power that was found -
    // so a row whose power wanders between runs will show a large spread,
    // which is the right answer: it did not reliably identify anything.
    // ----------------------------------------------------------------
    let note = |s: &scaling::ScalingStats| {
        format!("power {} (R²={:.3})", s.scaling.power, s.goodness_of_fit)
    };
    let scaling = [
        Row::measure_noting("O(N) sum, cheap", || {
            let s = bench_scaling_gen(
                |n| (0..n as u64).collect::<Vec<_>>(),
                |v| v.iter().sum::<u64>(),
                1,
            );
            (s.scaling.ns_per_scale, note(&s))
        }),
        Row::measure_noting("O(N log N) sort", || {
            let s = bench_scaling_gen(
                |n| (0..n as u64).map(|i| (i * 13 + 5) % 137).collect::<Vec<_>>(),
                |v| v.sort(),
                1,
            );
            (s.scaling.ns_per_scale, note(&s))
        }),
        Row::measure_noting("O(N) sleep", || {
            let s =
                bench_scaling(|n| std::thread::sleep(Duration::from_millis(n as u64)), 1);
            (s.scaling.ns_per_scale, note(&s))
        }),
    ];
    table("bench_scaling(), by benchmark", "/N^p", &scaling);

}
