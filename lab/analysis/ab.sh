#!/bin/sh
# Context (A) against long samples (B1/B3/B10) against keep-alive + probe (C), in six compositions,
# two passes, shuffled within each pass. Run from lab/ with CPU 2 reserved. BIN is the build to use.
# Known answers: slow_cpu3/slow_cpu4 = 3810000/2286000 links, exactly 5/3, since the per-call overhead is
# negligible at that length. slow_cpu/slow_cpu2 = 100/64 links is NOT exact: each call also pays a fixed
# 2.8-3.8 ns (1.2-1.6 links), so the ratio is about 1.54. Score it as R(slow_cpu/canary) - R(slow_cpu2/canary)
# = 36 links (good to 0.1-0.3%), or add slow_cpu5 at 136 links and use (t136 - t100)/(t100 - t64) = 1.
BIN=${BIN:-target/release/lab}
OUT=${OUT:-day/collect/ab}
export LAB_RUNGS=top2 LAB_SUBSETS=full LAB_PASSES=1 LAB_CAP_S=100000
export LAB_SLOW_ITERS=100 LAB_SLOW2_ITERS=64 LAB_SLOW3_ITERS=3810000 LAB_SLOW4_ITERS=2286000
BASE=slow_cpu,slow_cpu2,f64_sin,parse_u64,btree_miss,warm_dst
comp() {
  case $1 in
    0) echo $BASE ;;
    1) echo $BASE,slow_cpu3,slow_cpu4 ;;
    2) echo $BASE,fp_heavy ;;
    3) echo $BASE,mem_canary ;;
    4) echo $BASE,warm_src ;;
    5) echo $BASE,slow_cpu3,slow_cpu4,fp_heavy,mem_canary,warm_src ;;
  esac
}
cand() {
  case $1 in
    A)   echo "60s LAB_FP_HARNESS=1" ;;
    C)   echo "60s LAB_VPROBE=1 LAB_VPROBE_MIN_US=25" ;;
    B1)  echo "90s LAB_FP_HARNESS=1 LAB_RUNG_MAX_NS=2000000" ;;
    B3)  echo "120s LAB_FP_HARNESS=1 LAB_RUNG_MAX_NS=6000000" ;;
    B10) echo "240s LAB_FP_HARNESS=1 LAB_RUNG_MAX_NS=20000000" ;;
  esac
}
for pass in 0 1; do
  jobs=$(for c in 0 1 2 3 4 5; do for k in A C B1 B3 B10; do echo "$c $k"; done; done | shuf --random-source=<(yes $pass))
  echo "$jobs" | while read c k; do
    set -- $(cand $k); dur=$1; shift
    echo "pass $pass comp $c cand $k start $(date +%T)"
    env "$@" quiet-bench run $BIN collect $dur $OUT/$k/c$c/p$pass $(comp $c) 2>&1 | grep -E "rounds in|probe waits"
  done
done
echo "all done $(date +%T)"
