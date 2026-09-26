"""The verdict table over `dev/tools/arms.sh`'s CSV: per load and placement,
each arm's median of the mutators' cycles and instructions an iteration, the
first arms' spread as the tolerance (max 3 %, twice the spread), and the gates
the S65.24 protocol reads. The arms are A, B and C, as the S65.24 runs named
them. Usage: dev/tools/arms_table.py <csv>"""
import csv
import statistics
import sys
from collections import defaultdict

rows = list(csv.DictReader(open(sys.argv[1])))
cells = defaultdict(lambda: defaultdict(list))
for row in rows:
    cells[(row["load"], row["placement"])][row["arm"]].append(row)


def per_iteration(row, key):
    return int(row[key]) / max(1, int(row["iterations"]))


def median(rows, fn):
    return statistics.median(fn(r) for r in rows)


def spread(rows, fn):
    values = [fn(r) for r in rows]
    m = statistics.median(values)
    return (max(values) - min(values)) / m if m else 0.0


def verdict(new, old, tolerance):
    change = new / old - 1
    if change < -tolerance:
        return f"WIN {change:+.1%}"
    if change > tolerance:
        return f"LOSS {change:+.1%}"
    return f"tie {change:+.1%}"


cycles = lambda r: per_iteration(r, "mutator_cycles_in_the_loop")
instructions = lambda r: per_iteration(r, "mutator_instructions_in_the_loop")
cpu = lambda r: per_iteration(r, "mutator_cpu_in_the_loop_us")

print("load placement | cycles/it A B C (spreadA spreadB) tol | B vs A | C vs B | C vs A | instr/it A B C | cpu us/it A B C")
for (load, placement), arms in sorted(cells.items()):
    if not all(arm in arms for arm in "ABC"):
        continue
    a, b, c = (arms[x] for x in "ABC")
    tol_a = max(0.03, 2 * spread(a, cycles))
    tol_b = max(0.03, 2 * spread(b, cycles))
    ma, mb, mc = (median(x, cycles) for x in (a, b, c))
    ia, ib, ic = (median(x, instructions) for x in (a, b, c))
    ca, cb, cc = (median(x, cpu) for x in (a, b, c))
    print(
        f"{load} {placement} n={len(a)},{len(b)},{len(c)} | {ma:,.0f} {mb:,.0f} {mc:,.0f} "
        f"({spread(a, cycles):.1%} {spread(b, cycles):.1%}) {tol_a:.1%} | {verdict(mb, ma, tol_a)} | "
        f"{verdict(mc, mb, tol_b)} | {verdict(mc, ma, tol_a)} | {ia:,.0f} {ib:,.0f} {ic:,.0f} | "
        f"{ca:.1f} {cb:.1f} {cc:.1f}"
    )

print()
print("gates (medians): ledger_peak_bytes, withheld_peak, last_free_us_max, remnants_cleared, token_wait_longest_us, collector_cpu_us, verdict_collections, disposals, share_min, collectors_born, batches")
keys = ["ledger_peak_bytes", "withheld_peak", "last_free_us_max", "remnants_cleared",
        "token_wait_longest_us", "collector_cpu_us", "verdict_collections", "disposals",
        "mutator_counter_share_min", "collectors_born", "batches"]
for (load, placement), arms in sorted(cells.items()):
    for arm in "ABC":
        if arm not in arms:
            continue
        values = [statistics.median(float(r[k]) for r in arms[arm]) for k in keys]
        print(f"{load} {placement} {arm}: " + " ".join(f"{k.split('_')[0]}={v:,.0f}" if v >= 10 else f"{k.split('_')[0]}={v:g}" for k, v in zip(keys, values)))
