# Index

Map of the project for an agent: where to look, so the whole tree does
not have to be read. Pointers only — nothing is explained here, only
located.

## Modules

Knowledge map: `dev/ARCHITECTURE.md` — **not written yet, and agreed to
be written** (2026-07-21). What is missing is the document that says how
the crate works *together*, as opposed to where things are (this file),
why they were decided (`DECISIONS.md`) or what one file does (its module
doc): the layers and who owns whom, the four end-to-end paths
(allocation, reference store, object death, arena reset), and the
invariants that live *between* modules rather than inside one — the
block header being a tagged union with `kind` at offset 0, the pool's
`next` overlaying the heap's `used`, an escapee's hold-count living in
its `refcount`, who owns a block at each moment. Those cross-module
invariants are exactly where documentation drifted this week, because
there is nowhere to write them down. Until it exists, the module docs at
the top of each file are the map; `src/memory/heap.rs` and
`src/promote.rs` carry the fullest ones.

`docs/memory-manager.md` covers `src/memory/` end to end — layers, block
header layout and why, the heap, cross-thread free, abandonment, the
arena and its reset fixpoint, plus a closing list of what is *not*
implemented. `memory/mod.rs` declares the module implements it, so it is
normative and must move with the code (`dev/WORKFLOW.md`). Superseded
versions live in `docs/history/`, marked at the top.

## Entry points

- C ABI surface: `src/memory/context.rs` (arena + context),
  `src/object.rs` (`ll_object_new` factory, `ll_object_constructed` —
  the end-of-construction hook that registers the destructor),
  `src/memory/stdapi.rs` (`ll_malloc`/`ll_c_free`/aligned),
  `src/memory/barrier.rs` (`ll_store_ptr`/`ll_store_box`/`ll_drop`/
  `ll_ref_store`), `src/object.rs`
  (`ll_object_die`, dispatching to the descriptor's `dispose` —
  `ll_default_dispose` the stand-in), `src/refcount.rs`
  (`ll_retain`/`ll_release`).
- Crate root: `src/lib.rs`. Built as `rlib` + `staticlib` for the
  C++/LLVM layer.
- Tests: inline `#[cfg(test)]` per module, no `tests/` directory.
- Benches: `benches/alloc.rs`, `benches/standard.rs`; external probes
  in `bench-external/`.

`src/memory/reserve.rs` — the per-thread block reserve that keeps the
store barrier's log growth from failing; drawn in `Arena::grow_log`,
refilled at `ll_gc_maybe_collect`. Design in
`rfc/runtime/exceptions.md`, "The log reserve protocol".

## Hot paths

- Allocation: `Heap::alloc` → `ll_alloc`, expected to inline fully,
  cold tails split with `#[cold] #[inline(never)]`.
- Local free: `Heap::free`, including the `owner` check. Split into a
  fast path and out-of-line tails like `alloc` — except `relink_unfull`,
  which is out of line but not `#[cold]`, the boundary being crossed too
  often for that. Measured as no change outside the noise floor (H11 in
  `dev/BENCHMARKS.md`).
- Store barrier: the micro-ops `store_ptr` / `store_box` (publish) and
  `drop_ref` (release the displaced entity), and the `ref_store`
  composition; ABI `ll_store_ptr` / `ll_store_box` / `ll_drop` /
  `ll_ref_store`.
- Arena bump: `Arena::alloc` → `ll_arena_alloc`.

Measured by `cargo bench --bench standard -- our_heap` (larson,
rptest); headline comparison in `benches/RESULTS.md`, change log in
`dev/BENCHMARKS.md`.

## Layout contracts (pinned by tests)

- Block header halves and cache lines: `memory::heap::tests::`
  `block_header_halves_are_laid_out_as_the_design_requires`.
- `RcHeader` 8 bytes at offset 0: `refcount::tests::`
  `header_is_8_bytes_at_offset_zero`.
- `Value` 16 bytes, fixed offsets: `value::tests::`
  `box_is_16_bytes_with_fixed_offsets`.

## Key decisions

`dev/DECISIONS.md` — 2026-07-20: arena handle as a raw pointer;
trailing inline data through raw pointers; block header split by access
rule; cold concurrent structures take a lock rather than a CAS loop;
Miri against a UNIX target. 2026-07-21: the barrier owns the whole slot
and publishes it before teardown; a destructor is owed by the
constructor, not the factory; a refused destructor record fails the
creation; the store barrier is funded by a per-thread reserve.

## Diagrams

`dev/design/debug-modes.md` — observability and debug levels: object
registry, lifetimes, shadow metadata, integrity checks, metrics export.
Design only, nothing implemented.

## Traps

`dev/POSTMORTEM.md` — benchmarking against a stale baseline
(2026-07-20).

Also worth knowing before touching this crate:

- Formal-UB defects here all pass `cargo test`. Only Miri sees them,
  and only against a UNIX target — see `dev/WORKFLOW.md`.
- Miri is blind to leaks here (`-Zmiri-ignore-leaks` is mandatory) and
  runs in permissive provenance in the pointer-heavy modules, so a
  clean run is not proof there.
- The block header is a tagged union shared with the pool's
  `BlockHeader`: `kind` must stay at offset 0, and the pool's `next`
  overlays the heap's `used`.

## Conventions

`dev/WORKFLOW.md` — branches, commits, the required verification
sequence, test rules, Miri invocation.

Not obvious from the code: `AUDIT.md` and `.idea/` are deliberately
untracked and must stay so — this repository is public and the audit
lists unfixed defects. Design lives in the separate `limelight-lang/rfc`
repo and is kept in sync with behaviour changes.
