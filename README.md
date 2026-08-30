A lightweight benchmarking library which:

* measures until it reaches an accuracy you ask for, and tells you the
  accuracy it achieved;
* can measure simple polynomial or exponential scaling behavior;
* handles benchmarks which must mutate some state;
* has a very simple API!

```rust
use scaling::{bench, bench_env};

// Simple benchmarks are performed with `bench`.
println!("fib 200: {}", bench(|| fib(200) ));
println!("fib 500: {}", bench(|| fib(500) ));

// If a function needs to mutate some state, use `bench_env`.
println!("reverse: {}", bench_env(vec![0;100], |xs| xs.reverse()));
println!("sort:    {}", bench_env(vec![0;100], |xs| xs.sort()));
```

Running the above yields the following:

```none
fib 200:     72.17ns ± 0.46ns
fib 500:     254.9ns ± 2.5ns
reverse:     65.12ns ± 0.65ns
sort:        97.22ns ± 0.96ns
```

The `±` figure is the standard error of the reported time, in the same unit
as the time itself. Each benchmark keeps sampling until it is small enough
(within 1% by default), so cheap-to-measure benchmarks finish quickly and
noisy ones keep working until they have earned the precision.

Printing it absolutely rather than as a percentage is deliberate: to decide
whether two results really differ you compare the gap between them against
the `±`, and that is a direct digit-for-digit comparison only when
everything is in the same unit. The time itself is printed to exactly the
precision the error justifies — `72.17ns ± 0.46ns`, never `72.1683ns`,
since those last digits are noise wearing the costume of signal.

Iteration and sample counts are deliberately not printed. They were worth
showing back when the only quality signal was an R², which says nothing
about how well the answer is known; now that the `±` states the precision
outright, they are clutter on a line meant to be scanned down a column.
Both are still on `Stats` if you want them.

## Scaling behaviour

`bench_scaling` fits the measurements to `O(Nᴾ Eᴺ)` and reports the constant
in front of the law it picked, with the same kind of error bar:

```none
fib scaling:  (0.5507 ± 0.0055)ns/N (R²=0.999)
```

Here the R² *does* stay, because it answers a question the `±` cannot. Two
things have to be settled — which law, and how big its constant is — and
they can fail independently. R² is the signal for the first, and is set to
zero outright when the data cannot tell the candidate laws apart; the `±` is
the signal for the second. An error bar computed *after* a law was chosen
cannot vouch for that choice, so a tight `±` next to `R²=0.000` means
"precise about a shape I could not pin down", and deserves suspicion rather
than trust.

To ask for a different accuracy, use a `Config` — either as a fraction of
the measurement, or as a flat `Duration`:

```rust
use scaling::Config;
use std::time::Duration;

println!("fib 500: {}", Config::relative(0.001).bench(|| fib(500)));
println!("fib 500: {}", Config::absolute(Duration::from_nanos(1)).bench(|| fib(500)));
```

```none
fib 500:     250.49ns ± 0.25ns
fib 500:    258.389ns ± 0.037ns
```

An absolute target is often the more natural request: if you are trying to
tell apart two implementations you believe differ by about 5 ns, ask for an
error well under that and stop paying for precision you will not use.

If the target cannot be reached within the time budget, the benchmark says
so rather than quietly returning an over-confident number: `Stats::hit_limit`
is set and the output is marked `(limit)`.

## Quiescing the machine (Linux)

The `±` figure covers noise `scaling` can see while sampling. It cannot see
the machine around it — another process sharing your core, the CPU dropping
out of turbo as it warms up, an interrupt landing mid-sample. Those shift
the answer without widening the error bar.

This crate ships a `bench-quiet` binary that sets a machine up for
benchmarking:

```bash
sudo bench-quiet reserve 2
```

That reserves CPU 2: it moves every other process and (where the kernel
allows) every interrupt off it, offlines its SMT sibling, disables turbo,
pins the minimum frequency to 100%, switches to the performance governor,
and disables ASLR. Then run your benchmarks through it:

```bash
bench-quiet run cargo test --release
```

Benchmarks built against `scaling` **pin themselves to the reserved CPUs
automatically** — `bench-quiet run` advertises the reservation in
`SCALING_BENCH_CPUS`, and every benchmark in this crate honours it without
any code change. Set `SCALING_NO_PIN=1` to opt out.

`bench-quiet status` reports whether a reservation is in effect and whether
this process is actually on it. When you're done:

```bash
sudo bench-quiet restore
```

Everything here is Linux-only and entirely optional; on other platforms, and
when no reservation is active, benchmarks simply run as before.

## License

Licensed under either of

 * Apache License, Version 2.0, ([LICENSE-APACHE](LICENSE-APACHE) or
   http://www.apache.org/licenses/LICENSE-2.0)
 * MIT license ([LICENSE-MIT](LICENSE-MIT) or
   http://opensource.org/licenses/MIT)

at your option.

### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall
be dual licensed as above, without any additional terms or conditions.
