#!/usr/bin/env python3
"""Two arms of `dev/tools/arms.sh`'s CSV read by the decision rule `PLAN.md`
S65.28 fixed before its run: per load and placement, the mutators'
instructions an iteration, a win or a loss past max(3 %, twice the baseline's
spread); the gates on medians — heap garbage at the stop at most 1.10 x the
baseline's + 64 KiB, the last free at most the baseline's + 4 s, completion in
every repeat the baseline completes, iterations over 200 us at most 1.25 x the
baseline's + 10 — a failed gate a loss; the verdict per placement, the
candidate taken at `TAKEN_AT` deciding loads losing none. Then, per arm and
cell, the share of the mutators' token wait and of their withheld returns'
time spent in the grant's expiry and death check, the median over repeats
against 10 %, with the absolute microseconds beside it.

Usage: dev/tools/two_arms_table.py <csv> <baseline arm> <candidate arm>"""
import csv
import statistics
import sys
from collections import defaultdict

SEGMENTS = ["around", "expiry", "check", "trace"]
SHARE_LIMIT = 0.10
DECIDING = {
    "live-churn",
    "live-churn-dies-by-count",
    "deferred-then-dead",
    "deferred-live-large",
    "garbage-25",
    "registered-ring-live",
}
TAKEN_AT = 4
HEAP_FACTOR, HEAP_SLACK = 1.10, 64 * 1024
LAST_FREE_SLACK_US = 4_000_000
LONG_FACTOR, LONG_SLACK = 1.25, 10


def per_iteration(row, key):
    return int(row[key]) / max(1, int(row["iterations"]))


def instructions(row):
    return per_iteration(row, "mutator_instructions_in_the_loop")


def median(rows, key):
    return statistics.median(float(row[key]) for row in rows)


def spread(values):
    middle = statistics.median(values)
    return (max(values) - min(values)) / middle if middle else 0.0


def completed(row):
    """Every garbage member freed by the loop's polls or the drain, and every
    mutator's remnant of deaths behind entries cleared."""
    freed = int(row["freed_by_polls"]) + int(row["freed_in_the_drain"])
    return freed >= int(row["garbage_members"]) and int(row["remnants_cleared"]) == int(
        row["mutators"]
    )


def failed_gates(old, new):
    """The gates the candidate's cell fails against the baseline's."""
    failed = []
    if median(new, "standing_bytes") > HEAP_FACTOR * median(old, "standing_bytes") + HEAP_SLACK:
        failed.append("heap")
    if median(new, "last_free_us_max") > median(old, "last_free_us_max") + LAST_FREE_SLACK_US:
        failed.append("last-free")
    by_repeat = {row["repeat"]: completed(row) for row in new}
    if any(completed(row) and not by_repeat.get(row["repeat"], False) for row in old):
        failed.append("completion")
    if median(new, "long_iterations") > LONG_FACTOR * median(old, "long_iterations") + LONG_SLACK:
        failed.append("long")
    return failed


def cell_verdict(old, new):
    """"win", "loss" or "tie" for the candidate, with the change and the
    failed gates."""
    old_instructions = [instructions(row) for row in old]
    tolerance = max(0.03, 2 * spread(old_instructions))
    before = statistics.median(old_instructions)
    after = statistics.median(instructions(row) for row in new)
    change = after / before - 1 if before else 0.0
    failed = failed_gates(old, new)
    if failed or change > tolerance:
        return "loss", change, tolerance, failed
    if change < -tolerance:
        return "win", change, tolerance, failed
    return "tie", change, tolerance, failed


def segment_sum(row, prefix, segments):
    return sum(int(row[prefix + segment]) for segment in segments)


def share(row, prefix):
    """The expiry's and the check's share of `row`'s columns `prefix` + each
    segment; None where their sum is zero."""
    total = segment_sum(row, prefix, SEGMENTS)
    return segment_sum(row, prefix, ("expiry", "check")) / total if total else None


def median_share(rows, prefix):
    shares = [value for value in (share(row, prefix) for row in rows) if value is not None]
    return statistics.median(shares) if shares else None


def shown(value):
    return "-" if value is None else f"{value:.1%}"


def main(path, baseline, candidate):
    cells = defaultdict(lambda: defaultdict(list))
    for row in csv.DictReader(open(path)):
        cells[(row["load"], row["placement"])][row["arm"]].append(row)

    print(
        f"load placement n | instr/it {baseline} {candidate} tol | verdict | heap MB | "
        f"last free ms | long its | collector s (completed)"
    )
    tally = defaultdict(lambda: defaultdict(int))
    for (load, placement), arms in sorted(cells.items()):
        if baseline not in arms or candidate not in arms:
            continue
        old, new = arms[baseline], arms[candidate]
        outcome, change, tolerance, failed = cell_verdict(old, new)
        if load in DECIDING:
            tally[placement][outcome] += 1
        done = lambda rows: f"{sum(completed(row) for row in rows)}/{len(rows)}"
        gates = f" [{' '.join(failed)}]" if failed else ""
        print(
            f"{load} {placement} {len(old)},{len(new)} | "
            f"{statistics.median(instructions(r) for r in old):,.0f} "
            f"{statistics.median(instructions(r) for r in new):,.0f} {tolerance:.1%} | "
            f"{outcome} {change:+.1%}{gates}{'' if load in DECIDING else ' (guard)'} | "
            f"{median(old, 'standing_bytes') / 1e6:.2f} {median(new, 'standing_bytes') / 1e6:.2f} | "
            f"{median(old, 'last_free_us_max') / 1e3:,.0f} "
            f"{median(new, 'last_free_us_max') / 1e3:,.0f} | "
            f"{median(old, 'long_iterations'):,.0f} {median(new, 'long_iterations'):,.0f} | "
            f"{median(old, 'collector_cpu_us') / 1e6:.2f} ({done(old)}) "
            f"{median(new, 'collector_cpu_us') / 1e6:.2f} ({done(new)})"
        )

    print()
    for placement, outcomes in sorted(tally.items()):
        taken = outcomes["win"] >= TAKEN_AT and outcomes["loss"] == 0
        print(
            f"{placement}: {candidate} wins {outcomes['win']}, ties {outcomes['tie']}, "
            f"loses {outcomes['loss']} -> {candidate if taken else baseline}"
        )

    print()
    print(
        "arm load placement | expiry+check share of token wait, median (us over the "
        f"repeats) | of withheld-return time, median (us) | over {SHARE_LIMIT:.0%}"
    )
    for (load, placement), arms in sorted(cells.items()):
        for arm in (baseline, candidate):
            rows = arms.get(arm, [])
            if not rows:
                continue
            waits = median_share(rows, "token_wait_us_")
            returns = median_share(rows, "withheld_return_us_")
            wait_us = sum(segment_sum(r, "token_wait_us_", ("expiry", "check")) for r in rows)
            return_us = sum(
                segment_sum(r, "withheld_return_us_", ("expiry", "check")) for r in rows
            )
            over = any(value is not None and value > SHARE_LIMIT for value in (waits, returns))
            print(
                f"{arm} {load} {placement} | {shown(waits)} ({wait_us:,}) | "
                f"{shown(returns)} ({return_us:,}) | {'OVER' if over else 'under'}"
            )


if __name__ == "__main__":
    main(*sys.argv[1:4])
