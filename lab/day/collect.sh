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
#   f64_sin        34ns   9 rungs      1.0us/round
#   btree_miss    517ns   6 rungs      2.3us/round
#   urandom_read 19.1us   2 rungs     25.9us/round
#   str_find      2.2ms   2 rungs      2.91ms/round
#   copy_64mb     6.4ms   2 rungs      8.55ms/round
#
# The first two are where the measurement problem lives: a 370ns fixed cost
# is a large fraction of a 34ns iteration, so rung choice, subtraction and
# calibration all matter, and there are six to nine rungs to choose between.
# The last three have one rung and no choice; they are here because a round
# has to contain the neighbours a user would really have, and copy_64mb is
# the one most likely to move someone else's number.
#
# Five payloads, not seven: the powerset is exponential, and the slow ones
# compound it by setting the round period for everyone sharing the round.
ALL="f64_sin,btree_miss,urandom_read,str_find,copy_64mb"

# Every composition: does a workload's number move with the company it
# keeps? 31 subsets, and nothing else answers it. Cheapest compositions run
# first, so stopping this early costs the expensive ones rather than a
# random half of everything.
quiet-bench run "$BIN" collect "${1:-20h}" day/collect/powerset "$ALL"

# One composition, sampled hard: how an algorithm behaves in the round a
# user would actually get.
LAB_SUBSETS=full quiet-bench run "$BIN" collect "${2:-3h}" day/collect/deep "$ALL"
echo "collection done"
