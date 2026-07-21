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

---

## Layers

```
OS                 2 MB regions, block-aligned (VirtualAlloc / mmap)
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
line 0   BlockPrivate  kind, size_class, used, slots, free, bump,
                       linked, next, prev
         BlockShared   owner
line 1   BlockRemote   remote_free, alone
line 2   BlockLinks    owned_next, owned_prev
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

## Heap: small objects

The mimalloc model. 32 size classes from 16 B to 8 KB, chosen to keep
internal fragmentation under ~25%. The class index comes from a
compile-time lookup table: one array read, no branches.

Per class the heap keeps an `available` list of blocks with room, plus
at most one empty block held in reserve for instant reuse.

**Allocation** pops the head block's free list; if empty, it carves a
virgin slot at `bump`. Both are O(1) and branch-only.

The free list is **intrusive**: a free slot's `next` lives in the slot's
own first 8 bytes. It therefore costs no side allocation and no metadata
line — the link rides memory the caller is about to write anyway. A
bitmap was tried instead and lost; the module doc in `heap.rs` records
why, including the measurement.

**Local free** finds the block by mask, confirms ownership, pushes the
slot onto the block's free list and decrements `used`.

Rare tails — refill, walking past a full block, a block emptying, a full
block regaining room — live in separate functions. For `alloc` they are
`#[cold] #[inline(never)]`, which is what keeps the fast path a frameless
leaf that inlines into `ll_alloc`. `free` is split the same way. The
split was measured and changed nothing outside the noise floor, which is
the expected result for a path whose tails run a few times in ten
thousand calls (`dev/BENCHMARKS.md`, H11); it is kept for the shape, not
for a number.

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
otherwise, and its parked frees would sit forever.

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
logs — escapees, tracked destructors, deferred releases — live in the
arena's **own** bump memory, so there is no side `Vec` and everything
dies with the arena at once.

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

**Creation is two steps, and only the second owes a destructor.**
`ll_object_new` is the factory: it allocates and stamps the header, and
that is all. `ll_object_constructed` is called once the user constructor
has returned successfully — it sets the header's `HAS_DESTRUCTOR` flag
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

Reference counting for `GcHeap`. Arena objects are **not counted at
all** — they die with the arena. Immortals are never touched.

Cycles are collected by Bacon–Rajan trial deletion: a non-zero decrement
buffers the object as a candidate root, and crossing the threshold
**arms** a collection — it never runs one inline. Buffering happens
inside `ll_release`, mid-mutation, where refcounts and edges disagree;
the collection fires at a clean point, an explicit
`ll_gc_collect_cycles` or the compiler's `ll_gc_maybe_collect` poll.

### The store barrier

Every reference store the compiler could not resolve statically goes
through one door, `ref_store`. It performs both halves at once:

- **Counting.** Retain the new value; release the displaced one, with
  full teardown if that was its last reference.
- **The category barrier.** An arena reference stored into a
  longer-lived container is an *escape*: the barrier bumps a hold-count
  kept in the escapee's own `refcount`. A heap reference stored into an
  arena container would otherwise leak, so the barrier logs one
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

Two rules about the slot itself, both paid for by defects. **The barrier
writes the whole 16-byte `Value`, not just its payload word** — one slot
has one writer, and a caller stamping the tag afterwards leaves the slot
torn in between. And **the slot is published before the displaced value
is released**, because releasing it can run `__destruct`, which is user
code that may collect: a collection that still sees the old edge
subtracts a reference the count has already given up, and frees a value
whose teardown is on the stack.

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

Repeat until a pass releases nothing. A recursion bound is the only
backstop; hitting it is an error rather than a silent drop, since
dropping the unsettled tail would dangle.

**No holder slot is ever dereferenced** anywhere in this. Survival is
decided from counts carried in the objects themselves.

Not built yet, per RFC phasing: sparse-block evacuation, line recycling
of retained blocks, and the Immix-shaped `GcHeap` allocator. Promotion
today is retention only, which the RFC calls the whole of the first
implementation.

## What is not here

- **Strategy selection.** The RFC's four-interface contract is not
  represented in code, and deliberately so: `refcount.rs` and
  `object.rs` call `gc::*` directly, and the one composition built is
  `rc-trace`. A `GcStrategy` trait with trivial impls used to stand in
  for it and was removed — nothing constructed it, and being
  dispatch-shaped it could not deliver the build-time choice the
  contract asks for. When a `nogc` or pure-`rc` build is wanted, a
  cargo feature around those call sites compiles the buffering away,
  which is what "selected at build time" actually means.
- **Telemetry beyond block granularity.** Aggregate stats are always on
  and cost nothing per object; the object registry, lifetimes and
  per-allocation metadata are designed in
  [`dev/design/debug-modes.md`](../dev/design/debug-modes.md) and not
  implemented.
- **Actors.** Referred to in comments; no representation in this crate.
