#!/bin/sh
# Collect the recordings that every analysis replays from.
#
# Usage: day/collect.sh [powerset-budget] [deep-budget]
set -e
cd "$(dirname "$0")/.."

# Refuse to run unpinned. A timing taken on a housekeeping core, or worse an
# E-core, is not a noisier measurement of the same thing - it is a
# measurement of something else.
quiet-bench run true

# Run a *copy* of the binary, so the tree can be rebuilt while a collection
# is in flight. Cargo replaces an executable by rename, so a running process
# keeps its own inode - but the next invocation in this script would pick up
# whatever had just been compiled, and a recording half-written by one build
# and half by another is not a recording of anything.
mkdir -p day
BIN=day/lab-running
cp target/release/lab "$BIN"

# Payloads only: cpu_canary is in every round structurally, and naming it
# here would measure it twice under one name.
#
#   instant_now   32ns    9 rungs
#   f64_sin       34ns    9 rungs
#   btree_miss   501ns    6 rungs
#   urandom_read  19us    2 rungs
#   thread_spawn 192us    2 rungs
#   str_find     2.2ms    2 rungs
#   copy_64mb    6.6ms    2 rungs
#
# The first three are where the measurement problem lives - a 370ns fixed
# cost is a large fraction of a 32ns iteration, so rung choice, subtraction
# and calibration all matter. The rest have no rung to choose and are here
# to confirm nothing breaks on the easy case, and because copy_64mb is the
# neighbour most likely to move someone else's number.
ALL="instant_now,f64_sin,btree_miss,urandom_read,thread_spawn,str_find,copy_64mb"

# Every composition: does a workload's number move with the company it
# keeps? 127 subsets, and nothing else answers it.
quiet-bench run "$BIN" collect "${1:-8h}" day/collect/powerset "$ALL"

# One composition, sampled hard: how an algorithm behaves in the round a
# user would actually get.
LAB_SUBSETS=full quiet-bench run "$BIN" collect "${2:-2h}" day/collect/deep "$ALL"
echo "collection done"
