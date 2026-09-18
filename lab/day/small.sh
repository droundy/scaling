#!/bin/bash
# How small can a sample be before it stops paying?
#
# Last night said per-sample *relative* noise barely depends on batch
# duration - `instant_now` sat at 1.0-1.7% and `mem_canary` at ~14% across a
# 64x range - so a long batch buys no precision per sample while costing
# proportionally more time. Going from 120us to 15us samples bought a 3x
# better error per unit of machine time, which is ~9x less time for equal
# precision. The reason nobody could take that deal before is the intercept:
# at 15us a 300ns fixed cost is a 2% bias, far above the achievable noise.
# Last night also showed the subtraction removes it (0.84% -> 0.10% of
# composition sensitivity on a quiet machine), so the deal is now available.
#
# This sweep asks where it stops. Eleven rungs from 125ns to 128us - a 1000x
# span, the bottom two of which are batches of a few hundred nanoseconds,
# where the ~45ns of timer overhead is a tenth of the measurement. Somewhere
# in there the curve must turn around.
#
# Every subset, six passes, because the second question is whether the
# *composition* effect survives shrinking: neighbours add 300-570ns per
# measurement, which is 0.3% of a 100us sample and 200% of a 250ns one. If
# that cost is genuinely fixed the subtraction still removes it. If it grows
# as the batch shrinks, small samples are off the table whatever the noise
# says.
#
# No `copy_64mb` or `slice_sort`: both already cost more than a sample for a
# single iteration, so they have no small end to explore, and `copy_64mb` in
# 32 subsets would eat the week.
set -u
cd "$(dirname "$0")/.."
LAB=./target/release/lab
OUT=day

say() { echo "=== $(date '+%F %T')  $*" ; }

say "small: 6 workloads, 63 subsets, 11 rungs 125ns..128us, 6 passes x 200k"
LAB_LADDER=0.00125,0.0025,0.005,0.01,0.02,0.04,0.08,0.16,0.32,0.64,1.28 \
LAB_PASSES=6 \
  $LAB ladder 200000 $OUT/small \
  "nothing,cpu_canary,mem_canary,instant_now,btree_miss,mpsc_send" \
  > $OUT/small.log 2>&1

say "small sweep finished"
