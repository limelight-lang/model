#!/bin/bash
# S65.27's loop: one short `live-churn` cell of the rig on one binary, the
# counts that showed the collector's chain frozen, and RED when the drain
# freed less than half of what stood at the loop's stop. Seconds, not minutes.
#
# Usage: dev/tools/stall.sh <test binary>   (SECS, DRAIN, CPUS to vary)
BIN=$1
LOG=$(mktemp)
trap 'rm -f "$LOG"' EXIT
LL_RIG_PLACEMENT=spare-core LL_RIG_LOAD=live-churn LL_RIG_MUTATOR_CPUS=${CPUS:-2,4} \
LL_RIG_COLLECTOR_CPUS=${COLLECTOR:-6} LL_RIG_CAP=1 LL_RIG_SECONDS=${SECS:-3} LL_RIG_PACE_MS=1 \
LL_RIG_DRAIN_MS=${DRAIN:-3000} \
timeout 120 "$BIN" --ignored --exact --test-threads=1 --nocapture \
    cycle::worker::tests::the_rig::a_cell_of_the_rig > "$LOG" 2>&1
python3 - "$LOG" <<'P'
import re, sys
text = open(sys.argv[1]).read()
header = re.search(r'rig-header,(.*)', text).group(1).split(',')
values = re.search(r'\brig,(.*)', text).group(1).split(',')
line = dict(zip(header, values))
keys = ['iterations', 'garbage_members', 'freed_by_polls', 'backlog_at_the_stop',
        'freed_in_the_drain', 'batches', 'grants', 'verdict_collections',
        'chain_pushed_waiting', 'roots_from_r', 'roots_from_the_chain', 'turnovers']
print(' '.join(f"{key}={line[key]}" for key in keys))
backlog, freed = int(line['backlog_at_the_stop']), int(line['freed_in_the_drain'])
print("RED" if backlog > 0 and freed * 2 < backlog else "GREEN")
P
