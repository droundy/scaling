#!/bin/sh
# Collect the recordings that every analysis replays from.
#
# One runner, one parameter each: how long to spend. Nothing here chooses a
# rung, tests for convergence, or lets a workload leave the round early -
# see `collect` in src/main.rs for why each of those would spoil the data.
set -e
cd "$(dirname "$0")/.."

# Refuse to run unpinned. A timing taken on a housekeeping core, or worse an
# E-core, is not a noisier measurement of the same thing - it is a
# measurement of something else.
quiet-bench run true

# Payloads only: the two canaries are in every round structurally, and
# naming one here would measure it twice under the same name.
#
# Five and four, so both stay under the powerset threshold and every
# composition gets measured - which is the only way to ask whether a
# workload's number moves with the company it keeps.
FAST="instant_now,parse_u64,f64_sin,btree_miss,mpsc_send"
SLOW="slice_sort,str_find,copy_64mb,urandom_read"

# Split fast from slow because a round costs ~150us for one and ~13ms for
# the other; a shared budget would spend the night on copy_64mb.
quiet-bench run ./target/release/lab collect "${1:-1h}" day/collect/fast "$FAST"
quiet-bench run ./target/release/lab collect "${2:-1h}" day/collect/slow "$SLOW"
echo "collection done"
