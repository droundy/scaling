#!/bin/sh
# Collect the recordings that every analysis replays from.
#
# Usage: day/collect.sh <powerset-budget> <deep-budget>
set -e
cd "$(dirname "$0")/.."

# Refuse to run unpinned. A timing taken on a housekeeping core, or worse an
# E-core, is not a noisier measurement of the same thing - it is a
# measurement of something else.
quiet-bench run true

# Run a *copy* of the binary, so the tree can be rebuilt while a collection
# is in flight. Cargo replaces the executable by rename, so a running process
# keeps its own inode - but the next invocation in this script would pick up
# whatever had just been compiled, and a recording half-written by one build
# and half by another is not a recording of anything.
mkdir -p day
BIN=day/lab-running
cp target/release/lab "$BIN"
"$BIN" --version 2>/dev/null || true

# Payloads only: the two canaries are in every round structurally, and
# naming one here would measure it twice under the same name.
FAST="instant_now,parse_u64,f64_sin,btree_miss,mpsc_send,urandom_read,slice_sort"
ALL="$FAST,str_find,copy_64mb"

# Two experiments, because they want opposite things from the same machine.
#
# The powerset asks whether a workload's number moves with the company it
# keeps. That needs many compositions and only modest depth in each - enough
# to see a percent-scale shift in a mean, not enough to replay a whole
# algorithm.
quiet-bench run "$BIN" collect "${1:-4h}" day/collect/powerset "$FAST"

# The deep run asks how an algorithm behaves, which needs one realistic
# composition sampled hard. Every workload, including the slow ones, so the
# round is the round a user would actually get.
LAB_SUBSETS=full quiet-bench run "$BIN" collect "${2:-2h}" day/collect/deep "$ALL"
echo "collection done"
