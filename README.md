A lightweight benchmarking library which:

* measures until it reaches an accuracy you ask for, and tells you the
  accuracy it achieved;
* can measure how a benchmark scales, as a power of its input size;
* handles benchmarks which must mutate some state;
* tells you whether two implementations really differ, correcting for how
  many such questions you asked;
* lets a whole benchmark suite be declared where it belongs and run from a
  one-line binary;
* has a very simple API!

Put an attribute on a function and it is a benchmark. These can live
anywhere in your crate, next to the code they measure:

```rust
#[scaling::bench]
fn fib_200() -> usize { fib(200) }

#[scaling::bench]
fn fib_500() -> usize { fib(500) }

// A benchmark that mutates state says where the state comes from.
#[scaling::bench(make_input = || vec![0i32; 100])]
fn reverse(xs: &mut Vec<i32>) { xs.reverse() }

#[scaling::bench(make_input = || vec![0i32; 100])]
fn sort(xs: &mut Vec<i32>) { xs.sort() }
```

The binary that runs them is one line:

```rust
// benches/bench.rs, in its entirety
scaling::main!();
```

```toml
# Cargo.toml
[[bench]]
name = "bench"
harness = false
```

`cargo bench` then yields - module-qualified, `bench::` here because that is
what `[[bench]] name = "bench"` makes `module_path!()` at the top of that
file:

```none
bench::fib_200:    71.716ns ± 0.057ns
bench::fib_500:    262.75ns ± 0.14ns
bench::reverse:     51.80ns ± 0.62ns
bench::sort:        111.3ns ± 1.1ns
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

`#[scaling::bench_scaling]` measures how the cost grows with `N` and reports
the constant in front of the law it found, with the same kind of error bar:

```rust
#[scaling::bench_scaling(nmin = 0)]
fn fib_scaling(n: usize) -> usize { fib(n) }
```

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

## Running them

Every benchmark declared anywhere in the crate is discovered, measured
together with the default accuracy and budget, and printed as a table -
that is what `scaling::main!()` above gives you. There is no `Config` to
build, no list to add to and no printing to write.

Choosing a tighter budget or list output instead of a table is a
[`runner::Options`] built by hand in your own `main`:

```rust,no_run
use scaling::runner::{run, Options};

fn main() -> std::process::ExitCode {
    let options = Options::default();
    run(options).into()
}
```

`runner::measure` hands back the results instead of printing them, for a
script that wants to look at the numbers rather than show them - reached by
name, since nobody wrote those names down: they come from the module and
function each benchmark was declared in.

## Comparisons

`group` makes a function one candidate of a comparison — a bare
`group = "name"` for one group, or `group("a", "b")` to belong to several at
once without those groups being compared with each other. `baseline` says
which candidate the others are reported against; with none marked, the
first by name is used, and the report says which it was.

```rust
#[scaling::input(group = "sorting")]
fn sorting_data() -> Vec<u64> { (0..400).rev().collect() }

#[scaling::bench(group = "sorting", baseline)]
fn stable(v: &mut Vec<u64>) { v.sort() }

#[scaling::bench(group = "sorting")]
fn unstable(v: &mut Vec<u64>) { v.sort_unstable() }
```

All the candidates are measured in one interleaved round on the *same*
generated input, which is what lets the difference between them be reported
with its own error bar rather than by subtracting two independent numbers.

Candidates and inputs are registered independently and neither names the
other - a candidate says what type it takes, an input says what type it
makes, and every pairing sharing a group and a type is measured, with no
list of the pairings anywhere. That is what lets one input feed several
candidates, and one candidate be compared against several others, just by
adding declarations:

```rust
#[scaling::bench(group = "sorting", baseline)]
fn stable(v: &mut Vec<u64>) { v.sort() }

#[scaling::bench(group = "sorting")]
fn unstable(v: &mut Vec<u64>) { v.sort_unstable() }

#[scaling::input(group = "sorting", name = "sorted")]
fn already_sorted() -> Vec<u64> { (0..400).collect() }

#[scaling::input(group = "sorting", name = "reversed")]
fn reversed() -> Vec<u64> { (0..400).rev().collect() }
```

Four declarations, four cells; a third input would make six without touching
anything already written. One group can hold several unrelated type
families, since candidates are paired only with inputs of their own type - a
`String` candidate is never handed a `Vec<u8>`; if a group's candidates
disagree on type, it simply splits into one comparison per type rather than
refusing to run. Each input's cells are a comparison; sharing a group with
more than one input, they print together as a grid:

```none
sorting  (Vec<u64>)  baseline: stable
             reversed    sorted
  stable    423.716ns  303.207ns
  unstable  393.989ns  291.241ns
                -7.0%     -3.9%
```

A group with just one input - the common case, and the only shape a plain
`group = "..."` with no `#[input]` at all ever has - prints as an ordinary
comparison instead, with no grid to read.

## Why measuring them together matters

Benchmarks run one after another are measured in different machines: the
first on a cold package, the fiftieth on a warm one. Their numbers are not
comparable with each other, nor with the same suite run tomorrow.

A suite measures them interleaved instead — one sample each, in rotation —
so every benchmark's samples spread across the whole session and all of them
average the same drift.

It is also what makes the multiple-comparison correction right. Run five
comparisons and you have five chances at a false positive; the threshold
each one is judged at comes from how many the run actually holds, which is
knowable only once they have all been collected. (So a run filtered down to
one comparison judges it more leniently — correctly, but it does mean a
filtered run and a full one are not quite asking the same question.)

What this buys is a **bound**, not an improvement. Reversing the declaration
order of eight identical workloads moves an interleaved benchmark by
0.15–0.45%, whatever the session; measured one after another instead, the
same workloads move by anywhere from 0.10% to 1.19% depending on nothing but
how much the machine happened to be drifting at the time. The typical case is
a wash — the medians are 0.28% and 0.26%. The worst case is four times
better.

That is the trade the mechanism predicts: interleaving pays a floor it never
gets back, because every sample starts on a cache the rest of the suite has
been using, in exchange for a ceiling on drift. Where there is no drift, only
the floor shows.

So it will not make any single benchmark more reproducible — it averages
drift in rather than out — and a suite's numbers are not comparable with a
the same benchmark measured on its own. What it gives you is that the
numbers within one suite, and across runs of it, were measured in the same
machine.

Each benchmark still gets the full time budget of its own, so a suite of `n`
may take `n` times as long as one benchmark.

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
