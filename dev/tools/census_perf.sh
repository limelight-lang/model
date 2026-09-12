#!/bin/bash
# The hardware arm of PLAN.md S40.3: every load of the census through the
# driver in benches/, pinned to one CPU under `perf stat --control`, twice
# per cell (A, then A again as the control), plus the empty interval.
#
# Usage: dev/tools/census_perf.sh <out.csv> [cpu]
# Build first: cargo bench --no-run --features bench-loads --bench census_driver
set -eu
OUT=$1
CPU=${2:-2}
PERF=${PERF:-perf}
BIN=$(ls -t target/release/deps/census_driver-* | grep -v '\.d$' | head -1)
EVENTS=instructions:u,cycles:u,L1-dcache-load-misses:u,dTLB-load-misses:u,branch-misses:u,cache-misses:u
WORK=$(mktemp -d)
mkfifo "$WORK/ctl" "$WORK/ack"

run_cell() {
    local load=$1 collections=$2 run=$3
    taskset -c "$CPU" "$PERF" stat -x, -D -1 --control="fifo:$WORK/ctl,$WORK/ack" -e "$EVENTS" \
        -o "$WORK/stat" -- "$BIN" "$load" --collections "$collections" --control "$WORK/ctl" "$WORK/ack" \
        > "$WORK/driver" 2>&1
    # perf's CSV fields: value, unit, event, run time in nanoseconds, and the
    # share of the interval the counter ran — 100 unless perf multiplexed the
    # events, which a reading must not be quoted under.
    grep -v '^#' "$WORK/stat" | grep -v '^$' | while IFS=, read -r value unit event runtime running rest; do
        echo "$load,$collections,$run,$event,$value,$running"
    done >> "$OUT"
    grep census_driver "$WORK/driver" | sed "s/^/# /" >> "$OUT"
}

echo "load,collections,run,event,value,running" > "$OUT"
for run in 1 2 3; do run_cell dense:256:381 0 "empty$run"; done
LOADS="dense:32:2 dense:32:16 dense:32:256 dense:32:381 \
dense:64:2 dense:64:16 dense:64:256 dense:64:381 \
dense:128:2 dense:128:16 dense:128:256 dense:128:381 \
dense:256:2 dense:256:16 dense:256:256 dense:256:381 \
sparse:256:2 sparse:256:16 sparse:256:256 sparse:256:381 \
retained:256:2 retained:256:16 retained:256:256 retained:256:381 \
group:256:32:0 group:256:32:7 full:256 full:32 edge2:256:381:0 edge2:256:381:254"
for load in $LOADS; do
    run_cell "$load" 8 A1
    run_cell "$load" 8 A2
done
# The retained ring is traced whole at the two collections after the warm-up
# and pruned at the entry from the fourth on, so its eight-collection cell
# mixes the two; the two-collection cell reads the full trace alone, and the
# difference over six is the pruned collection.
for load in retained:256:2 retained:256:16 retained:256:256 retained:256:381; do
    run_cell "$load" 2 A1
    run_cell "$load" 2 A2
done
rm -rf "$WORK"
