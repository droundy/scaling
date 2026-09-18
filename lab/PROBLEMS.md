# The four things that make a benchmark lie

Four separate sources of error. They are listed separately because they have
different causes, different fixes, and different tests - but they are not
independent, and most of the trouble so far has come from mistaking one for
another. All four need testing; none is settled.

The lab exists to tell them apart. Quieting the machine suppresses (1), which
is what makes it possible to study (2), (3) and (4) without (1) drowning them.

---

## 1. The clock moves

The core frequency changes over time and with what the machine is doing, so
the same work takes a different number of nanoseconds at different moments.

**Evidence.** `cpu_canary` is a dependent multiply-add chain with no memory
footprint at all, so its cost is essentially one over the clock. Measured
alone it reads 0.9122 ns/iter; sharing a round with `mem_canary` it reads
2.3661 - **2.6x slower**, with a monotone gradient in between tracking the
neighbour's memory intensity (`nothing` 0.9125, `instant_now` 1.0276,
`btree_miss` 1.1167). Nothing about caches can slow down a workload that
does not touch memory. Unquiesced machine.

**Mitigations.** The canary ratio, which is the portable one: divide by a
canary measured in the same round and a shared clock cancels. Quieting
(`performance` governor plus `no_turbo`) pins the clock outright, but needs
privilege and so cannot be what the shipped crate relies on.

**Still to test.** Whether the ratio actually cancels it across compositions;
whether quieting flattens `cpu_canary` across subsets as predicted; whether
there is a second, *uncore* clock that the `performance` governor does not
pin (see 3).

---

## 2. A fixed cost per measurement

Each timed batch carries a cost that does not scale with the iterations in
it - the two clock reads, the boxed call, and whatever warm-up the first
iterations pay. Divided by the batch size it appears as a per-iteration cost
of `a/n`, which shrinks as the batch grows and so is invisible at any single
batch size.

**Evidence.** `Instant::now` measures 22 ns, so the two calls bracketing a
batch are ~45 ns - real but below present resolution. Ladder fits found a
far larger `~580 ns` in one 7-workload round (`btree_miss` 613 ±172,
`slice_sort` 585 ±182, `instant_now` 554 ±205, and `cpu_canary` -195 ±194,
the one workload with no working set to warm).

**But that number did not survive test 3**, so treat it as unconfirmed: see
below.

**Mitigations.** Subtract two batch sizes (`(T_4n - T_n)/3n`), which is what
the `ladder` and `intercept` subcommands do. Or simply use a bigger batch,
which shrinks `a/n` without costing a second measurement.

**Still to test.** Whether it is the timer or the warm-up - `LAB_WARMUP` runs
untimed iterations before the timed batch, so whatever decays with the prefix
is warm-up and whatever survives is harness. And whether the cost is a
*constant* at all: `mem_canary` reads 179.3 / 165.6 / 151.4 ns/iter at n, 2n,
4n, which is equal drops per doubling - logarithmic in batch length, not
`a/n`. A constant intercept would have halved each drop. Nothing that shape
can be removed by subtracting two points.

---

## 3. The number moves with the company it keeps

A workload's measured cost depends on which *other* workloads share its
round. This is the one that matters most for benchmarking several functions
together, and it is the least understood.

**Evidence.** `btree_miss`, 150,000 rounds per composition:

| round | ns/iter | intercept |
| --- | --- | --- |
| `btree_miss` alone | 14.0540 | +159 ±195 |
| `+cpu_canary` | 11.6375 | -350 ± 30 |
| `+instant_now` | 12.3319 | -572 ± 36 |
| `+mem_canary` | 12.4347 | -324 ± 27 |
| `+slice_sort` | 13.4479 | +21 ± 48 |
| `+copy_64mb` | 18.7941 | +501 ± 47 |
| **spread** | **17.2%** | **-572 to +613** |

Two things follow. The measured cost moves 17% on composition alone, which
dwarfs the ~0.5% that (2) was worth. And **the intercept is not a property of
the workload** - it is not even the same sign, at 10-20 sigma. So the ~580 ns
of (2) was a property of that one round, and the agreement across three
workloads that looked like confirmation is better explained by their having
shared a round.

Subtracting does not fix it: 17.23% raw against 16.83% subtracted.

Note that neighbours can make a benchmark *faster* - `btree_miss` gains 17%
next to `cpu_canary` - which rules out cache eviction as the whole story.
Working hypothesis: two clocks. The core clock, which ALU-heavy neighbours
drive up, and an uncore/memory clock, which sustained streaming drives up and
which a latency-bound workload like `btree_miss` benefits from. `copy_64mb`
is then the one genuine eviction effect. Untested.

**Still to test.** Whether quieting removes it (it pins the core clock, maybe
not the uncore); whether the canary ratio removes it; how long a composition
takes to *establish* its regime, which is measurable as the settling
transient at the start of each subset's block.

---

## 4. The calibrated batch size wanders

Batch sizes are calibrated per run to hit a target sample duration, and the
calibration locks in whatever the clock happened to be doing at that instant.
The parent crate found counts varying 10-25% between runs.

That would be harmless if per-iteration cost were independent of batch size.
It is not - that is precisely what (2) says - so comparing two runs compares
them at two different points on a curve, and the difference reads as a
difference in the thing being measured.

**Evidence.** Indirect so far. The per-iteration trap has already been hit
once in this lab: dividing a payload by the canary's *batch* time rather than
its per-iteration time scored every ratio at ~20% between runs against ~1%
for raw nanoseconds, purely because the canary recalibrated to a different
`n` each run.

**A second way calibration goes wrong: the first execution is the coldest.**
`copy_64mb` allocated its destination with `vec![0u8; n]`, which takes the
`alloc_zeroed` path and hands back untouched zero pages. The first copy
faulted in 64 MiB and took **47 ms against a true cost of 6.4 ms** - and
since calibration is the first thing that ever runs, that was the measurement
calibration believed. It briefly went into this file as an eleven-fold
slowdown from quieting the machine. It was nothing of the sort; quieting
costs this workload about 1.4x.

The wrong number was the small half of the problem. The growth loop uses each
probe to decide how much to grow, so a first probe inflated sevenfold makes
it stop growing almost immediately and settle on a batch far too small -
silently, and looking exactly like a property of the workload. Any allocating
workload is exposed.

Fixed in two places: `copy_64mb` fills its destination in the constructor so
the faults are paid as setup, and `calibrate` runs one untimed batch before
it measures anything and takes a median of three probes at the end rather
than the single one that happened to end the loop.

**Mitigations.** Calibrate once and share the count across everything being
compared. Or fix the count outright and let the sample duration fall where it
may.

**Still to test.** How much the counts actually wander here, and how much of
the apparent between-run and between-composition spread it accounts for. Until
then, sweeps should calibrate once and reuse, so this is held fixed rather
than left to vary alongside whatever is under study.

---

## How they interact

- (1) causes (3), at least partly: composition changes the clock, and the
  clock changes everything in the round.
- (2) and (4) multiply: a fixed cost `a` biases the per-iteration estimate by
  `a/n`, so a wandering `n` makes even a perfectly constant `a` land
  differently each time.
- (3) swamps (2) at present, which is why measuring (2) requires either
  holding composition fixed or varying it deliberately and modelling it.

## A note on method

Do not interleave subsets to defend against drift. The start of a round is
the end of the one before it, so a subset needs a contiguous stretch to
settle into its regime; interleaving keeps every subset in a permanent
transient and drags them all toward the average, which masks (3) rather than
measuring it. Defend against drift with **replication** instead - several
complete passes over the subsets, each in a fresh random order - and record
enough machine state that drift can be seen rather than inferred.

---

# What the quiet sweeps settled (2026-09-16)

## The tick is the reason long samples are bad

The fraction of measurements corrupted by an outside event is **proportional
to the batch duration**, measured four independent ways:

| batch | corrupted | rate |
| --- | --- | --- |
| 64.1 us | 6.59% | 0.103 %/us |
| 127.9 us | 12.96% | 0.101 %/us |
| 70.8 us | 7.38% | 0.104 %/us |
| 141.6 us | 14.34% | 0.101 %/us |

That is a Poisson process at ~1020 events/s costing ~5 us each. The kernel on
this machine is `CONFIG_HZ=1000`: it is the scheduler tick, recovered from
timing data alone.

A sample's chance of being wrong is its exposure time. At 100 us one
measurement in ten is hit; at 1 us, one in a thousand. This also decides what
estimator is usable - trimming 0.1% costs nothing, trimming 10% is a serious
intervention on a distribution we have already found trimming can distort.

## Two noise regimes, wanting opposite sample sizes

Absolute sd of one measurement, over a 100x range of batch sizes:

| batch | cpu_canary | nothing | btree_miss | instant_now | mem_canary |
| --- | --- | --- | --- | --- | --- |
| 0.63 us | 160 ns | 1.9 ns | 22.6 ns | 9.3 ns | 95 ns |
| 8.70 us | 158 ns | 8.8 ns | 36.0 ns | 167 ns | 1410 ns |
| 68.9 us | 173 ns | 9.4 ns | 66.0 ns | 1560 ns | 12070 ns |

**Additive** (flat absolute sd - `cpu_canary` carries a fixed ~160 ns per
measurement whatever the batch): longer batches dilute the noise.
**Multiplicative** (sd proportional to duration - `instant_now` ~2%,
`mem_canary` ~18%): longer batches buy nothing and cost proportionally more.

Best sample size by relative error per unit machine time: `mem_canary`
0.20 us, `mpsc_send` 0.25 us, `instant_now` 0.34 us, `btree_miss` 34 us,
`nothing` 35 us, `cpu_canary` 64 us. **Every optimum is at or below 70 us;
none is at 100 us.** The spread between them is 300x, so this is a per-
workload measurement, not a constant to hard-code.

## Subtraction works, and it is what makes small samples possible

At a 0.2 us bottom rung a fixed cost of 27-127 ns is most of the
measurement, so the naive estimate is ruined and composition-dependent.
`cpu_canary`, true cost 2.36 ns/iter:

| round | naive | wide | fixed |
| --- | --- | --- | --- |
| alone | 2.8668 | 2.3604 | 27 ns |
| + 5 neighbours | 4.7674 | 2.3638 | 127 ns |

Naive is wrong by 21-102% depending on company. `wide` gives 2.360-2.364 in
every one of 32 compositions. Composition spread against the pass-to-pass
null:

| workload | naive | subtracted | null |
| --- | --- | --- | --- |
| `cpu_canary` | 18.69% | **0.05%** | 0.01% |
| `btree_miss` | 3.25% | **0.03%** | 0.01% |
| `instant_now` | 0.87% | **0.03%** | 0.03% |
| `nothing` | 0.24% | **0.01%** | 0.01% |

So the intercept stays *fixed* as batches shrink rather than growing, which
is what makes the small-sample regime available at all. Problem 2 and
problem 3 turn out to be mostly the same problem, and one subtraction
answers both.

## On a quiet machine the canary ratio is a liability

The ratio columns score *worse* than raw nanoseconds - 2.58-3.60% against
0.03% for `instant_now` and `nothing`. With the clock pinned there is no
shared drift to cancel, so dividing by a second noisy measurement can only
add its variance. The canary earns its keep when the clock moves (problem 1)
and costs when it does not.

## Still not well behaved

`mem_canary` (composition spread 7.88%, null 4.60%, ~18% per-sample, ~20%
process to process) and `mpsc_send` (11.02%, null 7.54%, chaotic above 16 us)
fail every test here. `mem_canary` being one of the two instruments is the
uncomfortable part.

## Not yet done

The head-to-head: current protocol (100 us samples, naive mean) against the
proposed one (small samples plus subtraction) at **equal machine time**,
scored on run-to-run reproducibility. Everything above says the proposed one
should win; none of it is that experiment.
