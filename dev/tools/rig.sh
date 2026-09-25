#!/bin/bash
# The rig's driver: every cell of `cycle::worker::tests::the_rig`, a placement
# and a load, in a process of its own, into one CSV line each. The placements
# are those of `dev/S64-GC-IMPROVEMENT-ANALYSIS.md`, "Какие опыты нужны", at
# C physical cores:
#
#   spare     C-1 mutators, the collector alone on the C-th core, cap 1
#   shared    C mutators, the collector on the C-th mutator's core, cap 1
#   cap-zero  C+1 mutators, the first core carrying two, cap 0; the elder,
#             which still rounds and asks under cap 0, on the C-th core
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
# the other.
LOADS="garbage-0 garbage-25 garbage-50 garbage-75 garbage-100 overlapping-live
       disjoint-live one-large-root partly-overlapping large-live-core"
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

# One line per physical core, in package and core order: its first logical
# CPU and its sibling list.
for dir in /sys/devices/system/cpu/cpu[0-9]*; do
    echo "$(cat "$dir/topology/physical_package_id") $(cat "$dir/topology/core_id")" \
         "${dir##*cpu} $(cat "$dir/topology/thread_siblings_list")"
done | sort -n -k1,1 -k2,2 -k3,3 | awk '!seen[$1 " " $2]++ { print $3, $4 }' > "$WORK/cores"

if [ $((FIRST_CORE + CORES)) -gt "$(wc -l < "$WORK/cores")" ]; then
    echo "the box has $(wc -l < "$WORK/cores") physical cores, fewer than $FIRST_CORE + $CORES" >&2
    exit 1
fi

CPU=()
: > "$OUT"
while read -r cpu siblings; do
    echo "# core $((${#CPU[@]} + 1)): cpu $cpu, siblings $siblings" >> "$OUT"
    CPU+=("$cpu")
done < <(tail -n "+$((FIRST_CORE + 1))" "$WORK/cores" | head -n "$CORES")

# The CPUs of the first `count` cores, comma-separated.
first_cpus() {
    local IFS=,
    echo "${CPU[*]:0:$1}"
}

LAST=${CPU[$((CORES - 1))]}
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
    run_cell spare "$load" "$(first_cpus $((CORES - 1)))" "$LAST" 1
    run_cell shared "$load" "$(first_cpus "$CORES")" "$LAST" 1
    run_cell cap-zero "$load" "$(first_cpus "$CORES"),${CPU[0]}" "$LAST" 0
done
