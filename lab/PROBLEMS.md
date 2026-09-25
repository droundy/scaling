# The things that make a benchmark lie

Five sources of error, listed separately because they have different causes,
different fixes and different tests. They are not independent, and most of
the trouble so far has come from mistaking one for another.

The lab exists to tell them apart. Quieting the machine suppresses (1), which
is what made it possible to study (2), (3) and (4) without (1) drowning them
- and (5) only became visible once the rest were quiet.

| | problem | status |
| --- | --- | --- |
| 1 | the clock moves | characterised; fix is quieting or the canary ratio, each with a cost |
| 2 | a fixed cost per measurement | **solved**: it is a genuine constant, and two-point subtraction removes it |
| 3 | the number moves with its neighbours | **mostly problem 2**; subtraction removes 100-370x of it |
| 4 | the calibrated batch size wanders | held fixed, not yet measured |
| 5 | outside events corrupt samples | **characterised below ~1 ms** (the scheduler tick); above that it inverts, and is largely unmeasured |

Evidence below is from quiet sweeps (`performance` governor, `no_turbo=1`,
reserved CPU) unless it says otherwise. That distinction matters more than
anything else here: several conclusions reverse between the two conditions.

---

## 1. The clock moves

The core frequency changes over time and with what the machine is doing, so
the same work takes a different number of nanoseconds at different moments.

**Unquiesced, this dominates everything.** `cpu_canary` is a dependent
multiply-add chain with no memory footprint at all, so its cost is
essentially one over the clock. Measured alone it read 0.9122 ns/iter;
sharing a round with `mem_canary` it read 2.3661 - **2.6x slower** - with a
monotone gradient in between tracking the neighbour's memory intensity
(`nothing` 0.9125, `instant_now` 1.0276, `btree_miss` 1.1167). Nothing about
caches can slow a workload that touches no memory. The governor sees the
round's average stall fraction and picks a clock for it, and every
measurement in the round inherits that choice.

**Quieting removes it.** The same spread falls to 0.07% across compositions
with the clock pinned, and to 0.37% (null 0.02%) in the large overnight
sweep. That is a 200-fold reduction and it is the single most effective
intervention found.

**But quieting is not free and not portable.** It needs privilege, so the
shipped crate cannot rely on it. It also moves the operating point: a pinned
core at base clock drives memory far more slowly, and `copy_64mb` costs 6.4
ms quiet against 4.5 ms unquiet. A number measured on a quiesced machine does
not describe the machine anyone actually runs on.

**And on a quiet machine the canary ratio is a liability.** Dividing by a
canary measured in the same round is the portable fix, and it works when
there is shared drift to cancel. When there is not, it can only add the
canary's own variance: the ratio columns scored 2.58-3.60% against 0.03% for
raw nanoseconds on `instant_now` and `nothing`. The canary earns its keep
exactly when the clock moves and costs when it does not, so whether to divide
is a decision that needs making per run, not once.

**Still open.** Whether there is a second, *uncore* clock that the
`performance` governor does not pin. Unquiesced, `btree_miss` got *faster*
next to `mem_canary` (14.05 -> 12.43 ns/iter), which a core-clock story
cannot explain - a stall-heavy neighbour should have slowed it. The
hypothesis is that sustained streaming drives a memory-side clock up and a
latency-bound workload benefits. Untested.

---

## 2. A fixed cost per measurement

Each timed batch carries a cost that does not scale with the iterations in
it - the two clock reads, the boxed call, and whatever the first iterations
pay to bring the working set back. Divided by the batch size it appears as
`a/n` per iteration, invisible at any single batch size.

**It is a genuine constant.** Seven rungs from 12 us to 986 us, per-iteration
cost in ns:

```
btree_miss    28.2565  28.1321  28.0694  28.0470  28.0746  28.0862  28.0940
cpu_canary     2.3904   2.3745   2.3669   2.3645   2.3654   2.3659   2.3655
instant_now   32.3666  32.3106  32.3076  32.2963  32.2944  32.2926  32.2899
mem_canary   135.9451 135.5053 134.0991 132.8573 132.4142 132.2840 132.0870
```

`btree_miss`'s successive drops are 0.124, 0.063, 0.022 - halving with each
doubling, which is the signature of `b + a/n` and not of anything else.
`cpu_canary` does the same (0.0159, 0.0076, 0.0024). Both then flatten and
tick slightly *upward* at the longest batches, which is problem 5 arriving.

**Size.** It depends on the round, but is constant within one. From the shape
sweep: `instant_now` 27 ns, `btree_miss` 76 ns, `cpu_canary` 165 ns,
`mem_canary` 277 ns. From the small-sample sweep, `cpu_canary` 27 ns alone
and 127 ns with five neighbours - which is problem 3, below.

**Subtraction removes it exactly.** `(T_last - T_first)/(n_last - n_first)`,
which is what `wide` computes. Least squares over all rungs is a dead heat
with it, within 0.05 percentage points on every workload: under
multiplicative noise the extreme pair is very nearly the optimal design, and
an unweighted fit slightly over-trusts the noisiest rung. Use two points; the
middle rungs are worth keeping as a check that the line is a line, not as
part of the estimate.

**Where it comes from is still open.** An untimed warm-up prefix
(`LAB_WARMUP`) removes only ~50 ns and saturates by 16 iterations. So the
300-570 ns that neighbours add is *not* recovered by re-running the workload
beforehand, which is strange - running it should restore exactly what the
neighbours evicted. Either the prefix does not reproduce the state the real
batch needs, or the cost lives in the measurement machinery rather than the
workload.

**Superseded.** An earlier reading of `~580 ns` shared across three workloads
was real but was a property of *that one round composition*, not of the
harness; the agreement that looked like confirmation is better explained by
their having shared a round. And `mem_canary` read 179.3 / 165.6 / 151.4
ns/iter at n, 2n, 4n unquiesced - equal drops per doubling, which is
logarithmic and could never be subtracted away. Quiet, it is 135.9 -> 132.1
over 64x, and the shape is noisy rather than logarithmic. That was the clock.

---

## 3. The number moves with the company it keeps

A workload's measured cost depends on which *other* workloads share its
round. This matters most for benchmarking several functions together.

**Unquiesced it is huge and cannot be subtracted away.** `btree_miss` across
six compositions, 150,000 rounds each:

| round | ns/iter | intercept |
| --- | --- | --- |
| `btree_miss` alone | 14.0540 | +159 ±195 |
| `+cpu_canary` | 11.6375 | -350 ± 30 |
| `+instant_now` | 12.3319 | -572 ± 36 |
| `+mem_canary` | 12.4347 | -324 ± 27 |
| `+slice_sort` | 13.4479 | +21 ± 48 |
| `+copy_64mb` | 18.7941 | +501 ± 47 |
| **spread** | **17.2%** | **-572 to +613** |

17.2% raw, and 16.83% after subtracting - no help at all. The intercept is
not even the same sign across compositions.

**Quiet, it is small and subtraction removes nearly all of it.** Same
workload, 255 subsets, four passes, with the pass-to-pass null underneath:

| | naive | subtracted | null |
| --- | --- | --- | --- |
| unquiet, ~100 us samples | 17.23% | 16.83% | not measured |
| quiet, ~100 us samples | 0.84% | **0.10%** | 0.04% |
| quiet, small samples | 3.25% | **0.03%** | 0.01% |

**Why subtraction reverses between the two conditions:** unquiet, the
composition effect is multiplicative - the neighbours change the clock, and
no subtraction can touch a multiplicative error. Quiet, what remains is an
additive per-measurement cost, which is precisely what a subtraction removes.
The two conditions were not disagreeing about the same quantity.

**The mechanism, with a working control.** From the 255-subset sweep:

```
btree_miss                                    28.0382   fixed  -86 ns
btree_miss+nothing                            28.0391   fixed  -85 ns
btree_miss+cpu_canary+instant_now+slice_sort  28.2816   fixed +260 ns
btree_miss+mem_canary+mpsc_send+slice_sort    28.4505   fixed +486 ns
```

`nothing` - a black-boxed empty loop - changes nothing at all, so it is the
neighbours' footprint and not merely their presence in the round. Real
neighbours add 300-570 ns per measurement, and `wide` stays at 28.08-28.19
throughout.

**At small samples this is the whole measurement.** `cpu_canary`, true cost
2.36 ns/iter, bottom rung 126 ns:

| round | naive | wide |
| --- | --- | --- |
| alone | 2.8668 | 2.3604 |
| + 5 neighbours | 4.7674 | 2.3638 |

Naive is wrong by 21-102% depending on company; `wide` returns 2.360-2.364
in all 32 compositions. Composition spread 18.69% -> **0.05%** against a
0.01% null.

So problems 2 and 3 are largely one problem, and one subtraction answers
both. That is also what makes small samples possible at all: the fixed cost
stays *fixed* as batches shrink rather than growing.

**Still open.** How long a composition takes to *establish* its regime. The
settling transient at the start of each subset's block is recorded and has
not been looked at.

---

## 4. The calibrated batch size wanders

Batch sizes are calibrated per run to hit a target duration, and calibration
locks in whatever the machine happened to be doing at that instant. The
parent crate found counts varying 10-25% between runs.

That would be harmless if per-iteration cost were independent of batch size.
It is not - that is exactly what (2) says - so comparing two runs compares
two different points on a curve, and the difference reads as a difference in
the thing being measured.

**Two ways it has actually bitten, both silent:**

*The per-iteration trap.* Dividing a payload by the canary's **batch** time
rather than its per-iteration time scored every ratio at ~20% between runs
against ~1% for raw nanoseconds, purely because the canary had recalibrated
to a different `n`.

*The first execution is the coldest.* `copy_64mb` allocated its destination
with `vec![0u8; n]`, which takes the `alloc_zeroed` path and returns
untouched zero pages. The first copy faulted in 64 MiB: **47 ms against a
true 6.4 ms**. Calibration is the first thing that runs, so that was the
measurement it believed - and it briefly went into this file as an eleven-fold
slowdown from quieting. It was nothing of the sort; quieting costs this
workload about 1.4x. The wrong number was the smaller half: the growth loop
uses each probe to decide how far to grow, so a first probe inflated
sevenfold makes it stop almost immediately and settle on a batch far too
small. Any allocating workload is exposed.

**Mitigations in place.** `copy_64mb` fills its destination in the
constructor so the faults are paid as setup; `calibrate` runs one untimed
batch before measuring anything and takes a median of three probes rather
than the single one that ended the loop; sweeps calibrate once and share the
counts, so a workload is measured at the same batch size in every subset.

**Still open.** How much the counts actually wander here, and how much of the
between-run spread that accounts for. Currently held fixed rather than
measured.

---

## 5. Outside events corrupt whole samples

Discovered rather than anticipated, and it is the most generally useful thing
the lab has produced.

The fraction of measurements corrupted by an outside event is **proportional
to the batch duration**:

| batch | corrupted | rate |
| --- | --- | --- |
| 64.1 us | 6.59% | 0.103 %/us |
| 127.9 us | 12.96% | 0.101 %/us |
| 70.8 us | 7.38% | 0.104 %/us |
| 141.6 us | 14.34% | 0.101 %/us |

Four independent measurements agreeing to 3%: a Poisson process at ~1020
events/s costing ~5 us each. The kernel here is `CONFIG_HZ=1000`. It is the
scheduler tick, recovered from timing data alone.

The distribution is a sharp spike at the median with a contaminated tail -
`cpu_canary` at 128 us is +3407 ns at p90 and +5792 ns at p99 above its
median - so this is not a widening, it is a fraction of samples being
replaced by wrong ones.

**But this is a regime, not a ceiling.** The numbers above all come from
samples shorter than a millisecond, where the expected number of hits per
sample is well below one: rare, and enormous relative to the sample. The
crossover is at one expected hit, `T = 1/r` - about **1 ms** here - and
above it the picture inverts. `copy_64mb`, the one genuinely slow workload
measured:

| batch | expected ticks | rel sd observed | tick model predicts |
| --- | --- | --- | --- |
| 6.2 ms | 6.3 | 1.242% | 0.203% |
| 12.4 ms | 12.7 | 1.226% | 0.143% |
| 18.6 ms | 19.0 | 1.252% | 0.117% |
| 24.8 ms | 25.3 | 1.232% | 0.101% |

Every sample is hit many times over, so the Poisson fluctuation averages
down - the tick's *relative* contribution falls as `E*sqrt(r/T)`, one over
root T. And it is not what limits this workload anyway: the observed 1.24% is
flat across a 4x range, which is the multiplicative signature of memory
bandwidth varying, and it is six to twelve times larger than the tick model
predicts. **For a slow function the tick is a minor term.**

So there are two regimes needing two different treatments:

- **Below ~1 ms: rare and catastrophic.** A hit is 5 us on a 1 us sample.
  A median or trimmed mean removes them almost losslessly, because the
  contaminated fraction is `r*T` - 0.1% at 1 us - and they sit far from the
  bulk. Robust estimator, and prefer short samples.
- **Above ~1 ms: ubiquitous and nearly constant.** There is no clean
  population to trim to, and no need: the variation averages. What is left
  is a systematic tax of `r*E` - very roughly 0.5%, though `E` is an
  order-of-magnitude estimate from the p99 excess on short samples, so call
  it uncertain to a factor of two. Ordinary mean, and the tick is probably
  not your problem.

**The two regimes measure different quantities**, which is worth being
deliberate about rather than discovering later. A trimmed short-sample
estimate reports the *interrupt-free* cost; a long-sample mean reports the
cost *including* its share of interrupt overhead. For predicting real-world
throughput the taxed number is arguably the honest one.

**For slow functions the fix is different in kind.** You cannot choose a
short sample when one call takes 10 ms, so the lever has to be elsewhere:
stop the tick rather than dodge it (`nohz_full` plus `isolcpus` - this kernel
is already `CONFIG_NO_HZ_FULL=y`, which the quiet harness does not currently
exploit), or measure `CLOCK_THREAD_CPUTIME_ID` rather than wall clock so
preemption is excluded, or simply accept a known and roughly constant tax.
None of those has been tried.

**Unmeasured.** Everything about the slow regime rests on one workload.
`copy_64mb` and `slice_sort` (234 us for a single iteration) are the only
workloads here that cannot be made short, and both were excluded from the
small-sample sweep precisely because they have no small end. The lab has
essentially no data above 1 ms.

---

## Choosing a sample size

Absolute standard deviation of one measurement, over a 100x range:

| batch | cpu_canary | nothing | btree_miss | instant_now | mem_canary |
| --- | --- | --- | --- | --- | --- |
| 0.63 us | 160 ns | 1.9 ns | 22.6 ns | 9.3 ns | 95 ns |
| 8.70 us | 158 ns | 8.8 ns | 36.0 ns | 167 ns | 1410 ns |
| 68.9 us | 173 ns | 9.4 ns | 66.0 ns | 1560 ns | 12070 ns |

Two regimes, wanting opposite things:

- **Additive** - flat absolute sd. `cpu_canary` carries a fixed ~160 ns per
  measurement whatever the batch length. Longer batches dilute it, so longer
  is better until (5) takes over.
- **Multiplicative** - sd proportional to duration. `instant_now` ~2%,
  `mem_canary` ~18%. Longer batches buy nothing per sample while costing
  proportionally more time, so shorter is better, steeply.

Best sample size by relative error per unit of machine time: `mem_canary`
0.20 us, `mpsc_send` 0.25 us, `instant_now` 0.34 us, `btree_miss` 34 us,
`nothing` 35 us, `cpu_canary` 64 us.

**Every optimum is at or below 70 us; none is at 100 us**, so the 100 us
default is past optimal for all six workloads, by between 1.5x and 300x. The
300x spread between them says this is a per-workload measurement, not a
constant to hard-code.

The recipe: ladder a workload, see whether absolute or relative sd is the
flat one, and pick accordingly.

**Scope.** All six of those workloads run in under 160 us per sample at the
top of their ladders, and most of them in under 10. **Sample size is only a
free parameter for functions faster than the sample you want.** A function
that takes 10 ms a call has exactly one choice, `n = 1`, and nothing in this
section applies to it - there is no optimum to find, and the questions that
matter become which clock to read and whether to stop the tick (see 5). An
earlier version of this file turned the observation above into a general
"never go far above ~50 us", which was an overreach from a set of workloads
chosen for being short.

The catch at the small end is that it is only available with the intercept
removed. At a 0.2 us batch a 27-127 ns fixed cost is most of the
measurement. Subtraction is what unlocks that regime - and note that this
cuts the other way too, since a slow function's single-iteration sample
carries that same fixed cost as a negligible fraction and needs no
subtraction at all.

---

## Why a ratio blows up

`lab pairs` counts a trial as a blowup when it stops believing it has met
its goal, but it is actually more than 4x the goal from the long-run ratio.
With an honest one-sigma bar that should almost never happen: a Gaussian
error of 4 sigma has odds of about 1 in 16,000. On the six `top2` recordings
the paired estimator did far worse than that. There are three separate
causes, found by dumping every trial with `LAB_TRIALS=file`. Each has a
different remedy.

**1. Mixed pairs on a noisy machine: the ratio itself moves (~30%).** A
clock-bound workload over a memory-bound one has no fixed ratio when the
clock moves. `btree_miss / f64_sin` over 13 s windows has an rms spread of
3-16%, depending on the pass. No estimator can fix this, and scoring them
against a long-run "truth" is only a reminder of it.

**2. Clock/clock pairs: the bar is sometimes lucky-small (0.3-0.4%).** This
is pure statistics. The machine plays no part in it:

- The early stopping checks judge the bar from 4 blocks, which is 3 degrees
  of freedom.
- A bar with 3 degrees of freedom comes out under half its true size about
  14% of the time.
- The stopping rule checks again and again, so it stops on exactly those
  lucky-small bars.

These blowups show the signature of that:

- More than half stopped at the 60-round floor, against a quarter of all
  trials.
- All the blocks agree with each other, so the bar is small, but they are
  all shifted the same way.
- The next 60 rounds are usually fine.

Simulated iid Gaussian rounds under the same stopping rule give 0.3-1.5%
blowups, the same rate with no machine at all. Requiring **at least 8
blocks** fixes it. That means a floor of 120 rounds, and it is now the
default; `LAB_PAIR_BLOCKS=4` restores the old rule.

| | blowups, 4 blocks | blowups, 8 blocks | rounds spent, at 2% / 1% / 0.5% goal |
| --- | --- | --- | --- |
| quiet | 33 / 10,731 | 1 / 10,742 | x1.58 / x1.22 / x1.03 |
| noisy | 46 / 10,348 | 14 / 10,328 | x1.44 / x1.14 / x1.03 |

It also helps coverage: 65-68% becomes 68-79%. The Student-t correction on
its own (`LAB_PAIR_T`) halves the blowups on the quiet recordings and does
nothing on the noisy ones. It inflates the bar by 20% at 3 degrees of
freedom, which is not enough to stop the lucky-small stops.

Scored cell by cell, with the paired estimator:

- **Quiet machine.** Clock/clock cells pass 54 of 54, up from 52.
- **Noisy machine.** They pass 51 of 54, up from 46. The three still
  failing are 0.5% goals with 3-4 blowups each, which is cause 3 below.
- **Mixed and memory pairs.** These go from 18 to 34 of 81 passing on the
  quiet machine and from 2 to 6 on the noisy one.

**Smaller blocks do not make it cheaper.** Another way to get more blocks
from fewer rounds is to shrink them (`LAB_PAIR_BLOCK_ROUNDS`). Blocks are 15
rounds so that each one reliably holds both rungs of both workloads. On
quiet clock/clock pairs, mean rounds per trial at a 2% / 1% goal were:

| blocks | floor | blowups | rounds, 2% goal | rounds, 1% goal |
| --- | --- | --- | --- | --- |
| 4 of 15 rounds (old default) | 60 | 33 | 99 | 281 |
| 8 of 15 rounds | 120 | 1 | 156 | 342 |
| 6 of 10 rounds | 60 | 10 | 129 | 344 |
| 8 of 10 rounds | 80 | 1 | 156 | 362 |
| 8 of 6 rounds | 48 | 0 | 196 | 383 |
| 12 of 5 rounds | 60 | 0 | 225 | 390 |

Lower floors cost *more*. Two reasons:

- Much of what 4 blocks seemed to save was stops made on a lucky-small bar,
  so an honest bar can only move later. It stops when the noise says it
  may, wherever the floor is.
- A block of a few rounds leaves each rung cell one or two samples to trim,
  so its estimate is noisier than the full estimator's. The bar then
  overstates the error: coverage rises to 78%, and the trial runs longer.

The noisy recordings agree. Fifteen-round blocks with a minimum of 8 are as
good as any variant tried.

**A stricter cutoff for fewer blocks works, but costs more.** The other
principled fix is to require confidence that the bar is tight enough. A bar
`s` from `b` blocks bounds the true error by `s * sqrt((b-1) / chi2_{b-1})`
at a chosen confidence, and the trial stops only when that bound meets the
goal (`LAB_PAIR_CONF_Z`). The bound is stricter the fewer the blocks. The
"relative" variant (`LAB_PAIR_CONF_REL`) divides by the factor at 20
blocks, so that only thin bars are held to more.

Quiet clock/clock pairs; rounds at the 2% / 1% / 0.5% goals, relative to
the old 4 blocks:

| rule | blowups | rounds |
| --- | --- | --- |
| 4 blocks (old default) | 33 | x1 / x1 / x1 |
| 8 blocks | 1 | x1.58 / x1.22 / x1.03 |
| 90% confident | 1 | x1.85 / x1.64 / x1.48 |
| 70% confident | 7 | x1.31 / x1.28 / x1.20 |
| relative, 90% | 8 | x1.34 / x1.18 / x1.03 |
| 8 blocks and relative, 90% | 0 | x1.74 / x1.28 / x1.03 |

On the noisy recordings every variant lands at 11-20 blowups, against 46
with 4 blocks.

The model is right about which way to lean and wrong about how far. It
assumes Gaussian block means looked at once. Here the rule looks again and
again, and the tails are heavier than Gaussian. The relative 90% rule is
cheaper than 8 blocks only at the loosest goal (x1.34 against x1.58), and
it keeps 8 of the 33 blowups. An absolute confidence level is stricter at every block count, so
it charges for long trials that were already safe.

A plain floor of 8 blocks is the simplest, and nothing tried beats it on
cost for the same safety.

**3. Memory-bound workloads: slow episodes longer than a measurement (1-4%,
even quiet).** On the quiet machine, `btree_miss` has minutes in which it
runs 1.5-4% slow, sometimes several in a row; `copy_64mb` weakly shares
them. Over the same minutes, `cpu_canary` is flat to 0.0%. The episodes do
not follow temperature. They follow the number of runnable processes only
faintly, and that is sampled once a second, which is too coarse to rule out
activity on other cores. Their cause is not identified.

A variation slower than a measurement is invisible to that measurement's
bar, since every block sees the same episode. So these blowups are not
statistical, and 8 blocks only trims them (1.8% to 1.2%). In 13 s windows
the rms wander of a `btree_miss` ratio is 1.2-1.7%, and 6-17% of those
windows are more than 2% from the long-run value. **A 0.5% goal is below
what `btree_miss` reproduces to at these time scales.** A measurement could
detect the problem, with a split-half check or block variance that grows
with block size, and refuse the answer. It cannot measure its way past it.

The same mechanism, weaker, is behind what 8 blocks leaves on noisy
clock/clock pairs: 10 of 14 are at the 0.5% goal, where trials are long.
Even matched ratios wander 0.3-1.8% per 13 s on the noisy machine, with
`str_find / urandom_read` the worst.

**Reading pass/fail.** In aggregate, clock/clock pairs were already under the
1% blowup limit. Most of their failing cells were 3 blowups where 2 would
pass, at a mean of about one per 200 trials.

---

## How they interact

- (1) causes much of (3): composition changes the clock, and the clock
  changes everything in the round. Removing (1) by quieting is what turned
  (3) into something subtraction could fix.
- (2) and (3) are largely the same problem once (1) is gone - a fixed cost
  per measurement whose size depends on the neighbours but not on the batch.
- (2) and (4) multiply: a fixed cost `a` biases the estimate by `a/n`, so a
  wandering `n` makes even a perfectly constant `a` land differently.
- (5) sets a ceiling on sample size that none of the others can argue with,
  and (2) sets the floor.

---

## Method notes

**Do not interleave subsets to defend against drift.** The start of a round
is the end of the one before it, so a subset needs a contiguous stretch to
settle into its regime; interleaving keeps every subset in a permanent
transient and drags them all toward the average, which masks (3) rather than
measuring it. Defend against drift with **replication** - several complete
passes over the subsets, each in a fresh random order.

**Always report the null.** The spread of one composition *across passes* is
what the spread *across compositions* has to beat. Without it, a night of
thermal drift and a real composition effect are the same number. The 17.2%
figure in (3) was reported before that line existed.

**Ladder in durations, not counts.** The fixed cost is paid per measurement,
so what makes it visible is how long a batch runs. `counts_for` derives
counts from target durations and gives a workload too slow for a target the
tightest ladder that still has distinct rungs - `copy_64mb` gets 1,2,3,4
iterations from the same spec that gives `cpu_canary` a geometric ladder,
rather than a geometric blowup that would buy lever arm and pay for it in (5).

**Record machine state, do not infer it.** An evening went into deducing the
clock from `cpu_canary`'s timings before anything logged
`scaling_cur_freq`.

**One measurement of a first execution is not a measurement.** See (4).

---

## Parked ideas, and dead ends worth not re-exploring

**Count page faults and context switches alongside the timing.**
`PERF_COUNT_SW_PAGE_FAULTS` and `PERF_COUNT_SW_CONTEXT_SWITCHES` are software
perf events, readable through the same mmap page as the cycle counter, and
available at any `perf_event_paranoid` level for one's own process. They turn
"this sample looks wrong" into "this sample took 412 page faults and 2
context switches", which is the difference between a tripwire and a
diagnosis. Cheap, and useful whatever instrument is chosen.

**Measured: the hardware cycle counter is a far better instrument, where it
applies.** `perf_event_open` with `PERF_COUNT_HW_CPU_CYCLES` and
`exclude_kernel=1`, read via `rdpmc`, on a 200 us ALU batch:

| instrument | rel sd | >1 us over median | p99 excess | read cost |
| --- | --- | --- | --- | --- |
| `CLOCK_MONOTONIC` | 0.412% | 20.16% | 4831 ns | 31.4 ns |
| `CLOCK_THREAD_CPUTIME_ID` | 0.410% | 20.11% | 4773 ns | 340.4 ns |
| cycles, `exclude_kernel` | **0.031%** | **0.00%** | 199 ns | **9.3 ns** |

The tick disappears entirely, and it is cheaper to read than the clock we use
now. Note the comparison is against the *current* protocol, not against small
samples with a robust estimator - the incremental gain over that is unmeasured.

**Dead end: `CLOCK_THREAD_CPUTIME_ID`.** Measured, and it removes nothing -
20.11% contaminated against the wall clock's 20.16%. It excludes time the
thread was *not running*, but a timer interrupt does not deschedule you: the
handler runs in your context and its time is charged to your thread. It also
costs 340 ns a read against 31 ns, eleven times the current clock, which
would dominate any sample under ~30 us. Do not revisit.

**Rejected: `nohz_full`.** Context tracking adds work to every syscall entry
and exit, so syscall-heavy benchmarks would get measurably slower and we
would be benchmarking our own mitigation. It also needs a reboot plus
`isolcpus` and `rcu_nocbs` on the same CPUs, and only stops the tick when
exactly one task is runnable there. The one thing it would still buy that the
cycle counter does not is removing the tick's *indirect* cost - the handler
pollutes cache and TLB, and our subsequent work runs slower even when the
handler's own cycles are not charged to us. Negligible on an ALU workload
(0.031% sd); unmeasured on a memory-bound one.

**Dead end: detecting syscalls to decide whether cycles are valid.** seccomp
answers "does this function make syscalls", but the question is "does the
user-cycle count see this function's whole cost", and those diverge. Three
ways to break the counter, and a syscall detector sees one:

| | kernel time | syscall | seccomp sees it |
| --- | --- | --- | --- |
| syscalls | yes | yes | yes |
| page faults | yes | **no** | no |
| involuntary preemption | not running | **no** | no |

Measured: touching 256 MiB of fresh anonymous pages takes 65,536 page faults
and **zero syscalls**, and the cycle counter sees 21% of the cost. seccomp
would have called it pure user-space. This is not contrived - it is exactly
`copy_64mb`'s first touch, which already produced a phantom 47 ms in this
project, and any allocating benchmark is exposed.

The ratio of user cycles to wall time catches all three, because it measures
the thing we care about instead of a proxy for it. Against a pure user-space
reference - which is what a cpu canary already is:

| workload | cyc/ns | vs reference |
| --- | --- | --- |
| spin (ALU) | 1.688 | 99.5% |
| `getpid` | 0.551 | 32.5% |
| read `/dev/urandom` | 0.112 | 6.6% |
| `nanosleep` 20 us | 0.002 | 0.1% |

Take the **median** ratio, never the mean: a pure user-space function that
catches a tick reads ~97.6% at 200 us samples, which would fail a 0.98
threshold on contaminated samples alone.

**Still open for seccomp.** It gives a *hard* guarantee where the ratio gives
a sample: a function that syscalls once in a million calls would slip past
calibration but die at once under `SECCOMP_RET_KILL`. The strong version -
measurement in a forked child under a filter, results returned through a
buffer mmap'd before the filter goes on - is a real design. It hardens the
one failure mode seccomp can see while leaving the two it cannot, so it is
worth revisiting only if a rare-syscall workload actually bites.

---

## Not yet done: the head-to-head

Everything above predicts that small samples plus subtraction beats the
current protocol. None of it is that experiment. Here is the one to run.

**Claim under test.** At equal machine time, a protocol using small samples
and a two-point subtraction produces a *more reproducible* per-iteration
number than 100 us samples with a trimmed mean.

**Reproducible across what.** Separate **processes**, separated in time -
each recalibrating from scratch, rebuilding its own workloads. Not blocks
within a run. Problem 4 only appears across processes, and so does
`mem_canary`'s 20% process-to-process wander. Block-splitting would score the
estimators while hiding two of the five problems.

**A 2x2, not a duel.** Sample size and estimator are separate choices and
cost the same to test together, so vary both:

| cell | rungs (multiples of SAMPLE) | estimator |
| --- | --- | --- |
| A — current | 1.0 (~100 us) | naive trimmed mean |
| B — proposed | 0.05, 0.4 (~5, 40 us) | two-point subtraction |
| C | 0.05 (~5 us) | naive trimmed mean |
| D | 1.0, 8.0 (~100, 800 us) | two-point subtraction |

C and D are what make the result interpretable: if B beats A, C says whether
that came from the small samples and D says whether it came from the
subtraction. A duel between A and B could not tell those apart.

**Equal machine time, not equal rounds.** B's ladder costs more per round
than A's single batch, so fixing the round count would hand A the advantage
and call it a result. Each cell gets the same **wall-clock budget** - this
needs a `LAB_BUDGET=<seconds>` mode, since sweeps currently take a round
count - and single-rung ladders need to be legal, which `ladder_from_env`
currently rejects.

**Shape.** All four workloads in one round together, which is both the
realistic case and the harder one. Suggested: `btree_miss` (additive,
optimum 34 us), `instant_now` (multiplicative, optimum 0.34 us),
`cpu_canary` (additive, optimum 64 us) and `slice_sort` (234 us for a single
iteration, so it sits in the crossover of (5) and cannot reach the small
cells at all). One process yields all four workloads' estimates at once.

**This 2x2 only tests fast functions, and needs a sibling.** Every cell above
assumes sample size is a free parameter, which it is only for functions
faster than the sample you want. For a function that takes milliseconds per
call there is one sample size, `n = 1`, and the 2x2 collapses. The
interesting comparison there is a different one entirely - wall clock against
`CLOCK_THREAD_CPUTIME_ID`, and tick-on against `nohz_full` - scored the same
way, on run-to-run reproducibility across separate processes. `copy_64mb`
(6.2 ms) is the workload in hand; a synthetic sleep-free spin of a chosen
duration would be better, since it would let the crossover at ~1 ms be
swept rather than sampled at one point.

24 repeats per cell; 20 s per cell per repeat. At 20 s even the coarsest cell
takes ~200,000 samples, so within-run statistical error is ~0.0005% - far
below the run-to-run spread being measured, which is the point: what gets
scored is systematic irreproducibility, not counting noise. Total 4 x 24 x
20 s ~= 32 min plus process setup.

**Order.** Cells shuffled within each repeat, so drift across the hour cannot
load onto one cell - the same reason sweeps use passes in random order.

**Second arm, and the more honest one.** Repeat with the round *composition
varying* between repeats (drop a random neighbour each time). Nobody re-runs
an identical suite; in practice the company a benchmark keeps changes between
one invocation and the next, and that is exactly the condition subtraction is
supposed to survive. Another ~32 min.

**Report both spread and centre.** A protocol can be perfectly reproducible
and wrong. The subtracted cells will read *lower* than the naive ones by the
fixed cost, which is the bias we have independently shown is real - so the
centres must be reported next to the spreads rather than only the spreads.

**Decision rule, fixed in advance.** B must beat A by at least 2x in
run-to-run spread to justify the extra machinery. If B wins by less than
that, or if C alone accounts for the gain, the recommendation is simply
"use smaller samples" and the ladder stays a diagnostic rather than becoming
part of the measurement protocol.

`mem_canary` and `mpsc_send` fail every reliability test here - `mem_canary`
at 7.88% composition spread against a 4.60% null, ~18% per-sample, ~20%
process to process; `mpsc_send` at 11.02% against 7.54%, and chaotic above
16 us. `mem_canary` being one of the two instruments is the uncomfortable
part, and a memory canary whose own reading moves 20% between processes
cannot calibrate anything.
