//! A benchmark binary shaped the way one using this crate would be.
//!
//! Run it as `cargo bench --features cli,registry --bench filtered`, adding
//! `-- --list` to see what is there or `-- --filter sort` to measure part of
//! it. `SCALING_FILTER=sort` does the same through a wrapper that passes no
//! arguments.

use scaling::{Config, Filter};
use std::time::Duration;

fn work(n: usize) -> u64 {
    (0..n as u64).fold(0u64, |a, x| a.wrapping_mul(31).wrapping_add(x))
}

fn main() {
    let cfg = Config::relative(0.05).with_max_time(Duration::from_millis(50));

    #[cfg(feature = "cli")]
    let filter = Filter::from_env_and_args();
    #[cfg(not(feature = "cli"))]
    let filter = Filter::everything();

    let mut suite = cfg.suite().with_filter(filter);

    let _ = suite.add("sorting_small", || {
        let mut v: Vec<u64> = (0..64).rev().collect();
        v.sort();
        v
    });
    let _ = suite.add("sorting_large", || {
        let mut v: Vec<u64> = (0..512).rev().collect();
        v.sort();
        v
    });
    let _ = suite.add("hashing", || work(200));
    let _ = suite.add_comparison(
        "summing",
        cfg.comparison()
            .add("fold", || (0..128u64).fold(0u64, |a, x| a.wrapping_add(x)))
            .add("iter", || (0..128u64).sum::<u64>()),
    );

    // The listing is printed by the caller rather than by the library:
    // choosing what to say, and where to say it, is the caller's business.
    if suite.filter().is_listing() {
        for name in suite.names() {
            println!("{name}");
        }
        return;
    }

    println!("{}", suite.run());
}
