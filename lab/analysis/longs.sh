#!/bin/sh
# Long samples against a known answer: slow_cpu and slow_cpu2 are the same chain, so their
# true ratio is exactly the link-count ratio, 1270000/762000 = 5/3. Rungs are 1 and 2 calls.
# 1,270,000 links take about 3 ms at 1.7 GHz, so K=3 is a 9 ms slow_cpu call and 5.4 ms slow_cpu2.
# FP: the old f64-converting harness (a build before 4b78976). INT: integer harness with LAB_VPROBE.
# Run from lab/ with CPU 2 reserved. FP and INT name the two builds.
FP=${FP:?path to an old-harness build}; INT=${INT:-target/release/lab}
export LAB_RUNGS=top2 LAB_SUBSETS=full LAB_PASSES=1 LAB_CAP_S=100000
for k in 3 5 10 20; do
  n=$((k * 1270000)); m=$((k * 762000)); ms=$((k * 3))
  LAB_SLOW_ITERS=$n LAB_SLOW2_ITERS=$m quiet-bench run $FP collect 60s day/collect/long/long-fp-${ms}ms slow_cpu,slow_cpu2
  LAB_VPROBE=1 LAB_SLOW_ITERS=$n LAB_SLOW2_ITERS=$m quiet-bench run $INT collect 60s day/collect/long/long-int-${ms}ms slow_cpu,slow_cpu2
done
