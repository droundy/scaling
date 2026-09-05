A lightweight benchmarking library which:

* measures until it reaches an accuracy you ask for, and tells you the
  accuracy it achieved;
* can measure how a benchmark scales, as a power of its input size;
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
fib 200:    71.716ns ± 0.057ns
fib 500:    262.75ns ± 0.14ns
reverse:     51.80ns ± 0.62ns
sort:        111.3ns ± 1.1ns
```

The `±` figure is the standard error of the reported time, in the same unit
as the time itself. Each benchmark keeps sampling until it is small enough
(within 1% by default), so cheap-to-measure benchmarks finish quickly and
noisy ones keep working until they have earned the precision.

Printing it absolutely rather than as a percentage is deliberate: to decide
whether two results really differ you compare the gap between them against
the `±`, and that is a direct digit-for-digit comparison only when
everything is in the same unit.

## Scaling behaviour

`bench_scaling` measures how the cost grows with `N` and reports the
constant in front of the law it found, with the same kind of error bar:

```none
fib scaling:  (0.5567 ± 0.0036)ns/N (R²=0.999)
```

It works in two stages. The first climbs from small values of `N`, timing one call
at each, until a call is long enough to time properly, and notes how fast
cost is growing; those measurements steer and are not part of the answer.
The second lays out six log-spaced values of `N` and times a single call at each,
repeatedly, until the answer is precise enough.

Repeating each size is what makes the rest work: every size ends up with an
error bar that was *measured* rather than assumed. That turns "is this the
right shape?" into a real test instead of a heuristic. Chi-squared asks
whether what the model failed to explain is as small as the error bars say
it should be, and a cost that no polynomial describes is rejected outright
rather than being fitted anyway.

Measuring stops only when the shape is settled *and* the constant is precise
enough. Both, because a wrong model does not present as an imprecise one -
fit a constant to a cost that grows and its prefactor is essentially the
mean of every measurement, precise straight away and quite wrong.

The `R²` figure answers a question the `±` cannot. Two things have to be
settled - which law, and how big its constant is - and they can fail
independently. `R²` is set to zero outright when the shape was rejected; the
`±` is the signal for the constant. Crucially, the `±` is computed from the
sizes and their error bars alone and never sees the timings, so a wrong
shape cannot hide inside a wider error bar - it has nowhere to go but the
`R²`. A tight `±` next to `R²=0.000` means "precise about a shape I could
not pin down", and deserves suspicion rather than trust.

Only power laws are fitted. A cost that is not one - `O(N log N)`, or
`O(2ᴺ)` - is reported as the integer power it most behaves like over the
range measured, with `R²=0.000` and `(limit)` to say that nothing described
it exactly.

## Quiescing the machine (Linux)

The `±` figure covers noise `scaling` can see while sampling. It cannot see
the machine around it — another process sharing your core, the CPU dropping
out of turbo as it warms up, an interrupt landing mid-sample. Those shift
the answer without widening the error bar.

This crate ships a `quiet-bench` binary that sets a machine up for
benchmarking:

```bash
sudo `which quiet-bench` reserve 2
```

(`cargo install` puts the binary on your PATH but not on root's, so a plain
`sudo quiet-bench` usually fails with "command not found". Copy it to
`/usr/local/bin` if you would rather type the short form.)

That reserves CPU 2: it moves every other process and (where the kernel
allows) every interrupt off it, offlines its SMT sibling, disables turbo,
pins the minimum frequency to 100%, switches to the performance governor,
and disables ASLR. Then run your benchmarks through it:

```bash
quiet-bench run cargo test --release
```

Benchmarks built against `scaling` **pin themselves to the reserved CPUs
automatically** — `quiet-bench run` advertises the reservation in
`SCALING_BENCH_CPUS`, and every benchmark in this crate honours it without
any code change. Set `SCALING_NO_PIN=1` to opt out.

`quiet-bench status` reports whether a reservation is in effect and whether
this process is actually on it. When you're done:

```bash
sudo `which quiet-bench` restore
```

Everything here is Linux-only and entirely optional; on other platforms, and
when no reservation is active, benchmarks simply run normally.

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
