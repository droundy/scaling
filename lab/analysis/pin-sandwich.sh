#!/bin/sh
# Migration against sandwiching, unquiesced with turbo: the c0 group schedule at the s3 shape
# (1,1,9 with 3 ms units), pinned to one P-core or not, with the canary sandwiched between each
# group member or not. Two passes, shuffled. Run from lab/ under the shared lock.
BIN=${BIN:-target/release/lab}
OUT=${OUT:-day/collect/pin-sandwich}
LOCK=${LOCK:-/tmp/scaling-timing.lock}
CORE=${CORE:-4}
export LAB_FP_HARNESS=1 LAB_SUBSETS=full LAB_PASSES=1 LAB_CAP_S=100000
export LAB_SLOW_ITERS=100 LAB_SLOW2_ITERS=64
export LAB_LAP_SHAPE=1,1,9 LAB_LAP_NS=3000000
export LAB_GROUPS="slow_cpu,slow_cpu2,cpu_canary;f64_sin,parse_u64;btree_miss,warm_dst"
SET=slow_cpu,slow_cpu2,f64_sin,parse_u64,btree_miss,warm_dst
for pass in 0 1; do
  for v in plain-free plain-pin sand-free sand-pin; do echo $v; done | shuf > /tmp/pinsand-order-$$-$pass
  while read v; do
    case $v in *pin) pin="taskset -c $CORE" ;; *) pin="" ;; esac
    case $v in sand*) sand="LAB_SANDWICH=cpu_canary" ;; *) sand="LAB_SANDWICH=" ;; esac
    echo "pass $pass $v start $(date +%T)"
    env $sand flock $LOCK $pin $BIN collect 120s $OUT/$v/p$pass $SET 2>&1 | grep -E "rounds in"
  done < /tmp/pinsand-order-$$-$pass
done
echo "all done $(date +%T)"
