# lab

A bench for building a benchmarker. Standalone: its own workspace, no
dependencies, and the `scaling` crate beside it never builds or sees it.

The question it exists to answer is **reproducibility** - how much an
estimate moves between independent runs of the same unchanged code. That is
the number to minimise, and a good one is a fraction of a percent.

## Use

```sh
cargo run --release -- run 4000 runs/a.csv     # measure; repeat for b, c, ...
cargo run --release -- compare runs/*.csv      # score every estimator
```

`run` touches the machine and is the slow, noisy half. `compare` is pure
arithmetic over recordings, so **a new estimator can be scored in a second
against data you already have**. Collect five or six runs once, then iterate
on `estimate.rs` without measuring anything again.

Runs should be genuinely independent - separate invocations, ideally minutes
apart - because that is the variation users actually suffer. Segments of one
long run are far more optimistic and will flatter a bad estimator.

## Where things are

| file | what it holds | how often you will touch it |
| --- | --- | --- |
| `estimate.rs` | the estimator variants | constantly |
| `workloads.rs` | the things being timed | when adding a benchmark |
| `main.rs` | calibrate, round-robin, report | rarely |
| `timing.rs` | measure, or replay from a file | almost never |

**Adding a workload**: write a `fn(&Ctx, u64, u64) -> u64` that performs
`iters` iterations and returns something derived from the work, then add a
line to `workloads::all()`. The third argument is a fresh pseudo-random seed
each call; ignore it unless the workload needs to move through memory.

**Adding an estimator**: write a `fn(&Run, &str) -> f64` and add a line to
`estimate::all()`. `Run::get` gives the per-iteration timings and
`Run::ratio` gives the per-round ratio to a canary.

## What is already known

Three results are baked into the code as comments, because each was found
the expensive way:

* **Per iteration, never per sample.** Calibration happens once at whatever
  clock speed prevailed, so iteration counts vary 10-25% between runs.
  Comparing per-sample durations across runs inherits all of that.
* **A memory canary with a fixed start is not a memory canary.** It goes
  cache resident and reads 23 ns instead of 142 ns - perfectly steady, and
  measuring the wrong thing. Same for an array that fits inside L3, and for
  calibrating with a different access pattern than production uses.
* **A random permutation each round, not an alternating sweep.** Alternating
  removes position bias and substitutes a period-2 oscillation.

And one the lab itself surfaced on its first run: `ratio_auto` picks a
canary from within-run data, flips between runs, and is much worse than
naming the canary by hand. It is kept, and labelled, as the cautionary
variant. `ratio_corr` picks differently and flips too. Selecting a
denominator appears to need several runs - like the additive `a*cpu + b*mem`
fit, which is the obvious next estimator and does not fit the single-run
signature at all.

## Replay

```sh
LAB_REPLAY=runs/a.csv cargo run --release -- run 4000 /dev/null
```

Serves recorded timings instead of measuring, so a change to the *driver* -
a stopping rule, a different round structure - can be tested against
identical data. Not needed for estimator variants; use `compare` for those.
In replay mode the workload functions are not called, so nothing load-bearing
should live inside one.
