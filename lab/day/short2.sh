#!/bin/bash
# Does subtraction let the sample be short enough to pay for itself?
set -u
cd "$(dirname "$0")/.."
LAB="quiet-bench run ./target/release/lab"
OUT=day/proto
quiet-bench run true 2>/dev/null || { echo "no reservation"; exit 1; }
echo "cell,budget_s,rep,workload,estimate,se_naive,se_batch,half1,half2,samples,rounds,calib_s,total_s,note" > $OUT/short2.csv
for b in 0.003 0.01 0.03 0.1 0.3 1 3 10; do
  for rep in $(seq 1 150); do
    LAB_PROTO_DUMP=$OUT/raw/short2 $LAB protocol F-short2 $b $rep >> $OUT/short2.csv 2>/dev/null
  done
  echo "=== $(date '+%H:%M:%S') budget $b done"
done
echo "=== done"
