# Index

Map of the project for an agent: where to look, so the whole tree does
not have to be read. Pointers only — nothing is explained here, only
located.

## Modules

Knowledge map: `dev/ARCHITECTURE.md` — layers and the sanctioned upward
edges, the per-module knowledge table ("does not know" is the contract), the
shared resources with the header-bit ledger, the end-to-end paths and the
cross-module invariants; drawn in `docs/architecture.md`. Module docs at the
top of each file are the normative detail.

`docs/memory-manager.md` — `src/memory/` end to end, normative for that
module (`memory/mod.rs` declares it) and moved with the code
(`dev/WORKFLOW.md`); superseded versions are in `docs/history/`, marked at
the top.

Analyses kept in `dev/` because the crate refused or has not adopted what
they analyse (`dev/DECISIONS.md`, "analysis of a candidate that may be
refused stays in `dev/`, and the rfc moves only on adoption"):
[Shadow rows: flat or chunks](SHADOW-ROW-REPRESENTATION-ANALYSIS.md), whose
§3.1 is the chunked form `cycle::census::replay` prices against the flat one
(`dev/BENCHMARKS.md`, 2026-09-12).

[The collector keeps the live roots: review and possible repairs](THE-COLLECTOR-KEEPS-THE-LIVE-ROOTS-REVIEW.md)
(Russian, 2026-09-26) reviews the [proposed collector-owned root
chain](design/the-collector-keeps-the-live-roots.md) at `5020d2a`: exit,
progress between R and the chain, recall, death-check scheduling, the
measurement's provenance and an experiment separating form D from the chain.
The preferred repairs keep retries, death checks and their schedule on the
collector, following the rule that the mutator should do less work.
The repairs are proposals, not adopted changes to the algorithm or plan.

The design S65 builds, kept until the stage closes: **[the package, third
version](CYCLE-SPLIT-PACKAGE-3.md)** (Russian), with [the lane
amendment](CYCLE-SPLIT-PACKAGE-3-LANE.md) and [its
Critic](CYCLE-SPLIT-PACKAGE-3-LANE-CRITIC.md), whose F2 and F3 are the form
taken (`dev/DECISIONS.md`, "the collector finds and the mutator judges, and a
recall of the token bounds the mutator's wait instead of the budget"); [the
Critic over the S65 plan](S65-PLAN-CRITIC.md), whose F1–F7 the steps carry;
and [the S64 analysis](S64-GC-IMPROVEMENT-ANALYSIS.md) (Russian), whose
"Какие опыты нужны" is the rig S65.16 and S65.19 build and S65.17 runs. [The progress review of
2026-09-25](S65-PROGRESS-REVIEW.md) (Russian) reads the stage before S65.12
against the S64 analysis: what the numbers prove for the mutator, what the
collector pays, the seven thin places of the algorithm as built, and three
counters proposed for the rig. The review chain that led to the
package, its first two versions and their Critics, and the Sage rulings on
the recall per take path were deleted on 2026-09-24; their outcomes stand in
`dev/DECISIONS.md` and in `PLAN.md`'s S65 steps, and the files in `git log
-- dev/`. The collector–mutator memory protocol of 2026-09-09 and its review
are in `docs/history/`, superseded by the collector thread that was built.

## Entry points

- **The cycle collector** is `rc-cycle` (`rfc/model/gc/rc-cycle.md`);
  `cycle::collect` is the order one collection runs the modules below in, on
  both of its paths, and `cycle::worker` is the collector thread beside it.
  What the group may not know is `dev/ARCHITECTURE.md`'s `cycle` row; what
  is not built, the compiler's side of the acyclic gate and a ruling on the
  turnover period, is `PLAN.md`, "The turnover period `N` is unruled". The
  deleted `rc-walk`, `rc-trace` and `rc-satb` are on the branch
  `archive/pre-rc-cycle` (`src/lib.rs`'s module doc; `dev/DECISIONS.md`,
  2026-08-26). Where each part is:

  | module | what is there | called by, in production |
  |---|---|---|
  | `queue` | the per-thread candidate ring R a registration writes into, its base block with the overflow buffer, the spare cells the poll fills, and the deferred lane; `compaction` is the close's in-place pass over all of them; `verdicts` is the ring P the collector posts into and the mutator disposes of | `refcount::release_word`, `gc`'s poll, `cycle::collect` |
  | `deferred_slot_reuse` | `ActiveTrace`, the window that withholds a physical return while a trace may address its row, and the stacks a foreign holder of the token withholds a slot, a block, a chunk or a run on | `stdapi::ll_free`, `BlockPool::put`, `buffer_arena` |
  | `collect` | the collection's order, its two paths — off the poll keeping its rows, under pressure harvesting a member list — and `may_collect`, the gate that refuses a collection reached from inside one, beside a teardown in flight or an open reset | `gc`'s two collecting entries; `memory::heap::entity_alloc` under pressure |
  | `arena` | `TraceScratchArena`, the collection's bump over the thread's workspace and into pooled blocks; `ensure_row`, `find_initialized_row` | `cycle::collect` |
  | `shadow` | the row: two bits of colour over thirty of working count; a block's array of rows, zeroed a group of eight at a time | `arena`, `mark`, `scan`, `maturation`, `membership`, `deferred_slot_reuse`, `worker` |
  | `row` | `resolve_edge_target`: which row a traced edge resolves to, by the block's kind | `trace`, `mark`, `scan`, `maturation`, `membership`, `worker` |
  | `epoch` | the mutator's epoch clock, a cell of turnovers on its record's hold line that the collector named to it advances, and the two-bit epoch a maturation stamp carries, `turnovers % 4`; every collection reads the cell once, at its arena's open, and a collector thread reads the record of the mutator it traces for | `cycle::arena` at the open, whose reading `cycle::finalization`, `cycle::mark` and `cycle::collect` carry; `cycle::worker`'s round for the advance |
  | `token` | the per-thread trace token: one byte of five states, taken by compare-and-swap, held by the mutator through its collection and by a collector for one batch, waited on through a mutex (`rfc/dev/design/trace-token-handshake.md`) | `cycle::collect`, `cycle::worker`, `gc`'s poll, `deferred_slot_reuse` at a free |
  | `mutator_record` | the 256-byte record the token stands in — four lines, the collector's and the mutator's words of the two rings — carved from GC-metadata blocks the process keeps | `heap::ll_thread_init` and `ll_thread_exit`, `cycle::token`, `queue`, `collect`, `worker` |
  | `worker` | the collector threads: the elder and its siblings, the timer, the round over the records named to a collector, and the batch for one mutator — request, consent, peek, trace through `cells::AtomicCells`, post into P, release to `POSTED`, the take of a ring that has stood non-empty below the threshold for `STANDING_INTERVAL` (4 s unless `ll_gc_set_standing_interval` says otherwise) — the standing list (`worker::Standing`, the requests a mutator did not answer inside the wait, threaded through the records with no capacity and read at the checkpoints after a byte event) and the mutator's epoch clock, advanced at a visit after X of the collector's clock (`epoch_interval`, 8 s unless `ll_gc_set_epoch_interval` says otherwise) or 64 of its batches; `worker::birth` is the thread's creation by the OS entry on a stack the slot keeps, and its join | `cycle::collect`'s pressure path, `gc`'s poll through `cycle::queue` |
  | `mark` | trial deletion over the rows; the prune that stops at a mature stamped target no queue names, with `pin_threshold` for a test at another `k` | `cycle::trace`, `cycle::worker` |
  | `scan` | the classification: live spreads, zero reads as potentially unreachable, a reached row is raised | `cycle::trace`, `cycle::worker` |
  | `trace` | both phases over one batch in the order the rows require: every root marks before any root scans; the collector thread runs the same two phases in `worker::trace` | `cycle::collect` |
  | `stack` | the trace worklist, 256-entry segments out of the arena; each entry an entity and the row its meeting found | `cycle::mark`, `cycle::scan`, `cycle::maturation` |
  | `records` | `RecordChain`, the segmented chain the worklist, the maturation's component stack and the deferred drops are built on | `stack`, `maturation`, `drops` |
  | `maturation` | the descent that stamps the live components a commit read: components over the rows, Pearce's index in the row's own count, the age one more than the youngest member's | `cycle::collect`, inside the commit and before the first guard |
  | `live_list` | the live core a collector's part read, listed in a chain of GC blocks the grant leaves on the record, stamped `{e, 1}` by the owner at its take from `POSTED` or before a block or a run of its goes back under it, and given back unread under pressure, at the exit and after an advance | `cycle::worker`'s batch; `token::HeldToken`'s take; `cycle::collect`'s teardown refusal; `BlockPool::put` and `large_entity::free` |
  | `members` | the entities a pressure collection takes out of its rows before the blocks go back, in the workspace's second fixed region | `cycle::collect`'s pressure path |
  | `membership` | the two forms a commit's membership takes — the harvested list, or the rows a collection off the poll keeps — behind one interface | `cycle::collect` and the modules a commit reads through |
  | `validation` | the exact validation of one component on the owning thread, and the zero-count-member rule | `cycle::finalization` |
  | `finalization` | the guard reference on every member, the weak cells nulled before any destructor, the destructor pass, the second reading with the guard subtracted, and the stamp on every component the exact validation read as externally referenced | `cycle::collect` |
  | `reclamation` | the teardown: room for the displaced children first, the sever, the frees through the ordinary death path, the drops after the last free | `cycle::collect` |
  | `drops` | the queue a teardown's displaced children wait in until the last member's free | `cycle::reclamation` |
  | `density`, `census`, `loads` | test builds only: the share of a touched block's slots a trace met, the census of one collection with its counters and its replay through both row forms (`dev/BENCHMARKS.md`, 2026-09-12), and the rings the census and `benches/census_driver.rs` build under the `bench-loads` feature | none |

  Two numbers about a row are pinned by tests: a count at the field's bound
  is a floor (`shadow::is_saturated`), and a block's first touch writes 121
  bytes against the 16 320 its rows reserve (`dev/BENCHMARKS.md`, 2026-08-27).
- The ring under the candidate queue: `src/ring.rs` — a single-producer
  single-consumer ring of pool blocks in moodycamel's `ReaderWriterQueue`
  form: `Writer`, `Reader` with its peek/commit pair, `Quiescent` for the
  owner's walk and pack, `Chain` for a lane spliced in whole; `cycle::queue`
  is its customer.
- Queue-work measurement, test only: `cycle::queue::take_queue_work` counts
  record passes, reads and moves on the current thread;
  `collect/tests/when_pressure_retires_members.rs` reads it.
- What a slot's first eight bytes read: `refcount::slot_state` — live, dead
  in place (`DEAD_IN_PLACE`, set by `ll_free`'s head, which refuses a second
  free), or free. What hands a slot back is `refcount::publish_header`, the
  close's compaction, and `memory::stdapi::hand_back_and_free`
  (`dev/DECISIONS.md`, "a second `ll_free` of an entity is refused, and the
  mark is the bit it is refused on").
- Giving back memory never published as an entity:
  `memory::stdapi::free_unpublished`; a plain `ll_free` there reads as a
  repeat on a recycled slot.
- What withholds a return: `cycle::deferred_slot_reuse::classify` returns a
  death at once where the collection never met its block and stacks it where
  it did, through the dead entities at `heap::FREE_LIST_LINK_OFFSET`.
- The candidate gate: `refcount::CANDIDATE_GATE_MASK` and
  `may_become_a_candidate`, five "this bit is zero" conditions read on the
  non-final decrement; what it admits goes to `cycle::queue::register_candidate`.
  The ownership mark is moved by `memory::barrier::store_ptr_owned` and read
  by `object::ll_default_dispose`; the acyclic proof is the class's
  `CLASS_ACYCLIC`, copied into the instance at `object::stamp_into`. Each
  condition is proved live by a `#[cfg(test)]` counter
  (`refcount::tests::the_candidate_gate`).
- GC C ABI and the safepoint: `src/gc.rs` — `ll_gc_collect_cycles`,
  `ll_gc_maybe_collect`, `ll_gc_checkpoint`, `ll_gc_checkpoint_ack`,
  `ll_gc_set_collector_cap`, `ll_gc_set_epoch_interval`,
  `ll_gc_set_standing_interval`; the poll's duties in order are
  `dev/ARCHITECTURE.md`'s `gc` row and `cycle::queue`'s module doc, "What the
  poll does for this module".
- Static blocks and thread exit: `src/static_block.rs` — the per-thread
  registry, a chunk of the thread's buffer arena closed behind the exit's
  pass and reopened by the next life's init, and that pass's release of
  each block's roots (`dev/DECISIONS.md`, "the exit path holds no container:
  the buffer arena is its thread-local, and the static registry is a chunk
  closed per life"). The exit's order is
  fixed in `heap::ll_thread_exit` (`dev/DECISIONS.md`, 2026-08-03: TLS
  destructor order is unspecified; nothing on that path may have drop glue).
  The exit's own collection is `cycle::collect::collect_before_exit`, its
  cases `cycle/collect/tests/what_an_exit_collects.rs` (`dev/DECISIONS.md`,
  "the exit collects in bounded rounds, and reports its residue as a
  record").
- Strings: `src/string.rs` — two layouts told apart by kind code, 8 inline
  and 9 out of line (`string::bytes_are_out_of_line`); `ll_string_new`,
  `ll_string_new_dynamic`, `ll_string_append`; `fits` is the 4 GiB gate; the
  COW barrier is `object::ll_cow_separate` plus `string::separate`; a
  survivor's payload leaves the arena through `string::carry_payload_out_of`
  (`dev/DECISIONS.md`, 2026-08-04). Interned names: `src/intern.rs`.
- Interpolated string templates: `src/template.rs` — `TemplateShape` is
  static data the compiler emits once; the instance is
  `RcHeader | class | shape | Value[n]` under one class for every site
  (`dev/DECISIONS.md`, 2026-08-05); the value count is read from the shape in
  one place, `object::for_each_body_cell` through `template::value_count_at`;
  `flatten` refuses a float or an object.
- The hash of a byte string: `src/hash/` — `hash_bytes` is rapidhash V3
  (`hash/rapidhash.rs`, ported from `vendor/rapidhash/`, pinned by
  `src/hash/vectors.rs` — a mistranscribed constant still hashes well and
  fails nothing else, `hash/tests/the_port_against_its_reference.rs`); zero
  is never returned. `hash/process_key.rs` is
  the per-process 32-byte key the collision defense draws from, unix only.
  `hash/seed.rs` is the seed and the `hash-folding` feature
  (`rfc/model/strings.md`; `dev/DECISIONS.md`, 2026-08-04).
- Arrays: `src/array/` — strategies 2 and 3 of `rfc/model/arrays.md`.
  `head.rs` holds the words a concurrent walker may read and the version
  bracket (`StorageHead::coherent`; the loom model is
  `version_bracket_model.rs`); `vector.rs` is the mixed vector a fresh array
  is stamped with (`dev/DECISIONS.md`, "a test asks for the ordered hash, or
  takes what the factory stamps"); `entry.rs` the 32-byte entry with the collision link in
  the element's tag word (`dev/DECISIONS.md`, 2026-08-07); `table.rs` the
  ordered hash — lookup, insert, remove, growth into a fresh chunk, and the
  flood ladder that ends in `InsertOutcome::AdmissionDenied`
  (`rfc/model/maps.md`, "Rung three, refusal"); `element.rs` the generic
  element layer with `canonical_key`, the five operations and
  `write_through`, the one separation composition (`dev/DECISIONS.md`,
  2026-08-08); `entity.rs` the `RcHeader` over the table, the copy for both
  depths (`separate`), the child walk and the teardown drain. The tag is
  stamped in `entity::new_with_storage`; `entity::as_table_mut` derives the
  disjoint head and storage (`dev/DECISIONS.md`, 2026-08-11, twice). Storage
  is a buffer-arena chunk, never an entity block.
- The event journal: `src/journal/` — 32-byte records in one ring per
  thread, read back by a `Mark` over every ring's cursor; an overflowed
  window answers `unknown` (`ring_model.rs` is the loom model). The ring is
  retired by the last act of `ll_thread_exit`. `journal/kinds.rs` holds the
  vocabulary and `journal_event!`; the sites are compiled under
  `debug-journal` (`dev/design/debug-modes.md` §9; `dev/DECISIONS.md`,
  2026-08-08). A collection's kinds are unnamed — `PLAN.md`, "The
  collection's journal kinds".
- Category → allocator routing: `src/memory/routing.rs` — `entity_alloc_in`,
  `body_alloc` / `body_ensure` / `body_free`, `slot_limit`.
- One entity per allocation: `src/memory/large_entity.rs` — a pooled block
  (`BLOCK_KIND_ENTITY_LARGE`) or an OS-direct run
  (`BLOCK_KIND_ENTITY_LARGE_RUN`), the entity at `+LINE_SIZE`; the zero pass
  commissions it; the registry of runs is threaded through the run headers
  (`dev/DECISIONS.md`, "the registry of OS-direct runs is threaded through
  the runs"). The doors above it are `heap::entity_alloc` past `MAX_SMALL`
  and `Arena::alloc_entity` past one block payload.
- The window a reset holds over its own frees: `src/memory/reset_window.rs`
  — defers both large-entity kinds' frees to the outermost close, reads
  `is_torn_down` off the header bit, and keeps the COW reconciliation's log
  (`dev/DECISIONS.md`, "the reset reads no corpse", "the record of a
  torn-down entity is its own header bit").
- Retained-block survivor lists: `src/memory/retained.rs` — the sorted
  survivor list of each retained former-arena block, in the arena's own
  memory, and the atomic count word that returns the block
  (`retained::release_emptied`, `hold_released`); placed by
  `promote::place_survivor_lists` (`rfc/model/gc/rc-cycle.md`, "The
  survivor list of a retained block"; `dev/DECISIONS.md`, "a retained
  block's survivor list lives in the arena's own memory, and the process
  registry goes").
- The safepoint bracket a batched run pays: `ll_gc_checkpoint_ack`, then
  `ll_release_batch` per reference, then `ll_gc_checkpoint`
  (`rfc/model/memory/bulk-operations.md`).
- Entity tracing: `src/cells.rs` — `trace_entity`, `trace_cells`, the sever
  dispatch, and the two readers `PlainCells` (owner) and `AtomicCells`
  (collector thread, every load `Acquire`;
  `cells::tests::what_a_collector_thread_reads` is the pairing
  ThreadSanitizer watches). `OutsideCells` is the group of six behaviours a
  class with cells outside its body carries (`CLASS_OUTSIDE_CELLS`; the one
  such class is `test_support::outside_block`).
- Weak references: `src/weak.rs` — the kind-11 cell, `notify_death` /
  `notify_member` / `drain_arena_weak_log`, `ll_weakref_create` /
  `ll_weakref_get`; the table is `src/weak/table.rs`, open-addressed in one
  long-lived buffer payload (`dev/DECISIONS.md`, "the weak table is the
  mutator's memory, and it comes from the buffer layer"). The design is
  `rfc/model/weak-references.md`, whose "The weak table: address →
  subscriber row" still writes the table as a `HashMap`.
- C ABI surface: `src/memory/context.rs` (arena and context), `src/object.rs`
  (`ll_object_new`, `ll_object_new_in`, `ll_object_constructed`,
  `ll_entity_die`, `ll_release_vector`, `ll_object_die`), `src/memory/heap.rs`
  (`ll_entity_reserve` / `ll_entity_cells_return`), `src/reference.rs`
  (`ll_reference_new`), `src/memory/stdapi.rs` (`ll_malloc` / `ll_c_free`),
  `src/memory/barrier.rs` (`ll_store_ptr` / `ll_store_box` / `ll_drop` /
  `ll_ref_store`, `ll_store_ptr_owned` / `ll_store_box_owned`),
  `src/refcount.rs` (`ll_retain` / `ll_release`).
- Crate root: `src/lib.rs`, built as `rlib` + `staticlib` and emitted as
  LLVM bitcode for the compiler (`README.md`, "LLVM IR export";
  `rfc/runtime/implementation-language.md`).
- Tests: one file per group beside the module — `src/foo.rs` declares
  `#[cfg(test)] mod tests;`, `src/foo/tests.rs` holds the shared fixtures
  and one `mod` per group, `src/foo/tests/<group>.rs` is the group, named by
  what it pins (`dev/DECISIONS.md`, "a test file holds one group"). Shared
  fixtures: `src/test_support.rs` (`outside_block`, `block_kind_and_used`,
  `a_child_run_ends_by_abort_saying`), `src/array/testing.rs`,
  `src/cycle/testing.rs` (the ring fixtures every collector case builds on).
  `src/test_support/tests.rs` holds the guard that a test reaching outside
  the process carries `#[cfg_attr(miri, ignore)]`. The three loom models —
  `array/version_bracket_model.rs`, `journal/ring_model.rs`,
  `cycle/token/free_path_model.rs` — stay outside this layout
  (`dev/WORKFLOW.md`, "Loom").
- The performance case: `docs/performance-case.md` and
  `docs/performance-case-decompositions.md`, figures dated into
  `dev/BENCHMARKS.md`, which is normative on conflict; the external comparand
  is `bench-external/canary/`.
- Benches: `benches/alloc.rs`, `standard.rs`, `barrier.rs`, `lifecycle.rs`,
  `strings.rs`, `value.rs`, and `census_driver.rs` under `bench-loads`; the
  store-side probe is inside the lib,
  `memory::barrier::tests::what_a_store_costs_by_working_set`
  (`dev/BENCHMARKS.md`, 2026-08-15).

`src/memory/reserve.rs` — the per-thread block reserve that funds the store
barrier's log growth; refilled at the poll (`rfc/runtime/exceptions.md`, "The
log reserve protocol").

`src/memory/gc_metadata.rs` — the one door through which cycle collection
takes and returns blocks, and the ledger: block count and high-water mark,
bytes charged and discharged; per thread under `cfg(test)`
(`dev/DECISIONS.md`, "GC memory is counted once, and the block kind is the
split", "the test-facing reading of the GC ledger is per thread").

The collection workspace — one 64 KiB block a thread draws at its first
collection and keeps to its exit: the withheld returns' control line, the
member list (8,256 bytes), and 56,960 bytes of bump, pinned by `const`
assertions in `cycle::arena` (`dev/DECISIONS.md`, "the member list is the
pressure path's alone").

Withholding a physical return — `memory::stdapi::ll_free` asks three windows
before a slot, a block or a run goes back: `CANDIDATE_BIT`, the open trace's
`ActiveTrace`, and a foreign holder of the thread's token, who withholds every
death until the owner reads the token free (`cycle::deferred_slot_reuse`, "A
foreign holder of the token"). The same holder withholds a chunk at
`buffer_arena::buffer_free_longlived_payload` and a block at the pool's `put`.

Buffer arena (`src/memory/buffer_arena.rs`) — where an entity's out-of-line
body lives, with the object heap's ownership rules; `buffer_ensure_longlived`
grows the last chunk in place (`dev/DECISIONS.md`, 2026-08-04 and 2026-08-05;
`rfc/model/memory/buffers.md`).

Arena reset and promotion: `src/promote.rs` — the fixpoint, the counting
pass, block retention and the survivor lists; a COW survivor's count is
settled by `reconcile_cow_counts` off the reset window's log
(`dev/DECISIONS.md`, 2026-08-04, "the COW count is the log's edges plus the
delta"; `dev/BENCHMARKS.md`, 2026-09-13).

## Hot paths

- Allocation: `Heap::alloc` → `ll_alloc`, expected to inline fully,
  cold tails split with `#[cold] #[inline(never)]`.
- Local free: `Heap::free`, including the `owner` check. Split into a
  fast path and out-of-line tails like `alloc` — except `relink_unfull`,
  which is out of line but not `#[cold]`, the boundary being crossed too
  often for that. Measured as no change outside the noise floor (H11 in
  `dev/BENCHMARKS.md`).
- Store barrier: the micro-ops `store_ptr` / `store_box` (publish) and
  `drop_ref` (release the displaced entity), the owned forms
  `store_ptr_owned` / `store_box_owned` for a compiler-proven slot, and
  the `ref_store` composition; ABI `ll_store_ptr` / `ll_store_box` /
  `ll_drop` / `ll_ref_store`, `ll_store_ptr_owned` / `ll_store_box_owned`.
- Arena bump: `Arena::alloc` → `ll_arena_alloc`.

Measured by `cargo bench --bench standard -- our_heap` (larson,
rptest); headline comparison in `benches/RESULTS.md`, change log in
`dev/BENCHMARKS.md`.

## Layout contracts (pinned by tests)

- Block header halves and cache lines:
  `memory::heap::tests::the_block_under_the_slots::block_header_halves_are_laid_out_as_the_design_requires`.
- `RcHeader` 8 bytes at offset 0, and the flags layout against the
  normative table: `refcount::tests::the_header_the_compiler_shares`.
- `Value` 16 bytes, fixed offsets, and the sample boxes of
  `rfc/model/values.md`, "ValueBox Layout", byte for byte:
  `value::tests::the_layout_generated_code_depends_on`.
- A published header is read through `refcount`'s helpers only: the fields
  are private since 2026-08-26 and `refcount::tests::who_may_read_a_header`
  reads the sources for the two places the compiler does not stand
  (`dev/DECISIONS.md`, "a header is read as narrowly as it is written, and
  through the helpers only", "`RcHeader`'s fields go private, and the source
  grep is re-aimed rather than retired"). Fixtures outside `refcount` reach a
  header through `refcount::entity_refcount` and its neighbours.
- No mutator access to a live published header spans byte 6, the collector's
  byte: `refcount::tests::the_flags_half_the_mutator_leaves_alone`
  (`dev/DECISIONS.md`, "the header's access width is a correctness rule, and
  no mutator access spans byte 6"). The maturation stamp at bits 16–19 is
  written by `refcount::write_maturation_stamp`, one byte wide:
  `refcount::tests::the_maturation_stamp_the_commit_writes`. The bit ledger
  is `dev/ARCHITECTURE.md`, "The header flag word is itself a shared
  resource".

## Key decisions

`dev/DECISIONS.md` is dated and newest first; the entries the code cites
most are the 2026-08-26 group (what the old collectors left behind, the
flags word re-laid for one collector, the ring-closing reserve at codes 0–7,
`refcount::EntityKind::closes_a_ring`), 2026-07-26 (entity blocks as a second
heap population), 2026-07-20 (the arena handle as a raw pointer, the block
header split by access rule, cold concurrent structures take a lock) and
2026-07-21 (the barrier owns the whole slot; a destructor is owed by the
constructor; the store barrier is funded by a per-thread reserve).

## Outside code

`dev/RESEARCH.md` — what was read in other projects, at which revision,
and what of it applies here, one dated entry per reading (its headings
are the list). Read it before evaluating one of those again.

## Diagrams

`docs/architecture.md` — the visual companion to `dev/ARCHITECTURE.md`
(which stays the source of truth): PlantUML layer picture, full wiring
graph, and the end-to-end paths as sequence diagrams, the collector thread's
batch among them. Rendered on demand (`plantuml -tsvg`); no images
committed. `docs/history/rc-cycle-infographic-2026-08-31.html` is the
picture of 2026-08-31, before the collection driver and the collector
thread, kept as a record.

`dev/design/debug-modes.md` — observability and debug levels: object
registry, lifetimes, shadow metadata, integrity checks, metrics export.
§9, the event journal, is built behind the `debug-journal` feature
(`src/journal/`); §§1–8 are design with nothing behind them.

Pure destructors: `rfc/model/gc/pure-destructors.md` is normative and
carries the 2026-08-23 amendment; the backlog line in `PLAN.md` is the owner.

`docs/history/` — superseded documents, each with a banner naming what
in it stands: the memory manager of 2026-07-03, the cycle collector review
of 2026-09-01, the stack-exit epoch GC
of 2026-08-18 and its review, the retained-index proposal of 2026-09-01,
the collector–mutator protocol of 2026-09-09 and its review.

`dev/tools/census_perf.sh` — the census's hardware arm: every load of
`cycle::loads` through `benches/census_driver.rs`, pinned to one CPU under
`perf stat --control`, twice per cell and the empty interval, into one CSV.
Build the driver with `--features bench-loads` first.

`dev/tools/take_perf.sh` — the hardware arm of a take's cost
(`dev/BENCHMARKS.md`, "S64.5 what a take costs by the shape of its roots"):
each arm of `cycle::worker::tests::what_a_take_costs` in a process of its own,
pinned to one CPU under `perf stat --control`, the probe opening the counting
interval around its timed collection alone. Build the test binary with
`cargo test --release --lib --no-run` first.

`dev/tools/rig.sh` — the rig's driver: every cell of
`cycle::worker::tests::the_rig`, a placement and a load, in a process of its
own, one CSV line each. It reads the physical cores and their SMT siblings from
`/sys/devices/system/cpu`; the probe pins each mutator itself and each
collector through `worker::testing::pin_collectors_to`, which
`begin_the_thread` reads. Build the test binary with
`cargo test --release --lib --no-run` first.

`dev/tools/arms.sh` and `dev/tools/arms_table.py` — a comparison of arms on
the rig: one test binary per arm, interleaved inside each repeat, the deciding
loads paced by the S65.24 protocol and the guards unpaced, then the table of
each arm's median cycles and instructions an iteration with the protocol's
tolerance and gates (`dev/BENCHMARKS.md`, "S65.24 A, B and C on a box with a
PMU"). The mutators' counters need a kernel that grants `perf_event_open`.

`dev/tools/two_arms_table.py` — two arms of `arms.sh`'s CSV by the S65.24
protocol as S65.28 fixed it: instructions an iteration as the gate, heap
garbage and the last free as the memory gates, iterations over 200 µs, the
collector's CPU beside completion, and each arm's share of the token wait and
of the withheld returns' time spent in the grant's expiry and death check
(`worker::testing::BatchSegments`).

`dev/tools/stall.sh` — S65.27's loop: one short `live-churn` cell on one
binary, RED when the drain frees less than half of what stood at the stop,
which is how the collector's chain froze behind a stale room of P.

`dev/tools/citations.py` — the heading-level citation check, pass 1 of
`dev/WORKFLOW.md`'s "Checks a grep cannot make": prints every cited
heading the named document no longer carries, and prints nothing on a
clean tree. A citation into a deleted document names the repository and
the branch first — `` `rfc`'s `archive/pre-rc-cycle`, `model/…md`, "…" ``
— and resolves through `git show`; `docs/history/` is skipped.

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

`docs/history/cycle-collector-review-2026-09-01.md` — the 2026-09-01
read-only review of `src/cycle/` with Edmond's ruling per finding, every
finding closed; what each became is its status paragraph. Code cites the
findings by number.

Documents deleted on 2026-08-26 with the collectors they described —
`dev/design/epoch-walk.md`, `epoch-walk-structures.md`,
`dev/RC_WALK_CRITICAL_REVIEW.md` and the four gc-horizon documents — are
on the branch `archive/pre-rc-cycle`, and why each went is
`dev/DECISIONS.md` under that date.

## Traps

`dev/POSTMORTEM.md` is the dated list and grows with each new trap; read
it before an instrument is trusted, not after. `dev/WORKFLOW.md` carries
what each tool can and cannot see here.

## Conventions

`dev/WORKFLOW.md` — branches, commits, the required verification
sequence, test rules, Miri invocation.

Names follow `rfc/dev/GLOSSARY.md`, and the two audits above are drafts
against it that lose where it moves. A term the glossary does not cover
is raised there rather than settled here: `dev/DECISIONS.md`, "an
uncovered term is a gap rather than a local ruling".

A stage whose section in `PLAN.md` would run past forty lines keeps its role
lines and its reasoning in `dev/plans/S<n>.md` and its steps in the plan, and
the file is deleted with the stage (rule 23.1.3). A ruling or a measurement
the file held goes to the journals before it does; the file is not a record.

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
