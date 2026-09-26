"""Two arms of `dev/tools/arms.sh`'s CSV read by the S65.24 protocol as S65.28
fixed it (`dev/BENCHMARKS.md`, "S65.24 A, B and C on a box with a PMU", the
Critic's reading): per load and placement, the mutators' instructions an
iteration as the gate, with the baseline's spread as the tolerance (max 3 %,
twice the spread); heap garbage at the stop and the last free after it as the
memory gates; iterations longer than 200 us; the collector's CPU beside
whether the cell completed. Then, for each arm, the share of the mutators'
token wait and of their withheld returns' time that fell in the grant's
expiry and death check, flagged past 10 % (`PLAN.md` S65.28).

Usage: dev/tools/two_arms_table.py <csv> <baseline arm> <candidate arm>"""
import csv
import statistics
import sys
from collections import defaultdict

SEGMENTS = ["around", "expiry", "check", "trace"]
SHARE_LIMIT = 0.10


def per_iteration(row, key):
    return int(row[key]) / max(1, int(row["iterations"]))


def instructions(row):
    return per_iteration(row, "mutator_instructions_in_the_loop")


def spread(values):
    middle = statistics.median(values)
    return (max(values) - min(values)) / middle if middle else 0.0


def verdict(new, old, tolerance):
    change = new / old - 1 if old else 0.0
    if change < -tolerance:
        return f"WIN {change:+.1%}"
    if change > tolerance:
        return f"LOSS {change:+.1%}"
    return f"tie {change:+.1%}"


def completed(row):
    """Every garbage member freed by the loop's polls or the drain, and every
    mutator's remnant of deaths behind entries cleared."""
    freed = int(row["freed_by_polls"]) + int(row["freed_in_the_drain"])
    return freed >= int(row["garbage_members"]) and int(row["remnants_cleared"]) == int(
        row["mutators"]
    )


def share(rows, prefix):
    """The expiry's and the check's share of the sum over `rows` of the
    columns `prefix` + each segment; None where the sum is zero."""
    total = sum(int(row[prefix + segment]) for row in rows for segment in SEGMENTS)
    if total == 0:
        return None
    part = sum(int(row[prefix + segment]) for row in rows for segment in ("expiry", "check"))
    return part / total


def shown(value):
    return "-" if value is None else f"{value:.1%}"


def main(path, baseline, candidate):
    cells = defaultdict(lambda: defaultdict(list))
    for row in csv.DictReader(open(path)):
        cells[(row["load"], row["placement"])][row["arm"]].append(row)

    print(
        f"load placement n | instr/it {baseline} {candidate} tol verdict | "
        f"heap garbage MB | last free ms | long iterations | collector s (completed)"
    )
    for (load, placement), arms in sorted(cells.items()):
        if baseline not in arms or candidate not in arms:
            continue
        old, new = arms[baseline], arms[candidate]
        old_instructions = [instructions(row) for row in old]
        tolerance = max(0.03, 2 * spread(old_instructions))
        middle = lambda rows, key: statistics.median(float(row[key]) for row in rows)
        io, inew = statistics.median(old_instructions), statistics.median(
            instructions(row) for row in new
        )
        done = lambda rows: f"{sum(completed(row) for row in rows)}/{len(rows)}"
        print(
            f"{load} {placement} {len(old)},{len(new)} | {io:,.0f} {inew:,.0f} "
            f"{tolerance:.1%} {verdict(inew, io, tolerance)} | "
            f"{middle(old, 'standing_bytes') / 1e6:.2f} {middle(new, 'standing_bytes') / 1e6:.2f} | "
            f"{middle(old, 'last_free_us_max') / 1e3:,.0f} {middle(new, 'last_free_us_max') / 1e3:,.0f} | "
            f"{middle(old, 'long_iterations'):,.0f} {middle(new, 'long_iterations'):,.0f} | "
            f"{middle(old, 'collector_cpu_us') / 1e6:.2f} ({done(old)}) "
            f"{middle(new, 'collector_cpu_us') / 1e6:.2f} ({done(new)})"
        )

    print()
    print(
        "arm load placement | expiry+check share of token wait, all repeats (worst) | "
        f"of withheld-return time (worst) | over {SHARE_LIMIT:.0%}"
    )
    for (load, placement), arms in sorted(cells.items()):
        for arm in (baseline, candidate):
            rows = arms.get(arm, [])
            if not rows:
                continue
            waits = [share([row], "token_wait_us_") for row in rows]
            returns = [share([row], "withheld_return_us_") for row in rows]
            worst = max((value for value in waits + returns if value is not None), default=None)
            over = worst is not None and worst > SHARE_LIMIT
            print(
                f"{arm} {load} {placement} | {shown(share(rows, 'token_wait_us_'))} "
                f"({shown(max((w for w in waits if w is not None), default=None))}) | "
                f"{shown(share(rows, 'withheld_return_us_'))} "
                f"({shown(max((r for r in returns if r is not None), default=None))}) | "
                f"{'OVER' if over else 'under'}"
            )


if __name__ == "__main__":
    main(*sys.argv[1:4])
