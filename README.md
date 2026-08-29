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
fib 200:   72.0000ns ± 0.93% (120000 iterations in 6 samples)
fib 500:  257.0000ns ± 0.31% (23142 iterations in 6 samples)
reverse:   66.0000ns ± 1.00% (52240000 iterations in 2612 samples)
sort:     111.0000ns ± 1.00% (2429904 iterations in 284 samples)
```

The `±` figure is the relative standard error of the reported time: each
benchmark keeps sampling until that figure drops below the target accuracy
(1% by default), so cheap-to-measure benchmarks finish quickly and noisy
ones keep working until they have earned the precision.

To ask for a different accuracy, use a `Config`:

```rust
use scaling::Config;

let cfg = Config { target_rel_error: 0.001, ..Config::default() };
println!("fib 500: {}", cfg.bench(|| fib(500)));
```

```none
fib 500:  258.0000ns ± 0.09% (56358 iterations in 9 samples)
```

If the target cannot be reached within the time budget, the benchmark says
so rather than quietly returning an over-confident number: `Stats::hit_limit`
is set and the output is marked `(limit)`.

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
