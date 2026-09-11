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

**Early-stop bias: measured on an unquiesced machine, and not found.** The
better hypothesis was that interleaving should attack the selection effect
recorded under "Findings worth keeping". Sampling stops when the standard
error of the mean is small enough, but that standard error assumes
independent samples, and samples taken back to back on a drifting machine
are not: they share a drift state, agree with each other for that reason,
and stop the run on a `±` no repeat of it will honour. Spread across a
session, the spread seen while sampling should be the spread really there.

Measured with the reservation off and the machine on battery - the condition
where it could show - on a deterministic workload so everything that varies
is the machine. Sixteen runs per arm, medians over four invocations:

| | median | robust spread | claimed +/- | samples |
| --- | --- | --- | --- | --- |
| sequential | 220.0-220.9ns | 0.03-0.28% | 0.24-0.46% | 19-75 |
| suite of one | 220.3-221.1ns | 0.05-0.14% | 0.18-0.49% | 20-29 |
| interleaved | 220.5-220.9ns | 0.06-0.21% | 0.22-0.45% | 19-63 |

Nothing. Medians equal, spreads equal, and the sample counts show no sign of
the mechanism - for it to work, interleaved samples would have to see a
larger standard error and therefore stop *later*, and they do not.

**Neither column above settles whether the `±` is honest, and an earlier
version of this entry claimed it did.** It said the claimed `±` ran two to
eight times larger than the robust spread and concluded the error bars were
conservative. That comparison is not valid: `std_error` is computed from
every sample in the run and so counts the tails, while a half-interquartile
range discards them by construction. Even on clean normal data the ratio
would read 1.48x "conservative" for free, since a half-IQR is 0.675 sigma;
and with one run in sixteen at twice the cost, that run contributes about a
quarter to sigma and nothing at all to the IQR. So the two statistics here
are wrong in opposite directions from one cause - the sd-based one called
the error bars 38-73x too narrow, the IQR-based one called them 2-8x too
wide - and the honest position is that this experiment did not measure it.

Measuring it wants like compared with like: sigma against `std_error`, or a
trimmed spread against a trimmed `std_error`. Better still, measure lag-1
autocorrelation of the per-sample times directly, which tests the mechanism
instead of inferring it. The sign matters and can go either way:
`Var(mean) = (sigma^2/k)(1 + 2*sum_j (1 - j/k) rho_j)`, and `Running`
assumes that bracket is 1. Positive `rho` - drift, samples agreeing because
they share a trend - makes the reported `±` too narrow, which is what (5)
fixed for comparisons. Negative `rho` makes it too wide, and negative `rho`
is measured here: "Findings worth keeping" records lag-1 of -0.58 and -0.24
at batch sizes of half and a quarter of a scheduler tick, the alternating
signature of aliasing. A benchmark can have both at once.

**The first version of that table said 38-73x dishonest against 2-10x, and
was wrong the same way the position table was.** The statistic was a
population standard deviation, and this machine produces roughly one run in
sixteen at *twice* the cost - one such run moves an sd by twenty percentage
points. Sequential's worst arm read min 220.4, median 220.8, max 439.1. The
`suite of one` control is what exposed it: a control that lands with
sequential in one invocation, with interleaved in the next, and between them
in a third is measuring noise. **On a machine like this, report medians and
inter-quartile ranges and never an sd.**

A 5% gap between `bench` and the suite path turned up while chasing this,
and was chased down: there is none. `suite_path_costs_nothing` alternates
the two on equal-length runs and reports -0.03%, -0.19%, -0.21%. The gap was
an artifact of the experiment - a three-arm rotation in which one arm ran
31x longer put `bench` after the long run two thirds of the time and the
suite before it two thirds of the time, and a run following 250ms of load
measures 1.4% faster than one following 250ms of idle. Which is a small
independent datum for the warm-up item under "Not demonstrated", measured
this time somewhere the frequency was free to move.

The machine's absolute level moved from ~220ns to ~268ns for the same
workload over the course of these measurements. Nothing measured in one
sitting here can be compared with anything measured in another.

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

### [ ] 13. Discard preempted samples, on evidence rather than on size

Ask the kernel whether a sample was interfered with, and drop the ones that
were. Read a second clock alongside the wall clock:

```rust
// around the batch, outside the timed region
libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut ts);
```

`CLOCK_THREAD_CPUTIME_ID` advances only while this thread is on a CPU, so
`wall - cpu` over the batch is the time it spent descheduled. A batch that
lost time that way partly timed some other process, and the mean should not
carry it.

**Prefer this to `getrusage(RUSAGE_THREAD)` and `ru_nivcsw`**, which was
the first idea here. Measured, they cost the same - 382.9ns against 382.4ns
per call on this laptop - and the clock says strictly more: not merely
*whether* the thread was preempted but *how long for*, which is a threshold
you can set rather than a binary you cannot. A 3us gap in a 100us batch and
a 3ms one are both one context switch.

**The point is that it discards on cause and not on magnitude.** Every
size-based rule - a median, a trimmed mean, dropping the slowest decile -
founders on the same case, and it is a case this crate already tests for.
`bimodal_cost` spends 10,000 rounds one call in ten and 1 round otherwise:
mean about 1001, median 1. Any estimator that throws away large values
reports a thousandth of the true cost, and
`estimates_the_mean_not_the_minimum` exists to catch precisely that. The
mean is also the quantity wanted - what N calls cost is N times the mean,
not N times the median.

Sample-level trimming is *safer than it looks*, because each sample already
averages `unit` iterations and so has the function's own tail reduced by
`1/sqrt(unit)` - at `unit` around 500, `bimodal_cost`'s per-sample values
vary by only about 14%, so a sample at twice the rest is far likelier to be
the machine. What kills it anyway is rarity: a tail event with `p = 1e-4`
costing `1e6` times the baseline puts *zero* events in most batches and one
in a few, so the samples read "mostly baseline, occasionally enormous" and
trimming deletes the entire signal. No timing-based rule can tell that
workload from an interrupted one. The kernel can.

What to watch for:

* **Cost.** 382.9ns a call, measured, and two calls a batch - so about 0.8%
  of the default 100us batch, and some 4% against `bench_scaling`'s 20us
  measurable floor. It sits outside the timed region so none of it lands in
  the answer, but `benches/harness-cost.rs` should be asked about the wall
  cost, and the scaling case may want its own decision. For contrast the
  ordinary clock is 26.8ns, which is why timing itself is free and this is
  not.
* **Selection.** Dropping samples selects on something correlated with the
  machine being busy, and (1) records how badly adaptive stopping can be
  fooled by selection. Discarded samples should probably still count against
  `MIN_SAMPLE_TIME`, and the number discarded is worth reporting: a run that
  threw away half its samples measured a busy machine, whatever the `±`
  says.
* **Syscalls in the benchmark.** Thread CPU time counts user *and* system
  time on this thread's own behalf, so a function that makes syscalls is
  measured fairly - unlike the cycle counter in (15), which
  `perf_event_paranoid` forces to exclude the kernel. Worth confirming
  rather than assuming, since it is the difference between the two
  approaches on syscall-heavy code.
* **Portability.** Linux only. Elsewhere this compiles away to today's
  behaviour, as the affinity code already does.

Independent of everything else here: it touches `time_batch` and `Running`
and nothing in the scheduling.

**It does not address the outliers that prompted it.** Measuring (4)
unquiesced turned up whole *runs* at twice the cost, and if a run is
uniformly slow - the frequency dropped for its whole duration - then every
sample is slow together, the within-run spread looks tight, and `std_error`
reports a confident `±` on a number that is wrong by a factor of two. That
is invisible to any statistic computed from the timings, and is what (14)
exists for - and (7) above it, for whatever moves that is not the clock.
Worth measuring first which of the two shapes those runs actually have,
since it decides whether this item would have helped at all.

### [ ] 14. Read the clock frequency around each sample

Sibling to (13), and the more important of the two, because it catches the
failure (13) cannot see.

Read `/sys/devices/system/cpu/cpu<N>/cpufreq/scaling_cur_freq` before and
after a sample, along with `sched_getcpu()`, and record whether either
moved. A sample measured at a different clock speed than its neighbours is
not comparable with them, and one measured on a different core may not be
either.

**Why this one matters most.** Measuring (4) with the reservation off turned
up whole *runs* at twice the cost, and (13) is blind to them by
construction: if the frequency dropped for a run's entire duration then every
sample is slow together, the within-run spread looks tight, and `std_error`
reports a confident `±` on a number that is wrong by a factor of two.
Nothing computed from the timings can see that, because from the inside
there is nothing to see. Asking what the clock was doing is the only way.

**Cost, measured on this laptop:**

```none
  open + read + close        7.7us     7.7% of a default 100us batch
  held-open fd + pread       0.5us     0.5%
```

So hold the descriptor open for the run rather than opening per sample; that
is the difference between unaffordable and affordable. `sched_getcpu()` is a
vDSO call and near enough free. Both go outside the timed region, so nothing
lands in the measurement itself - but `benches/harness-cost.rs` should still
be asked about the wall-clock cost.

**What to do with it.** Flag before filtering. A third flag on [`Stats`]
beside `hit_limit` and `untrustworthy` - the clock moved under this
measurement - needs no statistical decision and no selection, and matches
how the crate already reports a result it does not trust. Only once that is
in place is it worth asking whether to bin samples by frequency, or discard
the minority ones: both select on something correlated with the machine
being busy, and (1) records how badly adaptive stopping can be fooled by
selection. Binning is the more interesting of the two - "43.1ns/iter at
1.7GHz" is a *better* contract than an unqualified number - but it is a much
larger change, and needs enough samples in a bin to say anything.

**Caveats.**

* On `intel_pstate`, which is what this machine runs, `scaling_cur_freq` is
  an APERF/MPERF average over the driver's own sampling interval rather than
  an instantaneous reading. Its timescale may be coarser than a 100us batch,
  so a brief dip can be invisible. Worth checking what it actually resolves
  before trusting it at the sample level; at the level of a whole run it is
  certainly good enough.
* Frequency is not the only thing that moves - uncore and memory clocks do
  too, and are not exposed this cheaply.
* Linux only, like the affinity code, and compiles away elsewhere.

**It should be silent on a quiesced machine**, since `quiet-bench reserve`
pins the governor and disables turbo. That makes it a check that quiescing
actually worked, which is half of what (7) wants and a good deal cheaper
than (7)'s reference workload: this measures the mechanism directly where
(7) measures the symptom. The two belong together, and `quiet-bench status`
could report both.

**While here: `CLOCK_MONOTONIC_RAW` is free.** `Instant::now()` is
`CLOCK_MONOTONIC`, which NTP steers by up to 500ppm - 0.05%, right at the
edge of what a 0.1% target can afford, and a systematic rather than noise.
`CLOCK_MONOTONIC_RAW` is not steered and costs the same: 26.48ns against
25.81ns, both in the vDSO on this kernel, where `Instant::now()` itself
measures 26.80ns. It cancels out of a comparison and only shows in an
absolute `ns_per_iter`, so this is small - but it is free, and (15) makes it
matter more, since an unsteered clock is the one to divide cycles by.
`Instant` cannot be told which clock to use, so this means calling
`clock_gettime` directly on Linux and keeping `Instant` elsewhere.

Independent of the scheduling work, and shares its shape with (13): ask the
system whether a sample is trustworthy, rather than inferring it from the
sample's own size. They likely want one shared place to record what was true
around a batch.

**Measured against a first implementation.** Four things a second attempt
should start from:

* **`scaling_cur_freq` is an *effective* frequency, not a P-state.** Under
  `intel_pstate` it is an APERF/MPERF average, so it falls whenever the core
  halts briefly - an interrupt, or the read syscall itself. On a machine
  quiesced with the governor pinned and turbo off, three of four long runs
  still read 1.64GHz against a pinned max of 1.700GHz and flagged movement.
  Whatever tolerance is chosen, idle time reads as a lower clock.
* **Migration never fired.** Across every run measured - unquiesced,
  `taskset` to a pair, and a two-CPU reservation - `sched_getcpu()` changing
  was never what set the flag; the frequency term always was, and always
  fired first. On an unquiesced machine migration is redundant, and on a
  quiesced one it is the only term that *can* fire spuriously. Worth
  dropping from the flag and keeping only to know when to reopen the fd.
* **A short run cannot see anything.** `scaling_cur_freq` refreshes about
  once per millisecond here, so every reading in a run shorter than that is
  the same cached value. Before (1) landed, a fast benchmark stopped at six
  samples, i.e. 0.6ms - structurally blind. `MIN_SAMPLE_TIME` of 3ms mostly
  fixes this, which is worth noting as a dependency rather than a
  coincidence.
* **Hold the descriptor, and remember when you could not open it.** The
  `open`+`read`+`close` against held-open `pread` gap is the 7.7us/0.5us in
  the findings below. But a failed `open` needs recording too: retrying it
  per sample costs a failed syscall and a path allocation on every read
  forever, on exactly the machines that have no cpufreq to read.

And one thing the clock swap costs, which the paragraph above under-rates:
`CLOCK_MONOTONIC_RAW` runs slower than `CLOCK_MONOTONIC` by a *measured*
5.36ppm here - steady, not noise. That is -59us over the 11-second sleep in
`painfully_slow`, whose assertion has only a ~120us margin, so the test
became flaky. NTP is permitted 500ppm, which would be 5.5ms and would fail
it outright. Timing a `thread::sleep` with a clock other than the one sleeps
are scheduled against needs the margin to be checked, not assumed.

### [ ] 15. Count cycles as well as nanoseconds

Open a `perf_event_open` counter for `PERF_COUNT_HW_CPU_CYCLES` and read it
around each sample, alongside the wall clock. Supersedes (14) where it
works, and answers a question neither (13) nor (14) can.

**The cross-check is the point:**

```none
  ns/iter    cycles/iter    reading
  moved      moved          the code changed
  moved      steady         the clock changed - not a regression
  steady     moved          frequency compensated; suspicious
```

That is the separation (7), (13) and (14) are all circling: *did the machine
move, or did the code?* Nothing computed from timings alone can answer it.

It also gives (14)'s number for free and better. **cycles / nanoseconds is
the effective frequency over exactly the interval measured**, where
`scaling_cur_freq` on `intel_pstate` is an APERF/MPERF average over the
driver's own sampling window and may be coarser than a 100us batch.

**Do (14) first anyway.** The cheap path is 0.5ns for the frequency read and
2.3ns for `sched_getcpu`; this is 412ns a read and 824ns a batch, which is
0.8% of a default batch and some 4% of `bench_scaling`'s 20us floor. Most of
the diagnostic for a three-hundredth of the cost. `rdpmc` off the mmap'd
page would bring a read to ~10ns and remove that objection, but it is a good
deal more machinery, and `/sys/devices/cpu_core/rdpmc` is not even readable
as an ordinary user here, so whether it is permitted needs testing.

**Where it gets hard:**

* **Hybrid CPUs.** This laptop is an i5-1240P: eight P-cores at 4.4GHz on
  the `cpu_core` PMU and eight E-cores at 3.3GHz on `cpu_atom`. A single
  generic `PERF_TYPE_HARDWARE` event silently counts nothing - measured, all
  three encodings returned zero - because the thread ran on the PMU the
  event was not opened against. `perf stat` shows it plainly: unpinned it
  counted `cpu_atom/cycles` and reported `<not counted>` for `cpu_core`;
  under `taskset -c 0` it did the reverse. So both PMUs must be opened, and
  summing them is *wrong* if the thread migrated, because a P-core cycle and
  an E-core cycle are neither the same wall time nor the same work. Pinned -
  the mode this crate already recommends - it is clean, and that is probably
  the condition to require.
* **`exclude_kernel` is forced** at `perf_event_paranoid = 2`, which is what
  this machine runs. Cycles then exclude syscall time while the wall clock
  includes it, so a syscall-heavy benchmark shows a cycles-per-ns ratio that
  looks like a frequency drop and is not. (13)'s thread CPU time does not
  have this problem and could disentangle it.
* **Multiplexing.** More events than the PMU has counters and the kernel
  time-slices them and *scales* the result. `time_enabled` and
  `time_running` from the read must be compared, or an extrapolation gets
  reported as a measurement.
* **Availability.** Containers and hardened kernels often set
  `perf_event_paranoid = 3`. This has to degrade silently to no cycle data,
  never to a wrong number.
* **`libc` does not expose `perf_event_attr`**, so the struct has to be
  declared here. A zeroed 128-byte buffer with `type`, `size` and `config`
  poked at their offsets is accepted by `perf_copy_attr`, which is
  forward-compatible by design.

**Cycles are not a universal invariant, and I claimed they were.** For
memory-bound code they are not even frequency-invariant: DRAM latency is
fixed in nanoseconds, so the same stall costs *fewer* cycles at a lower core
clock. Cycles/iter holds still under frequency change only for core-bound
work. It answers "how much work was done", which is a genuinely different
and useful question from "how long did it take", but it is not a better
clock and it would not reliably have caught the 2x runs in (4).

**Related and not the same: `rdtsc` is not a cycle counter.** On modern x86
the TSC is invariant - a fixed rate regardless of P-state - so it measures
wall time, which is why it is 8.34ns (13.00ns behind an `lfence`) against
`clock_gettime`'s 26.8ns and why it says nothing about frequency. It is a
cheaper clock, not a counter. Worth remembering if (6) ever wants batches
small enough for 26.8ns to matter.

### [ ] 16. Report a ratio, not only a difference, from `compare`

`Comparison::std_error` reasons that whatever the machine does slowly
"moves both together and cancels out of each round's difference". That is
true of *additive* drift. The drift this machine actually has is
multiplicative - a frequency ramp scales everything - and under
`a_r = A·s_r` the round difference is `(A-B)·s_r`, which still carries
`s_r`.

Measured on interleaved rounds with known ground truth (two workloads, one
doing twice the work of the other; 8 runs per condition, 8000 rounds each):

| statistic | quiesced | unquiesced |
| --- | --- | --- |
| difference | 11791.7ns ± 0.1 | 12541.5ns ± 1077.0 |
| ratio | 1.99757 ± 0.00028 | 1.99713 ± 0.00185 |

The unquiesced runs drifted 1.6-2.1x within a run. The ratio survives it;
the difference is biased +6.4% and its run-to-run spread grows by three
orders of magnitude. So interleaving makes the *sign and existence* of a
change robust to drift, and does not make its *magnitude* robust.

Aggregating per-round log-ratios with a median is what recovers it - the
only statistic tried whose unquiesced performance matched its quiesced
performance:

| aggregation | null (truth 1.0), unquiesced | 2x config, unquiesced |
| --- | --- | --- |
| ratio of means | 0.99965 ± 0.00156 | 1.99713 ± 0.00185 |
| median of log-ratios | 1.00000 ± 0.00000 | 1.99785 ± 0.00009 |

*The median works here because the drift is concentrated in a minority of
rounds - the opening ramp.* A run that was uniformly slow throughout would
still bias the difference; only the ratio is safe there. That is the same
failure (14) exists to flag.

Note this also makes `is_changed()` perverse under drift: the spread of
round differences picks up a `|A-B|·sd(s)` term, so a *larger* true
difference becomes harder to call significant. The log-ratio has no such
term.

### [ ] 17. Trim whole rounds, on the round total

If (16) or any robust aggregation lands, the unit of trimming has to be the
round, not the sample. `paired_std_error` exists only because round *r*
contributed to both halves; trimming per function deletes `a_r` without
`b_r` and silently reverts the paired estimator to the combined form that
(5) measured as a third too wide.

**Select on the round total, never on the difference.** In a paired design
the sum and the difference are orthogonal contrasts - `Cov(a+b, a-b) =
Var(a) - Var(b)`, zero when the alternatives have similar spread - so
trimming on the sum does not bias the difference, while trimming on the
difference is selecting on the outcome. Measured against a true ratio of
exactly 1.0:

| selector | quiesced | unquiesced |
| --- | --- | --- |
| round total | 1.00000 | 0.99999 |
| round difference | 0.99451 (-0.55%) | 0.99079 (-0.92%) |

Selecting on the difference manufactures a half-percent difference where
there is none, and does so *even quiesced*, so it is selection bias and not
drift. Round-total trimming also beat per-function trimming 1.7x on the
config where the alternatives differ (sd 0.00057 against 0.00096) and was a
wash where they are identical - i.e. it matters exactly in the case worth
caring about.

Two limits: the orthogonality is exact only when the alternatives have
comparable spread, and the sum contains each function's own time, so
exogeneity is roughly `1/k` contaminated - solid for a `ComparisonSet`,
weakest at k=2, meaningless at k=1.

### [ ] 18. Fit out the fixed per-measurement cost in `bench`

`bench` times a batch and divides by its length, which is the move
`bench_scaling` deliberately rejects: dividing turns the fixed per-batch
overhead into a `c/N` term no polynomial represents, so it smears into the
answer. Measured, that overhead is **58ns** - two `Instant::now()` calls -
and it biases a 2x comparison by 0.115%, always toward 1. It is recoverable
from the same ladder-of-sizes machinery `bench_scaling` already has:
`weighted_poly_fit(ns, means, ses, 1)` returns `coefficients[0]` as the
overhead and `coefficients[1]` as the per-iteration cost with it removed.

Three things make that fit the right primitive rather than a coincidence of
shape: it already carries the constant term; its weights are *measured*
error bars with no residual rescaling, so `ses[1]` is a real standard error
for the slope; and `chi2_per_dof` at degree 1 is exactly a test of whether
per-iteration cost depends on batch size. `measure_scaling`'s loop shape
transfers too - a `Running` per rung, refit each round, stop on the
coefficient of interest. What does not transfer is everything about
locating an unknown power: `discover_sizes`, `choose_sizes`, `next_size`,
`power_fit`.

For `compare` this threatens to multiply the round by the number of rungs,
which would be self-defeating - a round only works while everything in it
sees the same machine state. Two things stop it:

* **The intercept is a property of the harness, not of the function.** Two
  clock reads cost the same whatever sits between them.
* **It drifts, so it wants measuring continuously rather than once.** The
  clock reads are themselves CPU work, so `c₀` at 850MHz is twice `c₀` at
  1.7GHz; a one-off calibration goes stale.

Which suggests: **the baseline carries the ladder and everyone else uses a
fixed batch.** Round cost becomes `m + (k-1)` rather than `k·m`, `c₀` is
re-measured every round by the function that deserves the most measurement
anyway, and the per-function fit demotes to an occasional diagnostic. The
one thing to carry: an error in `c₀` propagates as a *common* bias into
every candidate's slope and does not cancel in a ratio - it pushes ratios
away from 1 - so `ses[0]` is worth reporting rather than discarding.

### [ ] 19. Sample sizes below 100us

The "larger batches" null in the section below was measured from 100us
upward. Downward there is something: sweeping `SAMPLE_TIME` quiesced and
scoring by precision per second of budget (consecutive windows, so
correlation is preserved) rather than by sample count,

| nominal | wall/sample | CV/sample | true SE in 300ms | reported/true |
| --- | --- | --- | --- | --- |
| 10us | 23.1us | 5.888% | 0.000733 | 0.37 |
| **30us** | **63.3us** | **2.298%** | **0.000124** | 1.46 |
| 100us | 203.0us | 1.262% | 0.000520 | 0.37 |
| 300us | 582.5us | 2.630% | 0.000776 | 0.57 |
| 1ms | 1199.5us | 14.579% | 0.015817 | 0.11 |

30us is ~4x better than the current 100us, and 10us is *worse* than either -
per-sample CV rises faster than the extra samples repay. Note this is not
the overhead (18) removes: the fit takes out a bias, and what limits small
batches is variance. *One pool per size, 10-30 windows each, so each SE
carries ±13-22%. Wants three replicates before a constant moves.*

Two things fell out of the same sweep and are worth more than the sweep:

* **Every batch carries ~100% wall overhead** - 203us of wall time per
  nominal 100us sample. That is the untimed `xs.extend` setup loop; for
  plain `bench` the environment is `()`, so it is building a `Vec<()>` of
  ~169,000 elements at a cost comparable to the timed region. Removing it
  for the no-environment case roughly doubles samples per second at *any*
  batch size, and needs no statistical argument at all.
* **`reported/true` is 0.37-0.57 nearly everywhere**, on a quiesced machine,
  with lag-1 autocorrelation at ~0. So the `±` is 2-3x optimistic about a
  repeat run at the same budget, and the correlation responsible is
  long-range - the 1/f floor below, not sample-to-sample.

### [ ] 20. Measure the drift time, and set the sample time from it

(7) wants a machine-fitness check; this is the same probe with a number
attached. Take a long run of fixed batches and plot the variance of the mean
against block size - an Allan-deviation curve. Where it stops falling like
`1/n` *is* the correlation time of the machine's drift, and that is the
right criterion for both `MIN_SAMPLE_TIME` and the length of a round, both
of which are currently constants chosen on one laptop.

It would also explain the `reported/true` gap in (19) directly, rather than
leaving it as an observation, and it gives `quiet-bench status` the evidence
(7) asks for.

### [ ] 21. Two canaries, and report the ratio

Interleave two workloads of *known constant* cost with the real samples.
Because their true cost does not change, everything their timings do is the
machine - measured in the same units, on the same core, through the same
timing path, in the same round. This subsumes most of what (13), (14) and
(20) were separately reaching for, and it is the largest effect measured so
far by a wide margin.

**The result.** Spread of the estimate across *eight independent process
invocations*, per iteration:

| payload | raw, battery | raw, AC | ratio, battery | ratio, AC |
| --- | --- | --- | --- | --- |
| L1-resident | 5.07% | 16.79% | **0.034%** | **0.026%** |
| L2-resident | 5.02% | 16.95% | 0.827% | 0.317% |
| DRAM-bound | 12.51% | 6.01% | **0.092%** | **0.097%** |

Raw nanoseconds swing between 5% and 17% with the power source; the ratio
sits between 0.03% and 0.83% everywhere, and the well-matched cases barely
notice that the laptop was unplugged (0.026% against 0.034%). During the
battery runs the clock was swinging 400MHz-1400MHz *while idle*.

**The two canaries, and why two.**

* **CPU**: a dependent integer chain in registers. Minimal ILP, so it is
  close to a pure clock reading. Measured CV of **0.02%** quiesced - a
  200ppm ruler.
* **Memory**: a pointer chase from a *fresh start each call*, over an array
  sized against L3. Latency bound.

They are not redundant and cannot be merged: their cross-correlation is
-0.005, because core clock and the memory subsystem scale independently. A
frequency ramp moves the CPU canary and the CPU payloads and leaves the
memory canary alone; whatever scales the uncore does the reverse.

**Matching is not optional, and needs no user input.** Core-bound payload
divided by the CPU canary gives 0.03%; divided by the memory canary, 23%.
The DRAM payload is the exact mirror. Picking whichever canary yields the
lower ratio spread chose correctly **9 times out of 9**, never closer than
5x, so the choice can be made from the data and never exposed.

**A mixed workload wants both canaries, added.** For a workload that is
purely one thing, blending is strictly worse: sweeping `P / (C1^w *
M1^(1-w))` gives a monotone curve with no interior optimum, so the best
blend is always a pure canary. But a workload that spends part of its time
CPU bound and part of it memory bound is a different case, and there the
physical reading - `P = a*C1 + b*M1`, with `a` and `b` the amount of each
kind of work - wins decisively. Leave-one-out prediction of a held-out run,
against deliberately mixed payloads:

| payload | `a*C1 + b*M1` | best single canary | fitted memory share |
| --- | --- | --- | --- |
| built ~10% memory | **0.207%** | 1.193% | 11.3% |
| built ~50% memory | **0.505%** | 4.956% | 53.5% |
| pure CPU | 0.170% | 0.185% | 1.1% |
| pure memory | 0.116% | 0.107% | 99.7% |

Ten times better on the even mixture, and the fitted share recovers the
construction. Past about 90% of one kind the single canary is as good and
the second parameter costs slightly. Note this *is* the cross-correlation
idea done properly: the coefficient is built from `cov(y, C1)`, with two
refinements worth knowing - the shared part of the two canaries has to be
partialled out (barely matters here, their correlation is -0.005), and a
correlation is normalised, so it gives the CPU-versus-memory *share* but not
the scale. For `a` and `b` in real units you need the slope,
`cov(y,C1)/var(C1)`.

**But `a` and `b` cannot be fitted inside one run.** Round-to-round jitter
is mostly each canary's own noise, so a within-run regression is attenuated
- it returned an exponent of 0.64 where the truth was demonstrably 1 - and
the attenuation differs per run, so every run then reports a slightly
different quantity:

| `a`, `b` fitted from | spread of the estimate |
| --- | --- |
| within one run, per-sample | 12.63% |
| within one run, 100-round blocks | 32.69% |
| within one run, 500-round blocks | 49.99% |
| **across eight runs** | **0.505%** |

Bigger blocks make it worse, trading noise per point for too few points. So
the decomposition is a property of the *benchmark* that has to be learned
from the machine visiting several different states - which is what the
retry-and-combine loop produces anyway. Fit across the runs that were kept,
not inside any one of them. It is also identifiable exactly when it is
needed, since fitting requires machine-state variation and a machine with
none needs no correcting.

Generalising: **a fitted parameter must be fixed across the runs being
compared, or the runs are not reporting the same quantity.** Choosing the
weight per run by minimising its own within-run spread gave 86% where a
fixed pure canary gave 0.32%. This is also why the discrete canary choice
above is safe and a continuous weight is not: a 9-of-9 decision with a 5x
margin never flips, while a fitted weight always wobbles.

**Three ways a memory canary silently stops being one.** Each makes it
report a machine quieter than it is, which is the dangerous direction:

* *A fixed start.* A 100us canary touches only tens of KiB, so walking the
  same path every call goes cache-resident: 23ns a read instead of 142ns.
* *A fixed array size.* It must exceed L3, and L3 runs from ~4MiB on a
  laptop to 384MiB on EPYC. Size it from `cache/index3/size`.
* *Calibrating with a different access pattern than production.* Calibrating
  the chase from a fixed start oversized the count ~6x and made every sample
  three times too long.

Sizing, measured: dependent DRAM latency here is **142ns**, so a 100us
canary is ~700 reads touching ~44KiB - under L1, so it displaces almost
nothing. Across plausible machines (70-250ns) that is 400-1400 reads and
25-90KiB, never more than ~7% of a 1.25MiB L2. The design is insensitive to
the 3.5x spread in memory latency, but the read count must be *calibrated*
rather than hardcoded.

**The per-iteration trap, which cost me a day.** Each member calibrates its
iteration count once, at whatever clock prevails at that instant, so counts
vary **10-25% between runs** (24.8% for the streaming workload). Comparing
per-*sample* durations therefore inherits that noise and decorrelates two
workloads that are physically near-identical - it made the across-run gain
look like 1.1x when it is really 151x. Within a run the count is constant
and cancels out of any spread, so this bites only between runs. Divide by
the count before taking any ratio.

**Throw-out granularity: blocks, not rounds.** Gating on a single anomalous
canary sample scored *exactly the base rate* - no information - and under
sustained load it was actively harmful, because a round whose canary was
preempted is a round whose payload ran clean. A 64-round window median
against the run's own baseline, tested against a real `cargo build` in
another directory, found **94% of the contaminated rounds at 64%
precision**. So the natural unit is a contiguous stretch, which is also
exactly the unit a "wait for quiet and retry" loop discards.

**The pair is diagnostic.** That `cargo build` raised every member's median
~13%, but the CPU canary's tail went 0.10% -> 0.00% while the memory
canary's went 1.07% -> 4.00%. Level shift with no tail is multi-core turbo
budget; level plus tail is that *and* memory contention.

*Caveats.* All eight runs used one binary, so code layout was constant -
cross-build reproducibility is untested and is the open question in the
alignment entry below. The canary ratio is a machine-relative unit:
excellent for regression detection, and it needs a stored reference to
become nanoseconds again.

The one payload that resists all of this is the L2-resident one, at
0.32-0.83% where the matched cases reach 0.03%. It is *not* an unresolved
mixture: the additive fit attributes only 1-11% of it to memory, and the
blend sweep finds its optimum at a pure CPU canary, so there is no
combination left to find. Its within-run ratio spread is 0.13-0.38% against
0.32-0.83% across runs, which points at something constant within a run and
different between them - allocator placement deciding which L2 sets the
working set lands in. Canaries measured in the same round cancel *temporal*
variation and can do nothing about *configurational* variation. Closing that
gap means either a third canary genuinely resident in L2, or the alignment
question below; it is not a canary-combination problem.

**This wants its own PR and a design session before any code.** What is
described here is a different algorithm rather than an adjustment to the
existing one - it changes what is measured, what is reported, and how many
runs an answer takes - so it should be developed on a branch off `main`,
very likely standing apart from the current sampling code until it earns its
way in. Everything above is measurement, not design: the design question of
how canaries, the retry loop, the accuracy contract and the reported units
fit together has not been settled and should not be settled incrementally.

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
  regime-specific rather than general. Related to item (6). See (19) for
  what happens *below* 100us, which this did not cover.
- **A robust estimator (median or trimmed mean) instead of the mean.** The
  sign flips with the machine, which is why it is not a default. Applied to
  identical timings, relative to the mean's standard error:

  | | n=10 | n=30 | n=100 |
  | --- | --- | --- | --- |
  | quiesced, median | 0.53x | 0.27x | 0.33x |
  | quiesced, 10% trim | 0.84x | 0.93x | 0.87x |
  | unquiesced, median | 1.43x | 1.75x | 1.71x |
  | unquiesced, 10% trim | 1.05x | 1.09x | 1.11x |

  Quiesced, the residual noise really is a sparse right tail and the median
  is worth ~14x the samples. Unquiesced, the spread is *drift* - the whole
  distribution translating - which no robust estimator touches, while the
  widened pooled distribution makes them less efficient. Confirmed directly:
  under sustained contention every estimator shifted by ~70% (mean +69.4%,
  median +70.7%, trimmed +66.8%), so there is no clean subpopulation to
  recover. *Also: part of the quiesced gain may be quantisation - the
  median's SE stops falling like 1/sqrt(n) above n≈30, which looks like it
  snapping to a grid.* Where robustness does pay is on per-round contrasts,
  which is (16) and (17), not on raw samples.
- **Taking the minimum, for a ratio.** For a raw timing "the low ones are the
  accurate ones" is sound: noise is additive and positive. For a *ratio* it
  inverts, because noise in the denominator makes the ratio smaller - so the
  minimum systematically selects the rounds where the canary was most
  perturbed. Measured over per-round ratios it was the worst of five
  statistics by a wide margin (13-28% spread, against 0.007-0.5% for median
  and 10% trim). Median and trimmed mean are the ones that work there, which
  is the same answer as (17) reached from the other direction. *Turbo makes
  this worse even for raw timings: the minimum is "cost at the best clock
  this run reached", which is a thermal and power-state dependent quantity
  rather than a machine constant.*

### Decisions, not measurements

- **`nohz_full`.** Would remove the tick at its source, but needs a reboot
  and makes every kernel/user transition more expensive - which would
  distort exactly the benchmarks that change syscall counts. Judged not
  worth it; no measurement was taken.
- **`compare_scaling`.** Meaningful but judged too niche: a scaling
  regression shows up as `power: 1` becoming `power: 2`, which is legible at
  a glance and does not need a significance test.

## Findings worth keeping

- **What it costs to ask the machine something.** Measured with `bench`
  itself, i5-1240P, unquiesced. Items (13), (14) and (15) all turn on these
  numbers, so they live here rather than in any one of them:

  ```none
    sched_getcpu()                2.30ns
    CLOCK_MONOTONIC_COARSE        7.54ns   (1ms resolution - unusable here)
    _rdtsc()                      8.34ns
    lfence + _rdtsc()            13.00ns
    CLOCK_MONOTONIC              25.81ns
    CLOCK_MONOTONIC_RAW          26.48ns   (not NTP-steered)
    Instant::now()               26.80ns   (what the crate uses)
    CLOCK_THREAD_CPUTIME_ID     382.90ns
    getrusage(RUSAGE_THREAD)    382.44ns
    read(perf_event fd)         412.00ns
    open+read+close a sysfs file  7.7us    (0.5us with the fd held open)
  ```

  The gap that matters is the two orders of magnitude between the vDSO
  clocks and anything that enters the kernel. Timing is free - two reads
  bracket a whole batch, so 53ns against 100us is 0.05% - while asking who
  preempted you, or how many cycles you burned, is 0.8% a batch and some 4%
  of `bench_scaling`'s 20us floor.
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
- **The drift is multiplicative, and that is why ratios survive it.** A
  frequency ramp scales every size and every alternative by the same factor,
  so a ratio taken within a round is invariant to it and a difference is
  not. Measured across a 1.6-2.1x within-run drift, a ratio held to 0.005%
  while the difference of the same rounds moved 6.4%. This is the single
  fact behind items (16) and (17), and it is also why a *fitted power* is
  already safe - a common multiplier moves the log-log intercept, not the
  slope - while a fitted *coefficient* is not.
- **The opening of an unquiesced run is a different machine.** Timing the
  same benchmark for three seconds unquiesced, the first hundred samples run
  1.76-1.77x slower than the steady state, and the transient lasts ~200ms -
  hundreds of samples. It is *entirely* frequency: quiesced, the same
  measurement gives 1.000-1.004x, and three runs then agree to 0.045%
  against a claimed 0.009%. Two consequences. Where the governor is pinned,
  warm-up buys nothing and the "warm-up before measuring" entry above stays
  shut. Where it is not - anywhere `quiet-bench` has not been run, which is
  most places - a benchmark that finishes inside the transient reports a
  number wrong by nearly 2x with a tight `±` on it, which is exactly the
  failure (14) is for and the strongest argument that (14) is worth having.
- **Round-total normalisation is safe, and smaller than hoped.** Dividing
  each round by its own total before pooling, then rescaling by the median
  round total, leaves a fitted power unchanged to four decimals with no
  drift (0.8987 -> 0.8988 in L1, 1.0146 -> 1.0147 in DRAM) and under uniform
  drift. Under an *N-dependent* drift - the case where clock scaling reaches
  core-bound rungs more than memory-bound ones - it recovers only ~17% of a
  0.036 power bias, which integer rounding absorbs anyway. *The N-dependent
  model was synthetic, not measured; calibrating it needs two frequencies.*
  So the case for normalising is the coefficient and the per-rung error
  bars, not the power.
- **Autocorrelation fixes only what is shorter than the window you look
  through.** Correcting the standard error by the sample series' own
  autocorrelation - equivalently, taking it from the spread of block means -
  is right in principle and measurably insufficient in practice. On a
  quiesced machine it matters: memory-bound samples have a lag-1 of +0.59
  and a variance inflation of 5-8, so `sd/sqrt(n)` is ~2.6x too tight for
  them while being about right for CPU-bound work. On an unquiesced machine
  it recovers only a sixth of the gap (naive 0.01-0.07 of the truth,
  corrected 0.06-0.26), because the dominant variation there is not
  correlation at all - it is the frequency ramp, which is nonstationarity
  and no within-run statistic sees it. Correlation wants the ACF; drift
  wants (21).
- **Mains power is less reproducible than battery.** Across eight runs the
  CPU payloads spread 16.8% on AC against 5.1% on battery, because
  `powersave` on AC has 400MHz to 4.4GHz to roam through while the battery
  cap narrows the range. Plugging in is not the safe choice it looks like.
- **Load stabilises the memory clock.** The memory canary's across-segment
  spread fell from 21.4% on an idle battery machine to 4.3% during a
  `cargo build`: a busy machine holds its uncore clock up instead of idling
  down and back. Quiet is not the same as steady.
- **The calibrated batch misses its target duration.** Calibration happens
  once, at whatever clock prevails at that instant, so the samples it sizes
  land at 74-144us against a 100us target - 8-22% spread per workload across
  eight runs. Since per-sample spread depends strongly on sample duration
  (19), and the tick moiré sits at a particular duration, two runs of the
  same benchmark can end up in different noise regimes. An argument for
  re-checking the batch size when the clock has moved.
