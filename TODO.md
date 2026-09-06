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

### [ ] 2. Randomise comparison order

Time the candidate first on half the rounds. `compare` currently always
times baseline then candidate, so the two sample fixed and *different*
phases of any periodic disturbance - and there is a real one at exactly
1000Hz (see Findings). Cheap, and the right defence even though the
positional bias measured small (mean +0.04%, spread +/-0.8%).

### [ ] 3. K-way compare

Generalise `compare` to k alternatives, round-robin by batch with the
starting position rotated each round so every alternative spends equal time
in every slot. The multiple-comparison machinery
(`num_comparisons_planned`) already exists.

Holding k alternatives needs `Box<dyn FnMut()>`, and that erasure is a
*benefit* here: all alternatives go through one shared call path instead of
k separate monomorphisations at k different addresses, which should remove
the layout lottery below. Falsifiable prediction - worth measuring.

### [ ] 4. Interleave dissimilar benchmarks across a suite

Today benchmark #1 runs at t=0 and #50 at t=500s, sampling different thermal
states. Interleaved, every benchmark's samples spread over the whole
session, so all of them average the same drift. Needs time-sliced
scheduling rather than fixed batch counts, since the benchmarks differ in
input type and duration - more machinery than (3).

## Also open

### [ ] 5. Paired estimator in `Comparison::std_error`

`std_error()` combines the two halves as independent
(`sqrt(se_b^2 + se_c^2)`), but they are timed back to back under nearly
identical conditions. Taking the variance of the *per-round differences*
would cancel common-mode drift and tighten the bars. Never tested, and
plausibly the largest single win still available given how much of the
noise is drift.

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

### [ ] 9. Fix `scaling_error_bar_is_honest`

Rate uncertain and probably conditions-dependent. It failed 4 times in ~19
pinned runs one evening, then went 9 for 9 the next morning after (8)
landed - though the earlier measurements were taken while other analysis
was running on the machine, which by itself argues the test is reading the
neighbours.

The design flaw stands regardless: it compares a *between-run* spread
against a *within-run* claimed error, which `Stats::std_error` documents
that it does not bound. Isolated it passes comfortably (ratio 0.7-1.8
against a bound of 4.0); with 40s of suite load ahead of it, 5.0. It is
measuring the machine rather than the library, so it wants (7) - a measured
fitness gate - rather than a looser bound.

### [-] 10. Document the layout floor

*Skipped.* Documenting the layout floor: judged not worth the words.

### [-] 11. Student-t rather than z in `is_significant`

*Skipped.* Student-t rather than z: mooted by (1), which makes the sample counts large enough that t and z agree.

### [ ] 12. `flock` gap when the reservation comes from the environment

`reserved_cpus()` prefers `SCALING_BENCH_CPUS` and falls back to
`CPUS_PATH`, but `lock_reservation()` only ever opens the file. Setting the
variable by hand without a `quiet-bench reserve` therefore gets pinning and
the in-process mutex but *no* cross-process lock, silently.

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
