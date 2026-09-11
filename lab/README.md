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
| `workloads/` | the things being timed, one module each | when adding a benchmark |
| `main.rs` | calibrate, round-robin, report | rarely |
| `timing.rs` | measure, or replay from a file | almost never |

**Adding a benchmark**: give it a module beside `workloads/cpu_canary.rs`,
or a line in `workloads/payloads.rs` if it is small. A module needs three
things:

```rust
fn gen(seed: u64) -> Input { Input::Ints(shuffled(1024, seed)) }

fn run(i: &mut Input) -> u64 {
    let v = i.ints();
    v.sort_unstable();
    v[0]
}

pub fn workload() -> Workload { Workload::new("sort_1k", Kind::Payload, gen, run) }
```

Then add `module::workload()` to `workloads::all()`.

`gen` runs **before the timer starts** and `run` inside it, so everything you
do not want in the number goes in `gen` - allocating, filling a buffer, and
in particular restoring any order the previous iteration destroyed, since
sorting an already-sorted vector measures something else entirely.

A batch of `n` iterations generates all `n` inputs first, then times `n`
calls over them. The previous batch's inputs are dropped at the *start* of
the next `prepare`, so their destructors land outside the timed region too.

Both halves are plain `fn` pointers - there is no `dyn` anywhere in this
program - so they cannot capture. Put constants in the body, as `1024` is
above. If you need an input shape `Input` does not have, add a variant and an
accessor beside `Input::ints`; that is the only place where a benchmark costs more
than its module.

**The canaries are not special to the runner.** Each is a `Workload` like any
other, keeps its own table privately behind a `LazyLock`, and goes through
the same prepare-then-run path. `Kind` labels the report; it is not a branch
in the machinery. A canary does loop internally, so its `ns/iter` is per
chunk of work rather than per operation - the absolute figure is not meant
to be read, only its ratios.

**Adding an estimator**: write a `fn(&Run, &str) -> f64` and add a line to
`estimate::all()`. `Run::get` gives the per-iteration timings and
`Run::ratio` gives the per-round ratio to a canary.

## The recording

```none
# iters cpu_canary 339205
# iters mem_canary 483
seq,round,slot,workload,t_ms,ns
0,0,0,parse_int,1789145466930,135186
1,0,1,hashmap_256,1789145466931,363568
```

`ns` is raw, for the whole batch; divide by that workload's `iters` to get
anything comparable between runs. The other columns exist because a dump
that keeps only "these timings belong to this workload" has thrown away most
of what made it raw:

* **`slot`** is the position within the round, which is reshuffled every
  round on purpose. Position matters - a memory canary immediately before a
  payload leaves that payload's cache cold - and `compare`'s `slot` column
  reports how much, per workload.
* **`round`** is explicit rather than inferred, so a gap shows up as a gap.
* **`t_ms`** is wall clock, so a run can be lined up against something that
  happened outside it: a build starting, a laptop being unplugged.
* **`seq`** is the line's own index, redundant with file order and written
  anyway so the order survives being sorted or filtered by something else.

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

And a fourth, which the lab surfaced on its own first run: **a synthetic
payload built from the same primitives as a canary is not a payload.** It
tracks that canary perfectly by construction, so every ratio estimator
scores far better on it than on real code and the table stops meaning what
it says. The first version of `workloads.rs` had one that literally called
`mem_canary`. Keep exactly two canaries and let the payloads be things you
would actually ship.

That mistake also produced the one interesting failure so far. On payloads
sitting *between* the two canaries, `ratio_auto` - which picks a denominator
from within-run data - chooses differently in different runs, so each run
reports a different quantity: 122% where naming the canary by hand gave
0.145%. On the real `std` payloads here it is stable and free. So automatic
selection is not broken, but it **fails silently** unless someone reads the
MIXED column, and making it fail safely is an open design question.

The obvious next estimator is the additive `a*cpu + b*mem`, which beats
either canary alone by ~10x on a genuinely mixed workload. It does not fit
the single-run signature: `a` and `b` cannot be fitted from one run, because
within-run jitter is mostly each canary's own noise, so the fit is
attenuated and differs per run. It needs several runs, so it belongs in
`compare`.

## Replay

```sh
LAB_REPLAY=runs/a.csv cargo run --release -- run 4000 /dev/null
```

Serves recorded timings instead of measuring, so a change to the *driver* -
a stopping rule, a different round structure - can be tested against
identical data. Not needed for estimator variants; use `compare` for those.
In replay mode the workload functions are not called, so nothing load-bearing
should live inside one.
