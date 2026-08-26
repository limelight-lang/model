# Memory Manager: Implementation Algorithm

How this crate implements the memory design from the
[rfc](https://github.com/limelight-lang/rfc) repository
(`model/memory/*`, `model/gc/heap-design.md`). The RFC holds the *why*;
this document holds the *how* — the concrete algorithms and structures
in `src/memory/`.

**This describes the code as it is.** Where something is designed but
not built, it says so. Superseded versions live in
[`docs/history/`](history/) and are marked as such; the rule that keeps
this file honest is in [`dev/WORKFLOW.md`](../dev/WORKFLOW.md).

> **The crate has no cycle collector as of 2026-08-26.** `rc-walk`,
> `rc-trace` and `rc-satb` were deleted whole — code and documents — and
> `rc-cycle` (`rfc/model/gc/rc-cycle.md`) is the only design in force and is
> not built. Between stages S30 and S36 of `PLAN.md` a garbage ring is
> retained; acyclic garbage dies by counting as it always did. Passages below
> that describe a collector are dated and marked, and the code they described
> is on the branch `archive/pre-rc-cycle`.

---

## Layers

```
OS                 2 MB regions, block-aligned (via Rust's global
                   allocator today; VirtualAlloc / mmap later)
                   never returned in phase 1
  |
BlockPool          64 KB blocks, carved from regions
                   per-thread cache in front, batched refill/flush
  |
  +-- Heap         small objects, <= 8 KB: one block per size class,
  |                carved into fixed slots
  +-- Arena        per-request bump allocator
  +-- BufferArena  growable byte buffers
  +-- Immortal     bump, never freed: class metadata, interned strings
  +-- Large        8 KB..payload: one whole block
                   above payload: OS-direct block-aligned run
```

Every allocation lives inside a 64 KB block aligned to its own size.
That single fact carries the design: **the owning block of any pointer
is `ptr & !BLOCK_MASK`** — one AND, no radix tree, no page map, no
per-allocation size passed back in by the caller. Size-less `free(ptr)`
works because the block header says what kind of memory this is.

## The block header

The first 256-byte line (`LINE_SIZE`) of every block is header; the
payload starts at a fixed offset regardless of what the header holds, so
header layout never costs payload.

It is a **tagged union**. `kind` at offset 0 discriminates, and each
consumer overlays its own view:

| `kind` | Owner | View |
|---|---|---|
| `FREE` | pool | free-chain link |
| `HEAP` | `Heap` | `HeapBlockHeader` |
| `ARENA` / `RETAINED` | `Arena` | block-list link |
| `BUFFER` | `BufferArena` | `BufferBlockHeader` |
| `IMMORTAL`, `LARGE`, `LARGE_RUN` | — | kind only |

The union is why `kind` must stay at offset 0 in every view, and it is
the source of one class of hazard worth knowing: bytes that mean one
thing to one owner mean something else to another. A data race in
`BlockPool::pop_global` came from exactly that overlap — see the type
comment on `FreeList` in `block_pool.rs`.

### `HeapBlockHeader`, and why it is four structs

Field order here is a contract, pinned by
`block_header_halves_are_laid_out_as_the_design_requires`:

```
line 0   kind, size_class       the two words the collector reads
         BlockPrivate           used, slots, free, bump, linked, next, prev
         BlockShared            owner
line 1   BlockRemote            remote_free, alone
line 2   BlockLinks             owned_next, owned_prev
```

Two rules produced that layout, and both were measured:

- **Only `remote_free` is isolated.** It is the field other threads
  hammer with CAS, so sharing a line with the owner's hot fields cost
  the owner a coherence miss per cross-thread free.
- **Everything the fast paths touch stays in line 0**, including
  `owner`, which every local `free` reads to test ownership, and
  `next`/`prev`, touched on every full ↔ has-room transition. Both were
  tried on their own line and both measured slower.

The split into private and shared halves is not only about cache lines.
`&mut HeapBlockHeader` was a false claim of exclusivity, since it
covered atomics other threads read by design. The owner now borrows
`&mut (*block).private` and cannot name the shared half at all.

`kind` and `size_class` sit outside that half for the same reason, and
learning it cost a second round: the collector reads both for every
block of every region, and a `&mut` retag asserts uniqueness over its
whole range — including an `UnsafeCell`, which is where the atomic type
alone was not enough (2026-08-10, S11.9 in `PLAN.md`). Both are
`AtomicU32`; `block_pool::store_block_kind` is the only path that writes
a kind, and because a whole-header struct store would cover the word
plainly, every commissioning writes its header field by field.

## BlockPool

A chain of free blocks threaded through the header, behind a `Mutex`,
with a per-thread cache in front.

- `get`: thread cache → global chain (refill a batch) → carve a new
  region.
- `put`: thread cache; on overflow, flush half to the global chain.

The lock is affordable because the global chain is cold by construction:
batching means it is reached only on a cache miss or an overflow flush.
It replaced a lock-free Treiber stack whose `pop` raced the union
overlap described above; a correct lock-free version remains possible
and is described in `block_pool.rs`.

Regions are never unmapped in phase 1. `blocks_out` and
`regions_carved` are the crate's own occupancy accounting, and they are
the oracle for leak-shaped defects — Miri cannot see those, since a
stranded block is still allocated memory.

The pool also keeps the **region registry**: the base address of every
region ever carved, append-only, indices stable for the life of the
process. It exists for a whole-heap trace — counting regions without
recording their bases left nothing able to enumerate blocks. OS-direct
`LARGE_RUN` allocations are not regions and stay outside it: huge
objects are never traced, conservatively.

## Heap: small objects

The mimalloc model. 32 size classes from 16 B to 8 KB, chosen to keep
internal fragmentation under ~25%. The class index comes from a
compile-time lookup table: one array read, no branches.

Per class the heap keeps an `available` list of blocks with room, plus
at most one empty block held in reserve for instant reuse.

**Allocation** pops the head block's free list; if empty, it carves a
virgin slot at `bump`. Both are O(1) and branch-only.

The free list is **intrusive**: a free slot's `next` lives in the slot's
own bytes 8–15. It therefore costs no side allocation and no metadata
line — the link rides a cache line the caller has just stopped touching.
A bitmap was tried instead (one bit per slot, `tzcnt` to find a free
one) and **lost**, by 18-20% on a real `larson.cpp` through the C ABI.
It needs a side allocation per block, so every alloc pays a second
dependent load into a line nothing else touches; answering "is this
block full?" costs a scan of every word; and `free` needs
`(ptr - base) / class_size`, a real integer division, `class_size` not
being a power of two for most classes. The free list has none of those
(`rfc/model/memory/heap-slot-allocation.md`, "Fix 5", which also says why
the benchmark that first chose the bitmap was not measuring what it
claimed). The link lived in bytes 0–7 until the entity heap needed those
bytes: they must survive a free untouched, because in an entity block
they keep the dead entity's final refcount-0 header, which is how a trace
tells a free slot from a live entity. One offset for both populations keeps a
single code path, and the cache-line argument is unchanged (every class
is ≥ 16 bytes).

**Local free** finds the block by mask, confirms ownership, pushes the
slot onto the block's free list and decrements `used`.

Rare tails — refill, walking past a full block, a block emptying, a full
block regaining room — live in separate functions. For `alloc` they are
`#[cold] #[inline(never)]`, which is what keeps the fast path a frameless
leaf that inlines into `ll_alloc`. `free` is split the same way, with
one deliberate difference: `relink_unfull` is out of line but **not**
`#[cold]`, because a workload churning blocks across the full ↔
has-room boundary takes that branch constantly. The
split was measured and changed nothing outside the noise floor, which is
the expected result for a path whose tails run a few times in ten
thousand calls (`dev/BENCHMARKS.md`, H11); it is kept for the shape, not
for a number.

### Entity blocks: the second population

The heap runs **twice per thread** over the same code: a raw heap
(`BLOCK_KIND_HEAP`) for C-ABI buffers and an entity heap
(`BLOCK_KIND_ENTITY`) for GC entities, held as one `repr(C)` pair behind
the TLS slot — the raw heap first, so `ll_malloc`'s path still pays a
single TEB read and no offset. Segregation is the prerequisite of any
tracing collector: a trace reads every occupied slot's first 8 bytes as
an `RcHeader`, and a raw 40-byte buffer sharing a block with 40-byte
objects would make that a wild read.

Three rules distinguish the entity population:

- **Commissioning zeroes slot headers**: refill writes 8 zero bytes per
  slot before the block serves, whatever the block held before. With the
  factory publishing an object's header *last* (one 8-byte store), a
  slot therefore reads `refcount 0` from commissioning until the instant
  it is a fully formed entity — the walker's three-way classification
  never meets bytes that lie.
- **A free leaves bytes 0–7 untouched** (the link is at 8–15): the dead
  entity's final `refcount 0` header *is* the vacancy stamp. There is no
  teardown stamp to forget.
- **Abandoned lists are per population**: adoption never moves a block
  across populations, so a raw heap can never hand out entity-block
  slots.

`for_each_entity_slot` (the census primitive under `walk.rs`) enumerates
the registry's regions, skips every non-entity block, and visits
occupied slots bounded by each block's bump cursor.

**An entity past the largest size class is not packed at all**, and it
is inside the walk rather than outside it. It keeps its inline layout
whole as the sole occupant of a block-aligned allocation whose first
line is a block header of its own kind — `BLOCK_KIND_ENTITY_LARGE` for a
pooled block up to one payload, `BLOCK_KIND_ENTITY_LARGE_RUN` for an
OS-direct run above it (`memory/large_entity.rs`,
`rfc/model/memory/large-entities.md`). The kinds are new rather than
borrowed from the raw large path, which holds C buffers a walker must
never read as entities. Both enumerators visit exactly one slot in such
a block, and that count is soundness rather than economy: dividing the
payload by a class size there fabricates rows out of the object's own
cells. The pooled half rides the region scan; a run lies outside every
region and is found from the module's own registry, which is why its
free parks during an epoch like everything else that can put memory back
in circulation.

### Deferred free — deleted 2026-08-26 with `rc-walk`

*What follows described `deferred_free.rs`, which is gone.* `rc-cycle` parks
per slot on two windows of different widths — a queue entry naming the slot,
and a collection in flight — rather than epoch-wide, and stages S34.3 and
S36.2 build it. The lesson that survives is the last paragraph of this
section: parking is out of band, and the parked memory is not written.

While an rc-walk collection epoch was in flight, a free **parked** instead
of recycling (`deferred_free.rs`): one relaxed load of a global activity
bit and a predicted branch, after the kind dispatch, active only during
an epoch. The entity still dies on time, destructor included, and only
reuse waits, so a walked slot cannot become a different object
mid-epoch. Identity is what makes an exact test sound
(`rfc`'s `archive/pre-rc-cycle`, `model/gc/rc-walk.md`).

**Parking is out of band.** The parked pointer goes into a thread-local
vector, and the parked memory is not written until the flush. The first
draft threaded an intrusive link through the allocation's bytes 8-15,
which in an entity slot is the class word the walker dereferences one
pass after reading the header: a wild read under the walker's feet. Out
of band a corpse stays intact — header reading refcount 0, class word
live, fields nulled — so a walker chasing a stale pointer lands on
readable bytes. The price is a park path that may allocate, cold and
epoch-only.

**What rides.** Every block kind that reaches `ll_free` and can put
memory back in circulation: heap raw buffers, entity slots, pooled
large, OS-direct runs and retained blocks. A reset in flight takes two
of those before this test is reached: a large-entity body parks in the
reset's own window instead, and the free of a corpse in a block that has
no occupant index yet is dropped entirely (below, "Arena reset"). Buffer-arena chunks never
reach `ll_free` at all, `buffer_free_longlived_payload` calling
`BufferArena::free` directly, so that branch makes the test itself and
parks the whole call: `free` is size-carrying and can hand an emptied
block back to the pool to be re-stamped as another kind. A payload in a
retained block arrives from the same function and hands back no memory
of its own; what it may hand back is the block those bytes pinned. So a
parked record names the free it replays rather than deriving it from a
size.

**What does not.** The arena kind, which recycles nothing, so identity
holds without parking. A retained block rides for a reason of its own:
nothing is recycled inside it, former arena memory having neither stride
nor free list, but the death of its last live occupant hands the whole
block to the pool, and a block reissued mid-epoch is the identity loss
parking exists to prevent.

**Cross-thread frees ride like any other.** The epoch test fires on the
block kind alone and stands before the owner dispatch, so during an
epoch a free of another thread's heap or entity slot parks on the
freeing thread and reaches `free_foreign` only when the flush replays
it. The crate is single-mutator today, so nothing depends on that
ordering yet; actors reopen the question.

**Known limit.** A thread that parks and exits before flushing leaks its
parked list until process end, bounded by what that thread freed inside
one epoch window and measured in blocks rather than bytes: a dropped
chunk record leaves `live` above zero on its block forever, and a block
that never empties bounces between the abandoned list and its adopters
instead of going home, so one record can pin 64 KiB. A large-entity run
raises that ceiling to the run's own size
(`rfc/model/memory/large-entities.md`) and keeps its registry entry, so
the collector walks it once per epoch for the life of the process.

### Cross-thread free

Each block owns a lock-free MPSC stack, `remote_free`. A `free` whose
block belongs to another heap does one atomic push onto **that block's**
stack and touches nothing else.

Per block, not per heap, and that is load-bearing: it is what makes
adoption race-free. A freeing thread reads `owner`, sees it is not
itself, and pushes. If an adoption is racing that read it does not
matter which owner was seen — the message lands in the block, and the
block's *current* owner drains it. In a per-heap stack, a message posted
to a dying owner after adoption would be stranded forever.

There is no ABA hazard here, because there is no pop: producers only
push, writing the head value into their own node, and the owner takes
the whole chain with a single `swap`.

`used` is **owner-only** and is deliberately not touched by the freeing
thread; the owner accounts for parked slots when it collects. That is
what makes `used == 0` safe to act on — a parked slot still counts as
live, so a block with one can never look empty.

The owner collects in two cold places: when it has just run a block out
of slots, and when sweeping its blocks before asking the pool for more.
The sweep is not optional — a block unlinked as full is never revisited
otherwise, and its parked frees would sit forever, the thread refilling
instead: 34.2M to 2.3M ops/s on the bleeding pattern when it was
missing. mimalloc's full queue exists for the same reason.

### Thread exit: abandonment and adoption

A dying thread hands its blocks over: empty ones to the pool, ones still
holding live objects onto a global per-class abandoned list. The next
thread short of that class adopts one, claims `owner` with a plain
`Release` store, and drains its parked frees.

This is not an optimisation. Without it, every block a thread still
owned when it died was stranded permanently, along with every later
cross-thread free into it: 1.7 GiB resident against a 2.5 MiB live set
on `larson.cpp`, which respawns its worker every ~20 ms by design.

It happens automatically on **every** target, via a TLS guard installed
by `ll_thread_init`, and `Heap`'s own `Drop`, so a heap dying by any
route reclaims identically.

Known limit: an abandoned block is reclaimed only when someone adopts
it, so a permanently idle size class keeps its blocks. Bounded by what
was live at thread exit; no periodic trim exists yet.

## Arena: per-request memory

A bump allocator whose blocks are chained through their headers. Its
four logs — escapees, tracked destructors, deferred releases, large
payloads — live in the arena's **own** bump memory, so there is no side
`Vec` and everything dies with the arena at once.

**Two doors, split by what the caller is holding.** `Arena::alloc` is
the C ABI's, reached from `ll_arena_alloc`, where an entity and a byte
buffer are the same request; it refuses anything past one block payload,
because a slot that large has no home in a bump-packed block.
`Arena::alloc_entity` is routing's, and past the same bound it takes a
block-aligned allocation of its own and logs it with the large payloads,
so an unpromoted corpse is freed by the reset with every other run. A
survivor in such a block is the reset's one exception to block
retention: it is handed over rather than retained — no `BLOCK_KIND_RETAINED`
stamp, no entry in the retained index, and out of the arena's log through
`forget_large`. Stamping it retained would send a multi-megabyte OS
allocation to the 64 KiB block pool at the entity's death.

When that bump memory is exhausted a log segment is carved from the
thread's reserve instead (see "The log reserve" below), through a
separate cursor: a reserve block is linked into the arena's block list
but never becomes its bump, or ordinary allocation would spend it.

The arena handle crosses the runtime as `*mut Arena`, never `&mut`,
because destructors reenter and resolve the same arena. The rule the
code follows: **no borrow may be live across a call that can run user
code.**

## The object model

Every entity begins with `RcHeader` — 8 bytes, at offset 0:

```
refcount  u32
flags     u32    2 bits of which are the MemoryCategory
```

```
MemoryCategory:  GcHeap | RequestArena | LongLived | Immortal
```

An `Object` is that header, a class pointer, then 16-byte `Value`
property slots. `Class` descriptors are immortal and carry their vtable
inline, after the fixed fields.

Non-object entities carry no class pointer; the header's **kind field**
(bits 2–5) is what makes a bare pointer self-describing at teardown —
`ll_entity_die` switches on it (`rfc/model/classes.md`, "Entity kind and
non-object teardown"). The first produced non-object kind is the
**reference box** (`&`, kind 3): `RcHeader | Value`, 24 bytes
(`src/reference.rs`) — dying, it releases its one Value and frees.
The second is the **weak cell** (kind 11): the canonical `WeakReference`
entity *is* the cell, 16 bytes, always in the GC heap (`src/weak.rs`,
`rfc/model/weak-references.md`). Strings, arrays, `Box` and lazy objects
arrive with their own subsystems.

**Creation is two steps, and only the second owes a destructor.**
`ll_object_new` is the factory: it allocates — `GcHeap`/`LongLived` from
the entity population, arena and immortal from theirs — zero-fills the
body, writes the class, and publishes the header **last**, as one 8-byte
store (until that store the slot reads refcount 0, so a trace crossing
the block classifies it as free rather than reading a half-built
entity). That is all it does. `ll_object_constructed` is called once the user constructor
has returned successfully — it sets the header's `DESTRUCTOR_PENDING` flag
and, for an arena object, writes the destructor-log record. Teardown
dispatches on that flag, never on the class, so an object whose
constructor threw runs our own teardown and not its `__destruct`
(`rfc/runtime/object-lifecycle.md`). Registration can be refused when
memory is short: it reports false, and the creation fails with
memory-exhausted — the same observable outcome as a throwing
constructor.

Trailing inline data — property slots, vtables, string bytes — is always
reached through a raw pointer spanning the whole allocation, never
through a reference to the fixed header. A reference carries provenance
over what it points at, and the trailing bytes are not that.

## Lifetime

Reference counting for `GcHeap`. Arena objects are **not
lifetime-counted** — they die with the arena — with one exception: COW
values are counted in every category (`rfc/model/values.md`), and an
arena object's `refcount` field is reused as its escape hold-count while
`IS_ESCAPEE` is set. Immortals are never touched.

Cycles are collected by Bacon–Rajan trial deletion: a non-zero decrement
buffers the object as a candidate root, and crossing the threshold
**arms** a collection — it never runs one inline. Buffering happens
inside `ll_release`, mid-mutation, where refcounts and edges disagree;
the collection fires at a clean point, an explicit
`ll_gc_collect_cycles` or the compiler's `ll_gc_maybe_collect` poll.

*Both collectors this section describes were deleted on 2026-08-26.* What
remains of them in the crate is the entry points — `ll_gc_collect_cycles` and
`ll_gc_maybe_collect` in `gc.rs`, which report zero — and the kind-dispatched
tracer, which moved to `cells.rs` because the class descriptor, the arena
reset and the dispose path all stand on it. `rc-cycle` traces through it
rather than growing a stride of its own.

### The store barrier

A reference store the compiler could not resolve statically is not one
call but a few **micro-operations** the compiler composes per site
(`rfc/model/gc/strategies.md` §1): `store_ptr` / `store_box` to publish,
`drop` to release the displaced value. The runtime provides the pieces;
which of them, in what order, and with which checks elided is the
compiler's. `owner_cat` — the destination's memory category — is passed
as a compile-time constant, not read from an owner header, so a headerless
static block can be a store target too. `ref_store` remains as the
convenience composition (`store_box` + `drop`) for a Box-slot overwrite.

- **Publish (`store_ptr` / `store_box`).** Retain the new value and run
  the category barrier, then write the slot — 8 bytes for a bare pointer
  slot, the whole 16-byte `Value` for a Box slot. An initializing store is
  a publish alone: no old value, no drop.
- **Drop.** Release the displaced entity, with full teardown if that was
  its last reference. One exception: a heap value displaced from an
  *arena* container is not released here at all — its release-at-reset
  record owns that release, and doing both was the double-release the
  design exists to prevent. `drop` takes the displaced entity, not the
  slot, so one `drop` serves both slot kinds.
- **The category barrier** (inside publish). An arena reference stored
  into a longer-lived container is an *escape*: the barrier bumps a
  hold-count kept in the escapee's own `refcount`. A heap reference stored
  into an arena container would otherwise leak, so the barrier logs one
  release-at-reset record for it.

The escape count lives in the escapee, not in a remembered set of holder
slots. That is deliberate and was a correctness fix: a holder can die
before the arena resets, and reading its slot back at reset dereferenced
freed memory.

A dying holder owes its arena escapees a `lose` however it dies — the
store barrier does it on overwrite, teardown does it in phase 2, and the
cycle collector does it before freeing a white object. The trace itself
never sees arena entities (only the heap is traced), so nothing else
would.

Two rules about the slot itself, both paid for by defects. **`store_box`
writes the whole 16-byte `Value`, not just its payload word** (the
`store_ptr` form writes the 8-byte pointer) — one slot has one writer, and
a caller stamping the tag afterwards leaves the slot torn in between. And
**the slot is published before the displaced value is released** — the
`store_*` precedes the `drop` — because releasing can run `__destruct`,
which is user code that may collect: a collection that still sees the old
edge subtracts a reference the count has already given up, and frees a
value whose teardown is on the stack.

### The log reserve

Recording an escape or a release-at-reset can need memory, and the
barrier has no way to report that it did not get any: `ll_ref_store`
returns a `bool`, and that `bool` carries one refusal only, the escape
copy a COW value takes when it leaves the arena. So two blocks per
thread are held back (`memory::reserve`) and filled at
`ll_thread_init`, where a refusal is still reportable. `Arena::grow_log`
draws on them once the pool refuses.

That does not make failure impossible — it moves it. Drawing sets a flag
that `ll_gc_maybe_collect`, the compiler's safepoint poll, refills on;
the poll runs where a frame can raise, so the barrier's unreportable
failure becomes an ordinary memory-exhausted exception thousands of
records earlier. Behind the reserve the three logs that cannot lose a
record still abort — escapees, release-at-reset, large runs — while a
refused *destructor* record instead fails the object's creation
(`Arena::track_destructor` returns false).

Design and the compiler-side contract it rests on:
`rfc/runtime/exceptions.md`, "The log reserve protocol".

## Arena reset: the settling loop

Not "move the bump pointer back". Resetting an arena runs a fixpoint,
because destructors are user code and can allocate, escape, and create
more destructors.

Each pass:

1. **Settle.** Drain the escapee list; for each escapee whose hold-count
   is still non-zero, mark its surviving subgraph. Run the
   pre-destructors of dying, unescaped objects. If a destructor
   allocated — detected by the arena's bump cursor moving — re-read the
   survivors' children, since it may have stored a fresh arena object
   into an already-traced survivor, which is an arena→arena store the
   barrier does not escape.
2. **Count and retain.** For the new survivors, add internal
   arena→arena edges, and one compensating retain per heap entity a
   survivor holds — that entity's release-at-reset record assumed the
   holder would die, and it no longer does.
3. **Promote.** Rewrite each survivor's category to `GcHeap` in place,
   and stamp its block `RETAINED` so it stays out of the pool.
4. **Release.** Drain the deferred-release log, collecting the round
   first and releasing after, because teardown here runs user code.

The drain of step 4 can kill a survivor of step 3, so the reset holds a
**window** over its own frees for as long as it runs
(`memory/reset_window.rs`). Three things ride on it. A large-entity
body parks until the window closes, because its free returns memory to
the system and the passes after the fixpoint still read one header word
of every address they hold; an inner window hands what it parked to the
window outside it, so only the outermost close frees anything. The free
of a corpse in a block that has no occupant index yet is absorbed, since
the index built at the end of the reset declines to count an occupant
whose header reads zero and there is no count for that death to spend.
And every completed teardown is recorded, which is how the passes after
the fixpoint tell a corpse from a live survivor — and how the COW
reconciliation of step 2 gets the two correction terms that replace a
dead holder's edges (`dev/DECISIONS.md`, "the reset reads no corpse").

Repeat until a pass releases nothing. A recursion bound is the only
backstop; hitting it is an error rather than a silent drop, since
dropping the unsettled tail would dangle.

**No holder slot is ever dereferenced** anywhere in this. Survival is
decided from counts carried in the objects themselves.

**Thread exit is now a sequence, not a single act.** Before A6 it only
gave blocks back; since 2026-08-03 it first releases what the thread's
*static blocks* held (`static_block.rs`), which is the only step that
runs user code and therefore goes first, while every structure a
`__destruct` may touch is still alive. Then the weak table, then the
buffer arena, and only then the heaps — two steps shorter since
2026-08-26, the candidate buffer and the parked-free backlog having gone
with the collectors that owned them. The order is explicit because it cannot be delegated:
`ll_thread_exit` runs from a TLS destructor, TLS destructor order is
unspecified, and on glibc it is reverse registration order — so a
structure first touched during the request registers after the guard and
is destroyed before it runs. (Against the two slots `ll_thread_init`
itself touches, the barrier reserve and the pool's thread cache, the
guard registers *later* and therefore runs *first*; `heap.rs`'s
`ll_thread_exit` states that half, which is the one easy to get
backwards.) Every per-thread structure on this path is therefore a
pointer cell with no drop glue, freed by hand (`dev/DECISIONS.md`,
"thread exit owns the order its per-thread state dies in").

**The survivor list outlives the reset.** It is grouped per block and
registered as each retained block's object index (`memory/retained.rs`,
2026-08-03). That inventory is the only way those occupants can be
enumerated: an arena's bump allocator left them mixed-size with no
stride, so the walk cannot divide an offset by a size class the way it
does in an entity block. Without it a retained block's occupants are
root sources and a ring living entirely among promoted survivors is
never collected. The index is frozen — nothing allocates into a dead
arena — and a survivor that later dies leaves refcount 0 behind, which
is the walk's own occupancy test, so a stale entry is skipped like a
free slot.

Not built yet, per RFC phasing: sparse-block evacuation, gated on the
escapee-reference fixup. Promotion today is retention only, which the
RFC calls the whole of the first implementation. The Immix-shaped
`GcHeap` allocator and the line recycling of retained blocks were
listed here until 2026-07-25 and are now **dropped**, not deferred:
segregated entity blocks solved what Immix was drafted for, and a
retained block stays out of circulation while its survivors live
(`rfc/model/memory/arena-reset.md`, Retention). An emptied retained
block goes home since 2026-08-08: the last occupant's death reports
through `ll_free`'s retained arm, and a block held for a payload the
reset could not carry out waits for that payload's own free
(`memory/retained.rs`). A payload freed **inside** the reset that pinned
its block is the exception, and the reset holds a count of its own
against exactly that: until it has finished establishing occupant
counts, no death can empty such a block, and one that is empty when the
count goes is handed over by the reset itself.

## What is not here

- **Strategy selection as an interface.** The RFC's four-interface
  contract is not represented in code, and deliberately so:
  `refcount.rs` and `object.rs` call `gc::*` directly. One composition is
  built as of 2026-08-26, and there is no GC feature axis left: the two
  that claimed overlapping header bits were deleted, and with a single
  strategy the build-time choice has no subject. A `GcStrategy` trait with
  trivial impls
  used to stand in for the contract and was removed — nothing
  constructed it, and being dispatch-shaped it could not deliver the
  build-time choice the contract asks for. When a `nogc` or pure-`rc`
  build is wanted, a cargo feature around those call sites compiles the
  buffering away, which is what "selected at build time" actually means.
- **Telemetry beyond block granularity.** Aggregate stats are always on
  and cost nothing per object; the object registry, lifetimes and
  per-allocation metadata are designed in
  [`dev/design/debug-modes.md`](../dev/design/debug-modes.md) and not
  implemented.
- **Actors.** Referred to in comments; no representation in this crate.
