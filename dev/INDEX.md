# Index

Map of the project for an agent: where to look, so the whole tree does
not have to be read. Pointers only — nothing is explained here, only
located.

## Modules

Knowledge map: `dev/ARCHITECTURE.md` — how the crate works *together*:
layers and the sanctioned upward edges, the per-module knowledge table
("does not know" is the contract), shared resources including the
header-bit ledger, the end-to-end paths, and the cross-module
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

- **No collection runs yet.** `rc-walk`, `rc-trace` and `rc-satb` were
  deleted on 2026-08-26 and the design in force, `rc-cycle`
  (`rfc/model/gc/rc-cycle.md`), is built in parts: a garbage ring is
  retained, acyclic garbage dies by counting. What survives of the old
  code and why is `src/lib.rs`'s module doc and `dev/DECISIONS.md`,
  2026-08-26; the code is on the branch `archive/pre-rc-cycle`. `PLAN.md`
  S34 through S40 build the replacement in `src/cycle/`. What each module
  does and what it may not know is `dev/ARCHITECTURE.md`'s `cycle` row;
  where each one is:

  | module | what is there | production caller |
  |---|---|---|
  | `queue` | the per-thread candidate queue, its base block, spares and overflow buffer, and the cell that lends the collection workspace | `refcount::release_word`, `gc`'s poll |
  | `deferred_slot_reuse` | `ActiveTrace`, the physical-return barrier, its stack of withheld returns through the dead entities, and the detached candidate batch a collection traces | `stdapi::ll_free` |
  | `arena` | `TraceScratchArena`, the collection's bump over the thread's workspace behind the withheld returns' region, the worklist it holds, and `ensure_row`/`find_initialized_row` | none until S36.7 |
  | `shadow` | the row: two bits of colour over thirty of working count | none |
  | `row` | `resolve_edge_target`, which row a traced edge resolves to | none |
  | `mark` | the trace: trial deletion over the rows | none |
  | `members` | the entities a pressure collection takes out of its rows before the blocks go back, and the fixed region of the workspace they stand in | none until S36.7 |
  | `records` | `RecordChain`, the segmented record chain the trace's worklist is built on, and its one user since the withheld returns took the stack | none |
  | `stack` | the trace worklist, 256-entry segments out of the arena | none |
  | `scan` | the classification: live spreads, zero reads as potentially unreachable, a reached row is raised | none |
  | `trace` | both phases over one detached batch, in the order the rows require: every root marks before any root scans | none until S36.7 |
  | `validation` | the owner's exact validation of one component, and the zero-count-member rule | none until S36.7; `cycle::finalization` is what acts on its answer |
  | `finalization` | the guard reference on every member of a confirmed component and the weak cells naming them, nulled before any destructor | none until S36.7 |
  | `density` | test builds only: what share of a touched block's slots one trace met, and, in `tests::the_death_loads`, what the window's close costs in time and in cache lines | none |

  Two numbers about a row, both pinned by tests rather than by prose: a
  count at the field's bound is a floor and absorbs every subtraction, so
  an entity referenced more times than thirty bits hold is conservatively
  live and no scan may mark it potentially unreachable (`shadow::is_saturated`); and a block's
  first touch writes 121 bytes at the widest size class against the
  16 320 its rows reserve, which is what the group bitmap bought
  (`dev/BENCHMARKS.md`, 2026-08-27).

  A member list is derived from the potentially unreachable rows on one
  path only, and S36.12's slice (b) builds it: the collection an allocation
  failure started harvests them into a fixed region of the workspace, while
  the ordinary collection off the poll keeps its rows through the teardown
  and reads them directly (`dev/DECISIONS.md`, "the member list is the
  pressure path's alone").
- What a slot's first eight bytes read: `refcount::slot_state`, three
  states over the count and one flag — live, dead in place, free. A slot is
  dead in place when `ll_free` has taken it and nobody has handed it back
  (`refcount::DEAD_IN_PLACE`), which covers a slot on its block's free list
  and one whose return a trace window is withholding. The head of `ll_free`
  reads the bit and refuses a free that finds it up, so a second free of a
  size-class slot or a retained survivor does nothing for as long as the slot
  holds the first free; past the publication of its next occupant a free of the
  old pointer is undefined, as a free of any reissued memory is. A pooled large
  entity's second free is absorbed by the pool's re-stamp of the block kind,
  and an OS-direct run's memory is unmapped by its first free. What hands a
  slot back is `refcount::publish_header`,
  the window's close ahead of its return, the reset window's flush and
  `memory::stdapi::free_unpublished`
  (`dev/DECISIONS.md`, "a second `ll_free` of an entity is refused, and the
  mark is the bit it is refused on"). The count still reads zero under the
  bit, and a guard test bans the two-way occupancy test outside `refcount`.
- Giving back memory that was never published as an entity:
  `memory::stdapi::free_unpublished`, which hands the slot back and frees it.
  Its callers are the return of unconsumed reserved cells, the template
  factory's free after a refused store, and the tests that allocate from the
  entity heap without publishing. A plain `ll_free` there is read as a repeat
  whenever the allocator served a recycled slot, and the slot is lost.
- What withholds a return, which the header says nothing about:
  `cycle::deferred_slot_reuse::classify` returns a death at once where the
  collection never met its block and stacks it where it did. The window's
  close pops that stack — threaded through the dead entities themselves at
  `heap::FREE_LIST_LINK_OFFSET`, headed in the withheld returns' control
  line, which is the whole of the region the workspace keeps for it.
- The candidate gate: `refcount::CANDIDATE_GATE_MASK` and
  `may_become_a_candidate`,
  read on the non-zero decrement in `release_word`. Five conditions in
  one mask, each of them "this bit is zero" — GC-heap category, a kind a
  ring can close through, no acyclic proof, no ownership proof, not
  already a candidate. What it admits goes to
  `cycle::queue::register_candidate`, which sets `CANDIDATE_BIT` first; the acyclic and ownership proofs it also reads
  have no writer. What proves each condition live is a `#[cfg(test)]` counter
  past the gate (`refcount::tests::the_candidate_gate`), because a
  scenario test sees the pair and never one half.
- GC C ABI and the safepoint: `src/gc.rs` — the four symbols the
  compiler emits calls to (`ll_gc_collect_cycles`, `ll_gc_maybe_collect`,
  `ll_gc_checkpoint`, `ll_gc_checkpoint_ack`). The two collecting
  entries report zero until S36.7. The poll has four duties in order:
  refill the log reserve, refill the critical reserve, refill the queue's
  spare segments, drain its overflow buffer.
- Static blocks and thread exit: `src/static_block.rs` — the per-thread
  registry and the teardown pass that releases each block's roots at
  exit (A6, `rfc/model/classes.md` "Teardown at thread exit"). The order
  the whole exit sequence runs in is fixed in `heap::ll_thread_exit`,
  and the reason it must be fixed there — TLS destructor order is
  unspecified — is `dev/DECISIONS.md`, 2026-08-03. **Rule for anything
  new on that path:** no `thread_local!` it can reach may have drop
  glue.
- Strings: `src/string.rs` — the two layouts of the string entity, told
  apart by their kind codes, 8 inline and 9 out of line
  (`string::bytes_are_out_of_line`), rather than by `COW`, which means
  only copy-on-write. Both answer `refcount::is_string`, one mask test,
  because the codes differ in the kind field's low bit alone.
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
  run transfers, an in-block payload is copied, and bytes whose copy the
  allocator refused are a pinned payload, retaining the block instead
  (`dev/DECISIONS.md`, 2026-08-04).
- Interpolated string templates: `src/template.rs` — the parts of one
  interpolation site are a `TemplateShape`, static data the compiler
  emits once and never frees; the instance is
  `RcHeader | class | shape | Value[n]`, an ordinary entity under **one**
  class for every site (`dev/DECISIONS.md`, 2026-08-05, amending
  `rfc/model/strings.md` rule 3). Because the value count is the
  instance's and not the class's, the count is read from the shape — and
  since the walk-duplication refactor it is read there in **one** place,
  `object::for_each_counted_cell`, which serves the quiescent tracer, the
  collector's relaxed one and the sever alike (`cells::trace_cells`,
  `cells::sever_cells`). It was three strides, each branching on
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

  `hash/process_key.rs` holds the other secret, and the two must not be
  confused: 32 bytes drawn from the OS once per process **in every
  build**, outside `STAMP` and exempt from `hash-folding`, because
  nothing compiled may depend on them. Every secret the collision defense
  draws comes from here (`rfc/model/maps.md`, "What the flood ladder
  becomes"), and a consumer takes the key whole as a keyed hash's key
  rather than by words. Unix only so far — the backlog carries the
  Windows door.

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
  and the walker, the sever and the dispose dispatch on it. **A fresh
  array is strategy 2**: `ll_array_new` stamps the mixed vector, and an
  array reaches the ordered hash by migrating under a key the dense range
  cannot hold (`element::representation_for`) or by being copied from a
  source that is one. What that means for a test is
  `dev/DECISIONS.md`, "a test asks for the ordered hash, or takes what
  the factory stamps". Two rules
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
  allocated chunk: sliding entries inside the published one raced the
  collector's relaxed loads), and the flood backstop, which is
  the collision defense's state: a chain-length threshold draws the table's
  salt for a salted rebuild of the index, a second chain-length threshold
  or an equal-hash threshold of eight identical hashes escalates to a
  keyed hash over the key bytes, and past both, an admission is
  refused rather than chained under the terminal admission denial
  (`rfc/model/maps.md`, "Rung three,
  refusal"). Every secret it draws comes from `hash/process_key.rs`. A
  COW copy takes the collision defense's state, draws a salt of its own from
  its own storage, and is presized to the entries it replays. It allocates no
  entity and calls no store
  barrier: both are `element.rs`'s, and `Table::insert` hands the
  displaced element back for that layer to release.
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
  the only place it does (`fill_from`).
  `entity.rs` is the wrapper supplying the `RcHeader`: an array carries no
  class pointer, the same construction as a string, because the entity
  kind already says what it is. Its children — elements **and** string
  keys — come from the one tracing stride, `cells::trace_cells`' Array arm,
  which reads the entries through `StorageHead::coherent` and
  `Table::entries_of`: a version counter brackets every move of an entry,
  and a walker that cannot get a coherent reading gives the array up for
  that collection rather than striding a fresh count over a stale chunk. Both ends of that bracket are ordered by
  a fence rather than by a release store and an acquire load, and
  `version_bracket_model.rs` is the loom model that exhibits what the
  other shape admits (`dev/WORKFLOW.md`, "Loom"). The version is read
  inside that bracket and nowhere else: `coherent` reads it, the four
  words, and it again, so the give-up is its whole answer and no version
  leaves the head. Nothing keeps one per walked row, and no cell the
  trace read is re-read against one — a cell is read a second time on
  the owning thread, which re-reads the current fields before any free
  (`cycle::validation`), and the collector-thread reader answers no version
  either, a torn read costing at most a phantom edge or a missed one
  (`PLAN.md` S38.0). `entity::for_each_counted_child` is an
  adapter over it, and `ll_entity_die`'s Array arm goes through that; the
  release side uses the barrier's `drop_ref`, so a child the array held
  last is torn down rather than only decremented. An arena array that
  survives a reset **takes its storage with it**, the same two routes a
  string's payload takes (`array::entity::carry_storage_out_of`, reached
  from `promote::carry_external_memory`); it gets there as a child of an
  escapee, never on its own, an array being COW and therefore copied at
  the barrier rather than counted as an escapee. Both COW doors have their Array arm
  now: `object::ll_cow_separate` separates a shared array and
  `object::escape_copy` copies an arena one out, and they are one body —
  `array::entity::separate` — with the destination category supplying the
  depth, each child published through `barrier::publish_child` rather
  than retained bare — the retain, the category barrier and the reference
  reconciliation the array's four publications share, `store_into`'s
  value and key and `fill_from`'s. Neither the copy nor the
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
  ring's block goes back to the allocator. Never journaling and a
  retirement both **close** the thread's cell, which is why it has three
  states and not two: a thread journals nothing after its exit, and a
  refused ring is not asked for again, though the thread **is** counted:
  a never-journaled thread is in no window, and the count is what keeps
  its silence from reading as inactivity. The ring is retired by the
  **last act** of `ll_thread_exit`, after every
  step of the teardown — the reserve and the pool's thread cache are
  drained there by hand rather than by their own destructors, which run
  later — so a `__destruct` body's records and every block handover are
  inside it and a window over a thread's death is complete. Past that act
  completeness ends and honesty does not: a record arriving on a retired
  thread's cell is counted and reported as `Lost`, while a never-journaled
  thread's records are not counted again on top of its own answer.
  `ll_thread_init` reopens the cell, so a pool thread's second life
  journals into a ring of its own. An evicted ring is freed by
  the next thread to journal or to mark, never by one inside its own exit,
  whose pending-free list is gone by then — the three-valued
  `heap::ExitPhase` is what tells those apart, a boolean having conflated
  a heap rebuilt mid-exit with a new life. The module is in every build; what the
  `debug-journal` feature gates is the record **sites**, and those are
  built now. `journal/kinds.rs` holds the vocabulary, the enabled mask
  and `journal_event!`, which is how a site is written: it expands to
  nothing without the feature, and with it evaluates its payload only
  after the mask says the kind is on, so a disabled site costs a load and
  a branch and reads nothing. Twelve sites carry §9.5's default set —
  entity birth at `refcount::publish_header` and death at each kind's own
  teardown body (`ll_object_die`, `string_die`, `array_die`,
  `reference_die`, `weakref_die`, since the kind switch above them is
  reached by one of the two object doors only and by a nested array not
  at all); the arena reset's two ends in `promote::arena_reset_full`; a
  block's two in `BlockPool::get` and `put`, the second carrying the kind
  the block arrived with, which is how §9.5's third block event is asked
  for; and the thread's two in `heap`. A collection's two kinds were
  deleted with the collectors that raised them, and `rc-cycle`'s are
  S36's to name. A site must not sit anywhere the *first* record's path
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
  soundness rather than economy. **The registry is a doubly linked list
  threaded through the run headers themselves**, a head behind a mutex and
  two link words per run, so registering a run takes no memory and freeing
  one gives none back — which is what a free inside a collection's close
  requires (`dev/DECISIONS.md`, "the registry of OS-direct runs is threaded
  through the runs"). A free arriving while a trace
  reads the block is withheld, and for a run that is soundness rather than
  economy — its memory is unmapped at the free while a trace may still
  address it. `cycle::deferred_slot_reuse` defers that return and owns the
  sweep-before-return order (`PLAN.md` S36.2).
  The doors
  above it are
  `heap::entity_alloc` past `MAX_SMALL` and `Arena::alloc_entity` past
  one block payload; the arena logs the run it takes, so an unpromoted
  corpse dies with the reset, and a survivor is handed over instead —
  not stamped `BLOCK_KIND_RETAINED`, not indexed in `retained.rs`, out
  of the arena's log through `forget_large` (`promote.rs`). Stamping it
  would send a multi-megabyte run to the 64 KiB block pool at the
  entity's death, and the omission is silent, which is why that rule
  carries a test of its own.
- The window a reset holds over its own frees: `src/memory/reset_window.rs`
  — per-thread, both builds, opened by `promote::arena_reset_full` and
  closed by a stack guard. It parks both large-entity kinds until the
  outermost close, absorbs the free of a corpse in a block whose
  occupant count is not established yet, records every completed teardown so the passes
  after the fixpoint skip what died, and holds the COW
  reconciliation's two correction terms. Windows nest, because a
  destructor of one reset can resolve a second arena and reset it
  (`dev/DECISIONS.md`, "the reset reads no corpse").
- Retained-block survivor lists: `src/memory/retained.rs` — the sorted
  survivor list of each retained former-arena block, written by `promote`
  at reset into memory the arena already holds (the block's own tail, else
  the reset's current block, else one fresh block shared by the lists that
  missed) and published in the block's own collector line beside one
  atomic count word; read by the trace's row dispatch and by `heap`'s
  test-only enumerator, which finds the blocks by kind in the region scan.
  No process-global table and no lock (`rfc/model/gc/rc-cycle.md`, "The
  survivor list of a retained block"; `dev/DECISIONS.md`, "a retained
  block's survivor list lives in the arena's own memory, and the process
  registry goes"). This is what makes a bump-filled former-arena block
  walkable at all; without it its occupants are root sources and a ring
  among them never dies (`rfc/model/gc/rc-cycle.md`, "Where the shadow
  count lives", the retained-block arm). The count word is what returns
  the block: live occupants in its low half, pinned payloads and the lists
  of other blocks standing in the block in its high half, decremented by
  whichever thread frees, and the decrement that reaches zero in both
  halves returns the block, spending its own list's hold on the holder
  first (`retained::release_emptied`). Four shapes sit beside that — a
  block retained for a **payload** the reset could not carry out waits for
  that payload's own free the way it waits for an occupant's death, the
  pin being a count because one block can hold several survivors'
  payloads; a block holding another block's list waits for that block's
  return; a block nothing holds at the end of its reset is handed over by
  the reset itself, after `finish_reset`, through a sentinel arm of
  `ll_free` that decrements nothing; and a payload freed **inside** the
  reset that pinned its block spends a pin the reset is still holding a
  second count against, released through `retained::reset_pin_released`
  once occupant counts are established (`dev/DECISIONS.md`, "the reset
  holds a pin of its own, and releases it after the index is real"). The
  payload's free arrives through `buffer_arena::buffer_free_longlived_payload`,
  which reads a retained block under the pointer, leaves the bytes where
  they are — former arena memory has no free list — and reclaims the
  block instead. That call defers no reuse: it returns the block straight
  to the pool, which is the gap `PLAN.md` S38.3 owns.
- The safepoint bracket a batched run pays: lowering emits
  `ll_gc_checkpoint_ack` before the run, `ll_release_batch` per
  reference, and `ll_gc_checkpoint` after it (decision 2026-07-28;
  `ll_release_vector` the same). The bracket is emitted in every build
  and both bodies are empty while no collector is wired
  (`rfc/model/memory/bulk-operations.md`).
- Entity tracing: `src/cells.rs` — the kind-dispatched tracer
  (`trace_entity`, `trace_cells`), the sever dispatch, and a
  `#[cfg(test)]` heap census over `memory::heap::for_each_entity_slot`;
  entity blocks and the region registry are in `heap.rs`/`block_pool.rs`.
  It is the upper half of the deleted `walk.rs`, moved on 2026-08-26
  under a name that is not a collector's, and `cycle::mark` traces
  through it rather than growing a stride of its own.
- Cells a class owns **outside** the object body: `src/cells.rs`'s
  `OutsideCells`, a group of four behaviours — the walk, the sever, the
  free and the arena carry — reached through
  `class::Class::outside_cells` when the descriptor carries
  `CLASS_OUTSIDE_CELLS`. A coroutine's waker block and a map's table
  chunk are the customers, both outside this crate, so the only class
  with the flag today is `src/test_support/outside_block.rs`. The
  storage is drawn under the instance's own category, which is what makes
  a corpse's storage die with the arena's pages; a survivor's is carried
  out by the group's own `carry`, reached from `promote::external_memory`
  (`dev/DECISIONS.md`, "a class with cells outside itself carries one flag
  and one group of five" and "the arena carry is the group's sixth
  member, and a refusal answers the bytes it left behind" — that refusal
  is `OutsideCarry::Pinned` now).
- Weak references: `src/weak.rs` — the kind-11 weak cell, death
  notification (`notify_death` / `notify_members` / `drain_arena_weak_log`)
  and the `ll_weakref_create` / `ll_weakref_get` ABI. Notification sites
  live in `object.rs` (dispose phase 2, first act) and in arena reset;
  `notify_members` is the cycle teardown's and has no caller until S36.3.
  Design: `rfc/model/weak-references.md`, whose "The weak table: address →
  subscriber row" still writes the table as a `HashMap`.
- The weak table itself: `src/weak/table.rs` — target address → subscriber
  row, open-addressed in one long-lived buffer payload, capacity a power of
  two at a load of one half, and a refusal answered at
  `ll_weakref_create` (`dev/DECISIONS.md`, "the weak table is the mutator's
  memory, and it comes from the buffer layer").
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
  `src/test_support.rs` or a submodule of it — `test_support::outside_block`
  is the class three modules build on, and `prop_offset` the offset its
  `store_prop` takes — and what one family of modules needs is beside
  those modules: `src/array/testing.rs`, and `src/cycle/testing.rs` for
  the two readers that say what the trace left in a shadow row.
  `src/test_support/tests.rs` holds the one rule that is about the suite
  rather than about a module: a test reading a file or spawning a process
  carries `#[cfg_attr(miri, ignore = "…")]`, without which Miri stops at it
  and runs nothing after it (`dev/WORKFLOW.md`, "Tests").
  The two `loom` models are outside this layout
  and stay so: each is a hand-written copy of a protocol rather than a
  group of tests over a module, and each is compiled only under
  `--cfg loom`.
- The performance case: `docs/performance-case.md` — the claim ("on the
  mutator's hot paths this runtime carries no known avoidable work, within
  the stated resolution of the instruments that measured them"), the canary bracket, and the list of claims not made
  before Phase D; figures are dated citations into `dev/BENCHMARKS.md`,
  which stays normative on conflict. Its instruction-level companion is
  `docs/performance-case-decompositions.md` — the pair, the counted
  publish and the death branch, every instruction tied to a contract
  sentence or listed as residue with its lead. The external comparand
  is `bench-external/canary/` (`pair_canary.cpp` + `accept.sh`, the
  disassembly acceptance re-run per rebuild); the strategy's record is
  `dev/DECISIONS.md`, "the performance case's external comparand is a
  canary, not a self-authored floor". No collector-side count exists: the
  epoch's parked-memory probe went with the two collectors on
  2026-08-26, and `PLAN.md` S40.1 is the step that measures the trace.
- Benches: `benches/alloc.rs`, `benches/standard.rs`,
  `benches/barrier.rs` (the store barrier's three directions and the
  arena logging inside them; it resets the arena between timed regions,
  because a log segment comes out of the arena's own bump and only
  `finish_reset` gives it back),
  `benches/lifecycle.rs` (object create/release GC-protocol tax, one
  configuration since 2026-08-26), `benches/strings.rs` (hash across the function's branch
  boundaries, create-hash-die, and the append loop in both memory
  categories — the harness the bump-top growth optimization was blocked
  on); no collector-side probe since the two collectors were deleted on
  2026-08-26; external probes in `bench-external/`.
  Store-side probe, same shape and same reason:
  `memory::barrier::tests::what_a_store_costs_by_working_set::measure_store_cost`
  — inside the lib, because a bench is a separate crate and reaches every
  micro-op through a call the optimizer keeps, and over two working sets,
  because a loop publishing one child measures a dependency through one
  header line rather than a store (`dev/BENCHMARKS.md`, 2026-08-15). It
  carries four directions and a sweep over how many of a region's stores
  name an arena owner, which is what prices the release-at-reset record
  without subtracting one direction from another, and every figure twice,
  with the log cache-hot and with it evicted (`dev/BENCHMARKS.md`,
  2026-08-15, "what the release-at-reset record costs, and the statistic
  that decides the answer").

`src/memory/reserve.rs` — the per-thread block reserve that keeps the
store barrier's log growth from failing; drawn in `Arena::grow_log`,
refilled at `ll_gc_maybe_collect`. Design in
`rfc/runtime/exceptions.md`, "The log reserve protocol".

`src/memory/gc_metadata.rs` — the one door through which cycle collection
takes and returns memory. A block it holds carries `BLOCK_KIND_GC_METADATA`,
and one current plus one high-water block count is what the manager can
answer about collection's reservation (`dev/DECISIONS.md`, "GC memory is
counted once, and the block kind is the split"). Beside it `charge` and
`discharge` keep the second pair, bytes in use inside those blocks, moved at a
structural transition rather than per grant so the registration path stays free of
it; the three residues that follow are entered in the high-water figure by the
transition that ends them, and by a mark rather than a charge, which is exact
on one thread and can miss a maximum two threads stood in together. The pool and the critical reserve refuse a
block still stamped with that kind. Under `cfg(test)` the same four figures are
counted per thread as well, and `thread_stats` is what an exact assertion
reads: the process figures are moved by every thread the suite is running
(`dev/DECISIONS.md`, "the test-facing reading of the GC ledger is per
thread").

The collection workspace's two fixed regions — one 64 KiB block a thread draws
at its first collection and keeps until it exits, whose head the bump does not
grant. First the withheld returns' control line, 64 bytes
(`cycle::deferred_slot_reuse`); behind it the member list, a control line and
1,024 eight-byte records, 8,256 bytes (`cycle::members`). Prefix 8,320 and bump
56,960, both pinned by `const` assertions in `cycle::arena`. The list is what a
collection an allocation failure started reads its teardown off, the rows
having gone back with the blocks; a collection off the poll keeps its arena
instead and arms nothing (`dev/DECISIONS.md`, "the member list is the pressure
path's alone").

Withholding a physical return — `memory::stdapi::ll_free` asks two windows
before a slot, a retained block or a large run goes back: an entity whose
header still carries `CANDIDATE_BIT` waits with no record kept, the queue entry
being the record; an open trace sends the return to
`cycle::deferred_slot_reuse::ActiveTrace`, which stacks it through the dead
entity and makes it after its trace scratch arena has swept its rows and
before that arena gives its own blocks back. The two close in either order.

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
pass and block retention. Children come from `cells::trace_entity`, so a
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
- A published header is read through `refcount`'s helpers, and since
  2026-08-26 the compiler enforces the field half: `RcHeader`'s `refcount`
  and `flags` carry no visibility modifier, so nothing outside that module
  can name them, and a rename, a local or a reference binding fails to
  build rather than passing a grep. The struct carries no method either —
  `memory_category()` and `lifetime_counted()` were deleted the same day for
  having no caller, so a `&RcHeader` reaches nothing and the binding that
  formed one has no motive. `refcount::tests::who_may_read_a_header` still
  reads the crate's own sources, for the two places the compiler does not
  stand: a revert of the privacy, which breaks no build, and a
  `#[cfg]`-disabled branch, which parses without resolving a name
  (`dev/DECISIONS.md`, "a header is read as narrowly as it is written",
  "`RcHeader`'s fields go private" and the amendment that retires its
  keep-clause; the instrument that exhibits the race is `dev/WORKFLOW.md`,
  ThreadSanitizer). Fixtures
  **outside** `refcount` reach a header through `refcount::entity_refcount`
  and its two neighbours, `#[cfg(test)]` shorthands over an entity pointer
  of any type; `refcount`'s own read and write the fields plainly, on
  headers built in a local — and in one case on byte 6, which is how a test
  models the collector's store. A `Class` descriptor's own `flags` word is
  read through `Class::flags_of`, at its offset rather than through the
  pointer deref the guard greps for.
- **No mutator access to a live published header spans byte 6** — four
  bytes for the counter, two for the mutator's half of the flags. The
  eight-byte accesses are the four outside a header's life: `publish_header`
  itself, the commissioning zero-fill in `heap::refill` and
  `large_entity::commission`, and the arena's death zero for an OS-direct
  run (`memory/arena.rs`). A wider access would overlap the collector's byte store
  without covering it, which is a mixed-size atomic access
  (`dev/DECISIONS.md`, "the header's access width is a correctness rule";
  pinned by `refcount::tests::the_flags_half_the_mutator_leaves_alone`).

## Key decisions

`dev/DECISIONS.md` — 2026-08-26: what the old collectors left behind is
deleted, and the two things kept from it are named. 2026-08-26: the flags
word is re-laid for one collector, and the ring-closing reserve is widened
to codes 0–7 — a kind holds counted slots a ring can close through exactly
when its code is below eight (`refcount::EntityKind::closes_a_ring`,
`{Object, Lazy, Array, Reference}`), so the gate is the mask test
`kind_may_close_a_cycle` and four codes stand free for the next such kind.
It supersedes 2026-08-07, where the same policy was a bitset because the
codes of that day admitted no mask: `Reference` was `011` and `String`
`001`, and no subset mask separates them.
2026-07-26: entity blocks as a second heap population. 2026-07-20: arena
handle as a raw pointer;
trailing inline data through raw pointers; block header split by access
rule; cold concurrent structures take a lock rather than a CAS loop;
Miri against a UNIX target. 2026-07-21: the barrier owns the whole slot
and publishes it before teardown; a destructor is owed by the
constructor, not the factory; a refused destructor record fails the
creation; the store barrier is funded by a per-thread reserve.

## Outside code

`dev/RESEARCH.md` — what was read in other projects, at which revision,
and what of it applies here. Entries so far: Concurrency Kit (the seqlock
that found the version-bracket defect, the epoch proof,
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
graph, and the end-to-end paths as sequence diagrams. Rendered on demand;
no images committed. Its collector half is stale and says so at its top.

`dev/design/debug-modes.md` — observability and debug levels: object
registry, lifetimes, shadow metadata, integrity checks, metrics export.
§9, the event journal, is built behind the `debug-journal` feature
(`src/journal/`); §§1–8 are design with nothing behind them.

`dev/design/pure-destructors.md` — a pointer at
`rfc/model/gc/pure-destructors.md`, which is normative and carries the
2026-08-23 amendment. The backlog line in `PLAN.md` is the owner.

`dev/design/retained-index-ownership.md` — proposal, ruled on 2026-09-01
and superseded on the storage question: the list is written into the
arena's own memory rather than a per-thread chain of manager blocks
(`dev/DECISIONS.md`, "a retained block's survivor list lives in the
arena's own memory, and the process registry goes"). Kept as the record
of what was considered; the code is S36.9 (e).


`dev/tools/citations.py` — the heading-level citation check, pass 1 of
`dev/WORKFLOW.md`'s "Checks a grep cannot make": prints every cited
heading the named document no longer carries. A citation into a document
that was deleted names the repository and the branch first — `` `rfc`'s
`archive/pre-rc-cycle`, `model/…md`, "…" `` — and resolves through
`git show`.

`dev/CYCLE-TERMINOLOGY-AUDIT.md` and `dev/PROJECT-TERMINOLOGY-AUDIT.md` —
the two mapping tables the vocabulary rests on, the first for `cycle`
and the second for the groups outside it. Three guards in
`src/cycle/tests/` name them in their failure messages, so a reader
sent here by a red test is reading one of these:
`the_words_the_crate_retired` reads identifiers with comments cut,
`the_metaphors_the_names_still_carry` reads file names and declarations
as case-insensitive substrings, and
`the_metaphors_the_comments_still_carry` reads comment text with quoted
spans spared. Why the three are three, and what they still do not
cover, is `dev/DECISIONS.md`, "the vocabulary is held by three guards,
one per surface".

`dev/CYCLE-COLLECTOR-REVIEW.md` — the 2026-09-01 read-only review of
`src/cycle/` with Edmond's ruling per finding: arena tail waste, the
scan's double row lookup, the retained registry lock, and three T1
items. Read before the S36 memory steps.

Documents deleted on 2026-08-26 with the collectors they described —
`dev/design/epoch-walk.md`, `epoch-walk-structures.md`,
`dev/RC_WALK_CRITICAL_REVIEW.md` and the four gc-horizon documents — are
on the branch `archive/pre-rc-cycle`, and why each went is
`dev/DECISIONS.md` under that date.

## Traps

`dev/POSTMORTEM.md` is the list, thirty-three entries and growing; read
it before an instrument is trusted, not after. `dev/WORKFLOW.md` carries
what each tool can and cannot see here.

## Conventions

`dev/WORKFLOW.md` — branches, commits, the required verification
sequence, test rules, Miri invocation.

Names follow `rfc/dev/GLOSSARY.md`, and the two audits above are drafts
against it that lose where it moves. A term the glossary does not cover
is raised there rather than settled here: `dev/DECISIONS.md`, "an
uncovered term is a gap rather than a local ruling".

A comment that says a capability is absent names the `PLAN.md` step that
builds it, and the commit deleting that stage sweeps the number out of
`src/` and `benches/`: `dev/WORKFLOW.md`, "How a debt is written". The
ban this replaced, and why it fell, is `dev/DECISIONS.md`, "a comment
names the plan step that owes it, and the stage's deletion sweeps the
number".

Not obvious from the code: `AUDIT.md` and `.idea/` are deliberately
untracked and must stay so — this repository is public and the audit
lists unfixed defects. Design lives in the separate `limelight-lang/rfc`
repo and is kept in sync with behaviour changes.
