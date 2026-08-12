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
  threaded driver. The re-check is two questions per edge:
  `Edge::still_designates_its_child` re-reads the recorded cell by its
  width — a sixteen-byte one has its flags tested beside the payload —
  and `Epoch::row_still_has_its_cells` asks whether the storage those
  cells came out of is still the row's. Block snapshots: `heap::snapshot_entity_blocks`;
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
- Strings: `src/string.rs` — both layouts of the string entity (kind 1),
  told apart by `STRING_OUT_OF_LINE` (bit 15, string-scoped) rather than
  by `COW`, which means only copy-on-write.
  Inline: `RcHeader | len (u32) | hash (u64) | bytes`, one
  allocation, fixed size, `ll_string_new`. Out of line:
  `… | capacity (u32) | … | data`, payload through the memory manager's
  buffer machinery, `ll_string_new_dynamic` and `ll_string_append`, heap
  or request arena only. Two things put a string in the second layout: a
  compiler proof of single ownership, which clears `COW` and lets an
  append write in place, and **content past what the category packs in
  one slot**, which keeps `COW` — `ll_string_new` and `new_uninit` make
  that choice against `routing::slot_limit`
  (`rfc/model/memory/large-entities.md`). `len` at +8 and `hash` at +16 in both, so only
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
- Arrays: `src/array/` — storage strategies 2 and 3 of
  `rfc/model/arrays.md`: the mixed vector (`vector.rs`) and the ordered
  hash designed in `rfc/model/arrays-hashtable.md`. **What a concurrent
  walker may read is `head.rs`**, a `StorageHead` holding the version
  bracket, the chunk, the two counts and the strategy tag. It is a
  **field of the entity**, `LLArray { rc, head, storage }`, for two
  reasons: the 2 → 3 migration replaces the representation under a walker
  mid-stride, and a mutating operation is reached through `&mut
  (*a).storage`, which would otherwise assert uniqueness over the very
  words the walker reads (`dev/DECISIONS.md`, 2026-08-11, twice — the
  second entry overturns the first's placement). So every table and
  vector operation over those words takes `head: &StorageHead` as a
  parameter, and `entity::as_table_mut` is the one place the disjoint
  pair is derived. `entity::Storage` is the union of the two private
  tails; the tag is stamped in one place, `entity::new_with_storage`,
  and the walker, the sever and the dispose dispatch on it. Two rules
  belong to whoever writes the chunk rather than to the head, and both
  are written where the head is: `used` never falls while `storage`
  stays the same — `Vector::sever_entries` is the one exemption and says
  why — and a release goes through the window like a move, both
  `dispose` bodies publishing the empty state inside one bracket
  (`dev/DECISIONS.md`, 2026-08-11). A vector keys on the position, so it stores no key, has
  no index, no holes and none of the hash's flood defences. One storage
  allocation holds `u32` index slots followed by a dense insertion-ordered
  array of 32-byte entries (`entry.rs`): `hash_or_key`, `key`, and the
  element Box, whose reserved bytes carry the collision link as a `u32` at
  +28. That link is the reason the element field is private and every
  write to it goes through `Entry::store_element` / `store_link`, which
  compose tag, flags and link into one relaxed atomic store — the width
  the collector's load uses — while `Entry::value` clears the bytes on the
  way out so a link never travels in a copy (`dev/DECISIONS.md`,
  2026-08-07).
  `table.rs` is the core — lookup, insert, remove, growth by doubling or by
  dropping the holes (one body, `move_entries`, and both into a freshly
  allocated chunk since S13.1: sliding entries inside the published one
  raced the collector's relaxed loads), and the flood backstop that
  escalates a table once to a keyed hash over the key bytes. It allocates no entity and calls no store
  barrier: both are `element.rs`'s, and `Table::insert` hands the
  displaced element back for that layer to release (S6.1).
  Storage is a **buffer-arena** chunk in the two long-lived categories,
  a request-arena body in a request array and an immortal-region
  allocation in an immortal one; both arenas split by size, so a storage
  over one block payload is a dedicated run. It
  never comes from `entity_alloc`, whose blocks the collector reads as
  entities.
  `element.rs` is the generic element layer above the table — above rather
  than inside it because what `Map` reuses is the table, and a table that
  canonicalised keys would be unusable there:
  `canonical_key` turns a numeric string into the integer
  key PHP means by it, and `get`, `set`, `append` and `unset` are the
  operations, each taking the holder's slot and the writes returning `bool`
  like every other store-side barrier. One function holds the separation
  composition for all three writes, `write_through` — publish the copy,
  spend its creation reference, drop the displaced original, in that order,
  because `drop_ref` runs `__destruct` bodies that can displace the copy
  from the slot just written (`dev/DECISIONS.md`, 2026-08-08). Three
  refusals report `false` with every array unchanged: the separation's
  copy, the publication of an arena COW value or key, and the table's
  growth; `append` has a fourth that allocates nothing, an append cursor
  with no successor left. `get` never separates and reads a boxed element
  through its box, and `make_ref` boxes one inside `write_through` —
  `element::box_element` is that composition, an element reference being
  a `ReferenceBox` because growth moves an entry — so `&$a[k]` separates
  before it boxes and an absent key is created as null first. The box is
  a heap entity whatever the array's category, so boxing an element of
  an arena array crosses the boundary twice: the element enters a
  longer-lived holder — copied if it is an arena COW
  value, counted as an escape otherwise — and the box enters the arena
  entry, which logs its release against the reset.
  A store into an element already in a reference state goes
  **through** the box (`barrier::ref_store`), and a copy shares that box
  only while a second name holds it: `entity::element_for_copy` unwraps a
  box whose refcount is one, which is where PHP collapses a reference and
  the only place it does (`fill_from`, S3.2 in `PLAN.md`).
  `entity.rs` is the wrapper supplying the `RcHeader`: an array carries no
  class pointer, the same construction as a string, because the entity
  kind already says what it is. Its children — elements **and** string
  keys — come from the one tracing stride, `walk::trace_cells`' Array arm,
  which reads the entries through `StorageHead::coherent` and
  `Table::entries_of`: a version counter brackets every move of an entry,
  and a walker that cannot get a coherent reading skips the array for that
  epoch rather than striding a fresh count over a stale chunk. Both ends of that bracket are ordered by
  a fence rather than by a release store and an acquire load, and
  `version_bracket_model.rs` is the loom model that exhibits what the
  other shape admits (`dev/WORKFLOW.md`, "Loom"). The version travels
  out with the reading: the walk answers with it, the epoch keeps one
  per walked row (`collector::Epoch::storage_versions`), and Phase 3
  asks it before re-reading any recorded cell — an address inside a
  chunk the array has left reads the walk's own value back, the epoch
  having parked the free (`Epoch::row_still_has_its_cells`). `entity::for_each_counted_child` is an
  adapter over it, and `ll_entity_die`'s Array arm goes through that; the
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
  depth, each child published through `barrier::publish_child` rather
  than retained bare — the retain, the category barrier and the reference
  reconciliation the array's four publications share, `store_into`'s
  value and key and `fill_from`'s (S6.2). Neither the copy nor the
  teardown recurses into nesting: both drain a `WorkList` in a buffer-
  arena chunk, because the depth is the caller's input and a frame set
  per level is a stack
  overflow (`dev/DECISIONS.md`, 2026-08-07 and 2026-08-08). The copy
  also **preserves the source's sharing** — one copy per distinct source
  entity, held once per entry naming it, through a source → copy
  association that is the generic `Table` used bare beside that list
  (`dev/DECISIONS.md`, "the deep copy preserves the source's sharing").
  A refusal anywhere in it hands the destination back through
  `object::destroy_unpublished`, the one door for an entity no slot ever
  named. An object chain still tears down through the machine stack.
  What is unbuilt is listed at the head of `PLAN.md`.
- The event journal: `src/journal/` — what the runtime did, as
  32-byte records in a ring per thread, read back by marking a window
  with every ring's cursor before and after (`dev/design/debug-modes.md`
  §9). No global ring and no global sequence number, so the write path
  takes no atomic read-modify-write and a hard-allocating thread cannot
  evict the records of the thread under investigation; the price is that
  records in different rings have no order between them. **An overflowed
  window answers `unknown`, never `none`** — the reader's re-read of the
  cursor after copying a record decides it, and that re-read is a bracket
  ordered by a **pair** of fences rather than by a release store and an
  acquire load, the array table's version bracket again
  (`journal/ring_model.rs` is the loom model, and three of its four
  combinations admit a lapped record). A ring the registry has *freed*
  answers `unknown` too, by a count of evictions each `Mark` carries.
  A ring comes from `ll_malloc` on the thread's first record, outlives its
  thread on the registry's retired list (`journal::retire_thread_ring`,
  called last in `heap::ll_thread_exit`), and the oldest beyond
  `RETIRED_KEPT` are freed. A `Mark` names rings by identity, never by
  address, and a read resolves them under the registry's lock — a freed
  ring's block goes back to the allocator. A refusal and a retirement both
  **close** the thread's cell, which is why it has three states and not
  two: a thread journals nothing after its exit, and one refusal is not
  retried, though it **is** counted: a refused thread is in no window, and
  the count is what keeps its silence from reading as inactivity. The
  ring is retired by the **last act** of `ll_thread_exit`, after every
  step of the teardown — the reserve and the pool's thread cache are
  drained there by hand rather than by their own destructors, which run
  later — so a `__destruct` body's records and every block handover are
  inside it and a window over a thread's death is complete. Past that act
  completeness ends and honesty does not: a record arriving on a closed
  slot is counted and reported as `Lost`. `ll_thread_init` reopens the cell, so a pool thread's
  second life journals into a ring of its own. An evicted ring is freed by
  the next thread to journal or to mark, never by one inside its own exit,
  whose parked backlog is gone by then — the three-valued
  `heap::ExitPhase` is what tells those apart, a boolean having conflated
  a heap rebuilt mid-exit with a new life. The module is in every build; what the
  `debug-journal` feature gates is the record **sites**, and those are
  built now. `journal/kinds.rs` holds the vocabulary, the enabled mask
  and `journal_event!`, which is how a site is written: it expands to
  nothing without the feature, and with it evaluates its payload only
  after the mask says the kind is on, so a disabled site costs a load and
  a branch and reads nothing. Fourteen sites carry §9.5's default set —
  entity birth at `refcount::publish_header` and death at each kind's own
  teardown body (`ll_object_die`, `string_die`, `array_die`,
  `reference_die`, `weakref_die`, since the kind switch above them is
  reached by one of the two object doors only and by a nested array not
  at all); the arena reset's two ends in `promote::arena_reset_full`; a
  block's two in `BlockPool::get` and `put`, the second carrying the kind
  the block arrived with, which is how §9.5's third block event is asked
  for; the thread's two in `heap`; and the epoch's two in `collector`,
  under `rc-walk`. A site must not sit anywhere the *first* record's path
  reaches, that one initialising the thread, allocating and locking —
  which is why `BlockPool::put` stages its overflow flush in a fixed
  array and pushes with nothing borrowed (`dev/DECISIONS.md`,
  2026-08-08).
- Category → allocator routing: `src/memory/routing.rs` — the one place
  that answers where a memory category's bytes come from.
  `entity_alloc_in` for anything with an `RcHeader`, `body_alloc` /
  `body_ensure` / `body_free` for the bytes an entity owns outside its
  own slot. A factory that has a category to refuse refuses it itself,
  before calling.
- One entity per allocation: `src/memory/large_entity.rs` — where an
  entity goes when it is past what its category's allocator packs into a
  shared block (`rfc/model/memory/large-entities.md`). It keeps its
  inline layout whole as the sole occupant of a block-aligned allocation
  whose first line is a block header of its own kind pair, entity at
  `+LINE_SIZE`: `BLOCK_KIND_ENTITY_LARGE` for a pooled block up to one
  payload, `BLOCK_KIND_ENTITY_LARGE_RUN` for an OS-direct run above it.
  The kinds are new rather than reused because `BLOCK_KIND_LARGE`/
  `LARGE_RUN` also hold raw C buffers, and a walker reading one as an
  `RcHeader` is what block-kind segregation exists to prevent.
  **The commissioning rule is the zero pass**, not the publication order:
  the entity's first 8 bytes are zeroed before the kind is stored, so a
  commissioned block reads as an empty slot until a factory publishes —
  which is why a run may be entered into the module's registry at
  allocation. Discovery follows the split: the pooled half rides the
  region scan both enumerators already perform, a run is found from that
  registry and nowhere else, and both carry `slots = 1`, which is
  soundness rather than economy. `deferred_free` parks both kinds; for a
  run that is soundness too, its memory being unmapped at free while a
  snapshot still addresses it. The doors above it are
  `heap::entity_alloc` past `MAX_SMALL` and `Arena::alloc_entity` past
  one block payload; the arena logs the run it takes, so an unpromoted
  corpse dies with the reset, and a survivor is handed over instead —
  not stamped `BLOCK_KIND_RETAINED`, not indexed in `retained.rs`, out
  of the arena's log through `forget_large` (`promote.rs`). Stamping it
  would send a multi-megabyte run to the 64 KiB block pool at the
  entity's death, and the omission is silent, which is why that rule
  carries a test of its own.
- Retained-block object indexes: `src/memory/retained.rs` — block
  address → its occupants, sorted, and how many of them are still
  alive. Registered by `promote` at reset, read by both of `heap`'s
  enumerators. This is what makes a bump-filled former-arena block
  walkable at all; without it its occupants are root sources and a ring
  among them never dies (`rfc/model/gc/retained-block-walk.md`, built
  2026-08-03). The live count is what returns the block: each
  occupant's death reports through `stdapi::ll_free`'s retained arm, and
  the last one drops the index, restamps the block and hands it to the
  pool (`dev/DECISIONS.md`, 2026-08-08). Two shapes sit beside that — a
  block retained for a **payload** the reset could not carry out waits
  for that payload's own free the way it waits for an occupant's death,
  the pin being a count because one block can hold several survivors'
  payloads, and a block whose every occupant died inside the reset is
  handed over by the reset itself, after `finish_reset`. The payload's
  free arrives through `buffer_arena::buffer_free_longlived_payload`,
  which reads a retained block under the pointer, leaves the bytes where
  they are — former arena memory has no free list — and reclaims the
  block instead; during an epoch that call parks like any other.
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
  kind 3; it takes neither a category nor a context, because a box is a
  GC-heap entity in every case — `dev/DECISIONS.md`, 2026-08-08),
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
- Tests: one file per group, beside the module. `src/foo.rs` declares
  `#[cfg(test)] mod tests;`; `src/foo/tests.rs` holds the fixtures the
  groups share and, after them, one `mod` declaration per group; each
  group is `src/foo/tests/<group>.rs`, named by what it pins rather than
  by which function it calls, and opening with the `//!` that says it.
  The declarations come after the fixtures because a `macro_rules!`
  fixture is textually scoped (`dev/DECISIONS.md`, "a test file holds one
  group", where the rest of the layout's price is recorded too). No
  `tests/` directory at the crate root: every test is a unit test and
  reads crate-internal state. A fixture a second module needs is in
  `src/test_support.rs`, and one only the array modules need is in
  `src/array/testing.rs`. The two `loom` models are outside this layout
  and stay so: each is a hand-written copy of a protocol rather than a
  group of tests over a module, and each is compiled only under
  `--cfg loom`.
- Benches: `benches/alloc.rs`, `benches/standard.rs`,
  `benches/lifecycle.rs` (object create/release GC-protocol tax, both
  configs), `benches/strings.rs` (hash across the function's branch
  boundaries, create-hash-die, and the append loop in both memory
  categories — the harness the bump-top growth optimization was blocked
  on); collector-side epoch cost probe:
  `collector::tests::the_epoch_as_a_whole::measure_epoch_cost` (ignored, run with
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

`dev/DECISIONS.md` — 2026-08-07: rc-trace's candidate gate is a **set of
kinds** (`refcount::CANDIDATE_KINDS`, `{Object, Array, Reference, Lazy}`)
rather than a mask over their codes, because no mask admits `Reference 011`
while excluding `String 001`; a kind is in the set exactly when it holds
counted slots a cycle can close through, so the policy stops depending on
which numbers the kinds were given. 2026-07-26: GC strategy is the build-time `rc-walk`
cargo feature (the two collectors share header bits; verification runs
both configurations); entity blocks as a second heap
population (rc-walk step 1). 2026-07-20: arena handle as a raw pointer;
trailing inline data through raw pointers; block header split by access
rule; cold concurrent structures take a lock rather than a CAS loop;
Miri against a UNIX target. 2026-07-21: the barrier owns the whole slot
and publishes it before teardown; a destructor is owed by the
constructor, not the factory; a refused destructor record fails the
creation; the store barrier is funded by a per-thread reserve.

## Outside code

`dev/RESEARCH.md` — what was read in other projects, at which revision,
and what of it applies here. Entries so far: Concurrency Kit (the seqlock
that found the version-bracket defect ahead of S2.7, the epoch proof,
event counts, the per-bucket probe bound), `ankerl::unordered_dense` (the
fingerprint byte, for the array-performance stage), mimalloc and snmalloc
(the two answers to cross-thread free), and rpmalloc 2.0.1, read from
source — reallocation in place, which `ll_realloc` never does; the band
between the 8 KiB class and a whole block; the zeroed-block flag that
would retire `refill`'s per-slot pass; decommit on a threshold; and the
length carried in the cross-thread free list. Read it before evaluating
one of those again.

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
(2026-07-20); an entity killed at refcount 1, which read as a census
flake (2026-08-06); a test heavy enough to stop the Miri gate from
finishing (2026-08-08); a guard checking a different limit from the call
below it (2026-08-11).

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
