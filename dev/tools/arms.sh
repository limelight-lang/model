#!/bin/bash
# The arms of one comparison on the rig (`dev/BENCHMARKS.md`, "S65.24 A, B and
# C on a box with a PMU"): every arm a test binary of its own in ARMS_DIR,
# interleaved inside each repeat in an order rotated by one arm a repeat, one
# process a cell, the rig's line with the arm and the repeat in front. The deciding loads are paced as the S65.24
# protocol paces them; the guards run unpaced and birth no collector.
#
# Usage: dev/tools/arms.sh <out.csv> <repeats> <deciding|guards|web>
# Env: ARMS_DIR (binaries named by arm, default target/arms), ARMS ("A B C"),
#      MUTATORS (2,4), SPARE (6), SHARED (4): CPUs for the two placements;
#      LOADS, the deciding phase's loads as load:pace, default all six.
# The web phase is the web loads' protocol (`dev/design/the-web-loads.md`):
# six mutators on CPUs 2-12, one a core (WEB_MUTATORS); CAPS ("1 4"), cap 1's collector on 14
# and cap 4's on 14, 0, 15 and 1 (WEB_COLLECTORS_CAP1, WEB_COLLECTORS_CAP4);
# WEB_LOADS as load:interarrival_ms, the interarrival each load's pilot of best
# D found; 116 s with a 20 s warm-up and a 12 s drain;
# each cell's requests beside OUT for dev/tools/paired_excess.py; a cell whose
# line reads void is run again once after the last repeat, and a second void
# is reported, not run again.
# Build each arm as code bound for an executable is compiled, so that no arm
# pays a TLS call frame the shipped code does not have
# (`dev/design/the-general-algorithm.md`, "the Critic on P6"):
# `RUSTFLAGS="-C relocation-model=pie" cargo test --release --lib --no-run
# --target x86_64-unknown-linux-gnu [--features …]`, and copy the binary into
# ARMS_DIR under the arm's name.
set -u
OUT=$1
REPEATS=$2
PHASE=$3
ARMS_DIR=${ARMS_DIR:-target/arms}
ARMS=${ARMS:-"A B C"}
MUTATORS=${MUTATORS:-2,4}
SPARE=${SPARE:-6}
SHARED=${SHARED:-4}
LOADS=${LOADS:-"live-churn:1 live-churn-dies-by-count:1 deferred-then-dead:1 deferred-live-large:1 garbage-25:15 registered-ring-live:15"}
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
        LL_RIG_PACE_MS=$pace LL_RIG_DRAIN_MS=$drain LL_RIG_REPEAT=$repeat \
        timeout 400 "$ARMS_DIR/$arm" --ignored --exact --test-threads=1 --nocapture "$CASE" \
        > "$LOG" 2>&1
    record "$arm" "$placement $load" "$repeat"
}

# Append the cell's line from LOG to OUT, the header first; answers 1 where
# the line reads void (`other_cpu_cores` past half a core). The line is laid
# out by OUT's header, which the first cell wrote: an arm built before or
# after a column was added leaves that column empty or drops it, rather than
# shifting every column after it, and the drop is said on stderr.
record() {
    local arm=$1 what=$2 repeat=$3
    if ! grep -q '1 passed' "$LOG"; then
        echo "FAILED $arm $what $repeat" >&2
        tail -5 "$LOG" >&2
        return 0
    fi
    if [ -z "$HEADED" ]; then
        echo "arm,repeat,$(grep -o 'rig-header,.*' "$LOG" | cut -d, -f2-)" > "$OUT"
        HEADED=1
    fi
    python3 - "$LOG" "$OUT" "$arm" "$repeat" <<'PY'
import re, sys
log, out, arm, repeat = sys.argv[1:]
text = open(log).read()
header = re.search(r'rig-header,(.*)', text).group(1).split(',')
line = re.search(r'\brig,(.*)', text).group(1).split(',')
cell = dict(zip(header, line))
columns = open(out).readline().rstrip('\n').split(',')[2:]
dropped = [column for column in header if column not in columns]
if dropped:
    print(f"{arm} {repeat}: columns not in {out}'s header, dropped: {' '.join(dropped)}",
          file=sys.stderr)
with open(out, 'a') as appended:
    appended.write(','.join([arm, repeat] + [cell.get(column, '') for column in columns]) + '\n')
sys.exit(1 if cell.get('void') == '1' else 0)
PY
    [ $? -eq 1 ] && return 1
    return 0
}

# One cell of the web phase: arm, cap, load, interarrival in ms, repeat.
web_cell() {
    local arm=$1 cap=$2 load=$3 interarrival=$4 repeat=$5
    local collectors=$WEB_COLLECTORS_CAP1
    [ "$cap" = 4 ] && collectors=$WEB_COLLECTORS_CAP4
    LL_RIG_PLACEMENT=cap-$cap LL_RIG_LOAD=$load LL_RIG_MUTATOR_CPUS=$WEB_MUTATORS \
        LL_RIG_COLLECTOR_CPUS=$collectors LL_RIG_CAP=$cap LL_RIG_SECONDS=116 \
        LL_RIG_WARM_UP_SECONDS=20 LL_RIG_DRAIN_MS=12000 LL_RIG_ARRIVALS=1 \
        LL_RIG_INTERARRIVAL_MS=$interarrival LL_RIG_REPEAT=$repeat \
        LL_RIG_REQUESTS_TO="$REQUESTS_DIR/$arm-cap$cap-$load-$repeat.csv" \
        timeout 600 "$ARMS_DIR/$arm" --ignored --exact --test-threads=1 --nocapture "$CASE" \
        > "$LOG" 2>&1
    record "$arm" "cap-$cap $load" "$repeat"
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

if [ "$PHASE" = web ]; then
    WEB_MUTATORS=${WEB_MUTATORS:-2,4,6,8,10,12}
    WEB_COLLECTORS_CAP1=${WEB_COLLECTORS_CAP1:-14}
    WEB_COLLECTORS_CAP4=${WEB_COLLECTORS_CAP4:-14,0,15,1}
    CAPS=${CAPS:-"1 4"}
    WEB_LOADS=${WEB_LOADS:?"WEB_LOADS as load:interarrival_ms, from each load's pilot"}
    REQUESTS_DIR="${OUT%.csv}-requests"
    mkdir -p "$REQUESTS_DIR"
    VOID=()
    for repeat in $(seq 1 "$REPEATS"); do
        ORDER=$(rotated "$repeat")
        for cap in $CAPS; do
            for spec in $WEB_LOADS; do
                for arm in $ORDER; do
                    web_cell "$arm" "$cap" "${spec%%:*}" "${spec##*:}" "$repeat" \
                        || VOID+=("$arm $cap $spec $repeat")
                done
            done
        done
    done
    for void in "${VOID[@]}"; do
        read -r arm cap spec repeat <<< "$void"
        echo "void, run again: $void" >&2
        web_cell "$arm" "$cap" "${spec%%:*}" "${spec##*:}" "$repeat" \
            || echo "void twice, reported: $void" >&2
    done
    exit 0
fi

for repeat in $(seq 1 "$REPEATS"); do
    ORDER=$(rotated "$repeat")
    for placement in spare-core shared-core; do
        if [ "$PHASE" = deciding ]; then
            for spec in $LOADS; do
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
