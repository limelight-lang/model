"""Abstract model of R2 from dev/COLLECTOR-MUTATOR-MEMORY-PROTOCOL.md.

Checks record and segment preservation, not Rust pointer validity, accounting,
concurrent access, or the production queue implementation.
Run from the crate root: python3 dev/tools/check_memory_protocol_compaction.py
"""

from itertools import product


def reverse(nodes, head):
    previous = None
    while head is not None:
        following = nodes[head][1]
        nodes[head][1] = previous
        previous, head = head, following
    return previous


def compact(capacity, fills, keep):
    nodes = {}
    value = 0
    for i, fill in enumerate(fills):
        records = list(range(value, value + fill)) + [None] * (capacity - fill)
        value += fill
        nodes[i] = [records, i + 1 if i + 1 < len(fills) else None]
    original_head = 0 if fills else None
    original_fill = fills[0] if fills else 0
    reversed_head = reverse(nodes, original_head)
    read_node = write_node = reversed_head
    write_index = 0
    last_used = None
    written = 0
    physical_read_rank = 0
    seen = []
    while read_node is not None:
        bound = original_fill if read_node == original_head else capacity
        for read_index in range(bound):
            candidate = nodes[read_node][0][read_index]
            seen.append(candidate)
            if keep[candidate]:
                assert written <= physical_read_rank * capacity + read_index
                last_used = write_node
                nodes[write_node][0][write_index] = candidate
                written += 1
                write_index += 1
                if write_index == capacity:
                    write_node = nodes[write_node][1]
                    write_index = 0
        read_node = nodes[read_node][1]
        physical_read_rank += 1
    assert sorted(seen) == list(range(value)), (capacity, fills, seen)
    if last_used is None:
        surplus = reversed_head
        result_head = None
        result_fill = 0
    else:
        surplus = nodes[last_used][1]
        nodes[last_used][1] = None
        result_head = reverse(nodes, reversed_head)
        result_fill = write_index or capacity
    output = []
    occupied_nodes = set()
    current = result_head
    while current is not None:
        assert current not in occupied_nodes
        occupied_nodes.add(current)
        fill = result_fill if current == result_head else capacity
        output.extend(nodes[current][0][:fill])
        current = nodes[current][1]
    surplus_nodes = set()
    while surplus is not None:
        assert surplus not in surplus_nodes | occupied_nodes
        surplus_nodes.add(surplus)
        surplus = nodes[surplus][1]
    assert occupied_nodes | surplus_nodes == set(nodes)
    assert sorted(output) == [v for v in range(value) if keep[v]]
    assert len(output) == len(set(output))
    assert len(occupied_nodes) == (written + capacity - 1) // capacity


cases = 0
for capacity in range(1, 5):
    for segment_count in range(4):
        for headfill in (range(1, capacity + 1) if segment_count else [0]):
            fills = [headfill] + [capacity] * (segment_count - 1) if segment_count else []
            if sum(fills) > 11:
                continue
            for keep in product([False, True], repeat=sum(fills)):
                compact(capacity, fills, keep)
                cases += 1
print(f'{cases} exhaustive cases passed for exact reverse/compact/reverse R2; no lost/duplicate records or nodes.')
