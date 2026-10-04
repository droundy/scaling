#!/bin/sh
# Group-shaped rounds, like the crate's: each round runs groups contiguously (groups in a fresh random
# order, members likewise), so a pair shares a short group round with other groups' work between.
# Suites c0, c5 x laps 1, 3, 10, 30 ms x two passes. Run from lab/ unquiesced, under the shared lock.
BIN=${BIN:-target/release/lab}
OUT=${OUT:-day/collect/laps-groups}
export LAB_LAP=4 LAB_FP_HARNESS=1 LAB_SUBSETS=full LAB_PASSES=1 LAB_CAP_S=100000
export LAB_SLOW_ITERS=100 LAB_SLOW2_ITERS=64 LAB_SLOW3_ITERS=3810000 LAB_SLOW4_ITERS=2286000
export LAB_GROUPS="slow_cpu,slow_cpu2,cpu_canary;f64_sin,parse_u64;btree_miss,warm_dst;slow_cpu3,slow_cpu4;fp_heavy,mem_canary,warm_src"
BASE=slow_cpu,slow_cpu2,f64_sin,parse_u64,btree_miss,warm_dst
comp() {
  case $1 in
    0) echo $BASE ;;
    5) echo $BASE,slow_cpu3,slow_cpu4,fp_heavy,mem_canary,warm_src ;;
  esac
}
dur() {
  case $1 in 1) echo 45s ;; 3) echo 60s ;; 10) echo 120s ;; 30) echo 240s ;; esac
}
for pass in 0 1; do
  for c in 0 5; do for l in 1 3 10 30; do echo "$c $l"; done; done | shuf > /tmp/lapg-order-$$-$pass
  while read c l; do
    echo "pass $pass comp $c lap ${l}ms start $(date +%T)"
    LAB_LAP_NS=$((l * 1000000)) flock /run/quiet-bench.cpus $BIN collect $(dur $l) $OUT/l$l/c$c/p$pass $(comp $c) 2>&1 | grep -E "rounds in"
  done < /tmp/lapg-order-$$-$pass
done
echo "all done $(date +%T)"
