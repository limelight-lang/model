# Architecture — the knowledge map

How the crate works *together*: layers and ownership, who knows what
and — the actual contract — who does **not** know what, the shared
resources, the end-to-end paths, and the invariants that live between
modules rather than inside one.

What this file is not: not a locator (`INDEX.md`), not a decision log
(`DECISIONS.md`), not per-module detail (each file's module doc stays
the normative source for its own internals — this map must agree with
them, and loses to them where it drifts).

Design is authoritative in the `rfc` repository; this map records what
is built.

## Layers

```
L4  collectors        gc (ABI + safepoint) · cells · cycle · promote
L3  object model      object · class · reference · weak · intern ·
                      static_block · string · template · array
LB  mutation          memory/barrier — model-level code living in memory/
L2  memory manager    context · arena · heap · immortal · buffer ·
                      buffer_arena · reserve · critical · retained · stats ·
                      stdapi · routing · large_entity · reset_window ·
                      gc_metadata · ring · journal
L1  entity substrate  refcount · value · hash
L0  block supply      memory/block_pool · memory/os
```

Rules:

- A module may know anything at or below its own layer. The substrate
  sits *below* the memory manager deliberately: `RcHeader` is the one
  vocabulary both sides share — the manager stamps and tests headers
  (arena logs, heap occupancy), the model counts with them — and
  `refcount`/`value` know nothing of blocks, arenas or entities'
  bodies in return.
- Allocation knowledge never flows upward: nothing at or below L2
  knows what a class, an object body or a verdict is.
- The barrier (LB) is placed by what it knows, not where its file
  lives: it composes refcount + category + arena-log writes, so it
  sits above the memory manager even though it is in `memory/`.
- `ring` and `journal` sit in L2 by what they know: a ring is blocks
  linked in a circle whose entries are words it never dereferences, and
  the journal is records in a ring drawn from `ll_malloc`; neither knows
  an entity. `hash` is a pure function over bytes and sits with the
  substrate.
- `lib.rs` is the crate root and `memory/mod.rs` the folder root:
  re-exports and the module-doc declaration that `memory/` implements
  `docs/memory-manager.md`; no logic, no layer.

**Sanctioned upward edges.** At module granularity the layering is not
acyclic: entity death and GC scheduling flow back down through
call-backs. These upward edges exist in production code (re-enumerated
mechanically 2026-09-18 by resolving every `crate::…` path against the
module layers above, test modules and `#[cfg(test)]` items excluded), each
entered at a named point:

| Edge | Point of entry | Why |
|---|---|---|
| `object → gc` | `ll_release_vector`, the checkpoint bracket and the poll at `POLL_STRIDE` | the safepoint bracket a batched run pays: `ll_gc_checkpoint_ack` before it, `ll_gc_checkpoint` after (decision 2026-07-28). Both bodies remain empty because the built in-line collector needs no handshake; the future accelerator owns their final contract |
| `object`, `class`, `template`, `array::entity` `→ cells` | `for_each_counted_cell`, the descriptor's outside-cell group, the template's shape stride, `for_each_counted_child` | one kind-dispatched tracer rather than a stride per customer; the array adapter calls up rather than striding the entries a second time |
| `arena → weak` | `reset` | draining the arena weak log is part of arena death |
| `context → promote` | `ll_arena_reset` | the reset ABI drives the full discipline — `promote::arena_reset_full` consumes the arena's logs through arena's own drain primitives, not the reverse |
| `barrier → object` | `drop_ref` | the release cascade ends in `ll_entity_die`; `header_category` reads |
| `class → object` | descriptor construction | carries `ll_default_dispose` as the default dispose pointer (data, not a call) |
| `heap → static_block`, `weak`, `cycle::queue`, `cycle::mutator_record`, `cycle::deferred_slot_reuse`, `cycle::members`, `cycle::collect` | `ll_thread_exit`, `ll_thread_init`, `entity_alloc`, and two reads on the allocation path — `collect_remote` asks `deferred_slot_reuse::returns_are_withheld`, `require_thread_started` asks `queue::queue_base_present` | thread exit owns the order its per-thread state dies in, because TLS destructor order is unspecified and puts the exit guard last (decision 2026-08-03). Most are disposal calls: `heap` names `dispose`-shaped functions in a fixed sequence and learns nothing about cells or verdicts from them. Two of the three calls into `cycle::collect` start collections — the pressure path at `entity_alloc`'s refusal, and the exit's rounds between the static blocks' teardown and the window's disposal — and answer a count and the exit's residue, never a verdict; the third reads the entry gate at the top of `ll_thread_exit`, which records the request under a closed gate instead of running the sequence. The deferred-reuse module is asked before the heaps, and answers by refusing an exit inside the thread's own open trace window rather than by freeing anything, holding no block of its own to free; `cycle::members` gives back a harvested list the same way; the queue and the record join at both ends — the base block is drawn and then the record at init, the record given back on the rollback, and at exit the queue's segments go back before the reserves drain, then the collection workspace, then the base block, then the record (`mutator_record::release_thread_record`), then the reserves |
| `block_pool`, `buffer_arena`, `stdapi` `→ cycle::deferred_slot_reuse` | `BlockPool::put`, `buffer_free_longlived_payload`, `ll_free` | a physical return asks the trace windows before the memory goes back — a block, a chunk, a slot or a run a foreign holder of the thread's token withholds is stacked for the owner to return later (`dev/DECISIONS.md`, "a foreign holder of the token withholds every death, and the owner makes the returns"). The layer learns one boolean per return: withheld or not |
| `object → cycle::queue` | `ll_release_vector` reads `POLL_STRIDE` | a constant, not a call: how many releases a batched run makes between polls |
| `block_pool`, `refcount` `→ journal` | the record sites (`journal_event!`), compiled under `debug-journal` alone | a site is where the event happens; the journal learns an address and a kind and nothing of the block or the header |
| `refcount → cycle::queue` | `release_word`, the non-final decrement the candidate gate admits | the candidate set is fed from the release path and nowhere else, registration being edge-triggered (`rfc/model/gc/cycle/questions.md`, Y6). `refcount` learns nothing back: `register_candidate` cannot fail and answers nothing, and the bit is set before the write |

**Where the collector's duty sits.** A dying slot a queue entry names owes the
collector one thing and it is the free's rather than a door's: the slot
is withheld while an entry names it or a trace may still address its row,
and every route — the bare-pointer door, `array::entity`'s drain —
reaches the same free. A duty that is not the free's would be owed at
both doors instead of one.

Any new upward edge is a design event: stop, discuss, record in
`DECISIONS.md` — do not just add it to this table.

## Knowledge map

"Does not know" is the contract column. "(hot)" marks the measured hot
paths (`dev/INDEX.md`, "Hot paths").

The "Depends on" columns restate the production import graph as
re-enumerated 2026-09-18 from the `crate::` paths of production code (test
modules and `#[cfg(test)]` items excluded), at the granularity of the rows:
an edge between two submodules of one row is not listed. Where they drift,
the imports win — and a boundary change must update this file in the same
commit (`WORKFLOW.md`).

### L0 and L2 — memory

| Module | Responsible for | Knows | Does not know | Depends on |
|---|---|---|---|---|
| `memory/block_pool` | 2 MB OS regions carved into size-aligned 64 KB blocks; global free chain (mutex) + per-thread cache; region registry | OS allocation; the `BlockHeader` base fields | what any payload contains; entities, classes, GC | `cycle`, `journal`, `memory/os` — `cycle` for the block a foreign trace withholds at `put` — its `cycle::queue` references are the shared test-lock harness only; `journal` is a record site |
| `memory/os` | memory straight from the operating system: `map_aligned` cuts a size-aligned span out of an oversized mapping, `unmap` gives it back; every refusal is reported to the caller, never aborted | `mmap` / `VirtualAlloc`; under Miri, the table of untrimmed mappings | what a region holds | — |
| `memory/arena` (hot: bump) | request arena: bump allocation; self-contained bookkeeping — block list through block headers, destructor / escapee / release-at-reset logs as segment chains in its own memory; the log-drain primitives promote drives at reset | block pool; drawing the reserve for log growth; the `RcHeader`s its logs point at | object layout, classes, GC strategy; which escapees survive (promote's); the reset discipline itself — promote drives it from above | `memory/block_pool`, `memory/large_entity`, `memory/reserve`, `memory/stdapi`, `refcount`, `weak` (`large_entity` for an entity past one block payload) |
| `memory/heap` (hot) | small-object heap, mimalloc model: one block per size class, intrusive free list + bump cursor, per-block MPSC remote-free, thread-exit abandonment / adoption; runs twice per thread (raw + entity heaps); both enumerators — `for_each_entity_slot` and the block snapshots — which cover retained former-arena blocks through the survivor list their header names as well as entity blocks by striding; and the collector line on the header line's free tail — the shadow pointer, an entity block's reciprocal and size class, a retained block's list address, length and count word, and the words an arena reset keeps there while it runs: its two chains, the list it has placed for a block and what its fill pass has accounted for — written at commissioning and read by a row lookup | block pool; slot occupancy (the header word at bytes 0–7); that a retained block has no stride; the slot index an address derives, and that the line carrying the collector's words is the one line of a header a non-owner writes | entity kinds and out-edges; verdicts; classes; the epoch protocol; who built the list | `cycle`, `journal`, `memory/block_pool`, `memory/buffer_arena`, `memory/critical`, `memory/large_entity`, `memory/reserve`, `memory/stdapi`, `refcount`, `static_block`, `weak` — thread init, the exit sequence, the pressure path and two reads on the allocation path are the edges into `cycle`; the exit's release of roots and of the weak table are the edges into `static_block` and `weak` |
| `memory/immortal` | global bump region: class metadata, interned strings; nothing is ever freed | block pool | the contents of what it hosts | `memory/arena`, `memory/block_pool`, `memory/os` |
| `memory/buffer` | growable `{data, len, capacity}` payload, no header; extend-in-place at the arena bump top, else copy; OS-direct above block payload | the mounted arena's bump; context resolution | entity lifecycle, headers; the long-lived buffer arena | `memory/arena`, `memory/block_pool`, `memory/context` |
| `memory/buffer_arena` | long-lived buffer blocks (`BLOCK_KIND_BUFFER`): bump + per-block intrusive LIFO free list, pressure modes, per-block live count returning empty blocks | block pool; the buffer pressure protocol | the object heap; entities; GC | `cycle`, `memory/arena`, `memory/block_pool`, `memory/buffer`, `memory/context`, `memory/retained`, `memory/stdapi` — `cycle` for the chunk a foreign trace withholds at its free |
| `memory/reserve` | the two-block per-thread reserve funding store-barrier log growth; drawn only after ordinary refusal; sets the refill flag the poll checks | block pool | what a log records; barrier semantics | `memory/block_pool` |
| `memory/critical` | the eight-block per-thread critical reserve, the second of `exceptions.md`'s three and separate from the first so neither consumer's worst case is the sum; drawn only after the pool refuses, and `give_back` refills it before a returned block reaches the pool | block pool; that a returned block is the reserve's before it is the pool's | who draws and what for — the collection's arena decides its own order. Two customers draw today, the arena and the candidate queue; the mutator that cannot collect is answered null and draws nothing (`PLAN.md`, Fog, "The threshold arming policy") | `memory/block_pool` |
| `memory/retained` | the survivor list of each retained former-arena block — sorted, in the arena's own memory, named by the block's header line — and the atomic count word that returns the block: held occupant slots (live survivors and dead candidates awaiting owner retirement), pinned payloads and the lists of other blocks standing in it; published by the reset, read by the trace and the test-only enumerator, no process-global table and no lock | block addresses and arrays of addresses, plus the one question it asks through them: whether a slot still holds an allocation the block must retain, which `refcount::slot_state` and the candidate bit answer | what lives at those addresses — entities, classes, verdicts; where the arena placed the list | `memory/block_pool`, `memory/heap`, `refcount` |
| `memory/stats` | block-granular telemetry computed at query time; counters only on pool get/put — zero hot-path tax | pool counters | per-object events (the opt-in event log, unbuilt); arena/heap internals | `memory/block_pool` |
| `memory/stdapi` | the size-less allocator front door: `ll_malloc`/`ll_free`/`calloc`/`realloc`/aligned + `GlobalAlloc`; routes `ptr & !BLOCK_MASK` → header `kind`; asks `refcount::is_registered_candidate` and `cycle::deferred_slot_reuse::withhold_under_a_trace_or_make_returns` before a physical return | every block kind's free route; the heap's `MAX_SMALL`; that a queue entry or an open trace withholds a physical return | entity semantics; who its callers are | `cycle`, `memory/block_pool`, `memory/heap`, `memory/large_entity`, `memory/os`, `memory/reset_window`, `memory/retained`, `refcount` — `cycle` for the windows `ll_free` asks before a slot, a block or a run goes back |
| `memory/context` | `LLContext` and the TLS current context (NULL-context fallback); the composition root wiring arena + thread heaps + immortal behind one ABI, `ll_arena_reset` included | the arena mount; which module implements each ABI it fronts | class layout; GC strategy; thread-heap init (heap's `ll_thread_init`, the embedder's one call per thread life) | `memory/arena`, `memory/heap`, `memory/immortal`, `promote`, `refcount` |
| `memory/routing` | where a memory category's bytes come from: `entity_alloc_in` for anything with an `RcHeader`, `body_alloc` / `body_ensure` / `body_free` for the bytes an entity owns outside its slot, and `slot_limit`, the size past which a category's entity leaves the shared block | every allocator the categories route to | what kind of entity is being placed; a factory refuses a category before calling | `memory/block_pool`, `memory/buffer`, `memory/buffer_arena`, `memory/context`, `memory/heap`, `memory/immortal`, `refcount` |
| `memory/large_entity` | one entity per allocation for an entity no size class serves: a pooled block up to one payload (`BLOCK_KIND_ENTITY_LARGE`) or an OS-direct run above it (`BLOCK_KIND_ENTITY_LARGE_RUN`), the entity at `+LINE_SIZE`; the zero pass that commissions it; the registry of runs threaded through the run headers | the block header layout; the pool and the OS | the entity's layout past its first eight bytes; who withholds a run's free (`cycle::deferred_slot_reuse` asks it through `stdapi`) | `memory/block_pool`, `memory/os` |
| `memory/reset_window` | the window a reset holds over its own frees: the deferred free of both large-entity kinds until the outermost close, the torn-down bit a survivor's header reads, and the COW reconciliation's log in segments drawn through `stdapi::ll_alloc` | the reset's frame; the retained block's count word | which entity is a survivor — `promote` tells it; the collector | `memory/arena`, `memory/heap`, `memory/retained`, `memory/stdapi`, `refcount` |
| `memory/gc_metadata` | the one door through which cycle collection takes and returns pool blocks: the stamp `BLOCK_KIND_GC_METADATA`, the block count and its high-water mark, and `charge` / `discharge` for bytes in use inside those blocks; per-thread figures under `cfg(test)` | the pool and the critical reserve, which refuse a block still stamped | what a GC block holds; the ring's links, which cross no function here | `memory/block_pool`, `memory/critical` |
| `journal` | the event journal: 32-byte records in one ring per thread, drawn through `stdapi::alloc_outside_the_heap` at the thread's first record, the one route past `require_thread_started`, because the first record is raised inside `ll_thread_init`, retired at the last act of `ll_thread_exit`, read back by a `Mark` over every ring's cursor; the record sites are compiled under `debug-journal` alone | its own ring, the registry of retired rings, the kinds mask | what an event means to the module that raised it; entities and blocks beyond the address a record carries | `memory/block_pool`, `memory/heap`, `memory/stdapi` |
| `ring` | a single-producer single-consumer ring of pool blocks in moodycamel's `ReaderWriterQueue` form: blocks linked in a circle, `front` and `tail` per block on their own lines with local copies of each other, release/acquire on the index words; the writer's push, splice and unlink, the reader's take and its peek/commit pair, and the owner's quiet walk, pack and dismantle while no reader runs | that the two block pointers are the caller's words, kept where its own lines put them; that consumed blocks are the writer's again around the circle; that a block after the tail block is empty | what an entry means — a word, never dereferenced; where a fresh block comes from, which the caller's closure answers; who excludes the reader while the owner packs | `memory/block_pool` |

### LB and L1 — mutation and substrate

| Module | Responsible for | Knows | Does not know | Depends on |
|---|---|---|---|---|
| `memory/barrier` (hot) | store-barrier micro-ops: `store_ptr` / `store_box` (retain + category barrier + write), `drop_ref` (release + cascade), `ref_store`; escape and release-at-reset recording into the mounted arena's logs | header category bits; `Value`; that `owner_cat` is a parameter, never a load | the per-site composition (lowering's) | `memory/arena`, `memory/context`, `object`, `refcount`, `value` |
| `refcount` | the 8-byte `RcHeader` at offset 0 of every entity: refcount + flag word (category, entity kind, the collector's maturation stamp at bits 16-19 and 20-31 free); `ll_retain` / `ll_release` / `ll_release_batch`; relaxed-atomic header accessors, all narrow — four bytes for the counter, two for the mutator's half of the flags — because a wider one would overlap the collector's byte store without covering it. **The two fields are private to this module**, so *who may name a header* is the compiler's answer rather than a source grep's; *how wide the access is* stays the accessors', privacy having nothing to say about width and the module's own wide accesses being deliberate (`dev/DECISIONS.md`, "`RcHeader`'s fields go private") | its own bit layout, and which bits are lent to whom (see the ledger below) | entity bodies past 8 bytes; blocks; when to collect | `cycle`, `journal` — `cycle::queue` from the non-final decrement the candidate gate admits — the write cannot fail, so the bit set before it always names an entry |
| `value` | the 16-byte ValueBox as two words: `+0` the immediate value or the pointer arm's tag word, `+8` the counted pointer (bit 0 clear) or the tag word (bit 0 set), which alone decides the arm; `TAG_WORD_UNDEF` as a bit of the tag word, deliberately not a tag; the decode, the one-word type tests, and the word-level test every container and walker decide by (`is_pointer_word`) | the tag codes; that the arm is the counted flag | unboxed representations (compiler contract) | `refcount` |
| `hash` | `hash_bytes`, the one function every hashed byte string goes through (rapidhash V3, ported from the vendored header and pinned by its vectors), the per-process seed and the `hash-folding` axis, and the per-process 32-byte key the collision defense draws its secrets from | the seed's provenance; that zero is never returned | what is being hashed; the table that defends against flooding | — |

### L3 — object model

| Module | Responsible for | Knows | Does not know | Depends on |
|---|---|---|---|---|
| `intern` | interned names as immortal string entities — one address per string for the process lifetime, inline hash; the global lookup table (Rust-owned metadata) | the string entity layout | classes — it serves them names and knows nothing of them | `hash`, `memory/immortal`, `refcount`, `string` |
| `class` | class descriptors: the inline vtable train (`[Class][vtbl][itables…]`, pure code-pointer arrays), method table, Cohen display; property layout as three typed runs; the trace lists (`ptr_runs` / `box_runs`); link-time construction | immortal allocation; interned names; the default dispose pointer | instance state; memory categories; GC; who calls the methods | `cells`, `intern`, `memory/immortal`, `object`, `string` |
| `object` | `ll_object_new` factory; `ll_object_constructed` (destructor registration); three-phase `ll_object_die`; the kind-switched `ll_entity_die`; `for_each_counted_child` | class runs; every category's allocator; the weak gate bit; the destructor-debt protocol | collector internals; block internals; per-site barrier composition | `array/entity`, `cells`, `class`, `cycle`, `gc`, `journal`, `memory/arena`, `memory/barrier`, `memory/context`, `memory/routing`, `memory/stdapi`, `refcount`, `reference`, `string`, `template`, `value`, `weak` — `cycle` for the poll stride `ll_release_vector` reads, a constant |
| `reference` | the `&` reference box, entity kind 3: `RcHeader \| Value` — the model's only extra indirection, self-describing at teardown via the kind field | its own kind | classes; typed slot references (future) | `journal`, `memory/barrier`, `memory/routing`, `memory/stdapi`, `object`, `refcount`, `value` |
| `static_block` | the per-thread registry of static blocks and the teardown pass that releases their roots at thread exit (A6): registration in first-touch order into a chunk of the thread's buffer arena, drained in reverse, closed behind the exit's pass | that a static block is headerless and laid out by a descriptor; that a `__destruct` may register another block mid-pass; that a growth the arena refuses is a registration refused | how a static block is allocated; what its slots mean — the release policy is the barrier's, the walk over the slots `object`'s and the emptying `cells`' | `cells`, `class`, `memory/barrier`, `memory/buffer`, `memory/buffer_arena`, `object`, `refcount` |
| `weak` | the kind-11 weak cell (the canonical `WeakReference` *is* the cell); the per-thread weak table; every notification rule (`notify_death` / `notify_member` / `drain_arena_weak_log`); `ll_weakref_create` / `ll_weakref_get` | the `HAS_WEAK_REFERENCES` gate; that cells always live in the GC heap; that only the owning thread touches the table; where the table's rows come from and what a refused one answers | *when* to call in — that duty belongs to the death sites (dispose phase 2 first act, both collectors, arena reset) | `journal`, `memory/arena`, `memory/buffer_arena`, `memory/context`, `memory/heap`, `memory/stdapi`, `object`, `refcount` |
| `string` | the string entity in two layouts told apart by kind code — inline `RcHeader \| len \| hash \| bytes` (kind 8) and out of line with spare capacity (kind 9) — `ll_string_new`, `ll_string_new_dynamic`, `ll_string_append`, the 4 GiB length gate `fits`, the cached hash, the COW `separate`, and `carry_payload_out_of` for a survivor's payload at reset | both layouts; where a payload comes from (`routing`) | classes; arrays; the collector | `hash`, `journal`, `memory/arena`, `memory/block_pool`, `memory/buffer`, `memory/buffer_arena`, `memory/context`, `memory/routing`, `memory/stdapi`, `object`, `refcount` |
| `template` | the interpolated string template: `TemplateShape` is static data the compiler emits once, the instance `RcHeader \| class \| shape \| Value[n]` under one class for every site; `flatten` builds the string in one allocation | the shape's value count, which the instance's cell walk reads in one place (`object::for_each_counted_cell`) | floats and objects, which it refuses to flatten | `cells`, `class`, `memory/barrier`, `memory/context`, `memory/routing`, `memory/stdapi`, `refcount`, `string`, `value` |

The array is six modules under `mod.rs`, with a loom model beside them
under `cfg(loom)`, and the cut between them is what the rows record:
`entry` and `table` are the ordered hash with no entity lifetime in it,
which is the half `Map` is meant to reuse, while `element` and `entity`
carry what an entity brings — the store barrier, the reference box, the
teardown.

| Module | Responsible for | Knows | Does not know | Depends on |
|---|---|---|---|---|
| `array/head` | the words a concurrent walker may read — version, chunk, index-slot count, element count, strategy tag — and the seqlock bracket that makes reading them coherent (`begin_move` / `end_move`, `coherent`) | that a walker validates a reading rather than locking, that each word it reads is written by one atomic store of the same width, and that giving a reading up leaks one epoch rather than freeing early; that both fences are needed and why their ends differ (`version_bracket_model.rs`); and the two rules it states for whoever writes the chunk — `used` never falls while `storage` stays the same, and a release goes through the window like a move | what the words mean: it knows no stride, no entry, no element, and holds no representation — the strategy tag it stores is opaque to it beyond being one of three | nothing but `core` |
| `array/vector` | storage strategy 2, the mixed vector: dense integer keys `0..len`, one `Value` per element, no key stored and no index; the 2 → 3 migration's source | the head's words; its own `used` rule | keys beyond the dense range — a key it cannot hold migrates the array (`element::representation_for`); the collector | `array/head`, `memory/routing`, `refcount`, `value` |
| `array/entry` | the 32-byte entry — `hash_or_key`, `key`, and the element Box whose tag word carries the collision link as a `u32` in its top bytes, at the entry's +28 on the immediate arm and +20 on the pointer arm — the sentinel `NONE`, which ends a chain and empties an index slot alike, with the `MAX_ENTRIES` cap a `u32` index imposes; and every store into a word the collector reads — both words of the element and the key word, `make_hole` included | that the link shares the element's tag word, selected by the `+8` word's arm, so tag, flags and link publish as one relaxed atomic store of the width the collector loads, and that a null element is spelled `(0, 0x0001 \| link << 32)` so the `+8` word is never an even non-zero non-pointer; which key states the raw word encodes, the hole among them; that an entry above the published count is filled by the plain setters instead, no reader being able to reach it yet | the index's shape and every operation over the entries: it supplies the sentinel and reads no slot, hashes nothing, and does not know what an element points at | `string`, `value` |
| `array/table` | one storage allocation (`u32` index slots, then the dense entry array in insertion order) and the operations over it: lookup, insert, remove, growth by doubling or by dropping the holes, both into a fresh chunk, the collision defense's salted rebuild and keyed-hash escalation, and the bracket it opens around every move of an entry | the memory category, handed to it as a parameter by every allocating call (`array::entity::category_of` reads it) — except at the carry out of a dying arena, which names `GcHeap` because the owner's header still says `RequestArena` until promotion rewrites it; a string key's bytes and its cached hash; that nothing inside the storage points into it, so promotion copies it whole; that the words a walker reads are not its own — the chunk, the two counts, the tag and the version arrive as `head: &StorageHead` on every call that touches them | entities altogether: no kind, no header, no reference. It allocates none, retains none, releases none and calls no store barrier — it states the ownership its callers owe (`insert`'s one reference per stored key, `remove`'s `#[must_use]` pair) and hands the displaced element back for the layer above to act on. It holds no category of its own either, that field having drifted once (2026-08-07), and no storage head, a `&mut Table` being unable to span one (2026-08-11) | `array/entry`, `array/head`, `hash`, `memory/routing`, `refcount`, `string`, `value` |
| `array/element` | the generic element layer over the table: `canonical_key`, the five operations, the separation composition every write goes through, the element reference box, and the teardown of anything it could not publish | COW separation and the order it publishes in; that an element reference is a `ReferenceBox` because growth moves an entry, and that the box is a heap entity whatever the array's category; that canonicalisation belongs above the table, a map keying exactly | the entry layout, the index, the chains — it names keys and elements, never an entry | `array/entity`, `array/head`, `array/table`, `array/vector`, `memory/arena`, `memory/barrier`, `memory/context`, `object`, `refcount`, `reference`, `string`, `value` |
| `array/entity` | the `RcHeader` over the table — kind Array, COW set, no class pointer — with the factories, the copy for both depths (`separate`), the child walk, the teardown drain that takes a nesting down without the machine stack, and — since the head moved here — the access paths every representation is reached through (`as_table_mut`), the storage's disposal and its carry out of a dying arena | the entity kind, the memory category and the COW state; that a nested array leaves the candidate buffer here, never having reached `ll_entity_die`; which representation the union holds — it owns the tag and asserts it, and it owns the rule that no reference may span the head or the whole entity | classes, an array having none; the element operations above it; the collector's phases | `array/entry`, `array/head`, `array/table`, `array/vector`, `cells`, `journal`, `memory/arena`, `memory/barrier`, `memory/block_pool`, `memory/routing`, `memory/stdapi`, `object`, `refcount`, `reference`, `string`, `value` |

Its one upward edge is `array/entity`'s, into `cells`, and it is in the table above.
What `entry` and `table` promise `Map` is that they read no entity but a
string key, whose bytes they compare and whose cached hash
`LLString::hash` fills on first use; they allocate none, retain none and
call no barrier, and they read no header either (`dev/DECISIONS.md`,
"the table is handed its category and reads no header") — the category
arrives as a parameter, the way `owner_cat` arrives at the barrier and
for the same reason, a destination that may have no header at all.
`element` and `entity` also close a cycle with `object` — the COW doors
and `ll_entity_die`'s Array arm dispatch in while the copy and the
teardown they run call back out — which is why `object` names
`array/entity` and both array rows name `object`.

### L4 — collectors

| Module | Responsible for | Knows | Does not know | Depends on |
|---|---|---|---|---|
| `gc` | the GC C ABI and the safepoint: `ll_gc_collect_cycles`, `ll_gc_maybe_collect`, `ll_gc_checkpoint`, `ll_gc_checkpoint_ack`, the embedder's `ll_gc_set_collector_cap`, and the poll's duties in order — refill the log reserve, refill the critical reserve, refill the queue's spare cells and drain its overflow buffer, read the token byte and act on it (consent to a request, `POSTED` arming the collection over P), make the returns a foreign trace left withheld, re-offer the deferred lane where the epoch moved, then behind `may_collect` fire what the arming word names — P alone or R whole — note a freeing disposition for the collector's timer, and signal the collector the record names | that the checkpoint bracket is emitted in every build, collector or none | the arming policy (compiler's); any collector's internals past the token byte, the arming word and the epoch count it reads — the two collecting entries call `cycle::collect` and read nothing of it | `cycle`, `memory/critical`, `memory/reserve` |
| `cells` | the kind-dispatched tracer (`trace_entity`, `trace_cells`), the single sever dispatch, and a `#[cfg(test)]` heap census | entity kinds and each kind's out-edges; the outside-cell group's four behaviours | slots, blocks, occupancy — the heap's side of the split | `array/entity`, `array/entry`, `array/head`, `array/table`, `array/vector`, `class`, `memory/arena`, `memory/barrier`, `object`, `refcount`, `reference`, `value` |
| `cycle` | `rc-cycle` whole: the in-line collection and the collector thread beside it; which submodule holds which part is `dev/INDEX.md`, "Entry points". The mutator's side: the candidate ring R a non-final decrement writes into by two plain stores (`queue`, in `ring`'s form, grown out of spare cells the poll fills and never refusing), the trace token that excludes a second reader of the thread's graph (`token`, standing in the record the process keeps past the thread, `mutator_record`), the window that withholds a dead slot's physical return while a row may still name it (`deferred_slot_reuse`; a queue entry's hold is `refcount`'s bit, read in `ll_free`), and the collection in the order `collect` runs it — the scratch arena over the thread's workspace (`arena`), the rows and the block dispatch that finds one (`shadow`, `row`), mark and scan over one batch (`mark`, `scan`, `trace`, `stack`, `records`), the exact validation on the owning thread (`validation`), guards, weak nulling, destructors and the second reading (`finalization`), the maturation stamp and the epoch it is read against (`maturation`, `epoch`), the sever and the frees (`reclamation`), the harvested member list a pressure collection tears down from (`members`, `membership`), and the close's in-place compaction of R (`queue::compaction`). The collector's side (`worker`): a round over the records named to it that, per owner at or above the threshold, requests the token, waits a bound for consent, peeks a batch from behind the writer, traces it on its own arena through `cells::AtomicCells` under a block budget, posts one verdict per root into P (`queue::verdicts`) and releases to `POSTED`, which arms the owner's next collection over P; the elder is born at the poll's first wake or at a pressure collection's ending, and births siblings under the embedder's cap | which block kinds carry rows; that a retained block places an occupant by its position in the reset's index and a large entity by being alone in its block; that an edge out of the GC heap is an external live reference rather than an error; what a row means before the trace has met it; that a component reaches validation as its own member list on the pressure path, the rows having gone back with the arena, and as the rows themselves on the path off the poll; that the token is held from the take through the close's last store, so a collector's claim fails on a collecting mutator in one swap | strides, size classes and slot occupancy, which are `heap`'s; entity kinds and out-edges, which are `cells`' and which it asks that module for rather than dispatching on itself; how the index it searches was built, which is `promote`'s; entity layout inside a queue entry; owner retirement reads only its count and free mark through `refcount` | `memory/block_pool`, `memory/heap`, `memory/retained`, `memory/large_entity`, `memory/critical`, `memory/stdapi`, `memory/gc_metadata`, `memory/barrier`, `memory/buffer_arena`, `memory/reset_window`, `memory/os`, `refcount`, `cells`, `object`, `weak`, `gc`, `ring`, `journal` — re-enumerated 2026-09-18 from the `crate::` paths of production code; `memory/os` for the stack a collector slot keeps |
| `promote` | arena death with promotion (retention only): the destructor/escapee fixpoint, internal-edge counting, in-place category rewrite to GcHeap, `BLOCK_KIND_RETAINED` stamping over a cleared collector line, each retained block's survivor list placed in the arena's own memory — the block's own tail, the current block, a fresh block — and published in its header, the release-at-reset log | escapee hold-count semantics; the retained block kind; that a block it publishes as retained carries no shadow rows | copying / evacuation (future); who mounted the arena; how the index is read; what the collector puts in that word afterwards | `array/entity`, `cells`, `class`, `journal`, `memory/arena`, `memory/block_pool`, `memory/heap`, `memory/large_entity`, `memory/reset_window`, `memory/retained`, `memory/stdapi`, `object`, `refcount`, `string`, `weak` (`arena::alloc_preferring` places the survivor lists) |

## Shared resources

| Resource | Lives | Owner | Borrowers |
|---|---|---|---|
| Block pool + region registry | process-global mutex + per-thread caches | `block_pool` | `arena`, `heap`, `immortal`, `buffer_arena`, `reserve` get/put blocks |
| Thread heaps (`ThreadHeaps`: raw + entity) | TLS | `heap` (built by `ll_thread_init`, once per thread life; an allocation path finding none on a thread with no base block ends the process) | `stdapi` routes frees in; blocks migrate between threads only via the abandoned lists |
| The mounted arena | per `LLContext` | `context` (mount), `arena` (mechanics) | `barrier` and `buffer` write its logs; `promote` consumes them at reset |
| Log reserve (two blocks) | per thread | `reserve` | arena log growth draws; the `ll_gc_maybe_collect` poll refills |
| Critical reserve (eight blocks) | per thread | `critical` | two borrowers: the collection's arena, for a block **above its workspace**, which the reserve may never fund, drawn after the pool refuses and given back at its reset; and the candidate ring, for a block when both its spare cells are empty, which asks no pool first. The withheld returns are not a borrower: they draw nothing at all, each standing in the dying entity's own memory. The `ll_gc_maybe_collect` poll refills |
| Candidate ring R, its base block and its spare cells | one 64 KiB base block per thread for the life of the thread, holding the overflow buffer and the control line; the ring's blocks linked in a circle; two spare cells; the deferred lane as a chain of the same blocks | `cycle::queue` | `refcount::release_word` writes entries; a collector holding the token reads behind the writer; the `ll_gc_maybe_collect` poll fills the spares and drains the overflow buffer; `memory::gc_metadata` counts the blocks, charged whole from their link |
| Mutator record and the verdict ring P | four 64-byte lines per thread in GC-metadata blocks the process keeps, reused through a free list; one block for P drawn with the record | `cycle::mutator_record` | `cycle::token` is the byte in the first line; `cycle::worker` reads the rings' words and writes P; the owner reads P in the collection `POSTED` arms; `heap::ll_thread_init` and `ll_thread_exit` draw and return them |
| Collection workspace | one 64 KiB block per thread from its first collection to its exit, its address in the base block's control line; two fixed regions at its head — the withheld returns' 64-byte control line and the member list's 8,256 bytes — and 56,960 of bump behind them | `cycle::queue` lends it, `cycle::arena::TraceScratchArena` bumps in it, `cycle::members` writes the second region | drawn through the ordinary allocation path alone and rewound at every trace close; `memory::gc_metadata` counts the block, and charges the bump region alone |
| Shadow rows and the collection's bump | the workspace, plus 64 KiB blocks for the length of one collection where one workspace does not hold the rows | `cycle::arena::TraceScratchArena` | `mark`, `scan` and `stack` read and write rows out of it, and `cycle::reclamation`'s deferred-drop queue takes segments of it after the rows are gone; `memory::gc_metadata` counts the blocks |
| In-line trace withheld returns | one stack through the dead entities themselves and nothing besides; TLS holds one non-owning pointer to the control line the head stands in, inside the workspace's fixed region, and there is no drop glue | `cycle::deferred_slot_reuse::ActiveTrace` | `stdapi::ll_free` pushes physical returns while mark or scan may still address a row; the window pops the stack after its owned arena has swept its rows, and the arena gives its own blocks back after that — so an unwind out of that hand-back still finds every return made; `memory::gc_metadata` counts nothing of it, the region being the workspace's |
| Immortal region | process-global mutex | `immortal` | `class`, `intern`, `object` (immortal category) |
| Intern table | process-global mutex, Rust-owned | `intern` | `class` looks names up |
| Retained-block survivor lists | the block's own header line and the arena's memory; no process-global structure, no lock, one atomic count word per block | `retained` | `promote` places and publishes at reset; the trace and `heap`'s test-only enumerator read the header |
| Static-block registry | one long-lived buffer chunk while blocks are registered, TLS holding the `Buffer` and no drop glue | `static_block` | the static initializer registers; `heap`'s `ll_thread_exit` drains and then closes it, and `ll_thread_init` reopens it for the thread's next life; `buffer_arena` owns the chunk |
| Weak table | one long-lived buffer payload for the life of the thread, TLS holding one non-owning pointer to it and no drop glue | `weak::table` | death sites call in, gated by `HAS_WEAK_REFERENCES`; the collector thread never touches it; `buffer_arena` owns the chunk it sits in, and no figure of `gc_metadata`'s ledger moves with it |

Three rows left this table on 2026-08-26 with the collectors that owned
them: `rc-trace`'s candidate buffer, `rc-walk`'s confirmation queue and
handshake, and its GC activity flag with the parked lists. `rc-cycle`'s
replacements are a per-thread root queue (`cycle::queue`), per-block shadow rows in
an arena of their own (`cycle::shadow`; the triple that reaches them is
on each block's header line already), the in-line owner's deferred-reuse
list (`cycle::deferred_slot_reuse`), the per-thread trace token (`cycle::token`: one byte of five
states, `FREE / MUTATOR / REQUESTED|s / COLLECTOR|s / POSTED`, taken by
compare-and-swap, held by the owner from its take through its close and by
a collector for its batch, with a mutex an owner waits on when a collector
holds it, standing in a record the process keeps past the thread,
`cycle::mutator_record`), and the two rings beside it: the
candidate ring a collector reads behind the owner's writer, and the verdict
ring the owner reads in the collection its byte, at `POSTED`, arms (`rfc/dev/DECISIONS.md`, "the candidate
queue is read behind its writer, and the collector's verdicts come back by
a second ring").

**The header flag word is itself a shared resource.** `refcount` owns
the layout (its constants are normative; this ledger records who each
field is lent to):

- bits 0–1, memory category — stamped at allocation, read by the
  barrier and every death path; rewritten in place only by `promote`;
- bits 2–5, entity kind, four bits — written once at creation,
  dispatched on by `cells` and `ll_entity_die`. It sits beside the
  category because the codes are assigned so that three questions are
  range tests, and a range is one comparison only while the field's high
  bits carry it: `kind_may_close_a_cycle`, `carries_a_class_word` and
  `is_string` are those three. Codes 0–7 are held for kinds a ring can
  close through and four of them stand free, which is what a fifth such
  kind takes instead of a renumbering; `EntityKind::closes_a_ring` is the
  classification and a `const` assertion ties it to the bound;
- bit 6, `COW` — retain/release become no-ops, a write separates;
  read by `refcount` and the barrier, stamped by `intern` (the
  mechanism behind invariant 13);
- bit 7, `ARENA_RESET_MARK` — the reset's transient survivor mark,
  written and consumed by `promote`. It shares the word with the
  collector's fields safely because an arena entity is never a
  candidate, so a reset and a collection never mark the same entity;
- bits 8–10, the collector's three marks — `ACYCLIC_GATE`,
  `OWNERSHIP_MARK`, `CANDIDATE_BIT`. The release path reads them together
  with the category and the kind's top bit as one `flags & 0x723`
  (`refcount::CANDIDATE_GATE_MASK`), so a constant landing on any of them
  would make the gate refuse candidates for a reason the design does not
  have. `CANDIDATE_BIT` is written by `refcount::release_word` beside the queue
  entry and cleared only by the owner's exact retirement of that entry; the
  thread-exit drain deliberately leaves it standing. `OWNERSHIP_MARK` is
  moved by the barrier's owned store (`memory::barrier::store_ptr_owned`)
  and read again by `object::ll_default_dispose`, which destroys a marked
  child with its holder; `ACYCLIC_GATE` is written once, by
  `object::stamp_into` from the class's `CLASS_ACYCLIC`;
- bit 11, `IS_ESCAPEE` — repurposes the refcount as the escapee
  hold-count (see invariant 5);
- bit 12, weak gate (`HAS_WEAK_REFERENCES`) — lent to `weak`; death
  sites test it before calling in;
- bits 13–14, destructor state (`DESTRUCTOR_PENDING` / `DESTRUCTOR_RAN`)
  — the debt protocol between `object` and the death paths;
- bit 15, `DEAD_IN_PLACE` — `ll_free` has taken this slot and has not handed
  it back. The head of the free sets it for every entity kind and refuses a
  free that finds it already up, which is what makes a second `ll_free` of one
  entity do nothing rather than put one address on a free list twice. Three
  headers carry it: a size-class slot, a retained survivor and a large
  entity's own header. It stands from the free of one occupant to the
  publication of the next, so a free-listed slot carries it and so does one
  whose return a trace window is withholding; what separates those two is the
  physical return, not the header, and the withheld ones are found through the
  window's own stack rather than through any bit. What hands a slot back is
  `refcount::publish_header`, owner candidate retirement, and
  `memory::stdapi::hand_back_and_free`, the one pairing of the hand-back with
  the free, reached by the trace window's close ahead of its return, the
  reset window's flush and every path that frees a slot it never published
  (`memory::stdapi::free_unpublished`). The
  count stays zero under it, so `refcount::slot_state` is the
  one occupancy test and a guard test bans the two-way one. The bit carried
  `STRING_OUT_OF_LINE` until the string's two layouts became the kind codes 8
  and 9, and taking it fills the mutator's half: a further mutator flag needs
  a re-lay rather than a free position;
- bits 16–19, the **maturation stamp** — the epoch a commit wrote it in at
  16–17 and the age at 18–19. The one writer is the owning
  thread's commit, through either of its two producers — the component its
  exact validation read as externally referenced (`cycle::finalization`) and
  the live components its trace walked (`cycle::maturation`) — and it writes
  both fields with one
  byte-wide read-modify-write at offset 6, so the access covers no bit a
  mutator path reads. The age counts consecutive collections of one epoch and
  saturates at 3; an age under any epoch but the current one reads as no age,
  which retires a stamp with no pass to clear it. The reader of record is the
  mark's descent, which stops at a mature edge target it finds registered in
  no candidate queue (`cycle::mark`);
- bits 20–31, **free**. Bits 20–23 are the collector's reserve and share byte
  6 with the stamp, which is why the stamp is written as a
  read-modify-write rather than as a store of the byte.
  `refcount::tests::the_header_the_compiler_shares` asserts that no mutator
  constant claims a bit above 15 and that the collector's own fields stay
  inside 16–19.

## End-to-end paths

**1. Object allocation.** Compiler (or C caller) → `ll_object_new`
(`object`) → category dispatch: arena bump (`arena`) / `entity_alloc`
(`heap`, entity population) / `immortal` → body zero-filled, header
stamped (`refcount`) → `ll_object_constructed` records the owed
destructor: for arena objects in the mounted arena's log, for
heap/immortal objects as `DESTRUCTOR_PENDING` alone. The compiler
inlines the bump-pointer version when class and category are
statically known; both perform the same steps. No GC test rides this
path (the checkpoint moved to the death branch, 2026-07-27).

**2. Reference store.** The compiler composes micro-ops per site
(`barrier`): `store_ptr`/`store_box` = retain (`refcount`) + category
barrier — a cross-category store records an escape or release-at-reset
into the mounted arena's log (`context` answers which arena; `reserve`
funds growth when the pool refuses; failure becomes a flag the next
poll turns into a raise) — + the write. An overwriting store then
`drop_ref`s the displaced entity: release, and at zero the cascade
into path 3. Publish before teardown, always in that order.

**3. Entity death (refcount path).** `ll_release` hits zero and the
death takes the ordinary path — nothing is consulted to decide it.
Compiler-batched release runs are bracketed by the safepoint pair,
split around the run (2026-07-28): `ll_gc_checkpoint_ack` before,
`ll_release_batch` per reference, `ll_gc_checkpoint` after →
`ll_entity_die` kind-switch (`object`) → for objects, three-phase
`ll_object_die`: dispose (pre-destructor with resurrection check; weak
notification is the *first act* of phase 2, before children drop via
the class's typed runs) → free by category — arena memory just stays,
heap memory goes through the size-less `ll_free` funnel (`stdapi`).

A **non-zero** decrement is where `rc-cycle` registers a candidate:
`refcount::CANDIDATE_GATE_MASK` decides and `cycle::queue` stores it.
The free path refuses physical reuse while either identifier stands —
the queue entry through `CANDIDATE_BIT`, and
`cycle::deferred_slot_reuse::ActiveTrace`
while mark or scan may still address the slot's row. The trace window
owns its shadow arena, so rows are nulled before any withheld return is made.

**4. Arena reset.** `ll_arena_reset` (`context`) →
`promote::arena_reset_full` drives the whole discipline, draining the
arena's logs through arena's own primitives: the fixpoint (survivors
marked from escapee hold-counts via `ARENA_RESET_MARK`;
pre-destructors of the dying may create new escapes and destructors,
hence the loop) → internal-edge counting → survivor categories
rewritten in place to GcHeap, their blocks stamped
`BLOCK_KIND_RETAINED` and kept out of the pool → the release-at-reset
log pays one release per record → the arena weak log drains (`weak`) →
the survivor list is grouped per block, written into the arena's own
memory and published in each retained block's header (`retained`),
which is what makes their occupants walkable at all → every other
block, the reserve-drawn ones included, returns to the pool.

**4a. Thread exit.** `ll_thread_exit` (`heap`), reached explicitly or
from the TLS guard → the static-block pass (`static_block`) releases
each registered block's roots in reverse registration order through the
barrier's `drop`, and the registry closes behind it → the exit's collection
(`cycle::collect::collect_before_exit`)
claims the thread's trace token for good, waiting for a holder, then collects what the
thread left registered in rounds until one makes no progress, and reports
what is still registered as the exit's residue; these two are the steps
here that run user code → `weak::dispose` returns the weak table, after
every death that could still need a row → `buffer_arena::dispose` returns
the thread's buffer arena, whose blocks go to the process-global pool,
after every step above that can still free a buffer into it → the thread's
heaps are dropped and their blocks are abandoned or returned.

**5. In-line cycle collection.** A non-final decrement registers the
entity (`refcount` → `queue`: two plain stores into the ring R). A
collection fires off the safepoint — `ll_gc_collect_cycles`, or
`ll_gc_maybe_collect` when the arming word names R whole or, at `POSTED`,
P alone — or under pressure at `entity_alloc`'s refusal, and `cycle::collect`
runs it in one order on both paths: `may_collect` refuses a collection
reached from inside one, beside a teardown in flight or an open reset; the
token is taken (`token`), waiting on a collector that holds it; the trace
scratch arena opens over the thread's workspace and the window inside it
(`arena`, `deferred_slot_reuse`), the one refusal left being a first
collection that cannot draw the workspace; the batch is R read whole behind
its writer with P's roots ahead in it — the explicit fire, the exit's rounds
and an arming for R — or P's proposed roots alone when `POSTED` armed it
(`queue::verdicts`); the poll under a collector's claim returns instead of
waiting (E9), the other three entries wait;
`mark` subtracts the internal edges over the rows and stops at a mature
stamped target no queue names; `scan` colours what survives;
`validation` re-reads the members on the owning thread; `finalization`
guards the component, nulls the weak cells naming it, runs the destructors,
reads it again and stamps every component either reading proves live
(`maturation`, `epoch`); `reclamation` severs, frees through the ordinary
death path and drops the displaced children after the last free; the close
compacts R in place — completed deaths retired through `ll_free`, roots read
live marked into the deferred lane, the rest kept in order — and disposes of
P's batch (`queue::compaction`); the token is held from the take through the
close, on both paths, and its release is the close's last store
(`rfc/dev/design/trace-token-handshake.md`, E10). The pressure path differs after the trace
alone: it harvests the member list (`members`) and gives its blocks back
before the first destructor, because the destructors allocate
(`membership`). The design is `rfc/model/gc/rc-cycle.md`.

**6. The collector thread's batch.** The elder is born at the poll's first
wake or at a pressure collection's ending, through `ll_thread_init` like any
thread (`worker::ensure_thread`), and wakes on an owner's poll that filled a
block of R, on an owner's consent to a request it left standing, on a
pressure collection, or on its timer. A round walks the records named to it
(`mutator_record`) and, for an owner whose R holds the threshold or more and
whose byte reads `FREE`, requests the token — `REQUESTED|s` — and waits a
bound for the owner's consent, given at its next poll or slot free; a
collecting owner's byte reads `MUTATOR` and the request's swap fails on it
(path 5). Under `COLLECTOR|s` it
peeks up to K entries from behind R's writer, clamped to P's room, traces
them through `cells::AtomicCells` on its own arena under a block budget,
posts one verdict per root into P in R's order, advances R past them by a
guard that runs from the unwind too, and releases to `POSTED`, which the
owner's next slot free or poll reads as the arming of path 5 over P. Every
death on the owner's side while a collector holds its token is withheld
(`deferred_slot_reuse`, "A foreign holder of the token") and returned when
the owner reads the token free. Siblings born under the embedder's cap
(`ll_gc_set_collector_cap`) take every second backlogged owner by the word
in its record, and the elder ends an idle one.

## Cross-module invariants

The things that broke documentation the week there was nowhere to
write them. Each is load-bearing for at least two modules.

1. **The block header is a tagged union**: `kind` at offset 0 in every
   block, always; the pool's `next` overlays the heap's `used`. Layout
   pinned by `heap::tests::the_block_under_the_slots::block_header_halves_are_laid_out_as_the_design_requires`.
2. **Every block is 64 KB and size-aligned**, so `ptr & !BLOCK_MASK`
   finds its header. Foundation of size-less free, remote free, and
   slot walking. Regions never return to the OS (phase 1).
3. **Every entity begins with the 8-byte `RcHeader` at offset 0**
   (pinned by `refcount::tests::the_header_the_compiler_shares::header_is_8_bytes_at_offset_zero`).
   What `+8` holds depends on the entity kind; nothing reads `+8`
   without a kind dispatch.
4. **A dead entity slot keeps its final refcount of zero** in bytes
   0–7, which is the walker's occupancy test. That is why every intrusive link
   through dead memory (heap free list, remote-free list) lives at
   bytes 8–15, and why entity blocks are zeroed at commissioning. The flags
   half of that word is written on a free, by `ll_free`'s own head and by
   nothing below it (`refcount::DEAD_IN_PLACE`); the count is what the
   invariant is about and no free touches it.
5. **An escapee's hold-count lives in its `refcount`** while
   `IS_ESCAPEE` is set: the barrier and holder teardown maintain it,
   `promote` consumes it. Promotion rewrites the category in place —
   the pointer-tag alternative was rejected exactly because this
   rewrite must be possible.
6. **Raw and entity blocks never mix.** The heap runs twice per
   thread — a raw heap for C-ABI buffers, an entity heap for GC
   entities — because the walker reads every occupied slot's first
   8 bytes as an `RcHeader`: one raw buffer in an entity block and the
   census reads garbage. `stdapi` routes by block kind; `entity_alloc`
   is the only door into entity blocks.
7. **A reserve block never becomes an arena bump block** — otherwise
   ordinary allocation would eat the reserve and the barrier's
   no-failure conversion collapses.
7a. **A walkable block is either strided or indexed, never both.** An
   entity block has one size class, so a slot is `payload + s *
   class_size` and an address divides back. A retained former-arena
   block was bump-filled and has no stride at all: its slots are the
   addresses in the survivor list its header names, and an address is
   found by exact match in it. Both enumerators and the census branch on exactly this,
   and the census does so *after* one shared binary search, which is
   what keeps row omission and edge omission one decision at one test.
   A survivor list is frozen because nothing allocates into a dead
   arena, and a stale entry is harmless because a dead survivor reads
   refcount 0 — invariant 4 again.
7b. **No `thread_local!` the exit path can reach may have drop glue.**
   `ll_thread_exit` runs from a TLS destructor and, since A6, runs user
   code; TLS destructor order is unspecified, and on glibc it is reverse
   registration order, which destroys the exit guard last precisely
   because it registers first. A key with drop glue is therefore
   reliably already gone, `with` panics with `AccessError`, and a panic
   in a destructor cannot unwind — the process aborts. Every such
   structure is a cell with no drop glue — a `Cell<*mut T>`, or the
   structure itself where it has no `Drop` (`static_block`'s `Buffer`) or
   is held under `ManuallyDrop` (the buffer arena) — disposed of by an
   explicit `dispose` in the order `ll_thread_exit` fixes. `block_pool`'s cache and `reserve` are
   the sanctioned exceptions: they use `try_with`, and there failure
   means "go to the global tier", which is sound.
8. **Publish before teardown**: the barrier owns the whole slot; an
   overwriting store is `store_*` then `drop_ref`, never the reverse.
9. **Death is owner-bound.** Every free and every weak-table touch runs
   on the entity's owning thread. The design sharpens that for
   `rc-cycle`: a collector proposes a shortlist, and every reduction of
   state is the owner's, taken on an exact reading at its own checkpoint
   (`rfc/model/gc/rc-cycle.md`). The in-line collection retires completed
   candidate deaths after membership and shadow readers end; the collector
   thread's verdicts are a shortlist the owner disposes of at its poll
   and into every in-line collection's batch, re-reading the entity
   before it retires anything (`cycle::queue::verdicts`). Collections run from the poll, explicit fire and
   allocation refusal (`cycle::collect`).
10. **Arm vs fire**: nothing collects mid-mutation. A collection fires
    only at a clean point, and the crate has three of them: the ABI's
    `ll_gc_collect_cycles`; the `ll_gc_maybe_collect` poll, whose one
    caller inside the crate is `object::ll_release_vector`'s backedge;
    and the entity allocation a refusal ends, where
    `memory::heap::entity_alloc` runs
    `cycle::collect::collect_under_pressure` once and asks again — the GC-heap
    and long-lived categories only, an arena entity being served by
    `Arena::alloc_entity` and a bulk reservation by `Heap::reserve_cells`,
    neither of which passes through that door. The rule is a
    correctness requirement rather than a policy: a store lowers the old
    value's count before overwriting the pointer, and a collection
    firing in that window would subtract one reference twice
    (`rfc/model/gc/strategies.md`, "Collection requests and triggers").
11. **One configuration.** The GC axis went with the two collectors on
    2026-08-26; `hash-folding` and `debug-journal` are what remains of
    the build matrix (`WORKFLOW.md`).
12. **Every death is eager, and a teardown drops on a zero-count member**
    (eager-death amendment, 2026-07-27, superseding the F5
    deferral/marker scheme): a release reaching zero always tears down
    at the natural point, with only the memory's reuse deferred
    (out-of-band — the window that defers a slot's reuse writes the stack's
    link in the dead entity's byte 8 and nothing else of the memory; the flags
    bit in its header is `ll_free`'s own and says nothing about the
    deferral). The zero-count-member rule that goes with it opens the
    cycle teardown: a component holding a member already at `rc 0` is
    dropped whole, before any field is traced or any guard written
    (`rfc/model/gc/rc-cycle.md`, "Cycle finalization and reclamation",
    step 1).
13. **An interned name *is* a valid immortal string entity** — the
    future string machinery reads it as-is; immortal + COW makes
    retain/release no-ops on it.
14. **Class descriptor addresses are process-stable** (immortal): the
    foundation for inline caches; dispatch tables stay pure
    code-pointer arrays with no embedded headers.
15. **One trace at a time per mutator thread**, the claim being that
    thread's rather than the process's (ruled 2026-08-29,
    `rfc/dev/DECISIONS.md`, "a trace stays inside the blocks of the
    thread it claimed"). A trace never reaches another thread's blocks,
    because a transfer leaves no reference behind: the graph arriving in
    a thread holds no reference to an object that stays in the source
    (`rfc/dev/DECISIONS.md`, "a transfer leaves no reference behind").
    So the rows two collectors touch are disjoint and several collectors
    run at once on different threads. A block changes threads only at
    thread exit, through `abandon_all` and `adopt`, and the exit claims the
    token for good before it hands anything over, waiting for a holder
    (path 4a; `rfc/dev/DECISIONS.md`, "a thread waits for the trace,
    collects, and then exits"), so no trace addresses a block while it
    changes hands. What the exit's rounds could not take keeps its
    candidate bit and is registered again by nobody once its blocks are
    adopted, a bounded leak the rfc's plan carries. The process-wide form,
    `amSolo`, is withdrawn with the premise it rested on. The claim word is
    `cycle::token`'s.
