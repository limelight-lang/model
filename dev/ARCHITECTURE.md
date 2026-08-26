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
L4  collectors        gc (ABI + safepoint) · cells · promote
L3  object model      object · class · reference · weak · intern ·
                      static_block
LB  mutation          memory/barrier — model-level code living in memory/
L2  memory manager    context · arena · heap · immortal · buffer ·
                      buffer_arena · reserve · retained · stats · stdapi ·
                      routing · large_entity · reset_window
L1  entity substrate  refcount · value
L0  block supply      memory/block_pool
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
- `lib.rs` is the crate root and `memory/mod.rs` the folder root:
  re-exports and the module-doc declaration that `memory/` implements
  `docs/memory-manager.md`; no logic, no layer.

**Sanctioned upward edges.** At module granularity the layering is not
acyclic: entity death and GC scheduling flow back down through
call-backs. Exactly these upward edges exist in production code (re-enumerated
mechanically 2026-08-26 by resolving every `crate::…` path against the
module layers above, test modules excluded), each entered at a named
point:

| Edge | Point of entry | Why |
|---|---|---|
| `object → gc` | `ll_release_vector` | the safepoint bracket a batched run pays: `ll_gc_checkpoint_ack` before it, `ll_gc_checkpoint` after (decision 2026-07-28). Both bodies are empty while no collector is wired |
| `object`, `class`, `template`, `array::entity` `→ cells` | `for_each_counted_cell`, the descriptor's outside-cell group, the template's shape stride, `for_each_counted_child` | one kind-dispatched tracer rather than a stride per customer; the array adapter calls up rather than striding the entries a second time |
| `arena → weak` | `reset` | draining the arena weak log is part of arena death |
| `context → promote` | `ll_arena_reset` | the reset ABI drives the full discipline — `promote::arena_reset_full` consumes the arena's logs through arena's own drain primitives, not the reverse |
| `barrier → object` | `drop_ref` | the release cascade ends in `ll_entity_die`; `header_category` reads |
| `class → object` | descriptor construction | carries `ll_default_dispose` as the default dispose pointer (data, not a call) |
| `heap → static_block`, `weak` | `ll_thread_exit` | thread exit owns the order its per-thread state dies in, because TLS destructor order is unspecified and puts the exit guard last (decision 2026-08-03). These are disposal calls only: `heap` learns nothing about cells or verdicts, it names `dispose`-shaped functions in a fixed sequence |

**What a collector will add back.** A dying enrolled slot owes the
collector a duty at two doors (`object::ll_entity_die` and
`array::entity`'s drain, both marked S34.3). The release path's
enrolment gate is built — it decides and counts — but has no queue to
post to until S34.1, so the edge it will make is not there yet. Those are
upward edges and are a design event when they land, not a table entry
added quietly.

Any new upward edge is a design event: stop, discuss, record in
`DECISIONS.md` — do not just add it to this table.

## Knowledge map

"Does not know" is the contract column. "(hot)" marks the measured hot
paths (`INDEX.md`, "Hot paths").

The "Depends on" columns restate the production import graph as
verified 2026-07-27 (test modules excluded); where they drift, the
imports win — and a boundary change must update this file in the same
commit (`WORKFLOW.md`).

### L0–L1 — memory

| Module | Responsible for | Knows | Does not know | Depends on |
|---|---|---|---|---|
| `memory/block_pool` | 2 MB OS regions carved into size-aligned 64 KB blocks; global free chain (mutex) + per-thread cache; region registry | OS allocation; the `BlockHeader` base fields | what any payload contains; entities, classes, GC | — (bottom; its `heap`/`reserve` references are the shared test-lock harness only) |
| `memory/arena` (hot: bump) | request arena: bump allocation; self-contained bookkeeping — block list through block headers, destructor / escapee / release-at-reset logs as segment chains in its own memory; the log-drain primitives promote drives at reset | block pool; drawing the reserve for log growth; the `RcHeader`s its logs point at | object layout, classes, GC strategy; which escapees survive (promote's); the reset discipline itself — promote drives it from above | `block_pool`, `reserve`, `refcount`, `stdapi` (OS-direct payloads); upward at reset: `weak` |
| `memory/heap` (hot) | small-object heap, mimalloc model: one block per size class, intrusive free list + bump cursor, per-block MPSC remote-free, thread-exit abandonment / adoption; runs twice per thread (raw + entity heaps); both enumerators — `for_each_entity_slot` and the block snapshots — which cover retained former-arena blocks through their index as well as entity blocks by striding | block pool; slot occupancy (the header word at bytes 0–7); that a retained block has no stride | entity kinds and out-edges; verdicts; classes; the epoch protocol; who built the index | `block_pool`, `reserve` (filled at thread init), `refcount`, `stdapi` (OS-direct runs), `retained` |
| `memory/immortal` | global bump region: class metadata, interned strings; nothing is ever freed | block pool | the contents of what it hosts | `block_pool` (+ `arena::round_up_8`) |
| `memory/buffer` | growable `{data, len, capacity}` payload, no header; extend-in-place at the arena bump top, else copy; OS-direct above block payload | the mounted arena's bump; context resolution | entity lifecycle, headers; the long-lived buffer arena | `arena`, `context`, `block_pool` |
| `memory/buffer_arena` | long-lived buffer blocks (`BLOCK_KIND_BUFFER`): bump + per-block intrusive LIFO free list, pressure modes, per-block live count returning empty blocks | block pool; the buffer pressure protocol | the object heap; entities; GC | `block_pool`, `buffer`, `context`, `stdapi`, `arena` (`round_up_8`), `retained` (a payload's free is a retained block's release event) |
| `memory/reserve` | the two-block per-thread reserve funding store-barrier log growth; drawn only after ordinary refusal; sets the refill flag the poll checks | block pool | what a log records; barrier semantics | `block_pool` |
| `memory/retained` | the object index of each retained former-arena block: block address → its occupants, sorted; registered by the reset, read by both enumerators | block addresses and arrays of addresses, plus the one word it tests through them: a slot's refcount, which is occupancy | what lives at those addresses — entities, classes, verdicts | `block_pool` (stamping an emptied block and handing it over), `refcount` (`header_refcount`, the occupancy test's narrow read) |
| `memory/stats` | block-granular telemetry computed at query time; counters only on pool get/put — zero hot-path tax | pool counters | per-object events (the opt-in event log, unbuilt); arena/heap internals | `block_pool` |
| `memory/stdapi` | the size-less allocator front door: `ll_malloc`/`ll_free`/`calloc`/`realloc`/aligned + `GlobalAlloc`; routes `ptr & !BLOCK_MASK` → header `kind` | every block kind's free route; the heap's `MAX_SMALL`; the deferred-free parking check | entity semantics; who its callers are | `block_pool`, `heap`; `refcount` in the `#[cfg(test)]` assertion alone, which reads a freed slot's refcount and is the one place this module knows an entity at all |
| `memory/context` | `LLContext` and the TLS current context (NULL-context fallback); the composition root wiring arena + thread heaps + immortal behind one ABI, `ll_arena_reset` included | the arena mount; which module implements each ABI it fronts | class layout; GC strategy; thread-heap init (heap's `ll_thread_init`, reached from the allocation cold paths) | `arena`, `heap`, `immortal`, `refcount`; upward: `promote` (`ll_arena_reset`) |

### LB–L2 — mutation and substrate

| Module | Responsible for | Knows | Does not know | Depends on |
|---|---|---|---|---|
| `memory/barrier` (hot) | store-barrier micro-ops: `store_ptr` / `store_box` (retain + category barrier + write), `drop_ref` (release + cascade), `ref_store`; escape and release-at-reset recording into the mounted arena's logs | header category bits; `Value`; that `owner_cat` is a parameter, never a load | the per-site composition (lowering's); SATB hooks (A5, future) | `refcount`, `value`, `arena`, `context`; upward: `object` |
| `refcount` | the 8-byte `RcHeader` at offset 0 of every entity: refcount + flag word (category, entity kind, and bits 16-31 held for the collector); `ll_retain` / `ll_release` / `ll_release_batch`; relaxed-atomic header accessors, all narrow — four bytes for the counter, two for the mutator's half of the flags — because a wider one would overlap the collector's byte store without covering it. **The two fields are private to this module**, so *who may name a header* is the compiler's answer rather than a source grep's; *how wide the access is* stays the accessors', privacy having nothing to say about width and the module's own wide accesses being deliberate (`dev/DECISIONS.md`, "`RcHeader`'s fields go private") | its own bit layout, and which bits are lent to whom (see the ledger below) | entity bodies past 8 bytes; blocks; when to collect | nothing upward: the non-zero decrement's enrolment gate decides here and posts nowhere until S34.1 gives it a queue, and the death branch consults none |
| `value` | the 16-byte Box: payload + type tag + flags; `VALUE_UNDEF` as a flags bit, deliberately not a tag | the tags; which tags are counted | unboxed representations (compiler contract) | `refcount` |

### L3 — object model

| Module | Responsible for | Knows | Does not know | Depends on |
|---|---|---|---|---|
| `intern` | interned names as immortal string entities — one address per string for the process lifetime, inline hash; the global lookup table (Rust-owned metadata) | the string entity layout | classes — it serves them names and knows nothing of them | `immortal`, `refcount` |
| `class` | class descriptors: the inline vtable train (`[Class][vtbl][itables…]`, pure code-pointer arrays), method table, Cohen display; property layout as three typed runs; the trace lists (`ptr_runs` / `box_runs`); link-time construction | immortal allocation; interned names; the default dispose pointer | instance state; memory categories; GC; who calls the methods | `immortal`, `intern`; upward: `object` (dispose default) |
| `object` | `ll_object_new` factory; `ll_object_constructed` (destructor registration); three-phase `ll_object_die`; the kind-switched `ll_entity_die`; `for_each_counted_child` | class runs; every category's allocator; the weak gate bit; the destructor-debt protocol | collector internals; block internals; per-site barrier composition | `class`, `refcount`, `value`, `context`, `heap`, `immortal`, `stdapi`, `barrier`, `reference`, `array/entity` (the Array arms of the kind switch and of both COW doors), `gc` (forget candidate), `weak` (notify) |
| `reference` | the `&` reference box, entity kind 3: `RcHeader \| Value` — the model's only extra indirection, self-describing at teardown via the kind field | its own kind | classes; typed slot references (future) | `refcount`, `value`, `context`, `heap`, `immortal`, `stdapi`, `barrier`, `object` |
| `static_block` | the per-thread registry of static blocks and the teardown pass that releases their roots at thread exit (A6): registration in first-touch order, drained in reverse | that a static block is headerless and laid out by a descriptor; that a `__destruct` may register another block mid-pass | how a static block is allocated; what its slots mean — the release policy is the barrier's, the teardown `object`'s | `class`, `refcount`, `object`, `barrier` |
| `weak` | the kind-11 weak cell (the canonical `WeakReference` *is* the cell); the per-thread weak table; every notification rule (`notify_death` / `notify_members` / `drain_arena_weak_log`); `ll_weakref_create` / `ll_weakref_get` | the `HAS_WEAK_REFERENCES` gate; that cells always live in the GC heap; that only the owning thread touches the table | *when* to call in — that duty belongs to the death sites (dispose phase 2 first act, both collectors, arena reset) | `refcount`, `arena`, `context`, `heap`, `stdapi`, `object` |

The array is four modules under `mod.rs`, with a loom model beside them
under `cfg(loom)`, and the cut between them is what the rows record:
`entry` and `table` are the ordered hash with no entity lifetime in it,
which is the half `Map` is meant to reuse, while `element` and `entity`
carry what an entity brings — the store barrier, the reference box, the
teardown.

| Module | Responsible for | Knows | Does not know | Depends on |
|---|---|---|---|---|
| `array/head` | the words a concurrent walker may read — version, chunk, index-slot count, element count, strategy tag — and the seqlock bracket that makes reading them coherent (`begin_move` / `end_move`, `coherent`) | that a walker validates a reading rather than locking, that each word it reads is written by one atomic store of the same width, and that giving a reading up leaks one epoch rather than freeing early; that both fences are needed and why their ends differ (`version_bracket_model.rs`); and the two rules it states for whoever writes the chunk — `used` never falls while `storage` stays the same, and a release goes through the window like a move | what the words mean: it knows no stride, no entry, no element, and holds no representation — the strategy tag it stores is opaque to it beyond being one of three | nothing but `core` |
| `array/entry` | the 32-byte entry — `hash_or_key`, `key`, and the element Box whose reserved bytes carry the collision link as a `u32` at +28 — the sentinel `NONE`, which ends a chain and empties an index slot alike, with the `MAX_ENTRIES` cap a `u32` index imposes; and every store into a word the collector reads — the element's second word and the key word, `make_hole` included | that the link shares the element's second word, so tag, flags and link publish as one relaxed atomic store of the width the collector loads; which key states the raw word encodes, the hole among them; that an entry above the published count is filled by the plain setters instead, no reader being able to reach it yet | the index's shape and every operation over the entries: it supplies the sentinel and reads no slot, hashes nothing, and does not know what an element points at | `value`, `string` |
| `array/table` | one storage allocation (`u32` index slots, then the dense entry array in insertion order) and the operations over it: lookup, insert, remove, growth by doubling or by dropping the holes, both into a fresh chunk, the flood ladder's two rungs, and the bracket it opens around every move of an entry | the memory category, handed to it as a parameter by every allocating call (`array::entity::category_of` reads it, S10) — except at the carry out of a dying arena, which names `GcHeap` because the owner's header still says `RequestArena` until promotion rewrites it; a string key's bytes and its cached hash; that nothing inside the storage points into it, so promotion copies it whole; that the words a walker reads are not its own — the chunk, the two counts, the tag and the version arrive as `head: &StorageHead` on every call that touches them | entities altogether: no kind, no header, no reference. It allocates none, retains none, releases none and calls no store barrier — it states the ownership its callers owe (`insert`'s one reference per stored key, `remove`'s `#[must_use]` pair) and hands the displaced element back for the layer above to act on. It holds no category of its own either, that field having drifted once (2026-08-07), and no storage head, a `&mut Table` being unable to span one (2026-08-11) | `entry`, `refcount`, `value`, `string`, `hash`, `memory/routing`, `memory/arena`, `memory/block_pool` |
| `array/element` | the generic element layer over the table: `canonical_key`, the five operations, the separation composition every write goes through, the element reference box, and the teardown of anything it could not publish | COW separation and the order it publishes in; that an element reference is a `ReferenceBox` because growth moves an entry, and that the box is a heap entity whatever the array's category; that canonicalisation belongs above the table, a map keying exactly | the entry layout, the index, the chains — it names keys and elements, never an entry | `array/table`, `array/entity`, `barrier`, `reference`, `object` (`ll_cow_separate`, `ll_entity_die`), `refcount`, `value`, `string`, `memory/context`, `memory/arena` |
| `array/entity` | the `RcHeader` over the table — kind Array, COW set, no class pointer — with the factories, the copy for both depths (`separate`), the child walk, the teardown drain that takes a nesting down without the machine stack, and — since the head moved here — the access paths every representation is reached through (`as_table_mut`), the storage's disposal and its carry out of a dying arena | the entity kind, the memory category and the COW state; that a nested array leaves the candidate buffer here, never having reached `ll_entity_die`; which representation the union holds — it owns the tag and asserts it, and it owns the rule that no reference may span the head or the whole entity | classes, an array having none; the element operations above it; the collector's phases | `array/table`, `refcount`, `value`, `barrier`, `object`, `reference`, `string`, `memory/routing`, `memory/arena`, `memory/stdapi`, `journal`; upward: `cells` |

Both upward edges are `array/entity`'s and both are in the table above.
What `entry` and `table` promise `Map` is that they read no entity but a
string key, whose bytes they compare and whose cached hash
`LLString::hash` fills on first use; they allocate none, retain none and
call no barrier, and since S10 they read no header either — the category
arrives as a parameter, the way `owner_cat` arrives at the barrier and
for the same reason, a destination that may have no header at all.
`element` and `entity` also close a cycle with `object` — the COW doors
and `ll_entity_die`'s Array arm dispatch in while the copy and the
teardown they run call back out — which is why `object` names
`array/entity` and both array rows name `object`.

### L4 — collectors

| Module | Responsible for | Knows | Does not know | Depends on |
|---|---|---|---|---|
| `gc` | the GC C ABI and the safepoint: `ll_gc_collect_cycles`, `ll_gc_maybe_collect`, `ll_gc_checkpoint`, `ll_gc_checkpoint_ack`, and the log-reserve refill inside the poll | that the checkpoint bracket is emitted in every build, collector or none | the arming policy (compiler's); any collector's internals — the two collecting entries report zero until S36.7 | `reserve` |
| `cells` | the kind-dispatched tracer (`trace_entity`, `trace_cells`), the single sever dispatch, and a `#[cfg(test)]` heap census | entity kinds and each kind's out-edges; the outside-cell group's four behaviours | slots, blocks, occupancy — the heap's side of the split | `heap`, `refcount`, `object`, `value`, `class`, `array/entry`, `array/table`, `array/head`, `array/vector`, `array/entity` |
| `promote` | arena death with promotion (retention only): the destructor/escapee fixpoint, internal-edge counting, in-place category rewrite to GcHeap, `BLOCK_KIND_RETAINED` stamping, the survivor list handed on as each retained block's object index, the release-at-reset log | escapee hold-count semantics; the retained block kind | copying / evacuation (future); who mounted the arena; how the index is read | `arena`, `block_pool`, `object`, `refcount`, `weak`, `retained`, `array/entity` (a survivor's storage carries out with it) |

## Shared resources

| Resource | Lives | Owner | Borrowers |
|---|---|---|---|
| Block pool + region registry | process-global mutex + per-thread caches | `block_pool` | `arena`, `heap`, `immortal`, `buffer_arena`, `reserve` get/put blocks |
| Thread heaps (`ThreadHeaps`: raw + entity) | TLS | `heap` (initialized lazily on its allocation cold paths) | `stdapi` routes frees in; blocks migrate between threads only via the abandoned lists |
| The mounted arena | per `LLContext` | `context` (mount), `arena` (mechanics) | `barrier` and `buffer` write its logs; `promote` consumes them at reset |
| Log reserve (two blocks) | per thread | `reserve` | arena log growth draws; the `ll_gc_maybe_collect` poll refills |
| Immortal region | process-global mutex | `immortal` | `class`, `intern`, `object` (immortal category) |
| Intern table | process-global mutex, Rust-owned | `intern` | `class` looks names up |
| Retained-block object indexes | process-global mutex, Rust-owned | `retained` | `promote` registers at reset; both of `heap`'s enumerators clone the `Arc` under the lock and read it outside |
| Static-block registry | TLS, no drop glue | `static_block` | the static initializer registers; `heap`'s `ll_thread_exit` drains |
| Weak table | TLS, no drop glue | `weak` | death sites call in, gated by `HAS_WEAK_REFERENCES`; the collector thread never touches it |

Three rows left this table on 2026-08-26 with the collectors that owned
them: `rc-trace`'s candidate buffer, `rc-walk`'s confirmation queue and
handshake, and its GC activity flag with the parked lists. `rc-cycle`'s
replacements are a per-thread root queue (S34), per-block shadow rows in
an arena of their own (S32, S33) and a single process claim word (S38.1).

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
  `OWNERSHIP_MARK`, `ENROLLED`. The release path reads them together
  with the category and the kind's top bit as one `flags & 0x723`
  (`refcount::ENROLMENT_GATE_MASK`), so a constant landing on any of them
  would make the gate refuse candidates for a reason the design does not
  have. None has a writer yet: the queue that sets `ENROLLED` is S34.1's,
  and the two proofs are S37.2's and S37.3's;
- bit 11, `IS_ESCAPEE` — repurposes the refcount as the escapee
  hold-count (see invariant 5);
- bit 12, weak gate (`HAS_WEAK_REFERENCES`) — lent to `weak`; death
  sites test it before calling in;
- bits 13–14, destructor state (`DESTRUCTOR_PENDING` / `DESTRUCTOR_RAN`)
  — the debt protocol between `object` and the death paths;
- bit 15, **free**. It carried `STRING_OUT_OF_LINE` until the string's
  two layouts became the kind codes 8 and 9: a code says the same thing
  in a field every teardown and trace path already loads, and it says it
  without a second bit. `string::bytes_are_out_of_line` is the one
  reader of that distinction now;
- bits 16–31, **free and asserted free**. `rc-trace` kept a candidate
  index across 15–31 and `rc-walk` an epoch byte at 16–23, and both went
  on 2026-08-26. `refcount::tests::the_header_the_compiler_shares`
  asserts that no constant claims a bit above 15, so S31 — which lays
  epoch, maturation age and a collector reserve there — starts against a
  check that is already red for it.

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

A **non-zero** decrement is where a collector enrols a candidate, and
nothing does today: the enrolment is S31.3's gate and S34's queue, and
until they land a garbage ring is retained. The free path likewise parks
nothing; S34.3 and S36.2 are its two windows.

**4. Arena reset.** `ll_arena_reset` (`context`) →
`promote::arena_reset_full` drives the whole discipline, draining the
arena's logs through arena's own primitives: the fixpoint (survivors
marked from escapee hold-counts via `ARENA_RESET_MARK`;
pre-destructors of the dying may create new escapes and destructors,
hence the loop) → internal-edge counting → survivor categories
rewritten in place to GcHeap, their blocks stamped
`BLOCK_KIND_RETAINED` and kept out of the pool → the release-at-reset
log pays one release per record → the arena weak log drains (`weak`) →
the survivor list is grouped per block and registered as those blocks'
object indexes (`retained`), which is what makes their occupants
walkable at all → every other block, the reserve-drawn ones included,
returns to the pool.

**4a. Thread exit.** `ll_thread_exit` (`heap`), reached explicitly or
from the TLS guard → the static-block pass (`static_block`) releases
each registered block's roots in reverse registration order through the
barrier's `drop`, which is the only step here that runs user code →
`weak::dispose` returns the weak table, after every death that could
still need a row → `buffer_arena::dispose` returns the thread's buffer
arena, whose blocks go to the process-global pool, after every step above
that can still free a buffer into it → the thread's heaps are dropped and
their blocks are abandoned or returned.

**5. Cycle collection — none.** There is no fifth path. `rc-walk`,
`rc-trace` and `rc-satb` were deleted on 2026-08-26 and `rc-cycle` is
unbuilt, so a garbage ring is retained and acyclic garbage dies by
counting. The shape the path will take is in
`rfc/model/gc/rc-cycle.md`: a mutator-fed candidate set, trial deletion
on shadow counts held off the heap, maturation by age, and a teardown
whose order is binding ("Cycle teardown"). `PLAN.md` S31 through S40
build it, and this section is redrawn when the boundaries are real
rather than before — a path diagram of an unbuilt collector reads as
structure that exists.

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
4. **A dead entity slot keeps its final refcount-0 header** in bytes
   0–7 — the walker's occupancy test. That is why every intrusive link
   through dead memory (heap free list, parked-free list) lives at
   bytes 8–15, and why entity blocks are zeroed at commissioning.
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
   addresses in `retained`'s index, and an address is found by exact
   match in it. Both enumerators and the census branch on exactly this,
   and the census does so *after* one shared binary search, which is
   what keeps row omission and edge omission one decision at one test.
   A retained index is frozen because nothing allocates into a dead
   arena, and a stale entry is harmless because a dead survivor reads
   refcount 0 — invariant 4 again.
7b. **No `thread_local!` the exit path can reach may have drop glue.**
   `ll_thread_exit` runs from a TLS destructor and, since A6, runs user
   code; TLS destructor order is unspecified, and on glibc it is reverse
   registration order, which destroys the exit guard last precisely
   because it registers first. A key with drop glue is therefore
   reliably already gone, `with` panics with `AccessError`, and a panic
   in a destructor cannot unwind — the process aborts. Every such
   structure is a `Cell<*mut T>` freed by an explicit `dispose` in the
   order `ll_thread_exit` fixes. `block_pool`'s cache and `reserve` are
   the sanctioned exceptions: they use `try_with`, and there failure
   means "go to the global tier", which is sound.
8. **Publish before teardown**: the barrier owns the whole slot; an
   overwriting store is `store_*` then `drop_ref`, never the reverse.
9. **Death is owner-bound.** Every free and every weak-table touch runs
   on the entity's owning thread. `rc-cycle` keeps the rule and sharpens
   it: a collector proposes a shortlist, and every reduction of state —
   the free, the acquittal, the queue entry — is the owner's, taken on
   an exact reading at its own checkpoint (`rfc/model/gc/rc-cycle.md`).
10. **Arm vs fire**: nothing collects mid-mutation. Collection fires
    only at clean points — `ll_gc_collect_cycles`, the
    `ll_gc_maybe_collect` poll, and the allocation slow path. The rule
    is a correctness requirement rather than a policy: a store lowers
    the old value's count before overwriting the pointer, and a
    collection firing in that window would subtract one reference twice
    (`rfc/model/gc/strategies.md`, "Triggering: arm vs fire").
11. **One configuration.** The GC axis went with the two collectors on
    2026-08-26; `hash-folding` and `debug-journal` are what remains of
    the build matrix (`WORKFLOW.md`).
12. **Every death is eager, and a teardown drops on a corpse**
    (eager-death amendment, 2026-07-27, superseding the F5
    deferral/marker scheme): a release reaching zero always tears down
    at the natural point, with only the memory parked (out-of-band — a
    parked slot is never written until the flush). The corpse rule that
    goes with it opens the cycle teardown: a component holding a member
    already at `rc 0` is dropped whole, before any field is traced or
    any guard written (`rfc/model/gc/rc-cycle.md`, "Cycle teardown",
    step 1).
13. **An interned name *is* a valid immortal string entity** — the
    future string machinery reads it as-is; immortal + COW makes
    retain/release no-ops on it.
14. **Class descriptor addresses are process-stable** (immortal): the
    foundation for inline caches; dispatch tables stay pure
    code-pointer arrays with no embedded headers.
15. **One collection at a time in the process** — the `amSolo` rule.
    `rc-walk`'s non-nesting epochs were the same constraint under
    another name, and `rc-cycle` keeps it because the shadow rows are
    one collection's scratch and a second would read the first's
    decrements (`rfc/model/gc/rc-cycle.md`, "Concurrency"). The claim
    word that enforces it is S38.1's.
