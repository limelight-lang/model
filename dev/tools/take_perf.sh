#!/bin/bash
# The hardware arm of the take's cost (`dev/BENCHMARKS.md`, "S64.5 what a
# take costs by the shape of its roots"): every arm of the probe in
# `cycle::worker::tests::what_a_take_costs`, one process per arm, pinned to
# one CPU under `perf stat --control`, the counting interval opened by the
# probe around its timed collection alone.
#
# Usage: dev/tools/take_perf.sh <out.csv> [cpu] [runs]
# Build first: cargo test --release --lib --no-run
set -eu
OUT=$1
CPU=${2:-2}
RUNS=${3:-2}
PERF=${PERF:-perf}
BIN=$(ls -t target/release/deps/ll_model-* | grep -vE '\.(d|ll|bc)$' | head -1)
CASE=cycle::worker::tests::what_a_take_costs::what_a_take_costs_by_the_shape_of_its_roots
EVENTS=instructions:u,cycles:u,L1-dcache-load-misses:u,branch-misses:u
WORK=$(mktemp -d)
mkfifo "$WORK/ctl" "$WORK/ack"

run_arm() {
    local arm=$1 run=$2
    LL_PERF_CTL="$WORK/ctl" LL_PERF_ACK="$WORK/ack" LL_TAKE_ARM="$arm" \
        taskset -c "$CPU" "$PERF" stat -x, -D -1 --control="fifo:$WORK/ctl,$WORK/ack" \
        -e "$EVENTS" -o "$WORK/stat" \
        -- "$BIN" --ignored --exact --test-threads=1 --nocapture "$CASE" \
        > "$WORK/probe" 2>&1
    # perf's CSV fields: value, unit, event, run time in nanoseconds, and the
    # share of the interval the counter ran — 100 unless perf multiplexed the
    # events, which a reading must not be quoted under.
    grep -v '^#' "$WORK/stat" | grep -v '^$' | while IFS=, read -r value unit event runtime running rest; do
        echo "$arm,$run,$event,$value,$running"
    done >> "$OUT"
    grep -E '^(take|baseline|control) over' "$WORK/probe" | sed "s/^/# /" >> "$OUT"
}

echo "arm,run,event,value,running" > "$OUT"
for run in $(seq 1 "$RUNS"); do
    for arm in overlapping:take overlapping:baseline overlapping:control \
               disjoint:take disjoint:baseline disjoint:control \
               overlapping-live:take overlapping-live:baseline overlapping-live:control \
               disjoint-live:take disjoint-live:baseline disjoint-live:control \
               mixed-0:take mixed-0:baseline mixed-0:control \
               mixed-16:take mixed-16:baseline mixed-16:control \
               mixed-32:take mixed-32:baseline mixed-32:control \
               mixed-47:take mixed-47:baseline mixed-47:control \
               mixed-63:take mixed-63:baseline mixed-63:control; do
        run_arm "$arm" "$run"
    done
done

rm -rf "$WORK"
