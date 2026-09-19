#!/bin/bash
# Final validation: the algorithm (G) against today's protocol (A) and the
# workflow as actually used (E-stop), across every workload the lab has.
set -u
cd "$(dirname "$0")/.."
LAB="quiet-bench run ./target/release/lab"
OUT=day/proto
WL=nothing,instant_now,f64_sin,btree_miss,parse_u64,str_find,slice_sort,mpsc_send,urandom_read,cpu_canary,mem_canary
quiet-bench run true 2>/dev/null || { echo "no reservation"; exit 1; }
echo "cell,budget_s,rep,workload,estimate,se_naive,se_batch,half1,half2,samples,rounds,calib_s,total_s,note" > $OUT/final.csv
for b in 0.3 1 3 10; do
  for rep in $(seq 1 100); do
    for c in A-current E-stop G-algo; do
      LAB_PROTO_SET=$WL LAB_PROTO_DUMP=$OUT/raw/final $LAB protocol $c $b $rep >> $OUT/final.csv 2>/dev/null
    done
  done
  echo "=== $(date '+%H:%M:%S') budget $b done"
done
echo "=== done"
