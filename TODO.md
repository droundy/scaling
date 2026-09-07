# TODO

Tick an item when it lands, and say underneath what was actually done.
`[-]` means deliberately skipped, with the reason.

Working notes on measurement quality. Numbers below were measured on one
laptop (i5-1240P, `CONFIG_HZ=1000`, CPU 2 reserved with its SMT sibling
offline and P-cores capped at 1.7GHz), so treat them as indicative of shape
rather than as constants. Where a result did not survive replication that is
said outright, because several did not.

## Planned

### [x] 1. Spend more time benchmarking by default

*Done, but not as predicted.* A `MIN_SAMPLE_TIME` of 3ms now floors the
sampling duration; `MIN_SAMPLES` stays at 6 and `max_time` stays at 10s.

The premise was that reproducibility keeps improving with time, which is
true of *pure averaging* (1ms 1.14% -> 10s 0.044%, slope reaching -0.51)
but not of the benchmark as a whole. Sweeping both floors together over
seven workloads - integer, transcendental, division and branchy, 20ns to
2.8us, round-robin so every cell met the same drift:

| time floor | spread | worst error bar | cost |
| --- | --- | --- | --- |
| none | 0.316% | 1.01x | 1.3ms |
| 1ms | 0.244% | 0.93x | 1.4ms |
| **3ms** | **0.143%** | 1.45x | 3.4ms |
| 10ms | 0.144% | 3.10x | 10.4ms |

Ten milliseconds buys no further reproducibility and costs a great deal of
honesty. Past a few milliseconds the reported `±` shrinks faster than the
answer settles, so sampling harder yields a tighter number that is *less*
true - which is the opposite of the point. The `branchy` workload drives
that alone: its spread will not come down (0.62-0.79% at every floor) while
its `±` keeps falling, so its error bar reaches 3.1x too small at 10ms.

Raising `MIN_SAMPLES` instead was measured and rejected: it buys no more
than the time floor does at COUNT=24 (0.157% against 0.143%), and it is not
scale-free - 24 samples of the crate's own 400ms-sleep test would take ten
seconds. A floor in time costs a slow benchmark nothing.

Also updated `an_absolute_accuracy_target_is_honoured`, which compared a
25ns and a 500ns target. The floor satisfies both, so it was comparing two
numbers the floor had made equal. It now compares 25ns against 5ns, where
the expensive side genuinely wants more than the floor supplies.

### [x] 2. Randomise comparison order

*Done.* Which function is timed first is chosen per round, so neither
occupies a fixed position and neither samples a fixed phase of anything
periodic in the machine.

Getting there took two wrong turns, both measured on a function compared
against itself, where every difference reported is false by construction:

| approach | bias | absolute-time cost |
| --- | --- | --- |
| fixed order, baseline first | +0.10% | none |
| fixed order, candidate first | -0.12% | none |
| swap via `if`/`else` | **-3% to -5%** | none |
| `&mut dyn` over the *function* | +/-0.03% | +2% at 28ns, **+14% at 9ns** |
| `&mut dyn` over the *batch* | **+/-0.02%** | **none** |

An `if`/`else` around two `time_batch` calls is far worse than doing
nothing: it duplicates the timing loop, and the copies are not equally
fast, so each function is measured by a *mixture* of two of them. That is
the layout lottery, reaching 5% here - twenty times the 0.22% positional
bias the change set out to remove.

Erasing the innermost function fixes the mixing but puts an indirect call
on every iteration. Erasing the *whole batch* fixes it and costs one
indirect call per batch, amortised over `unit` iterations: each function
keeps one consistently-compiled loop, and the order becomes a choice of
data. Bias and overhead both fall to noise.

### [x] 3. K-way compare

Generalise `compare` to k alternatives, round-robin by batch with the
starting position rotated each round so every alternative spends equal time
in every slot. The multiple-comparison machinery
(`num_comparisons_planned`) already exists.

Holding k alternatives needs `Box<dyn FnMut()>`, and that erasure is a
*benefit* here: all alternatives go through one shared call path instead of
k separate monomorphisations at k different addresses. **Measured while doing (2): it
works** - one `&mut dyn` call site took a function compared against itself
from -5% to +/-0.02%. Erase at the batch level rather than the function
level, as (2) does, and the indirect call is amortised over the batch and
costs nothing measurable.

*Done*, in `src/kway.rs`, as a builder: `cfg.comparison().add("old",
old).add("new", new).run()`. The erasure is at the batch level, so each
alternative keeps its own monomorphised timing loop and pays no
per-iteration indirect call - which does leave the layout lottery between
alternatives, but that is 0.1-1% and an order of magnitude below any
accuracy goal worth asking for. Each alternative beyond the baseline counts
as one comparison against the plan, and the budget is `k * max_time`.
Measured on the reserved CPU: three identical alternatives at a 5% goal came
back changed 0/20 times over six runs, and +/-10% differences were caught
20/20.

**One batch of inputs per round, cloned for each alternative.** Not one draw
per alternative: if the cost depends on the input, then each alternative
drawing its own means every difference carries the difference between two
draws, which is not a difference between the alternatives at all. This is
why `comparison_gen_input` needs `I: Clone` where `compare_gen_input` does
not. Measured: on inputs whose length spans 4000x, the paired standard error
comes to 0.48 of the combined one, averaged over 16 comparisons and steady
to +/-0.02 across runs. It only shows up when the batch is small enough that
which inputs it drew still matters - at a 200x span the batch ran to ~1600
draws, the input had already averaged itself away, and the ratio sat at 1.07.

This does not yet replace `compare`, which stays as the two-alternative
form that needs no names and no plan arithmetic.

### [x] 4. Interleave dissimilar benchmarks across a suite

Today benchmark #1 runs at t=0 and #50 at t=500s, sampling different thermal
states. Interleaved, every benchmark's samples spread over the whole
session, so all of them average the same drift. Needs time-sliced
scheduling rather than fixed batch counts, since the benchmarks differ in
input type and duration - more machinery than (3).

*Done*, in `src/suite.rs`: `cfg.suite()`, `add`/`add_input`/`add_gen_input`/
`add_scaling`/`add_comparison`, then `run()`. Each `add` hands back a token
to read that benchmark's answer from afterwards, so one suite holds flat
benchmarks, scaling benchmarks and whole `ComparisonSet`s with their result
types intact - no enum of kinds, no downcasting.

**Each benchmark is an `async fn` and the scheduler is forty lines.** The
hard part was never the round robin, it was that "stop in the middle and be
picked up later" means lifting `unit`, `probed`, the `Running` accumulator,
the measured-time total and the scratch `Vec<I>` out of a stack frame into a
struct with a `step` method - four times, once per kind. An `async fn`
leaves each loop looking like the loop it replaced and has the compiler
generate that struct, and boxing the future erases `I` and `O` in the same
move, which is the other thing a heterogeneous suite needed.

No runtime was added, and that is not dependency-aversion:

* The scheduling policy is the feature. `FuturesUnordered` polls in wake
  order and `async-executor` has its own ready queue; both would be fought.
* Runtimes park. A general `block_on` sleeps when everything returns
  `Pending`, waiting for an outside wake that here never comes. `Pending`
  always means "I have had my turn", so parking would be a deadlock - which
  is also why a no-op waker is sound and the waker protocol disappears.
* `async-executor` 1.14 pulls six direct dependencies into a crate that has
  none off Linux, and a benchmarking crate is a dev-dependency of
  everything downstream.

**The overhead is nothing measurable**, which was the bet. One poll per
sample amortises over `unit` iterations exactly as (2)'s batch-level erasure
does, against `harness-cost`:

| | before | after |
| --- | --- | --- |
| `bench` overhead/iter | 0.5927ns | 0.5919ns |
| `bench_input` | 1.489ns | 1.490ns |
| `bench_gen_input` | 1.521ns | 1.536ns |
| 1 round of spin | 3.260ns | 3.261ns |
| 10k rounds | 2.886us | 2.883us |

Every delta is inside its own spread.

**Position bias, which is what the item is for. It bounds it rather than
lowering it, and the first answer here was wrong.** Eight identical
workloads (one closure type, so one compiled loop - no layout lottery to
mistake for a positional effect), measured forward and then in reverse
declaration order, alternating which method goes first. How far each moved
between the two orders, over 21 passes across several sessions:

| | median | range | spread |
| --- | --- | --- | --- |
| sequential | 0.256% | 0.097 - 1.194% | 12x |
| interleaved | 0.280% | 0.146 - 0.453% | **3.1x** |

The medians are the same to within nothing. What interleaving changes is the
*range*: it never did better than 0.15% and never worse than 0.45%, while
the sequential arm ranged over a factor of twelve depending on nothing but
which session it ran in. On a session where the machine is drifting,
sequential reads 1.19% and interleaved reads 0.23%; on a quiet one,
sequential reads 0.10% and interleaved reads 0.30%.

That is exactly what the mechanism says it should be. Interleaving pays a
floor it can never get back - every sample starts on a cache the rest of the
suite has been using - in exchange for a ceiling on drift. Where there is no
drift there is nothing to buy, and the floor is all that shows.

**This entry first claimed "about three times less movement, consistently",
from three passes.** All three came from one session, and that session's
*sequential* arm read 0.77-1.19% where every later session read 0.10-0.32%
from byte-identical code. What was being measured was that session's drift,
not the technique. It is the same mistake as the -0.13 slope recorded at the
top of "Tried without success", made again, in a file that opens by warning
about it: three passes inside one session are one draw, not three.

Two things this does *not* say. It is not evidence that interleaving fails -
bounding the worst case is most of what was wanted, and the suite's other
purpose, making numbers comparable *within* one run, is not what this
experiment measures at all. And it is not evidence that the ordering within
a round does not matter: the round order was changed from a rotation to a
full shuffle while this was being measured, and the interleaved arm did not
move (0.309% -> 0.305%), so that change stands on its mechanism and not on a
measurement.

That is `position_bias_interleaved_versus_sequential`, `#[ignore]`d, and it
prints rather than asserts. It is one draw from a stochastic process, and
(9) deleted four tests of exactly this shape for being asserted as though
they were not. Run it several times, in different sessions, before believing
any of it - which is advice this entry had to learn twice.

Two things fell out along the way:

* **`Config`'s plan is now shared between clones.** `num_comparisons_planned`
  was a per-clone field while `num_comparisons_made` was behind the `Arc`, so
  two clones could disagree and whichever dropped last decided whether `Drop`
  complained. Fixing that was a prerequisite - and it lets `Suite::run` set
  the plan itself, from the comparisons it is about to make. A caller with
  five comparison sets no longer counts candidates by hand.
* **A scaling round is atomic, and for a reason worth writing down.** The
  plan had scaling yielding part-way through a round on a time slice. It
  must not: a round contributes one sample at *every* size and the fit
  compares the sizes against each other, so splitting one across the suite
  would let the machine drift between the small sizes and the large ones and
  land that drift in the fitted power. The same argument that makes a
  comparison's round atomic, one level up.

Still open, and deliberately not attempted here:

* **Cold starts.** Interleaved, every sample begins with the working set
  evicted by the rest of the suite, where before samples 2..k started warm.
  That argues for a larger `unit` to dilute it. The null recorded below
  under "Larger batches" does *not* cover this regime - it interleaved batch
  *sizes* within one warm benchmark - so the question is open rather than
  answered.
* **A round is a new period.** Fifty benchmarks at 100us is a 5ms round, and
  5ms is five scheduler ticks. The moire at `CONFIG_HZ` is the sharpest
  finding in this file, and a fixed round period could put every benchmark
  on the same tick phase every round, in lockstep - worse than what it
  replaced. The random starting offset per round breaks the lock partially;
  (6) attacks the period itself, and this may be the regime that makes it
  pay.
* **Retirement.** A benchmark that meets its target returns `Ready` and
  leaves, so the stragglers finish sequentially at the tail - and the
  stragglers are, by construction, the ones that never converge. `branchy`
  from (1) would be last man standing every time.
* **Suite numbers are not standalone numbers.** Everything is slower and
  noisier under interleaving, uniformly. Comparable within a suite and
  across runs of it; no longer comparable against a lone `bench()`.

## Also open

### [x] 5. Paired estimator in `Comparison::std_error`

*Done.* The standard error of the difference now comes from the per-round
differences rather than from combining the two halves as though they were
independent. They are timed back to back under nearly identical conditions,
so slow movement of the machine lands on both and cancels out of each
round's difference; adding their variances counted that movement twice.

Measured by comparing a function against itself, where the true difference
is zero and so the spread of the reported difference is exactly what the
`±` should describe:

| workload | combined | paired |
| --- | --- | --- |
| deterministic, 28ns | 0.96, 1.10, 1.02 | 1.03, 1.00, 1.01 |
| deterministic, 512ns | 1.08, 1.07, 1.10 | 1.06, 1.01, 0.95 |
| **randomised cost** | **0.75, 0.79, 0.74** | **1.01, 0.94, 1.01** |

(ratio of observed spread to claimed `±`; 1.0 is honest). On the workload
with real spread the combined form claimed 0.165ns where the answer moved
by 0.123ns - cautious rather than wrong, but a third wider than it needed
to be, which is a third of a regression it could not see.

The stopping rule drives down the paired estimate too, so it and the
verdict stay the same question. Stopping on the combined form was measured
as well and came out indistinguishable, so the tie went to coherence.

### [ ] 6. Batch-size jitter, revisited

Compensated jitter cuts the 1000Hz peak from 4.18% of spectral power to
0.38% and lag-10 autocorrelation from +0.88 to +0.49. No end-to-end gain
was measured - but that was measured entirely in the sub-100ms regime,
which is exactly where correlated noise fails to average. Worth retrying
once (1) lands, because the tick *is* the correlated noise that dominates
there.

### [ ] 7. Machine-fitness check instead of `quiesced()`

`quiesced()` gates on configuration (are we pinned?) rather than on
evidence (is this machine holding still?). A cheap probe - measure a fixed
workload N times, report the between-run spread - would gate the honesty
tests on a measured number, and would give `quiet-bench status` something
better to say than "CPU 2 is reserved".

### [x] 8. Pin automatically when a reservation exists

*Done.* `pin_if_requested` became `pin_if_reserved` and now pins whenever
`reserved_cpus()` finds a reservation, rather than requiring
`SCALING_BENCH_CPUS`. The original objection - that the CPUs might have
been set aside for something else - is answered by the `flock` in
`quiet::exclusive()`. `SCALING_NO_PIN=1` remains the escape hatch.

Consequence to watch: on a machine with a reservation, a plain `cargo test`
now pins and serialises, so the suite went from 12s to ~50s. That is the
cost of actually measuring rather than pretending to. The flakiness this
was deferred behind did not materialise - 9 consecutive green runs across
debug and release - but see (9), which is still open.

### [x] 9. Fix the flaky scaling tests

*Done by deletion.* Four tests went: `scaling_error_bar_is_honest`,
`scales_o_one`, `scales_o_n_log_n_looks_like_n` and `scales_o_n`. Before,
5 runs in 30 carried a failure; after, 0 in 15.

All four were single draws of a stochastic process asserted as though
deterministic, and every one of them passed in isolation and failed only
inside the full suite - which is to say they were measuring their
neighbours. `scales_o_one` is the sharpest example: its strict assertions
sat behind `if quiesced()` and so had *never executed* until (8) made
pinning automatic.

What is lost is end-to-end coverage of the scaling pipeline on real
timings; `mod fitting` still covers power identification deterministically
with synthetic clocks, but bypasses size selection and real noise.
`scales_o_n_square` remains and has the same shape, so it may follow.

`scaling_error_bar_is_honest` deserves a note of its own: it compared a
*between-run* spread against a *within-run* claimed error, which
`Stats::std_error` documents that it does not bound. It was a machine-quality
measurement wearing a library test's clothes, and the quantity it computed
is the one (7) wants for a fitness gate. Worth rebuilding there rather than
mourning here.

### [-] 10. Document the layout floor

*Skipped.* Documenting the layout floor: judged not worth the words.

### [-] 11. Student-t rather than z in `is_significant`

*Skipped.* Student-t rather than z: mooted by (1), which makes the sample counts large enough that t and z agree.

### [x] 12. `flock` gap when the reservation comes from the environment

*Done.* `reserved_cpus()` prefers `SCALING_BENCH_CPUS` and falls back to the
reservation file, but `lock_reservation()` only ever opened the file - so
setting the variable by hand pinned to the reserved CPUs with no
cross-process lock at all, silently.

Rather than patch the lock path, the invariant is now enforced from both
ends:

* `quiet-bench run` takes the machine lock itself before it pins, so it can
  never put a command on the reserved CPUs without having claimed them. Two
  concurrent runs queue - measured, the second waited 2.59s for a
  three-second first.
* `pin_if_reserved` refuses to pin at all unless a lock is available, so an
  ad-hoc `SCALING_BENCH_CPUS` with nothing behind it leaves the benchmark
  unpinned rather than on an unclaimable core.
* `quiet-bench run` sets `SCALING_BENCH_LOCKED`, and a benchmark seeing that
  takes the in-process mutex only. Without it the child would wait on the
  `flock` its own parent holds, which never comes free.

`an_inherited_lock_is_not_waited_on` covers that last one. It tests
`machine_lock` rather than `exclusive`, because `exclusive` also takes the
in-process mutex and the test would then be timing whatever other test
happened to be benchmarking - which is exactly how it failed first time.

## Tried without success so far

Read this section with suspicion. A negative result holds only over the
range it was measured, and at least one here was originally recorded as a
general truth when it was an artifact of too narrow a range:

> "More sampling time does not buy reproducibility - slope -0.13." That was
> measured from 150us to 77ms, which is *entirely* inside the regime where
> correlated noise fails to average. Measured properly from 1ms to 30s the
> slope is -0.38 and reaches -0.51 past a second. It became item (1).

So each entry below records what was actually tested, and what would change
the answer. None of these is closed.

### Not demonstrated (mechanism still plausible)

- **Chi-squared upper bound on sd for the stopping rule.** Won decisively in
  one session (spread 0.99% -> 0.32%, four passes, no overlap) and vanished
  in the next (0.82% vs 0.85%) with identical code. *Tested only at the
  default ~600us-2ms of sampling, i.e. the noisiest regime, and the four
  passes shared one session's drift state so they were not independent
  replicates.* The selection effect it targets is real and separately
  demonstrated. What would settle it: sessions separated by hours, and
  after item (1) raises the floor - though a higher floor also makes the
  correction moot, since the factor tends to 1 as samples grow.
- **Warm-up before measuring.** Reduced spread 0.170% -> 0.150% and cut runs
  stopping at the floor from 7/30 to 1/30 - a small *positive*, not a null.
  Dismissed too quickly on the grounds that frequency was pinned and
  temperature flat, which only rules out thermal warm-up, not cache or
  branch-predictor state. *Tested only in the sub-100ms regime.*
- **64-byte function alignment** (`-C llvm-args=-align-all-functions=6`).
  Median spread across 12 builds went 0.24% -> 0.35%. *But the per-build
  spread statistic was itself unstable - one build measured 1.308% and then
  0.236% - so this had little power to detect a real effect,* and it was
  measured on a synthetic 8-function program rather than real benchmarks.
- **Larger batches.** No effect once batch sizes were interleaved rather
  than measured sequentially: median block spread 0.078-0.107% from 100us to
  2ms. *Tested at one workload, with 100ms blocks - which straddles the
  crossover found in (1) - and only up to 5ms batches.* The aliasing it was
  meant to fix is real (lag-1 of -0.58 at half a tick), so the null may be
  regime-specific rather than general. Related to item (6).

### Decisions, not measurements

- **`nohz_full`.** Would remove the tick at its source, but needs a reboot
  and makes every kernel/user transition more expensive - which would
  distort exactly the benchmarks that change syscall counts. Judged not
  worth it; no measurement was taken.
- **`compare_scaling`.** Meaningful but judged too niche: a scaling
  regression shows up as `power: 1` becoming `power: 2`, which is legible at
  a glance and does not need a significance test.

## Findings worth keeping

- **There is a moiré at exactly the scheduler tick.** Dominant spectral peak
  at 1000.1Hz = `CONFIG_HZ`, harmonic at 1998.8Hz. With 100us samples
  against a 1ms tick, every tenth sample lands on the same phase:
  autocorrelation at lag 10 of +0.88. Batch sizes at 1/2 and 1/4 of a tick
  show *negative* lag-1 (-0.58, -0.24), the alternating signature of
  aliasing.
- **There is a 1/f drift floor.** Autocorrelation never decays - still +0.21
  at half a second - and spectral power falls from ~1e4 at 1Hz to ~3e2 at
  300Hz. This is what no sampling strategy has beaten.
- **The selection effect is real.** With the accuracy target disabled so
  nothing selects on the data, the mean is flat to 0.14% across a 200x range
  of durations. With adaptive stopping, short runs read ~1.2% faster than
  long ones: six samples that miss the slow tail of a right-skewed
  distribution have both a small sd (so the run stops) and a low mean.
- **Interleave everything you compare.** The one technique that has worked
  every time. Three results that looked clean under sequential measurement
  evaporated under interleaving. It is why `compare` beats two `bench` runs,
  and it is the reason for items 2, 3 and 4.
