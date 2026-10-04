#!/usr/bin/env python3
"""The paired latency excess of the web loads' protocol
(`dev/design/the-web-loads.md`).

Pairs two arms' requests files, written by the rig under `LL_RIG_REQUESTS_TO`,
by (mutator, index): the seeded streams make request i of a mutator the same
request in every arm, which the pairing asserts by its drawn CPU, its drawn
waits and its arrival. A request's excess is its wall less its drawn CPU and
waits, from its arrival and from its service's start; the script prints the
p99.9 of b's excess less a's over the pairs, both ways, and the requests either
file holds alone.

Usage: paired_excess.py <a.csv> <b.csv>
       paired_excess.py --self-test
"""

import csv
import math
import random
import sys


def read(path):
    """The file's requests by (mutator, index)."""
    with open(path, newline="") as file:
        return {
            (int(row["mutator"]), int(row["index"])): {
                key: int(value) for key, value in row.items()
            }
            for row in csv.DictReader(file)
        }


def excess(request):
    """The excess from arrival and from service, in nanoseconds."""
    drawn = request["cpu_ns"] + request["waits_ns"]
    return (
        request["end_ns"] - request["arrival_ns"] - drawn,
        request["end_ns"] - request["start_ns"] - drawn,
    )


def quantile(values, share):
    """The value below which `share` of `values` fall, by rank."""
    ordered = sorted(values)
    return ordered[max(0, math.ceil(len(ordered) * share) - 1)]


def paired(a, b):
    """The p99.9 of b's excess less a's from arrival and from service, the
    pairs, and the requests of a alone and of b alone."""
    keys = a.keys() & b.keys()
    for key in keys:
        for field in ("cpu_ns", "waits_ns", "arrival_ns"):
            assert a[key][field] == b[key][field], f"request {key} differs in {field}"
    differences = [
        tuple(eb - ea for ea, eb in zip(excess(a[key]), excess(b[key]))) for key in keys
    ]
    return (
        quantile([d[0] for d in differences], 0.999),
        quantile([d[1] for d in differences], 0.999),
        len(keys),
        len(a.keys() - keys),
        len(b.keys() - keys),
    )


def self_test():
    """A scripted pair whose answer is known: b's rows shuffled, one of a's
    requests missing from b, and one request of b 5 ms later than a's and the
    rest equal, so the p99.9 over 999 pairs is the 5 ms."""
    rows = []
    for index in range(1000):
        arrival = index * 27_000_000
        start = arrival + 1_000_000
        rows.append(
            {
                "mutator": 0,
                "index": index,
                "arrival_ns": arrival,
                "start_ns": start,
                "end_ns": start + 20_000_000,
                "cpu_ns": 6_000_000,
                "waits_ns": 10_000_000,
            }
        )
    a = {(row["mutator"], row["index"]): dict(row) for row in rows}
    b = {key: dict(row) for key, row in a.items() if key != (0, 500)}
    b[(0, 7)]["end_ns"] += 5_000_000
    shuffled = list(b.items())
    random.Random(1).shuffle(shuffled)
    b = dict(shuffled)
    from_arrival, from_service, pairs, a_alone, b_alone = paired(a, b)
    assert (from_arrival, from_service) == (5_000_000, 5_000_000), (from_arrival, from_service)
    assert (pairs, a_alone, b_alone) == (999, 1, 0), (pairs, a_alone, b_alone)
    b[(0, 8)]["cpu_ns"] += 1
    try:
        paired(a, b)
    except AssertionError:
        print("self-test passed")
        return
    raise AssertionError("a pair whose drawn CPU differs was accepted")


def main():
    if sys.argv[1:] == ["--self-test"]:
        self_test()
        return
    a, b = read(sys.argv[1]), read(sys.argv[2])
    from_arrival, from_service, pairs, a_alone, b_alone = paired(a, b)
    print(
        f"pairs {pairs}, alone {a_alone} and {b_alone}; p99.9 of b's excess less a's: "
        f"{from_arrival / 1e6:.3f} ms from arrival, {from_service / 1e6:.3f} ms from service"
    )


if __name__ == "__main__":
    main()
