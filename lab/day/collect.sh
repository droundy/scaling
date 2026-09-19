#!/bin/sh
# Collect the recordings that every analysis replays from.
#
# One runner, one parameter: how long to spend. Nothing here chooses a rung,
# tests for convergence, or lets a workload leave the round early - see
# `collect` in src/main.rs for why each of those would spoil the recording.
set -e
cd "$(dirname "$0")/.."

# Refuse to run unpinned. A timing taken on a housekeeping core, or worse an
# E-core, is not a noisier measurement of the same thing - it is a
# measurement of something else.
quiet-bench run true

FAST="cpu_canary,mem_canary,instant_now,parse_u64,f64_sin,btree_miss,mpsc_send"
SLOW="cpu_canary,slice_sort,str_find,copy_64mb,urandom_read"

# Split fast from slow because a round costs 2ms for one and 40ms for the
# other, so a shared budget would spend the night on copy_64mb.
quiet-bench run ./target/release/lab collect "${1:-30m}" day/collect/fast "$FAST"
quiet-bench run ./target/release/lab collect "${2:-30m}" day/collect/slow "$SLOW"
