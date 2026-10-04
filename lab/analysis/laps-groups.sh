#!/bin/sh
# Group-shaped rounds, like the crate's: each round runs groups contiguously (groups in a fresh random
# order, members likewise), so a pair shares a short group round with other groups' work between.
# Suites c0, c5 x lap configurations (below) x two passes. Run from lab/ unquiesced, under the shared lock.
BIN=${BIN:-target/release/lab}
# The shared timing lock. /run/quiet-bench.cpus exists only while quiet-bench holds a reservation,
# and only root can create it, so unquiet runs use a lock anyone can create.
LOCK=${LOCK:-/tmp/scaling-timing.lock}
OUT=${OUT:-day/collect/laps-groups}
export LAB_FP_HARNESS=1 LAB_SUBSETS=full LAB_PASSES=1 LAB_CAP_S=100000
export LAB_SLOW_ITERS=100 LAB_SLOW2_ITERS=64 LAB_SLOW3_ITERS=3810000 LAB_SLOW4_ITERS=2286000
export LAB_GROUPS="slow_cpu,slow_cpu2,cpu_canary;f64_sin,parse_u64;btree_miss,warm_dst;slow_cpu3,slow_cpu4;fp_heavy,mem_canary,warm_src"
BASE=slow_cpu,slow_cpu2,f64_sin,parse_u64,btree_miss,warm_dst
comp() {
  case $1 in
    0) echo $BASE ;;
    5) echo $BASE,slow_cpu3,slow_cpu4,fp_heavy,mem_canary,warm_src ;;
  esac
}
# Lap configurations: sN = shape 1,1,9 (warm-up, short, long) with an N ms unit, so a sample is 11 units;
# e10 = four equal 10 ms laps, for comparison with the quiet schedule.
cfg() {
  case $1 in
    s0.3) echo "60s LAB_LAP_SHAPE=1,1,9 LAB_LAP_NS=300000" ;;
    s1)   echo "90s LAB_LAP_SHAPE=1,1,9 LAB_LAP_NS=1000000" ;;
    s3)   echo "180s LAB_LAP_SHAPE=1,1,9 LAB_LAP_NS=3000000" ;;
    e10)  echo "180s LAB_LAP=4 LAB_LAP_NS=10000000" ;;
  esac
}
for pass in 0 1; do
  for c in 0 5; do for l in s0.3 s1 s3 e10; do echo "$c $l"; done; done | shuf > /tmp/lapg-order-$$-$pass
  while read c l; do
    set -- $(cfg $l); d=$1; shift
    echo "pass $pass comp $c lap $l start $(date +%T)"
    env "$@" flock $LOCK $BIN collect $d $OUT/$l/c$c/p$pass $(comp $c) 2>&1 | grep -E "rounds in"
  done < /tmp/lapg-order-$$-$pass
done
echo "all done $(date +%T)"
