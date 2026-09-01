# Cycle collector review, 2026-09-01

A read-only review of `src/cycle/` at `8ccf426`, with Edmond's ruling on
each finding. It records what the trace core costs today, in operations
and in memory, and what was agreed to change. Nothing here was edited in
code; the rulings are the input to the steps that will.

Read: `src/cycle/*.rs`, `refcount::release_word`, `gc.rs`, the row
accessors in `memory/heap.rs`, `memory/retained.rs`,
`rfc/model/gc/rc-cycle.md`, `rfc/dev/ALGORITHM-AUDIT.md`. Run:
`cargo test --lib -- --list` only, which lists 627 tests, 120 of them
under `cycle::`. No test was executed and nothing was timed; every figure
below is arithmetic over constants in the source unless a benchmark entry
is cited.

## State of the collector

`ll_gc_collect_cycles` returns zero. Built and tested: the candidate
queue, the trace arena, the shadow rows, mark, scan, the exact test and
the slot-reuse window. Not built: the collection driver (S36.7), the
member list of a condemned component (S36.12), the guard, weak nulling,
destructors and the sever (S36.3 to S36.5), the maturation prune (S37.1),
the turnover buffer (S37.4), the collector worker (S38).

The trace core is sound as far as reading finds. Mark and scan are
linear in vertices and edges: an entity enters the worklist once in mark
and at most twice in scan, because `Live` is final. Saturation is
absorbing. Rows are zeroed per group of eight at first touch, and a
block's first touch writes 121 bytes against the 16 320 its rows reserve
(`dev/BENCHMARKS.md`, 2026-08-27).

What the state costs. Nothing removes a live candidate from the queue: a
candidate found live is re-offered by design, and the mechanisms that
prune (S37.1) and age out (S37.4) are unbuilt. The rfc's own measurement
says a median root reaches the whole test heap. So today one collection
is one full trace of everything reachable from every candidate ever
registered, and the queue grows by eight bytes per candidate for the
life of the thread.

## Findings and rulings

| # | finding | where | ruling |
|---|---|---|---|
| 1 | A quarter of each arena block is lost at the smallest size class | `cycle/arena.rs` | agreed: allocate stack segments from the block's end |
| 2 | Scan resolves a row twice per entity | `cycle/scan.rs` | agreed: carry the row pointer in the worklist entry |
| 3 | One global mutex per edge into a retained block | `memory/retained.rs` | rework: `dev/design/retained-index-ownership.md` |
| 4 | Ledger atomics per entry on the pressure path | `cycle/queue.rs` | leave as is |
| 5 | Debug premise check in validation is quadratic | `cycle/validation.rs` | T1, linear in-degree array — done, `1e2306f` |
| 6 | `shadow::subtract` clamps at zero silently | `cycle/shadow.rs` | T1, `debug_assert` — done, `ba616d6` |

### 1. Arena waste at the smallest size class

`shadow::bytes_for(4080)` is 16 408 bytes. Three such arrays take 49 224
of a 65 280-byte payload; the 16 056-byte tail (24.6 %) is abandoned by
`TraceScratchArena::grow`, which never returns to an earlier block. At
class 32 the loss is 11.9 %, at class 64 it is 5.3 %, and it falls from
there.

Change agreed: two cursors in one block, row arrays from the front and
worklist segments (4112 bytes) from the back; `grow` when they meet. The
loss falls to under one segment per block. `residue` counts both ends.
Memory only, no time; noticeable only on heaps with many small-class
blocks.

### 2. Scan resolves the row twice

`classify_and_schedule_entity` finds the child's row, recolours it and
pushes the entity alone. The loop head then calls
`find_initialized_row_for_entity` on the popped entity, which is the
same `resolve_edge_target` plus bitmap test plus row read. For a retained
block it is also the registry lock a second time.

Change agreed: the worklist entry becomes the pair (entity, row pointer);
the pop reads the colour through the pointer. The pointer and not the
colour, because another path can recolour the row between push and pop
(`scan.rs`, "The colour is re-read at expansion"). Mark does not read
the row at pop and carries the pointer for one entry shape. Cost: a
segment holds 256 entries in 4112 bytes, or 512 in 8208.

Discussed and refused: replacing the explicit worklist with recursion.
Per node the two are within a few nanoseconds and the direction is
unmeasured. The depth is the objection: it equals the longest chain in
the graph, a frame here carries `trace_cells`'s state and the closure's,
and the release profile is `panic = "abort"`, so a stack overflow ends
the process where an arena refusal ends the collection. A bounded
recursion that falls back to the worklist would be a second traversal
beside two that already duplicate one loop.

### 3. The retained registry

`retained::occupant_index` takes a process-global `Mutex<BTreeMap>` and
binary-searches an `Arc<[usize]>` for every edge whose child is in a
retained block. `resolve_edge_target` calls it per edge in mark and per
edge and per pop in scan. Arena resets on other threads take the same
lock to register blocks.

Verified on the day: the only production readers of the registry ask
about one block by address (`register`, `occupant_index`,
`occupant_count`, `has_occupant_index`, `pin`, `payload_freed`,
`occupant_freed`, `reset_pin_released`). `snapshot`, the one
enumeration, is reached only from `heap::for_each_entity_slot`, whose
sole non-test caller is `cells::heap_census`, itself `#[cfg(test)]`. The
registry was built for `rc-walk`'s whole-heap walk (first commit of the
file, `918cf1d`) and outlived it by the 2026-08-26 decision.

Edmond's ruling: the logic is wrong at the root; an arena belongs to its
thread, and the thread answers for its retained blocks. The worked
proposal is `dev/design/retained-index-ownership.md`. It is a design
change and needs an rfc entry before code; the plan's slice is S36.9e.

### 4. Ledger atomics on the pressure path

`append_to_overflow` performs one `gc_metadata::charge` inside
`ll_release`, and `drain_overflow` one `discharge` per entry, each a
process-global atomic. Reached only after the pool refused. The Critic's
second round on S36.9b already refused batching: the batched figure
overstated the high-water mark between transitions. Left as is.

### 5. Quadratic debug check

`validation::member_counts_cover_internal_edges` walks every member's
cells once per member: for a component of 381 members that is 381 × 381
cell walks, in debug builds and tests only. A linear form is one pass
over the edges with an in-degree array indexed by the member's position
in the already sorted slice. T1.

### 6. Silent clamp in `subtract`

`saturating_sub` is right for a dirty pass, and the synchronous owner
trace cannot read below zero. A `debug_assert!(count(word) >= edges)`
before the subtraction makes a double subtraction visible in test
builds. T1.

## Recorded elsewhere and not re-argued

- The row array reserves the whole block at first touch (S40 decides the
  row form).
- Thread exit loses candidates for the life of the process (S39.1).
- A refcount at 2^32 wraps in the ordinary build, and the exact test
  reads the wrapped value (`cycle/validation.rs`, module doc).
- A1 to A6 of `rfc/dev/ALGORITHM-AUDIT.md` block the collector worker.

## Open

Whether `ll_default_dispose` nulls an object's cells or only releases
them. `mark` requires a live root, the queue can hold a zero-count
corpse, and the answer decides whether the S36.7 driver must drop
zero-count roots before marking or may rely on the drain's sort
(`rfc/model/gc/cycle/questions.md`, Y12 clause 5). Not checked in this
review.
