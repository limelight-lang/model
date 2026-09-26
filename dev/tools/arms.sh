#!/bin/bash
# The arms of one comparison on the rig (`dev/BENCHMARKS.md`, "S65.24 A, B and
# C on a box with a PMU"): every arm a test binary of its own in ARMS_DIR,
# interleaved inside each repeat in an order rotated by one arm a repeat, one
# process a cell, the rig's line with the arm and the repeat in front. The deciding loads are paced as the S65.24
# protocol paces them; the guards run unpaced and birth no collector.
#
# Usage: dev/tools/arms.sh <out.csv> <repeats> <deciding|guards>
# Env: ARMS_DIR (binaries named by arm, default target/arms), ARMS ("A B C"),
#      MUTATORS (2,4), SPARE (6), SHARED (4): CPUs for the two placements.
# Build each arm with `cargo test --release --lib --no-run [--features …]` and
# copy the binary into ARMS_DIR under the arm's name.
set -u
OUT=$1
REPEATS=$2
PHASE=$3
ARMS_DIR=${ARMS_DIR:-target/arms}
ARMS=${ARMS:-"A B C"}
MUTATORS=${MUTATORS:-2,4}
SPARE=${SPARE:-6}
SHARED=${SHARED:-4}
CASE=cycle::worker::tests::the_rig::a_cell_of_the_rig
LOG=$(mktemp)
trap 'rm -f "$LOG"' EXIT
HEADED=$( [ -s "$OUT" ] && echo 1 || echo "")

cell() {
    local arm=$1 placement=$2 load=$3 pace=$4 seconds=$5 drain=$6 repeat=$7
    local collector=$SPARE
    [ "$placement" = shared-core ] && collector=$SHARED
    LL_RIG_PLACEMENT=$placement LL_RIG_LOAD=$load LL_RIG_MUTATOR_CPUS=$MUTATORS \
        LL_RIG_COLLECTOR_CPUS=$collector LL_RIG_CAP=1 LL_RIG_SECONDS=$seconds \
        LL_RIG_PACE_MS=$pace LL_RIG_DRAIN_MS=$drain \
        timeout 400 "$ARMS_DIR/$arm" --ignored --exact --test-threads=1 --nocapture "$CASE" \
        > "$LOG" 2>&1
    if ! grep -q '1 passed' "$LOG"; then
        echo "FAILED $arm $placement $load $repeat" >&2
        tail -5 "$LOG" >&2
        return
    fi
    if [ -z "$HEADED" ]; then
        echo "arm,repeat,$(grep -o 'rig-header,.*' "$LOG" | cut -d, -f2-)" > "$OUT"
        HEADED=1
    fi
    echo "$arm,$repeat,$(grep -o '\brig,.*' "$LOG" | cut -d, -f2-)" >> "$OUT"
}

# The arms in the order repeat `$1` runs them: the list rotated left by
# `$1 - 1`, so that no arm always runs first after a cell of another's.
rotated() {
    local -a arms=($ARMS)
    local count=${#arms[@]} shift=$(( ($1 - 1) % ${#arms[@]} )) index
    for (( index = 0; index < count; index++ )); do
        echo "${arms[(index + shift) % count]}"
    done
}

for repeat in $(seq 1 "$REPEATS"); do
    ORDER=$(rotated "$repeat")
    for placement in spare-core shared-core; do
        if [ "$PHASE" = deciding ]; then
            for spec in live-churn:1 live-churn-dies-by-count:1 deferred-then-dead:1 \
                        deferred-live-large:1 garbage-25:15 registered-ring-live:15; do
                for arm in $ORDER; do
                    cell "$arm" "$placement" "${spec%%:*}" "${spec##*:}" 10 12000 "$repeat"
                done
            done
        else
            for load in garbage-0 overlapping-live disjoint-live large-live-core; do
                for arm in $ORDER; do
                    cell "$arm" "$placement" "$load" 0 3 1000 "$repeat"
                done
            done
        fi
    done
done
