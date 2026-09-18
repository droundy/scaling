#!/bin/bash
# The head-to-head: four protocols, scored on run-to-run reproducibility at
# equal wall-clock budget, with calibration inside the budget.
#
#   A-current   one sample at 100us, naive trimmed mean      (what we do now)
#   B-auto      measure the efficiency optimum, bracket it, subtract
#   C-small     one sample at 5us, naive                     (small, no subtract)
#   D-sub       samples at 100us and 800us, subtract         (big, subtract)
#
# C and D are what make a result interpretable: if B beats A, C says how much
# came from the shorter sample and D how much from the estimator.
#
# Every repeat is a separate process - fresh calibration, fresh allocations,
# separated in time - because that is the only way calibration wander and
# process-to-process variation get counted at all.
#
# Four arms:
#   fast     the budget sweep, where the crossover should be
#   general  six diverse workloads, to see whether the answer travels
#   slow     slow_cpu walked across the ~1ms tick crossover
#   vary     as `fast`, but the round composition changes every repeat
set -u
cd "$(dirname "$0")/.."
LAB=./target/release/lab
OUT=day/proto
mkdir -p $OUT
say(){ echo "=== $(date '+%F %T')  $*"; }
HDR="cell,budget_s,rep,workload,estimate,samples,rounds,calib_s,total_s,note"

# --- arm 1: budget sweep over three characterised workloads ----------------
# Budgets span 3ms (about six samples at 100us - the regime actually used in
# practice) to 10s. The interesting output is not which protocol wins but
# where the winner changes, because that is the rule a crate would encode.
say "arm fast: 3 workloads, 8 budgets, 4 cells, 150 reps"
echo "$HDR" > $OUT/fast.csv
for b in 0.003 0.01 0.03 0.1 0.3 1 3 10; do
  for rep in $(seq 1 150); do
    for c in A-current B-auto C-small D-sub; do
      $LAB protocol $c $b $rep >> $OUT/fast.csv 2>/dev/null
    done
  done
  say "  fast budget $b done"
done

# --- arm 2: does the answer travel? ----------------------------------------
# Six workloads spanning the ways a benchmark can be hard: multiplicative
# noise, additive noise, pure ALU, memory latency, an allocating input-
# sensitive sort that is forced to n=1, and a parse. Larger budgets only,
# since a six-workload round costs six times as much per round.
say "arm general: 6 workloads, 4 budgets, 4 cells, 100 reps"
echo "$HDR" > $OUT/general.csv
for b in 0.3 1 3 10; do
  for rep in $(seq 1 100); do
    for c in A-current B-auto C-small D-sub; do
      LAB_PROTO_SET=instant_now,btree_miss,cpu_canary,mem_canary,slice_sort,parse_u64 \
        $LAB protocol $c $b $rep >> $OUT/general.csv 2>/dev/null
    done
  done
  say "  general budget $b done"
done

# --- arm 3: the slow regime, across the crossover --------------------------
# One workload, six durations, from well below the ~1ms tick crossover to
# well above it. Every cell should collapse to the same thing once a single
# call outruns the sample target - if the protocols disagree here, the
# forced-n1 detection is not doing its job.
say "arm slow: slow_cpu at 6 durations, 4 cells, 60 reps"
echo "iters,$HDR" > $OUT/slow.csv
#          ~30us    ~100us   ~300us   ~1ms      ~3ms      ~10ms
for it in 100000 340000 1000000 3400000 10000000 34000000; do
  b=$(python3 -c "print(max(0.05, $it/1.7e9*300))")
  for rep in $(seq 1 60); do
    for c in A-current B-auto C-small D-sub; do
      LAB_SLOW_ITERS=$it LAB_PROTO_SET=slow_cpu,cpu_canary \
        $LAB protocol $c $b $rep 2>/dev/null | sed "s/^/$it,/" >> $OUT/slow.csv
    done
  done
  say "  slow iters=$it (budget ${b}s) done"
done

# --- arm 4: composition varying between repeats ----------------------------
say "arm vary: as fast, but one workload dropped at random each repeat"
echo "$HDR" > $OUT/vary.csv
for b in 0.01 0.03 0.1 0.3 1 3; do
  for rep in $(seq 1 150); do
    for c in A-current B-auto C-small D-sub; do
      LAB_PROTO_DROP=1 $LAB protocol $c $b $rep >> $OUT/vary.csv 2>/dev/null
    done
  done
  say "  vary budget $b done"
done

say "all arms finished"
wc -l $OUT/*.csv
