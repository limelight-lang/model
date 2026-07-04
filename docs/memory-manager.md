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

## ABI Surface (what generated code calls)

Every function takes an **allocation context** as its first parameter.
In generated code `ctx` lives in a dedicated register (pinned by the
calling convention, Go-style) and is passed through for free.
**`ctx == NULL` is legal**: the runtime falls back to the thread-local
current context — this covers calls from the C++ layer, host code, FFI.

```c
/* hot — inlined into PHP code from bitcode */
void*  ll_arena_alloc(LLContext* ctx, size_t size);
void   ll_arena_reserve(LLContext* ctx, size_t bytes);   /* compiler batch hook */
void*  ll_heap_alloc(LLContext* ctx, size_t size);
void*  ll_immortal_alloc(LLContext* ctx, size_t size);

/* mutable buffers — low-level primitive, caller owns the 3-word slot */
void   ll_buffer_init(LLContext* ctx, Buffer* buf, size_t capacity);
void*  ll_buffer_ensure(LLContext* ctx, Buffer* buf, size_t min_capacity);

/* warm — real calls */
void   ll_arena_track_destructor(LLContext* ctx, RcHeader* obj);
void*  ll_large_alloc(LLContext* ctx, size_t size);

/* cold — called by the host/server loop, not by PHP code */
void   ll_arena_reset(LLContext* ctx);
```

Design points baked into this surface:

- **Category is the choice of function, not a parameter** — the
  compiler already decided arena/heap/immortal; no runtime branching.
- **Arena internals are NOT ABI** — speed comes from bitcode inlining
  (`opt -O2` inlines the bodies), so field layout stays private.
- **`size` is usually a literal** — for `new Foo()` the compiler emits
  `ll_arena_alloc(ctx, 40)`; after inlining, constants fold.

### Analytics build

Under a build flag (`--features alloc-trace`) every allocation function
gains one trailing parameter — a pointer to a static, compiler-generated
per-call-site record:

```c
typedef struct LLAllocSite {
    const char* module;      /* all strings interned */
    const char* class_name;
    const char* function;
    uint32_t    line;
} LLAllocSite;
```

Different builds = different ABI, which is legal: runtime bitcode and
the code generator are always built and versioned in lockstep (the
single-LLVM-version rule extends to a single-ABI-version rule).

## Mutable Buffers

A **low-level growable-memory primitive** — not a heap entity. A buffer
has **no `RcHeader`**, no class, no lifecycle: it is exactly three words,
`{ data, len, capacity }`. Whatever needs to be a refcounted entity (a
mutable string) embeds a buffer and puts *its own* `RcHeader` in front.
Keeping the buffer header-free is deliberate: it is a mechanism many
things reuse, not an object.

```
Buffer (3 words, caller owns — stack or embedded):   payload (arena):
{ data, len, capacity }  ─────────────────────────→  [bytes.......]
```

The buffer struct is not referenced by address the way entities are —
only its `data` payload moves, and the owner updates the field. Growth
algorithm in `ll_buffer_ensure`:

1. `capacity` suffices → return `data`, zero work.
2. Payload is the **top of its block's bump** → extend in place: move
   the bump, grow `capacity`. No copy, no new memory — an arena-only
   trick that malloc-based runtimes cannot do.
3. Otherwise → new payload at 2× capacity, copy, swap `data`. The old
   payload is arena garbage (dies at reset) or heap-freed.
4. Payload beyond the large threshold → block runs; growth first tries
   to extend the run with adjacent free blocks.

The extra indirection through `data` is the honest price of mutability;
immutable strings keep inline bytes and never pay it. Freezing a buffer
into an immutable string (builder → string) is the string layer's job.

## Rust API (internal, not ABI)

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

// buffer.rs
pub struct Buffer { /* handle: len, capacity, data */ }
impl Buffer {
    pub fn with_capacity(cap: usize) -> ...;
    pub fn ensure(&mut self, min_capacity: usize) -> *mut u8;
}
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
2. `arena` — bump, reserve, destructor list, reset + `LLContext` ABI.
3. `buffer` — first-class mutable buffers.
4. `immortal` — trivial specialization of arena.
5. Large-object runs.
6. (later) remembered set + arena-reset modes; MMTK heap.

## Test Plan

**Correctness (unit tests, every stage):**
- Block invariants: 32 KB size, 32 KB alignment, `ptr & !MASK` lands on
  the header, payload starts at +256.
- Arena: sequential allocation, size rounding to 8, slow path takes a
  new block exactly at exhaustion, `reserve` prevents mid-loop refills,
  `try_extend_in_place` succeeds only at the bump top.
- Pool: get/put roundtrip reuses blocks (no new region carved);
  cross-thread put (a dying thread's cache flushes to the global
  stack, blocks are not lost).
- Reset: destructor list is handed to the caller; blocks return to the
  pool and are reused by the next arena.

**Performance (criterion benches, honest methodology):**
- Same workload for every contender: N allocations of 40 bytes *plus
  reclamation* — arena pays its reset, malloc pays its frees. No
  measuring allocation while hiding the cleanup.
- Contenders: our arena, the system allocator (malloc via `Box`),
  `bumpalo` (the best Rust bump allocator) as the direct rival.
- Variants: tight loop; with `reserve` (compiler batch hint);
  mixed sizes.
- Results published in the README with the exact bench code linked —
  reproducible by `cargo bench`.
