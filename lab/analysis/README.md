# analysis

The Python behind the numbers in `ALGORITHM.md` and `PROBLEMS.md`. Each
script reads recordings from `day/collect` (or `$LAB_COLLECT`), or trial
dumps that `lab pairs` writes, and prints the table it was written for.
Run them from a scratch directory: they cache decoded recordings in
`diag/` and read dumps from relative paths.

No dependencies beyond the standard library. `paired.py` is the lab's
paired ratio estimator, bar and stopping rule, mirroring `replay.rs`.

| script | what it reproduces | input |
| --- | --- | --- |
| `rounds.py` | decodes the six pair recordings to `diag/*.pkl` | `pairs-{quiet,noisy}` |
| `simstop.py` | 4 vs 8 blocks under iid noise, no machine at all | none |
| `variants.py` | blowups by block count and Student's t | `var/*.tsv` |
| `rerun.py` | a second trial after a gap, and the budget split in two | `rerun/*.tsv` |
| `rules.py` | the third-pass rules (the doc rule, random effects, majority) | `three/*.tsv` |
| `zrate.py` | how often \|error\| > z·σ, with and without t | `three/*.tsv` |
| `cross.py` | the same pair across separate processes | `battery-noisy`, `mains-noisy` |
| `cross3.py` | the multi-pass rules with each pass in its own process, plus the wander check | `diag/<cond>-run*.pkl` |
| `sens.py`, `predict.py` | clock sensitivity, and the wander it predicts | `diag/*.pkl` |
| `wander.py`, `levels.py` | how far a pair's ratio wanders over windows | `diag/*.pkl` |
| `cycles.py` | nanoseconds against bogo-nanoseconds across processes | `battery-*`, `mains-noisy` |
| `jumps.py` | whether a process's offset is fixed or episodic | `diag/<cond>-run*.pkl` |
| `slow.py` | `slow_cpu` against its exact ratio, one rung and two | `slow` |
| `slowpath.py` | the few-call path for slow functions | `pairs-quiet`, `slowq` |
| `verdicts.py` | changed / unchanged / refused after the whole multi-pass pipeline, two third-pass rules | `bonf/*.tsv` (`LAB_TARGETS=0.0035,0.007`) |
| `memfix.py` | two fixes for memory pairs' bars: variance ratio, and a floor | `bonf/*.tsv` |
| `vector_wake.rs` | the vector unit's wake-up penalty, with nothing else running | none |

The dumps come from the lab itself, run from `lab/`. `SP` is the scratch
directory:

```sh
# var/: block count and t (variants.py)
for v in 4 8; do LAB_PAIR_BLOCKS=$v LAB_TRIALS=$SP/var/B$v-quiet0.tsv \
  target/release/lab pairs day/collect/pairs-quiet/*.p0.bin; done
# rerun/: a second trial 0, ~1 and ~10 minutes later (rerun.py)
LAB_RERUN_GAPS=0,4700,47000 LAB_TRIALS=$SP/rerun/quiet0.tsv \
  target/release/lab pairs day/collect/pairs-quiet/*.p0.bin
# three/: three passes, for the third-pass rules and z-rates (rules.py, zrate.py)
LAB_RERUN_GAPS=4700,47000 LAB_TRIALS=$SP/three/quiet0.tsv \
  target/release/lab pairs day/collect/pairs-quiet/*.p0.bin
```

Repeat for each of `quiet{0,1,2}` and `noisy{0,1,2}`, naming the file
after the recording, as the scripts group by that prefix. `LAB_TARGETS`
replays other goals, such as the one-sigma target a Bonferroni-corrected
comparison stops at.
