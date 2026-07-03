# Memory Manager: Implementation Algorithm

How this crate implements the memory design from the
[rfc](https://github.com/limelight-lang/rfc) repository
(`model/memory/*`, `model/gc/heap-design.md`). The RFC holds the *why*;
this document holds the *how* — concrete algorithms, structures, and the
API that `src/memory/` implements.

---

## Layers

```
OS                  regions of 2 MB, aligned (VirtualAlloc / mmap)
                      │  carved into
Global block pool   32 KB blocks, aligned to their size
                      │  borrowed by
Consumers           request arenas · immortal region · GC heap (future) · large objects
```

One address space, one pool. Every consumer borrows blocks from the same
pool and returns them there. No per-thread heaps, no memory kingdoms.

---

## The Block

- **32 KB, aligned to 32 KB.** The owning block of any pointer is
  computable with one mask: `block = ptr & !(32768 - 1)`.
- Block header lives in the block's first 256-byte line: kind
  (arena / heap / large), list link for the pool, room for future
  metadata (line map, remembered-set hooks). Usable payload: 127 lines.

## Global Block Pool

Concurrency scheme borrowed from tcmalloc/mimalloc, nothing invented:

- **Per-thread cache**: a small array (~8 blocks). `get` = pop from
  array — no atomics at all. `put` = push; on overflow, flush half to
  the global stack.
- **Global free stack**: lock-free Treiber stack (`AtomicPtr` head,
  block headers are the links). Any thread may push or pop —
  cross-thread block transfer is a single CAS.
- **Refill**: cache and stack empty → reserve a new 2 MB region from
  the OS, carve it into 64 blocks, take one, stack the rest.

## Request Arena (bump allocation)

State: current block, `bump` pointer, `limit`, list of owned blocks,
destructor-tracking list.

```
alloc(size, align):                     # the hot path — inlined into
    p = align_up(bump, align)           # generated code as ~5 instructions
    if p + size <= limit:
        bump = p + size
        return p
    return alloc_slow(size)             # new block from pool — rare

reserve(n):                             # compiler batch hook: one limit
    ensure limit - bump >= n            # check for a whole loop of news

track_destructor(obj):                  # objects with side-effect
    destructors.push(obj)               # destructors (HAS_DESTRUCTOR bit)

reset():                                # end of request
    run pre-destructors (exactly-once, per object-lifecycle.md)
    return all blocks to the pool       # O(blocks), not O(objects)
    clear state
```

Objects larger than a block's payload never enter the arena path — see
Large objects.

Phase 2 (per `rfc/model/memory/arena-reset.md`, not built yet):
remembered set of escaped references, and reset() growing the
evacuate-or-retain decision. The block header reserves space for it.

## Immortal Region

A bump allocator that never frees and never resets. Class descriptors,
interned strings, itables. Written only during class loading; no
concurrency on the hot path (loading takes a lock, reading never does).

## Large Objects (> ~8 KB)

Dedicated block runs: contiguous blocks straight from the pool (or the
OS for very large), header marks the run length. Never mixed into
bump blocks — a huge string must not pin an arena block's worth of
small objects. Freed as a run.

## GC Heap

Future — arrives with the MMTK integration (Immix plan). Until then the
"general heap" category is served by a plain arena that never resets
(correct, leaky, temporary — enough for tests and the vertical slice).
Planned optimization once refcounts report deaths: per-class free lists
(slab-style) on top of line recycling.

---

## API Sketch (what `src/memory/` exposes)

```rust
// block_pool.rs
pub struct BlockPool;                  // process-global
impl BlockPool {
    pub fn get(&self) -> BlockRef;     // thread cache → global stack → OS
    pub fn put(&self, block: BlockRef);
}

// arena.rs
pub struct Arena { /* bump, limit, blocks, destructors */ }
impl Arena {
    pub fn alloc(&mut self, size: usize, align: usize) -> *mut u8;
    pub fn reserve(&mut self, bytes: usize);
    pub fn track_destructor(&mut self, obj: *mut RcHeader);
    pub fn reset(&mut self);
}

// ABI surface for generated code (inlined from bitcode):
// ll_arena_alloc, ll_arena_reserve
```

## Validated Against jemalloc / mimalloc / snmalloc

The scheme above was checked against the sources of the three
state-of-the-art allocators (2026-07). Confirmed: pointer→block by mask
(mimalloc does exactly this on segments; snmalloc pays shift+pagemap
load), 2 MB OS regions (snmalloc superpages, jemalloc hugepages),
per-thread cache over a global structure (all three), metadata embedded
at the region start (mimalloc; jemalloc externalizes it for
security/merging goals we don't have).

Adjustments adopted from the study:

1. **Block header cache-line layout.** 256 B = 4 cache lines. The first
   line holds read-mostly fields only (kind, run length, pool link);
   any future atomics (remembered set, line marks) get their own lines.
   All three allocators fight false sharing this way (snmalloc keeps
   queue front/back on separate lines; jemalloc separates read-only
   bin_info citing exactly this).
2. **Cache refill/flush constants.** Refill the thread cache in batches
   from the global stack; on overflow flush **half**, not all
   (jemalloc tcache pattern: fill ~2× slab, flush 1/2). Constants
   tunable, start: cache 8, refill 4.
3. **OS return policy** (was a hole): lazy, delay-based purge of fully
   free regions — industry numbers are mimalloc ~100 ms purge delay,
   jemalloc 10 s dirty decay. Start with ~1 s delay, full-region
   granularity, MEM_DECOMMIT / MADV_DONTNEED.
4. **Cross-thread frees (future, multi-threaded phase): MPSC queue per
   owning thread** with per-destination batching at the sender —
   snmalloc's scheme ("thousands of remote deallocations per atomic"),
   not a single shared stack. The phase-1 single global block stack is
   fine — pool traffic is one op per ~500 objects, three orders below
   malloc free-list traffic — but the sharding path is this one.

Note: mimalloc tried bump allocation inside its pages and measured no
win — because its pages must keep the free-list path anyway, so bump
added a test to the hot path. Our arena has no per-object free path at
all; the comparison does not transfer.

## Invariants

1. Every block is 32 KB and 32 KB-aligned; `ptr & !0x7FFF` always lands
   on a block header.
2. Freed memory returns to the global pool immediately — any consumer
   may reuse it.
3. The arena hot path performs no atomics, no locks, no function calls.
4. Nothing in the arena is freed per-object; reclamation is reset-only
   (phase 1).

## Build Order

1. `block_pool` — regions, carving, thread cache, global stack.
2. `arena` — bump, reserve, destructor list, reset.
3. `immortal` — trivial specialization of arena.
4. Large-object runs.
5. (later) remembered set + arena-reset modes; MMTK heap.
