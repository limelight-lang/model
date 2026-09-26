#!/bin/bash
# The rig's driver: every cell of `cycle::worker::tests::the_rig`, a mode, a
# mutator count and a load, in a process of its own, into one CSV line each.
# The modes are the three arms of the Sage's report (`model`'s
# `dev/CYCLE-SPLIT-SAGE-REPORT.md`, deleted in c7f9e56, "The arm that would
# prove the loss"), each at C-1, C and C+1 mutators, so that the three
# placements of `dev/S64-GC-IMPROVEMENT-ANALYSIS.md`, "Какие опыты нужны", are
# three of its cells and every mode is read at the same mutator count. At C-1
# mutators core C carries none, so `shared-core` there is a second
# `spare-core`:
#
#   spare-core   the collector alone on core C+1, cap 1
#   shared-core  the collector on core C, a mutator's, cap 1
#   cap-zero     cap 0; the elder, which still rounds and asks, on core C
#
# The mutators are laid over cores 1..C in turn, so C+1 of them put two on
# core 1.
#
# The topology is read from /sys/devices/system/cpu: one logical CPU stands
# for each physical core, the lowest-numbered of its SMT siblings, and the
# siblings are left idle and named in the CSV's head. The cores are taken in
# order from core FIRST_CORE, 1 unless the environment says otherwise.
#
# Usage: dev/tools/rig.sh <out.csv> [seconds] [cores]
# Build first: cargo test --release --lib --no-run
set -eu
OUT=$1
RUN_SECONDS=${2:-10}
CORES=${3:-4}
FIRST_CORE=${FIRST_CORE:-1}
BIN=$(ls -t target/release/deps/ll_model-* | grep -vE '\.(d|ll|bc)$' | head -1)
CASE=cycle::worker::tests::the_rig::a_cell_of_the_rig
# `LOADS` in `src/cycle/worker/tests/the_rig.rs`, by name: change one, change
# the other. A `LOADS` set in the environment runs those loads alone.
LOADS=${LOADS:-"garbage-0 garbage-25 garbage-50 garbage-75 garbage-100 overlapping-live
       disjoint-live one-large-root partly-overlapping large-live-core
       registered-ring registered-ring-live registered-ring-1000
       registered-ring-4000 registered-ring-interleaved deferred-live-large
       deferred-then-dead live-churn live-churn-dies-by-count"}
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

# One line per physical core, in package and core order: its first logical
# CPU and its sibling list.
for dir in /sys/devices/system/cpu/cpu[0-9]*; do
    echo "$(cat "$dir/topology/physical_package_id") $(cat "$dir/topology/core_id")" \
         "${dir##*cpu} $(cat "$dir/topology/thread_siblings_list")"
done | sort -n -k1,1 -k2,2 -k3,3 | awk '!seen[$1 " " $2]++ { print $3, $4 }' > "$WORK/cores"

if [ $((FIRST_CORE + CORES + 1)) -gt "$(wc -l < "$WORK/cores")" ]; then
    echo "the box has $(wc -l < "$WORK/cores") physical cores, fewer than $FIRST_CORE + $CORES + 1" >&2
    exit 1
fi

CPU=()
: > "$OUT"
while read -r cpu siblings; do
    echo "# core $((${#CPU[@]} + 1)): cpu $cpu, siblings $siblings" >> "$OUT"
    CPU+=("$cpu")
done < <(tail -n "+$((FIRST_CORE + 1))" "$WORK/cores" | head -n "$((CORES + 1))")

# The CPUs of `count` mutators laid over cores 1..C in turn, comma-separated.
mutator_cpus() {
    local cpus=() index
    for ((index = 0; index < $1; index++)); do
        cpus+=("${CPU[$((index % CORES))]}")
    done
    local IFS=,
    echo "${cpus[*]}"
}

LAST=${CPU[$((CORES - 1))]}
SPARE=${CPU[$CORES]}
LIMIT=$(awk "BEGIN { print int($RUN_SECONDS * 3) + 60 }")
run_cell() {
    local placement=$1 load=$2 mutators=$3 collectors=$4 cap=$5
    # A mutator that panics before the loop's start leaves the others at its
    # barrier, so a cell is bounded from outside: its run, the collection
    # after it, and a minute.
    if ! LL_RIG_PLACEMENT=$placement LL_RIG_LOAD=$load LL_RIG_MUTATOR_CPUS=$mutators \
         LL_RIG_COLLECTOR_CPUS=$collectors LL_RIG_CAP=$cap LL_RIG_SECONDS=$RUN_SECONDS \
         timeout "$LIMIT" "$BIN" --ignored --exact --test-threads=1 --nocapture "$CASE" \
         > "$WORK/cell" 2>&1 \
       || ! grep -q '1 passed' "$WORK/cell"; then
        echo "the cell $placement/$load failed:" >&2
        tail -n 20 "$WORK/cell" >&2
        exit 1
    fi
    # The probe prints the header before its line, the first of them after
    # the harness's "test … ..." on the same output line; the header is
    # written once, from the first cell.
    if [ -z "$HEADED" ]; then
        grep -o 'rig-header,.*' "$WORK/cell" | cut -d, -f2- >> "$OUT"
        HEADED=1
    fi
    grep -o 'rig,.*' "$WORK/cell" | cut -d, -f2- >> "$OUT"
}

HEADED=
for load in $LOADS; do
    for mutators in $((CORES - 1)) "$CORES" $((CORES + 1)); do
        run_cell spare-core "$load" "$(mutator_cpus "$mutators")" "$SPARE" 1
        run_cell shared-core "$load" "$(mutator_cpus "$mutators")" "$LAST" 1
        run_cell cap-zero "$load" "$(mutator_cpus "$mutators")" "$LAST" 0
    done
done
