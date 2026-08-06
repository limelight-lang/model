# Index

Map of the project for an agent: where to look, so the whole tree does
not have to be read. Pointers only — nothing is explained here, only
located.

## Modules

Knowledge map: `dev/ARCHITECTURE.md` — how the crate works *together*:
layers and the sanctioned upward edges, the per-module knowledge table
("does not know" is the contract), shared resources including the
header-bit ledger, the five end-to-end paths, and the cross-module
invariants. Module docs at the top of each file remain the normative
detail; `src/memory/heap.rs` and `src/promote.rs` carry the fullest
ones.

`docs/memory-manager.md` covers `src/memory/` end to end — layers, block
header layout and why, the heap, cross-thread free, abandonment, the
arena and its reset fixpoint, plus a closing list of what is *not*
implemented. `memory/mod.rs` declares the module implements it, so it is
normative and must move with the code (`dev/WORKFLOW.md`). Superseded
versions live in `docs/history/`, marked at the top.

## Entry points

- rc-walk collector side (`rc-walk` builds): `src/collector.rs` — the
  epoch state machine (Phases 1–3: snapshot, walk with the three-way
  classification, judge, condemn, snapshot-compare re-check, verdict
  posting), steppable for the forcing harness; `run_epoch` is the
  threaded driver. Block snapshots: `heap::snapshot_entity_blocks`;
  the walk's child test is the dense census (`collector::census_row`).
  Trigger is an explicit call — thresholds are unmeasured.
- Static blocks and thread exit: `src/static_block.rs` — the per-thread
  registry and the teardown pass that releases each block's roots at
  exit (A6, `rfc/model/classes.md` "Teardown at thread exit"). The order
  the whole exit sequence runs in is fixed in `heap::ll_thread_exit`,
  and the reason it must be fixed there — TLS destructor order is
  unspecified — is `dev/DECISIONS.md`, 2026-08-03. **Rule for anything
  new on that path:** no `thread_local!` it can reach may have drop
  glue.
- Strings: `src/string.rs` — both layouts of the string entity (kind 1).
  Inline (`COW = 1`): `RcHeader | len (u32) | hash (u64) | bytes`, one
  allocation, fixed size, `ll_string_new`. Dynamic (`COW = 0`):
  `… | capacity (u32) | … | data`, payload through the memory manager's
  buffer machinery, `ll_string_new_dynamic` and `ll_string_append`, heap
  or request arena only. `len` at +8 and `hash` at +16 in both, so only
  byte access and teardown branch — `string_bytes` is the accessor that
  does, and `LLString::hash` goes through it, since hashing the inline
  offset on a dynamic string would hash the payload's address.
  `fits` is the single 4 GiB length gate every creation and growth path
  passes; the hash the field caches is `hash::hash_bytes`, below. The COW
  write barrier is
  `object::ll_cow_separate` (whether to copy) plus `string::separate`
  (how): the copy's category comes from the holder, and it comes back
  at +1. An interned name is an inline string built through the same
  `init_at`: `src/intern.rs` is the table, not a second layout.
  An arena dynamic string that survives the reset **takes its payload
  with it**: `promote::carry_external_memory` asks one kind-dispatched
  question and `string::carry_payload_out_of` answers it — an OS-direct
  run transfers, an in-block payload is copied, and a refused copy
  retains the block instead (`dev/DECISIONS.md`, 2026-08-04).
- Interpolated string templates: `src/template.rs` — the parts of one
  interpolation site are a `TemplateShape`, static data the compiler
  emits once and never frees; the instance is
  `RcHeader | class | shape | Value[n]`, an ordinary entity under **one**
  class for every site (`dev/DECISIONS.md`, 2026-08-05, amending
  `rfc/model/strings.md` rule 3). Because the value count is the
  instance's and not the class's, the count is read from the shape — and
  since the walk-duplication refactor it is read there in **one** place,
  `object::for_each_counted_cell`, which serves the quiescent tracer, the
  collector's relaxed one and the sever alike (`walk::trace_cells`,
  `walk::sever_cells`). It was three strides, each branching on
  `CLASS_TEMPLATE` separately, and a walk that strides an object's slots
  without knowing this leaks rather than crashes — which is why the third
  one was found by review and not by the suite. `flatten` measures
  everything, allocates once through `string::new_uninit`, and writes
  each piece into place; it refuses a float or an object, each waiting on
  something outside this crate.
- The hash of a byte string: `src/hash/` — `hash_bytes` is what every
  hashed thing in the runtime goes through (a string's cached hash now,
  array keys later), and it is **rapidhash V3**, ported in
  `hash/rapidhash.rs` from the reference header vendored at
  `vendor/rapidhash/`. Which function hashes is a build-time choice
  (`rfc/model/strings.md`), because the compiler is meant to fold the
  hash of a literal key and the two sides agree only if built from one
  definition. The author publishes no test vectors, so
  `vendor/rapidhash/generate_vectors.c` compiles his header and prints
  `src/hash/vectors.rs`; that table is the only thing separating a port
  from a hash of its own invention, since a mistranscribed constant
  still hashes well and fails nothing else. Zero is never returned —
  it is the string field's "not computed" sentinel.

  `hash/seed.rs` holds the seed and the `hash-folding` cargo feature.
  Off (the default): the seed is drawn from the OS per process and the
  compiler folds nothing. On: the seed is fixed from `LL_HASH_SEED`,
  the compiler gets the same value and folds every literal key's hash,
  and the artifact carries the seed with it. One option, because a
  compiler that folds has to know the seed while it compiles. `STAMP`
  and `ll_hash_stamp_matches` are what keep a program folded under one
  seed from silently missing against a runtime holding another; the
  program's half of that check is owed by the compiler. Neither arm
  defends against hash flooding — that is the table's debt
  (`dev/DECISIONS.md`, 2026-08-04).
- Arrays: `src/array/` — storage strategy 3 of `rfc/model/arrays.md`, the
  ordered hash designed in `rfc/model/arrays-hashtable.md`. One storage
  allocation holds `u32` index slots followed by a dense insertion-ordered
  array of 40-byte entries (`entry.rs`); the collision link is an explicit
  `next` index rather than state threaded through the element, which the
  store barrier would sever by writing all sixteen bytes of a value.
  `table.rs` is the core — lookup, insert, remove, growth by compaction or
  doubling, the flood backstop that escalates a table once to a keyed hash
  over the key bytes, and element references, which are `ReferenceBox`es
  because growth moves an entry. Storage is a **buffer-arena** chunk in
  the two long-lived categories, a request-arena body in a request array
  and an immortal-region allocation in an immortal one; both arenas split
  by size, so a storage over one block payload is a dedicated run. It
  never comes from `entity_alloc`, whose blocks the collector reads as
  entities.
  `entity.rs` is the wrapper supplying the `RcHeader`: an array carries no
  class pointer, the same construction as a string, because the entity
  kind already says what it is. Its children — elements **and** string
  keys — come from one walk, `entity::for_each_counted_child`, which both
  `ll_entity_die`'s Array arm and `walk::trace_entity` go through; the
  release side uses the barrier's `drop_ref`, so a child the array held
  last is torn down rather than only decremented. An arena array that
  survives a reset **takes its storage with it**, the same two routes a
  string's payload takes (`table::Table::carry_out_of`, reached from
  `promote::carry_external_memory`); it gets there as a child of an
  escapee, never on its own, an array being COW and therefore copied at
  the barrier rather than counted as an escapee. Both COW doors have their Array arm
  now: `object::ll_cow_separate` separates a shared array and
  `object::escape_copy` copies an arena one out, and they are one body —
  `array::entity::separate` — with the destination category supplying the
  depth, each child published through `barrier::store_category_barrier`
  rather than retained bare. Its recursion is still the machine stack's.
  What is unbuilt is listed at the head of `PLAN.md`.
- Retained-block object indexes: `src/memory/retained.rs` — block
  address → its occupants, sorted. Registered by `promote` at reset,
  read by both of `heap`'s enumerators. This is what makes a
  bump-filled former-arena block walkable at all; without it its
  occupants are root sources and a ring among them never dies
  (`rfc/model/gc/retained-block-walk.md`, built 2026-08-03).
- rc-walk epoch protocol, mutator side (`rc-walk` builds):
  `src/epoch.rs` — the soft-handshake ack, the verdict message queue
  (confirm + acquit), and the non-reentrant checkpoint; checkpoints
  ride the death branch of `ll_release` and `ll_gc_maybe_collect`;
  batched runs split the checkpoint — `ll_gc_checkpoint_ack` before
  the run, `ll_release_batch` per reference, `ll_gc_checkpoint` after
  it (decision 2026-07-28; `ll_release_vector` same). The drain it
  dispatches to is `walk.rs`'s `drain_confirmed` (confirmations only —
  acquittals post nothing since eager death, 2026-07-27).
- Entity walking (rc-walk build steps 1–2): `src/walk.rs` —
  kind-dispatched tracer, heap census, and `walk::collect_cycles` (the
  synchronous whole-heap collection with the Phase-4 exact-test drain)
  over `memory::heap::for_each_entity_slot`; entity blocks and the
  region registry are in `heap.rs`/`block_pool.rs` (design:
  `rfc/model/gc/rc-walk.md`; decision entry 2026-07-26).
- Weak references (rc-walk step 4): `src/weak.rs` — the kind-5 weak
  cell, the per-thread weak table, death notification (`notify_death` /
  `notify_members` / `drain_arena_weak_log`) and the
  `ll_weakref_create` / `ll_weakref_get` ABI. Notification sites live
  in `object.rs` (dispose phase 2, first act), both collectors, and
  arena reset. Design: `rfc/model/weak-references.md`.
- C ABI surface: `src/memory/context.rs` (arena + context),
  `src/object.rs` (`ll_object_new` factory, `ll_object_new_in` —
  construct into a reserved cell, `ll_object_constructed` —
  the end-of-construction hook that registers the destructor,
  `ll_entity_die` — the kind-switched death for a bare entity pointer,
  `ll_release_vector` — one call per release batch),
  `src/memory/heap.rs` (`ll_entity_reserve` / `ll_entity_cells_return`
  — bulk cell reservation, `rfc/model/memory/bulk-operations.md`),
  `src/reference.rs` (`ll_reference_new` — the `&` reference box,
  kind 3),
  `src/memory/stdapi.rs` (`ll_malloc`/`ll_c_free`/aligned),
  `src/memory/barrier.rs` (`ll_store_ptr`/`ll_store_box`/`ll_drop`/
  `ll_ref_store`), `src/object.rs`
  (`ll_object_die`, dispatching to the descriptor's `dispose` —
  `ll_default_dispose` the stand-in), `src/refcount.rs`
  (`ll_retain`/`ll_release`).
- Crate root: `src/lib.rs`. Built as `rlib` + `staticlib` for the
  C++/LLVM layer, and emitted as LLVM bitcode to be merged with
  compiler-generated IR — the route the hot paths take into compiled PHP
  code, which is why a call across the C ABI is not the barrier it looks
  like (`ll_retain` inlines away after `opt -O2`). Commands and what was
  verified: `README.md`, "LLVM IR export". The decision behind it:
  `rfc/runtime/implementation-language.md`.
- Tests: inline `#[cfg(test)]` per module, no `tests/` directory.
- Benches: `benches/alloc.rs`, `benches/standard.rs`,
  `benches/lifecycle.rs` (object create/release GC-protocol tax, both
  configs), `benches/strings.rs` (hash across the function's branch
  boundaries, create-hash-die, and the append loop in both memory
  categories — the harness the bump-top growth optimization was blocked
  on); collector-side epoch cost probe:
  `collector::tests::measure_epoch_cost` (ignored, run with
  `--ignored`, release mode); external probes in `bench-external/`.

`src/memory/reserve.rs` — the per-thread block reserve that keeps the
store barrier's log growth from failing; drawn in `Arena::grow_log`,
refilled at `ll_gc_maybe_collect`. Design in
`rfc/runtime/exceptions.md`, "The log reserve protocol".

`src/memory/deferred_free.rs` (`rc-walk` builds) — the GC activity bit
and the parked-free list: while an epoch is in flight, `ll_free` parks
instead of recycling (slot identity for the walker); the owning thread
flushes after the epoch. A **buffer-arena chunk** never passes `ll_free`,
so `buffer_arena::buffer_free_longlived_payload` makes the same test in
its own branch and parks the whole call; that is why a parked record
carries `(pointer, size)` (`dev/DECISIONS.md`, 2026-08-04). Design:
`rfc/model/gc/rc-walk.md`, "Deferred physical release";
`rfc/model/gc/heap-design.md`.

Buffer arena (`src/memory/buffer_arena.rs`) — where an entity's
out-of-line body lives: a string's payload and an array's table storage.
Bump allocation with a per-block free list, and **the object
heap's ownership rules**: per-block `owner`, per-block lock-free stack
for frees from other threads, owner-written `live`, hand-over to a global
abandoned list at thread exit, adoption on the refill path
(`dev/DECISIONS.md`, 2026-08-04; `rfc/model/memory/buffers.md`).
`buffer_ensure_longlived` grows a payload that is still the last chunk
bumped by moving the bump, ahead of hole reuse in every pressure mode —
an append loop moves its payload once instead of nine times, which the
clock cannot resolve and a count can (`dev/BENCHMARKS.md`, 2026-08-05).
Each block carries its own bump cursor, so an adopted block is reused
and not only held: rotation takes an adopted tail, then any owned tail
(`resume_owned`), and only then the pool — the reverse of `heap.rs`'s
order, for a reason worth reading before changing it — while `critical`
searches the free lists of the whole owned chain under one budget
(`dev/DECISIONS.md`, 2026-08-05).

Arena reset and promotion: `src/promote.rs` — the fixpoint, the counting
pass and block retention. Children come from `walk::trace_entity`, so a
reference box's referent is promoted with it; a COW survivor's count is
left alone during the fixpoint (destructors read it) and settled once
afterwards by `reconcile_cow_counts` (`dev/DECISIONS.md`, 2026-08-04).

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

`dev/DECISIONS.md` — 2026-07-26: GC strategy is the build-time `rc-walk`
cargo feature (the two collectors share header bits; verification runs
both configurations); entity blocks as a second heap
population (rc-walk step 1). 2026-07-20: arena handle as a raw pointer;
trailing inline data through raw pointers; block header split by access
rule; cold concurrent structures take a lock rather than a CAS loop;
Miri against a UNIX target. 2026-07-21: the barrier owns the whole slot
and publishes it before teardown; a destructor is owed by the
constructor, not the factory; a refused destructor record fails the
creation; the store barrier is funded by a per-thread reserve.

## Diagrams

`docs/architecture.md` — the visual companion to `dev/ARCHITECTURE.md`
(which stays the source of truth): PlantUML layer picture, full wiring
graph, and the five end-to-end paths as sequence diagrams. Rendered on
demand; no images committed.

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
