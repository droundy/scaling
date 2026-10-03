# Measuring a time, and a ratio of times

This is the algorithm the lab has converged on, written to be implemented
cleanly in `scaling` itself. Part 1 is the design: what each step does, why,
and the evidence for it. Part 2 is a first draft of the user documentation
for the implementation.

The evidence lives in [PROBLEMS.md](PROBLEMS.md). The numbers quoted here
come from the six `top2` pair recordings in `day/collect/pairs-{quiet,noisy}`:
about 250,000 rounds each, on one machine (hybrid Intel, P-cores, quiet
pinned at 1.7 GHz or noisy with turbo to 4.4 GHz). Scoring replays the
algorithm from many starting points against each recording's own long-run
answer. Where something below has *not* been checked that way, it says so.

---

# Part 1: design

## The algorithm on one page

1. **Calibrate** each function into two batch sizes, `N` and `2N`, with the
   larger batch at most 20 us. A function too slow for that gets `1` and
   `2`, or just `1` if a call takes more than half a second.
2. **Measure in rounds.** Each round times every function in the set once,
   in a fresh random order, each at one of its two batch sizes picked by a
   coin flip. A CPU canary is a member of every round. Nobody leaves early.
3. **Estimate a ratio** between two functions from rounds they shared:
   - Take the log of each round's time ratio.
   - Fit a two-way model over those logs.
   - Exponentiate and subtract the batch sizes.

   The clock cancels inside each round, and the fixed cost per measurement
   cancels in the subtraction.
4. **Estimate an absolute time** for each function by subtracting its two
   batch sizes. If the canary shows the clock moving, report
   **bogo-nanoseconds** instead: the ratio to the canary, scaled to what
   it would be at the CPU's base clock.
5. **Put an error bar on everything by batch means, on a log scale:**
   - Cut the rounds into at least 8 contiguous blocks of at least 15
     rounds, and at most 20 blocks.
   - Estimate in each block.
   - The spread of the logs is the bar, read as a factor.
6. **Stop** when every number the user asked about is known to within the
   goal as a factor. Check first at 120 rounds, then every 1.3x more.
7. **Measure everything twice**, as two whole passes over the suite, each
   at a goal √2 looser, and combine them. Two passes that disagree get a
   third. A result that still cannot meet its goal is refused, not printed.
8. **Report a ratio in words people use:**
   - a percentage when it is small;
   - a factor when it is large;
   - the uncertainty as `±` a percentage.

   Absolute times always go alongside.

Each step answers a specific failure. The rest of Part 1 takes them in
order.

## Vocabulary

- **Iteration**: one call of the function.
- **Batch**: `n` iterations timed together. Its time is one *sample*.
- **Rung**: a batch size. Every function here has one or two.
- **Round**: one sample of every function in the set, in random order.
- **Pass**: one complete measurement of the whole suite, to a goal.
- **Goal**: the uncertainty asked for, as a fraction: 1% means "known to
  within a factor of 1.01".
- **Blowup**: a result more than 4x the goal from the truth while claiming
  to have met the goal. An honest one-sigma bar does this about 1 time in
  16,000. It is the failure this design is built to prevent.

## 1. Calibration: two batch sizes per function

**What.** Run one untimed batch, then time batches of `n = 1, 2, 4, ...`
until the next doubling would exceed **20 us**. Re-time the last size three
times and take the median. The rungs are the last two sizes, `N` and `2N`,
and the lower one has to be at least 100 ns.

- If one call already exceeds 20 us, the rungs are `1` and `2`, as long as
  two calls take at most a second.
- If two calls take more than a second, the only rung is `1`.

**Why two rungs.** Every timed batch carries a fixed cost that does not
scale with `n`: the clock reads, the call, and rewarming whatever the
neighbours evicted. Divided by `n`, it looks like part of the
per-iteration cost.

- It is a genuine constant (PROBLEMS.md, 2).
- It depends on which other functions share the round: 300-570 ns from real
  neighbours (PROBLEMS.md, 3).
- `(t(2N) - t(N)) / N` removes it exactly. On `cpu_canary` the composition
  spread fell from 18.69% to 0.05% against a 0.01% null.

**Why the top two, not the widest pair.** Batch time is not linear in `n` at
the bottom of a ladder. `cpu_canary`'s local slope is 5.30 ns from `n = 1`
to 64, then flat at 2.36 ns. The top two rungs are in the linear regime and
still span `N` iterations of lever arm.

**Why 20 us.** A scheduler tick adds about 5 us to whatever batch it lands
in, and the chance of landing is the batch length over the tick period.

- At 20 us about 2% of batches are hit, and a trimmed mean discards them.
- At 100 us 17% are hit, and trimming leaves a bias.
- Past 1 ms every batch is hit.

Sweeping the ceiling against a known answer put the boundary at 20 us.
Nothing is lost: lever arm comes from the *ratio* of the two batch sizes,
not from their duration.

**Why 100 ns at the bottom.** The harness costs about 370 ns per
measurement outside the timer, so below that the machine time buys almost
no information.

**Why the median of three.** Calibration runs first, and the first execution
is the coldest. `copy_64mb`'s first call took 47 ms against a true 6.4 ms,
because its pages had not yet been faulted in. One probe is one probe.

**Two paths, not four.** Every function leaves calibration with either one
rung or two, and everything downstream handles both cases with the same
model. The one-rung case is only for calls over half a second, where the
fixed cost is a part in a million and needs no subtracting.

## 2. Rounds

**What.**
- Each round runs every function in the set once, in a fresh random
  permutation.
- Each function independently draws one of its two rungs with equal
  probability.
- The CPU canary is always a member.
- The set never changes during a pass. Every member runs every round until
  the pass stops.

**Why rounds.** Everything in one round runs under the same clock, the same
thermal state and the same neighbours. Interleaving is what lets a ratio
cancel the machine's movement. Comparing two functions measured one after
the other compares two different machines.

**Why a fresh permutation.** Position matters: a memory-heavy neighbour
immediately before leaves a cold cache. A random order removes position as
a bias. Alternating sweeps were tried, and they replace the bias with a
period-2 oscillation.

**Why random rungs, at equal odds.** Over a block of rounds each function
sees both rungs about equally, so every block has the same design. Drawing
rungs unevenly made a block's estimate depend on which rungs it happened
to get. Under additive noise that made the claimed bar 80-200x too large.

**Why nobody leaves early.** A member's cost depends on its neighbours,
which is problem 3. A member that stops changes the composition for
everyone still measuring, by percent-scale amounts unquiesced.

**Why the canary.** It is a dependent multiply-add chain held in registers.
On the quiet machine it reads to 0.02%. On this machine it costs 4 core
cycles a link, a multiply and an add: against the logged clock frequency
it reads 4.04-4.08 at every clock from 1.6 to 4.4 GHz, with a 10-90%
range of under 0.01 within a run. It is cheap, at one batch of at
most 20 us per round, and it earns its place twice:
- it turns any function's time into bogo-nanoseconds (section 4);
- it shows how much the clock moved, which is what a refusal message needs
  to say (section 7).

A memory canary was built and dropped. It measured something, but nothing
in the algorithm had a use for it.

## 3. The ratio between two functions

This is the measurement that matters most. "How much faster is the new
implementation" is a ratio, and so is "before and after" when both versions
of a crate are linked into one benchmark.

**What.** For functions A and B, take every round. Each round gives a cell,
the pair of rungs `(n_A, n_B)`, and a value, `ln t_A - ln t_B`.

1. Within each of the (up to) four cells, take the **25% trimmed mean**,
   discarding a quarter from each end.
2. Fit the additive model `cell(n_A, n_B) = alpha(n_A) - beta(n_B)`, by
   least squares weighted by cell counts. Backfitting converges in a few
   dozen sweeps, and nothing is extrapolated.
3. `exp(alpha)` and `exp(beta)` are then each function's batch times, up to
   one shared factor. The slopes are

       s_A = (exp(alpha(2N_A)) - exp(alpha(N_A))) / N_A
       s_B = (exp(beta(2N_B))  - exp(beta(N_B)))  / N_B

   and the ratio is `R = s_A / s_B`. The shared factor cancels.

A function with one rung contributes one level, and its "slope" is
`exp(level) / n`. A block missing a cell it needs returns no estimate.

For a set of `k` functions, every ratio of interest - usually each
candidate against one baseline - is its own pairwise estimate from the
same rounds. Pairwise ratios do not multiply exactly (`R_AC` is close to
`R_AB * R_BC`, but not equal), and nothing should assume they do.

**Why logs within a round.** The clock multiplies every time in a round by
the same factor, and a log difference removes it exactly, whatever rungs
the two were on.

**Why the model, rather than subtracting each function's rungs and then
dividing.** That obvious estimator works on a quiet machine. On a noisy one
its subtraction pairs samples from different rounds, so it subtracts two
different clocks. For a clock-bound function, about one such pair of
samples in ten came out negative, and its bar described a steady clock that
was not there. Fitting in log space
first means no subtraction ever straddles two clocks. The subtraction still
happens, as `exp` then subtract, so the fixed cost per measurement is still
removed. Only the clock is handled in logs.

Clock/clock pairs, both with the 8-block bar of section 5. "Bar/sd" is
the claimed bar over the real spread of results; 1 is honest.

| machine | estimator | cells passing | blowups | coverage | median bar/sd |
| --- | --- | --- | --- | --- | --- |
| quiet | subtract, then divide | 54 of 54 | 0.04% | 70% | 1.05 |
| quiet | log model (this) | 54 of 54 | 0.01% | 74% | 1.15 |
| noisy | subtract, then divide | 26 of 54 | 1.63% | 54% | 0.67 |
| noisy | log model (this) | 51 of 54 | 0.14% | 71% | 1.03 |

On the quiet machine the two are equally good. On the noisy one, only the
log model's bar is honest.

**Why trim.** A tick lands on about one batch in fifty and adds microseconds
to it. A symmetric 25% trim keeps the centre where it is under symmetric
noise and discards one-sided excursions. Trimming only the top was tried,
and it biases low by about the size of the tick bias it removes.

**Limits:**
- **Only functions alike in how they respond to the clock.** Two clock-bound
  functions, or two memory-bound ones, share what a round did to them. A
  clock-bound function against a memory-bound one does not, and on a noisy
  machine their true ratio moves: `btree_miss / f64_sin` has an rms spread
  of 3-16% over 13-second windows. There is no fixed answer to find, and the
  two-pass check in section 7 is what notices.
- **It assumes the fixed cost scales with the clock too.** It mostly does,
  since it is instructions and cache refills.

## 4. Absolute times: nanoseconds, or bogo-nanoseconds

Ratios are the precise measurement, but people also need the absolute
number: whether a function's speed matters at all is a question about
nanoseconds against, say, a network round trip. Most often that should be
real nanoseconds.

**Nanoseconds.** For each function, `(T(2N) - T(N)) / N`, where `T` is the
25% trimmed mean of that rung's samples. When the clock holds still this
is the better number. Dividing by the canary would only add the canary's
own noise: 2.6-3.6% against 0.03% raw for `instant_now` in an earlier
sweep.

**Bogo-nanoseconds.** When the clock moves, nanoseconds measured now do not
describe later. Bogo-nanoseconds are the function's ratio to the canary,
from section 3, times the canary's cost at the CPU's base clock:

    bogo_ns = R(f / canary) x 4 cycles / f_base

`f_base` is the base, non-turbo frequency. Linux publishes it as
`cpufreq/base_frequency`; it is 1.7 GHz here. For clock-bound code this
is the time the function would take at base clock, which is exactly what
a `quiet-bench` machine measures. Checked against that, from the battery
and mains processes (PROBLEMS.md, "On battery"):

| workload | quiet, ns | unquiesced, bogo-ns (10 processes) | unquiesced, ns |
| --- | --- | --- | --- |
| `f64_sin` | 33.9 | 33.9-35.2 | 13.1-32.9 |
| `str_find` | 2.18 ms | 2.17-2.23 ms | 0.87-1.97 ms |
| `urandom_read` | 19.1 us | 19.0-19.2 us | 7.4-17.5 us |
| `btree_miss` | 760 | 796-1256 | 518-747 |

For clock-bound code, bogo-nanoseconds land within 0-4% of the quiet
machine's real nanoseconds, from a laptop whose clock was anywhere between
1.6 and 4.4 GHz. Across processes they are 10-60x more reproducible than
its nanoseconds. For memory-bound code neither number holds still on an
unquiesced machine. The two passes then disagree and say so (section 7).

**Which one is reported.** For each function, its time per iteration is
reported in nanoseconds if the clock is not changing, and in
bogo-nanoseconds if it is. The clock is changing if the canary's own
block-to-block spread - the same blocks as the bar, the standard deviation
of the log of the canary's time in each - exceeds a quarter of the goal.

Over a 1,000-round measurement that spread was:
- **quiet:** 0.03-0.08%, on mains or battery;
- **unquiesced:** 0.5-46%, with one exception at 0.007%, a stretch where
  the clock sat steady at turbo.

In that exception the nanoseconds are real, just at a fast clock. The
canary can only see the clock change *during* a measurement: a clock held
steady at turbo looks still, and its nanoseconds describe that clock.

**Stopping, for a single function.** The goal applies to whichever number
will be reported: its time per iteration as best it can be measured. That
is the nanosecond bar while the clock holds still, and the bogo-nanosecond
bar - the bar on the ratio to the canary - once it moves. The decision is
re-made at every check, so a clock that starts moving mid-measurement
switches the measurement over.

## 5. The error bar

**What.** Cut the rounds so far into `b` contiguous blocks, in order, with
`b = clamp(rounds / 15, 8, 20)`. Compute the estimate in each block and take
its log. The bar is the standard error of those logs,

    sigma = sd(ln R_1 .. ln R_b) / sqrt(b)

and it reads as a factor: the result is `R` times or divided by `e^sigma`.
For small `sigma` that is the familiar `±sigma` as a fraction. The same
recipe works for an absolute time, whose block estimate is the two-rung
subtraction within the block.

**Why batch means, rather than a formula.** Consecutive rounds are not
independent: they share a clock state, a cache state and a scheduler phase.
A textbook standard error assumes they are, and comes out confidently too
small. The spread of contiguous block estimates absorbs whatever is
correlated within a block. It also needs no formula for the error of a
ratio of two noisy slopes, which would assume away exactly that
correlation.

**Why logs.** On a log scale A/B and B/A are one measurement: their logs are
exact negatives, with the same spread. On a linear scale the two
orientations of one pair stopped at different points and scored
differently, and a unit test now checks that they do not. Being 2% high and
2% low also count alike, as factors.

**Why at least 8 blocks.** The bar is itself an estimate, and from 4 blocks
it has 3 degrees of freedom. A bar that uncertain comes out under half its
true size about one time in seven. The stopping rule looks at it over and
over, so it stops on exactly those lucky-small bars. That was most of the
blowups between clock-bound pairs:
- more than half stopped at the 60-round floor, against a quarter of all
  trials;
- every block was shifted the same way;
- the next 60 rounds were usually fine.

Simulated iid noise, with no machine at all, reproduces the rate:
0.3-1.5%. Eight blocks:

| clock/clock pairs | blowups, 4 blocks | blowups, 8 blocks |
| --- | --- | --- |
| quiet | 33 of 10,731 | 1 of 10,742 |
| noisy | 46 of 10,348 | 14 of 10,328 |

It also raises coverage from 65-68% to 68-79%.

**Why 15 rounds a block.** Each block's estimate needs both rungs of both
functions, so blocks have to be big enough to almost always have them.
Smaller blocks with the same minimum count were tried, to lower the floor.
They cost *more* rounds, not fewer: a block of a few rounds leaves one or
two samples per cell to trim, its estimate is noisier than the full one,
and the bar overstates the error. A cutoff that tightens as blocks get
fewer, from a chi-square bound on the bar, also works, but costs at least
as much for the same safety (PROBLEMS.md, "Why a ratio blows up").

**Why at most 20.** As data accumulates, blocks should grow longer rather
than only more numerous, so that each outlasts more of the correlation.

## 6. Stopping

**What.** Check at 120 rounds, the floor of 8 blocks of 15, and then each
time the round count has grown by 1.3x. Stop when every number the user
asked about has a bar `sigma <= ln(1 + goal)`, or when the time budget runs
out.

What the user asked about:
- for a comparison set, each candidate's ratio to the baseline;
- for a single function, its primary absolute time (section 4).

**Why geometric checks.** Recomputing every bar after every round is
quadratic, and looking more often only gives a noisy bar more chances to
dip. 1.3x costs at most 30% overshoot.

**Why the goal as a factor.** It is the same test for A/B and B/A. At goals
of a few percent, `ln(1 + goal)` and `goal` differ by under 1%, so a user's
"1%" means what they think it means.

**What a stop costs.** On the quiet machine's clock-bound pairs, the mean
rounds per trial were 156 at a 2% goal, 342 at 1% and 905 at 0.5%. A round
of five functions took about 13 ms, and a round of just two functions and
the canary is far cheaper. The 8-block floor accounts for most of the
2%-goal cost and almost none of the 0.5%.

## 7. Two passes, and refusing

Everything above sees only the noise inside one measurement. Some of the
machine's variation is slower than that:

- `btree_miss` has whole minutes in which it runs 1.5-4% slow, even on the
  quiet machine, while `cpu_canary` is flat to 0.0%;
- clock-matched ratios on the noisy machine wander 0.3-1.8% over 13
  seconds.

Every block of a measurement sees the same episode, so no bar computed from
inside it can include that variation. With 8 blocks in place, this is
what is left: of the noisy machine's clock/clock blowups, 43% ran 2,438
rounds or more, against 5% of all trials.

**What:**
1. Run the whole suite once at a per-pass goal of
   `sigma <= sqrt(2) * ln(1 + goal)`.
2. Run the whole suite again, the same way.
3. **Check clock sensitivity.** For each function, take its *clock
   sensitivity*: the slope of its log time against the canary's, over
   windows of rounds. Clock-bound code sits at 0.86-0.99, `btree_miss` at
   -0.17 to 0.31, and `copy_64mb` at 0.23-0.52. If the two functions'
   sensitivities differ by more than **twice the goal divided by the
   clock's spread**, refuse the comparison: its ratio has no fixed value on
   this machine. This happens before any combining, and the message says
   why (see "Knowing a ratio cannot be fixed" below).
4. If the passes **agree** - `|x_1 - x_2| <= 2 * sqrt(sigma_1^2 + sigma_2^2)`
   - report their inverse-variance weighted mean of `ln R`, with bar
   `sqrt(1 / (w_1 + w_2))`.
5. If they **disagree**, run that comparison a third time and combine all
   three by random effects (DerSimonian-Laird). That estimates how much
   the passes really differ beyond their own bars, and adds it to each
   pass's variance. If the combined bar misses the goal, **refuse**:
   report that the machine could not reproduce this number to the goal,
   and how far the passes were apart.

**Why not count the spread between two passes into the bar.** This was
the first version of the rule. It is wrong: with two passes, the spread
between them is a one-degree-of-freedom estimate. Replayed, it refused
2-26% of clock-pair results that were fine, the more the tighter the
goal. The rule above refuses 0.1-7% of them. The results it reports had
no blowups on the quiet recordings, and at most 3 in about 3,100 trials
on the noisy ones, against 10 for a single pass. That is over all six
long recordings, with passes 1 or 10 minutes apart.

**Why passes, and why whole-suite.** Replayed with a second trial started
`g` rounds after the first ended, at about 79 rounds a second:

| | gap | first-run blowups the second run disagrees with | blowups, both averaged | blowups, budget split in two |
| --- | --- | --- | --- | --- |
| quiet, memory pairs | none | 60% | 55 of 159 | 124 |
| | ~1 min | 72% | 33 | 64 |
| | ~10 min | 85% | 14 | 41 |
| noisy, clock/clock | none | 71% | 2 of 14 | 13 |
| | ~1 min | 100% | 2 | 2 |

The gap is what gives a second pass its power. An immediate rerun shares
the episode that fooled the first. Running the whole suite, and then
running it again, provides the gap at no cost, because every other
benchmark runs in between.

**Why every comparison, not just the close calls.** A blowup does not look
borderline. It has a tight bar and sits several bars from the truth, so it
looks like a confident result. A rule that reruns only comparisons that are
borderline significant would skip exactly the wrong answers.

**Why a √2 looser goal.** Two passes at `sqrt(2) * ln(1 + goal)` average to
about `ln(1 + goal)`. They cost 1.2-1.3x the rounds of one pass, not 2x,
because of the floor and the 1.3x stepping. With a 10-minute gap, that cut
the quiet machine's memory-pair blowups from 159 to 41.

**Why refuse.** A number whose two measurements disagree beyond their error
bars is not known to the precision asked for, whatever either bar says.
Printing it with a caveat invites someone to act on it. Refusing means
the result's status is "not reproducible", with the reason, instead of a
number. What happens next - failing the run, retrying, or carrying on - is
up to whatever consumed the result. The progression is
natural:
- a refusal is a failed measurement;
- a retry is a refusal with a loop around it;
- more passes are retries whose results are kept.

The user can ask for a looser goal, or for "run until the time limit" with
no goal at all. A result that merely ran out of budget before meeting its
goal is a different case: it is printed, and marked `(limit)`.

**Passes in separate processes.** The harshest test is each pass in its
own process. That is what rerunning a benchmark binary does, and the gap
in time comes with it. Here 4 unpinned processes ran on battery and 6 on
mains, the latter while the machine was in use. Clock-bound pairs:

| goal | one pass blows up | refused | still blown when reported |
| --- | --- | --- | --- |
| 2% | 3.3-3.6% | 10% | 0% |
| 1% | 6.7-7.1% | 37-61% | 0-1.2% |
| 0.5% | 11-21% | 79-87% | 1.1-1.2% |

All mixed pairs are refused, by the clock-sensitivity check, and none are
reported. On an unquiesced laptop, then, a ratio between clock-bound
functions is reproducible to 1-2%. Tighter than that needs a quiesced
machine, and the algorithm says so rather than printing a number.

Two things make separate processes harsher than one long recording:

- **Offsets fixed for the life of a process.** On battery, one process
  measured `str_find` 0.6-2.3% high in all eight of its windows, while the
  other three agreed with each other. The likely cause is layout: which
  addresses a process's inputs and code land at.
- **Episodes from other work on the machine.** On mains, `f64_sin`'s ratio
  to the canary jumped 3-6% in bursts within three of the six processes,
  matching when other cores were busy compiling and replaying. The other
  three processes were flat.

A second pass in the same process catches the episodes, but not the fixed
offsets.

**Not yet validated:**
- A lone `bench()` call outside a suite has no gap to borrow. It can only
  do its two passes back to back, which catches fewer drift blowups (60-71%
  rather than 72-100%) but all of the statistical ones.
- The random-effects rule was replayed with all three passes at a √2
  looser goal. A third pass at the full goal might do better.

### Knowing a ratio cannot be fixed

The canary is in every round, so each function's response to the clock
can be measured from the run itself. Regress its log time on the canary's,
over windows of 1,000 rounds. A ratio's wander is then predicted by

    |sensitivity_A - sensitivity_B| x sd(clock over windows)

Across 195 pairs in 13 unquiesced recordings, the log of this prediction
correlates with the log of the observed wander at 0.92: btree vs clock-bound
pairs predicted 17% and saw 16%; cpu_canary vs str_find predicted 0.2%
and saw 0.4%.

This is the canary doing what it was first imagined for: not correcting a
number, but saying why the machine cannot produce it. A refusal can say

    btree_lookup vs hash_lookup: not reproducible to 1%
        hash_lookup follows the CPU clock (sensitivity 0.97); btree_lookup
        barely does (0.12), and the clock moved by 18% during the run

## 8. Reporting

Logs stay internal. People think in percentages for small changes and in
factors for large ones: "twice as fast", not "50% less time".

- **A ratio within a factor of 1.25** prints as a percentage: `3.2% slower
  (±0.4%)`.
- **A larger ratio** prints as a factor: `2.31x faster (±0.6%)`. The `±`
  is how well the factor is known, so ±0.6% on 2.31x means 2.30x to 2.32x.
- **A bar wider than about 20%** prints as a factor too: `×/÷ 1.3`.
  Percentages stop being symmetric at that size.
- **Absolute times** always print alongside: in nanoseconds, or in
  bogo-nanoseconds when the clock moved during the measurement.
- **Flags.** `(limit)` means the budget ran out before the goal. A refused
  result prints the refusal, never the number.

## Constants, and where each came from

| constant | value | from |
| --- | --- | --- |
| batch ceiling | 20 us | tick contamination swept against a known answer |
| batch floor | 100 ns | ~370 ns harness cost per measurement |
| second rung for slow calls | if `2 t(1) <= 1 s` | a cost decision, not measured |
| trim | 25% each end | swept 0/10/25/40%; any trim removes tick bias, 25% matches the reference |
| rounds per block | >= 15 | each block needs both rungs of both functions |
| blocks | 8 to 20 | 8 removes lucky-small stops; smaller blocks cost more |
| first check | 120 rounds | 8 blocks x 15 |
| check growth | 1.3x | at most 30% overshoot |
| canary | 4 cycles a link | 4.04-4.08 against the logged clock, 1.6-4.4 GHz; sets the bogo-ns scale; verify per architecture |
| clock is changing | canary block spread > goal / 4 | quiet 0.03-0.08%, unquiesced 0.5-46% |
| passes | 2, at a √2 looser goal | replayed, 1.2-1.3x the cost |
| disagreement | > 2 combined bars | replayed: 3-7% of good clock-pair results trigger it |
| third pass | random effects, refuse if bar > goal | replayed: refuses 0.1-7% of good clock-pair results |
| clock-sensitivity refusal | predicted wander > 2x goal | separate processes: refuses every mixed pair; 0% blown at 2% goal |
| default goal | 1% | the crate's existing default |

## Limits it does not overcome

- **Memory-bound functions reproduce only to about 1% over minutes, even on
  a quiet machine.** A 0.5% goal for `btree_miss` is below what the machine
  reproduces at that scale. Two passes catch most of these, but catching is
  all they can do.
- **Mixed ratios on an unquiet machine have no fixed value.** The two-pass
  check will refuse them, which is correct, and the refusal should say why.
- **Functions slower than about 1 ms per call** are barely represented in
  the lab's data. Above that every batch is hit by ticks, the trimmed mean
  has no clean population to find, and the result includes a roughly
  constant interrupt tax of about 0.5%.
- **Offsets fixed for the life of a process are not caught.** Both passes
  run in one process, by decision. On battery, one of four processes
  measured `str_find` 0.6-2.3% high throughout, against its own bar of
  about 0.2%. Running the benchmark again, as a new process, is the only
  way to see one.
- **Quieting moves the operating point.** A pinned core at base clock drives
  memory more slowly, so `copy_64mb` costs 6.4 ms quiet and 4.5 ms not. A
  number measured quiet does not describe the machine people run on.

## Tests a clean implementation should carry

The lab's `replay.rs` has the first two as unit tests.

1. **Symmetry.** A/B and B/A give reciprocal estimates, identical bars and
   identical stopping points.
2. **Recovery.** A synthetic pair with a wandering clock (±1% a round,
   clamped to 0.6-1.6x), a fixed cost per batch, and ±0.5% noise recovers a
   known ratio of slopes to within 0.2%.
3. **The stopping rule on iid noise.** Gaussian rounds with no machine at
   all, sized so an honest stop needs about 240 rounds, should give at most
   about 0.1% blowups and 68-75% coverage. With 4 blocks they give about
   1%, so this test fails if the floor regresses.
4. **Rung balance.** Over any block, both rungs of every function appear.
   A block missing one returns no estimate rather than a smaller bar.
5. **Replay.** The lab's `lab pairs` scoring, pointed at the
   implementation's estimator, on the six pair recordings. Today's figures,
   for the paired estimator: every quiet clock/clock cell passes (54 of
   54), the noisy ones 51 of 54, with 1 and 14 blowups respectively.

## Decided

- **The default goal stays 1%, quiesced or not.** On an unquiesced laptop
  that means most comparisons tighter than 1-2% are refused, and the
  refusal says why. A user who wants a number anyway asks for a looser
  goal.
- **The crate does not restart itself.** Both passes run in one process,
  so offsets fixed for the life of a process are not caught. That is a
  stated limit (see "Limits it does not overcome"), not a hidden one.
- **Nanoseconds, or bogo-nanoseconds.** Absolute times are in real
  nanoseconds when the clock holds still, and in bogo-nanoseconds,
  relative to the canary, when it moves (section 4).
- **A single function** stops on the uncertainty of its time per
  iteration, in whichever of those two it will be reported.
- **The algorithm reports; the consumer decides.** Every result carries
  its status: measured to the goal, out of time (`(limit)`), or not
  reproducible, with the reason. The algorithm never exits, panics or
  fails a run on its own account. Whether an unreproducible result should
  fail `cargo test`, or be printed and ignored, is the caller's policy.

## Decisions still open

1. **Lone `bench()` calls.** Whether a single call outside a suite does two
   back-to-back passes, or one, by default.

---

# Part 2: draft user documentation

*Everything below is written as it would appear in the crate's docs. Names
such as `ComparisonSet` follow the `comparison` branch, and the numbers in
the examples are illustrative.*

---

## How `scaling` measures

`scaling` keeps measuring until it knows each answer to the accuracy you
asked for, 1% by default. It tells you how accurate the answer is, and
refuses to give an answer it cannot stand behind.

```none
parse_v1:        412.3ns ± 0.9ns
parse_v2:        178.4ns ± 0.4ns
parse_v2 vs v1:  2.31x faster (±0.4%)
```

### Comparisons are the precise measurement

To know which of two implementations is faster, and by how much, measure
them together:

```rust
let mut set = ComparisonSet::new("parse");
set.baseline("v1", || parse_v1(INPUT));
set.candidate("v2", || parse_v2(INPUT));
println!("{}", set.run());
```

They are timed in alternation, in the same rounds, so whatever the machine
does while they run - the clock changing speed, the package warming up -
happens to both. It cancels out of the comparison. Comparing two separate
`bench` results cannot do that, because each one measured a different
moment.

A small change is shown as a percentage, `3.2% slower (±0.4%)`. A large
one is shown as a factor, `2.31x faster (±0.4%)`. The `±` is how well that
number is known: ±0.4% on 2.31x means somewhere between 2.30x and 2.32x.

To compare a function against an earlier version of itself, link both
versions of the crate into one benchmark and put them in the same set.
Before-and-after then becomes a comparison within one run, and no stored
number from another day is needed.

### Absolute times

Every result also comes with its absolute time, because "does this even
matter?" is a question about nanoseconds. A function that takes 40 ns
inside a request that waits 2 ms on the network is fast enough, whatever
the comparison says.

On a machine that has not been quiesced (see `quiet-bench`), the clock
changes speed under load, and nanoseconds measured at one moment do not
describe another. `scaling` notices, because a small reference loop runs
alongside your code and tracks the clock. When the clock moves, it reports
**bogo-nanoseconds** instead:

```none
parse_v1:        731.0 bogo-ns ± 1.6
```

That is how long the function would take at the CPU's base clock,
measured against the reference loop, so it holds still while the real
clock wanders. For code limited by the CPU, it comes out close to the
nanoseconds a quiesced machine would measure. For code limited by memory,
neither number holds still, and the honest answer is to quiesce the
machine.

### What happens while it measures

1. **Batch sizes.** For each function it finds two batch sizes, `N` and
   `2N` calls, both short enough (at most 20 us) to dodge the operating
   system's timer interrupts. Subtracting the two cancels the fixed cost of
   timing anything at all, so a 2 ns function reads 2 ns, not 2 ns plus the
   cost of reading the clock.
2. **Rounds.** It runs everything in rounds: each function once, in a fresh
   random order, together with a small reference loop that tracks the clock.
3. **Error bars.** It estimates error bars from how much the answer varies
   between stretches of the run, not from a formula that assumes every
   measurement is independent. They are not, and a formula would claim
   more precision than the data has.
4. **Stopping.** It stops when every answer you asked about has reached its
   goal.
5. **A second pass.** It does all of that twice, as two passes over
   everything, and compares the passes. Some of a machine's variation is
   slower than one measurement: a minute in which memory is slower, a
   background job that comes and goes. Only a second look at a different
   moment can see it. The two passes together cost little more than one,
   because each is run to a looser goal.

### When it cannot reach the goal

There are two different failures, and they are reported differently:

- **Out of time.** The time limit ran out before the goal was met. The
  answer is printed with the accuracy it did reach, and marked `(limit)`.
- **Not reproducible.** The passes disagreed by more than their error bars
  allow, even after a third look. The answer is not printed, because it is
  not known to the accuracy you asked for:

  ```none
  btree_lookup vs hash_lookup: not reproducible to 0.5%
      passes measured 1.83x, 1.79x and 1.86x faster
      the CPU clock moved by up to 12% during the run
      ask for a looser goal, or quiesce the machine
  ```

Either way the result says which happened, so code consuming it can
decide what to do: fail a test, retry later, or carry on. `scaling` itself
only reports.

You can ask for a looser goal (`Config::relative(0.05)`), or for no goal at
all - measure until the time limit and report whatever accuracy that buys.

### Caveats

- **Read the `±` as a typical error, not a bound.** About a third of
  results land more than one `±` from the truth, and about 1 in 20 more
  than two.
- **Compare like with like.** A comparison between a function limited by
  the CPU and one limited by memory has no fixed answer on a machine whose
  clock moves, because the clock changes one and not the other. Expect
  those to be refused unless the machine is quiesced.
- **Memory-bound code reproduces to about 1%.** Even on a quiet machine, the
  memory system has slow moods lasting minutes. Goals much tighter than 1%
  for such code will often be refused, correctly.
- **Slow functions are measured one call at a time.** Above about a
  millisecond per call every measurement absorbs some timer interrupts, so
  the result includes a small, roughly constant overhead (about 0.5%) that
  no amount of measuring removes.
- **A quiesced machine is a different machine.** Pinning the clock makes
  results reproducible, but at base clock the memory system runs slower
  too, so the absolute numbers describe the quiet machine, not the one you
  deploy on. Comparisons travel better than absolute times.
