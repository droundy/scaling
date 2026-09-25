#!/bin/bash
# One night of quiet data, aimed at the intercept and the slope.
#
# Ordered most-important-first, so a night that gets cut short still answers
# the questions that matter most. Every sweep calibrates once and shares the
# counts, runs each subset as a contiguous block, and repeats the whole
# powerset several times in a fresh random order - see `sweep()` in main.rs
# and PROBLEMS.md for why each of those is the way it is.
#
# LAB_LADDER is in **multiples of SAMPLE (100us)**, not iteration counts, so
# a slow workload gets 1,2,3,4 iterations where a fast one gets a geometric
# ladder. See `counts_for`.
#
# Machine must be quiet: performance governor, no_turbo=1.
set -u
cd "$(dirname "$0")/.."
LAB=./target/release/lab
OUT=night

say() { echo "=== $(date '+%F %T')  $*" ; }

# --- B: the shape of the fixed cost -----------------------------------------
# Seven rungs over a 64x range of *durations*, the smallest an eighth of a
# sample. The fixed cost shows up as a/n per iteration, so it is most visible
# in the shortest batch - and short batches are the cheap ones, so this whole
# sweep costs less per measurement than a 1,2,4 ladder would.
#
# This is the sweep that says what kind of cost we have: drops that halve
# each doubling are a constant intercept, drops that stay equal are a warm-up
# that never stops, and flat is no fixed cost at all.
#
# Four cheap workloads only. Not because the others matter less, but because
# this sweep's value is *time resolution* - a 12us bottom rung - and nothing
# that costs milliseconds per iteration can have one at any price. The big
# workloads get their shape from sweep A's four rungs instead.
say "B shape: 4 workloads, 7 rungs over 64x, 3 passes x 80k"
LAB_LADDER=0.125,0.25,0.5,1,2,4,8 LAB_PASSES=3 \
  $LAB ladder 80000 $OUT/shape "cpu_canary,mem_canary,btree_miss,instant_now" \
  > $OUT/shape.log 2>&1

# --- D: where the fixed cost comes from -------------------------------------
# Untimed iterations before each timed batch. Harness overhead - the clock
# reads, the boxed call - is paid per measurement no matter what ran first, so
# a prefix cannot touch it. A cold start is paid outside the timer instead.
# Whatever decays across these runs is warm-up; whatever survives is harness.
for k in 0 1 4 16 64 256 1024; do
  say "D warmup=$k: 3 workloads, 3 passes x 30k"
  LAB_WARMUP=$k LAB_LADDER=0.5,1,2,4 LAB_PASSES=3 \
    $LAB ladder 30000 $OUT/warm$k "btree_miss,mem_canary,instant_now" \
    > $OUT/warm$k.log 2>&1
done

# --- A: does the number move with the company it keeps ----------------------
# All eight workloads, every subset, 255 of them, three passes.
#
# `copy_64mb` is in here rather than off in a corner of its own, and it is
# arguably the most important member: it is the one most likely to change
# what happens to everything else in the round, which is the entire question.
# Excluding the biggest disruptor from the disruption experiment would have
# been a poor trade for a shorter night. At a true 6.4 ms an iteration - not
# the 47 ms a mis-calibrated first touch once claimed - it is affordable so
# long as the round count stays modest, which is the trade made here: 255
# subsets at 2000 rounds rather than 63 at 10000.
#
# `nothing` and `cpu_canary` are the discriminating pair for the mechanism -
# both occupy a slot and take time, only one touches memory.
say "A composition: 8 workloads, 255 subsets, 3 passes x 2k"
LAB_LADDER=0.5,1,2,4 LAB_PASSES=3 \
  $LAB ladder 2000 $OUT/comp \
  "nothing,cpu_canary,mem_canary,instant_now,btree_miss,mpsc_send,slice_sort,copy_64mb" \
  > $OUT/comp.log 2>&1

say "all sweeps finished"
