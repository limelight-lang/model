# Research

Notes on code read outside this repository. An entry records what was
read, at which revision, what of it applies here, and which claims were
verified against source rather than taken from a summary. The point is
that a reading is done once and that a borrowed idea keeps its origin.

Read-but-rejected belongs here as much as read-and-taken: without it the
same library gets re-evaluated in six months.

## 2026-08-08 — Concurrency Kit

`github.com/concurrencykit/ck`, read at `b5475f5`, BSD-2-clause C. The
library itself is not a candidate dependency, being C and covering
ground `std::sync::atomic` already covers. Its value is the algorithms
and the reasoning written into the source comments.

### The version bracket in `array/table.rs` is half a barrier short

`ck_sequence.h` is the reference seqlock, and against it our version
counter orders one side of each bracket in the wrong direction.

Opening the window (`table.rs:428`, `begin_entry_move`) stores the odd
version with `Ordering::Release`. A release *store* orders accesses that
precede it; what the bracket needs is the opposite, that the odd version
becomes visible before the entry moves that follow. ck writes the odd
value with a plain store and then issues `ck_pr_fence_store()`. The
equivalent here is `store(v + 1, Relaxed)` followed by
`fence(Ordering::Release)`.

The read side has the mirror defect. `coherent_entries` opens with
`version.load(Acquire)` (`table.rs:472`), which is correct, because an
acquire load orders the three data loads after it. The closing check
(`table.rs:479`) is also an acquire load, and there the ordering is
needed in the other direction: the three data loads must be complete
before the version is re-read. ck puts `ck_pr_fence_load()` ahead of
that load, so the fix is `fence(Ordering::Acquire)` before it.

Closing the window (`end_entry_move`) is already correct: a release
store publishes the entry writes before the even version.

**Not observed, derived from the code.** On x86-64 neither defect can
fire in hardware, since TSO reorders neither store-store nor load-load,
so the exposure is compiler reordering — legal here, because the entry
writes are relaxed atomic stores and `used` and `nslots` are ordinary
writes. On aarch64 the hardware reorders both. We have no aarch64 box
and no test that would catch this on x86-64.

### ck_epoch: why reclamation happens at e+2

`src/ck_epoch.c` opens with a proof that three epoch values suffice, and
the argument is the one our epoch protocol needs to survive: active
threads hold `e_g` or `e_g - 1`, so objects logically deleted at
`e_g - 1` may still be referenced at `e_g`, and only at `e_g + 2` is
every active thread at `e_g + 1` or later. Reclaiming at `e_g + 1` is
sound only when no thread sits at `e_g`. The same file records why
blocking reclamation must not apply modulo-3 arithmetic to the global
counter itself, only to the deferral list index: under a bursty writer
the wrap-around live-locks.

Worth a deliberate comparison against `epoch.rs` and `deferred_free.rs`
before the next change to either. Not done yet.

### ck_ec: event counts instead of a condition variable

`include/ck_ec.h` implements an event count over futexes: the producer
mutates the structure and then increments the count, which doubles as a
write-write barrier and a wake; the consumer snapshots the value, reads
the structure, and blocks on that snapshot. A wake that arrives between
the snapshot and the block is not lost, because the block is conditional
on the value being unchanged. This is the primitive the event journal of
`PLAN.md` item 1 wants for a reader waiting on a ring, and the primitive
the collector wants while waiting for handshake acks.

### ck_hs: the per-bucket probe bound

`src/ck_hs.c` keeps, per bucket, the longest probe sequence ever used
from it (`ck_hs_map_bound_get`), so a miss stops at that bound instead of
walking to the first empty slot. Probing runs the eight slots of one
cache line first and only then takes a long stride
(`CK_HS_PROBE_L1`, `ck_hs_map_probe_next`). Deletion writes a tombstone
with a plain store, and insertion reuses tombstones, so readers need no
atomic operation at all; reclamation of a replaced map is left to the
caller's grace period.

Our table chains through a link at entry + 28 rather than probing, so
neither the bound nor the cache-line group transfers directly. The bound
is still the interesting half: it converts an unbounded miss into a
bounded one, and our flood backstop currently answers the same problem
by re-keying the whole table.

### ck_ring and ck_array

`ck_ring` is a bounded FIFO specialised four ways over single or many
producers and consumers, with the producer caching the consumer index to
avoid touching the shared line on every push. `ck_array` is an
append-only pointer array where readers iterate to `n_committed` and the
writer publishes that count with a store fence after writing the values,
growth going through a copy that is swapped in.

### What does not apply

`ck_pr` (atomics — `std::sync::atomic` covers it), the spinlock family
(MCS, CLH, ticket, Anderson), `ck_cohort` and `ck_rwcohort` (NUMA lock
composition, and we take a lock only on cold paths), `ck_elide` (needs
TSX), `ck_hp` (hazard pointers, where our design chose epochs),
`ck_bytelock` (the author marks it research-only).

## 2026-08-08 — Hash tables

### ankerl::unordered_dense

`github.com/martinus/unordered_dense`, MIT, read from `main`: the README
design section and `include/ankerl/unordered_dense.h:557`. Verified from
source, not from the description.

Same skeleton as our storage strategy 3: a dense vector of values plus a
flat index array. The bucket is eight bytes and splits into
`m_value_idx` (u32) and `m_dist_and_fingerprint` (u32), whose upper three
bytes hold the robin-hood distance and whose lowest byte holds one byte
of the hash. A lookup compares the fingerprint inside the index array and
touches the value only when it matches.

**What applies: the fingerprint.** Every probe along our collision chain
reads `hash_or_key` out of a 32-byte entry, so a colliding miss pays a
cache line per link. A fingerprint byte would answer most of those
inside the index array. The cost is where to put it: stealing eight bits
from the u32 index caps an array at 16M entries, and a parallel byte
array costs one more chunk and one more stride. Neither has been
measured, and this is a proposal, not a decision — it belongs to the
array-performance stage of `PLAN.md`.

**What does not apply: their removal.** They fill the hole by moving the
last value into it and patching the second bucket, which PHP semantics
forbid, since removal must preserve insertion order and leave a hole for
compaction to reclaim later.

### Named, not read

`abseil` swiss tables and folly `F14` (SIMD group probing over a
metadata byte array), `boost::unordered_flat_map`, `hashbrown`, CPython's
compact dict, and `zend_hash` itself. The SIMD family assumes open
addressing with movable entries, so it is a poor fit for an
order-preserving table; the CPython and PHP tables are the closest
relatives of what we build and are worth reading before the
array-performance stage rather than after.

## 2026-08-08 — Memory managers

Both entries below are read from project documentation only, not from
source. Treat the claims as reported, not verified.

### mimalloc

`github.com/microsoft/mimalloc`, MIT. The described design is the one we
built: a page holds one size class, and each page carries two free lists,
one for local frees and one for concurrent frees from other threads, so
a cross-thread free is a single CAS with no coordination. Contention
spreads across thousands of lists rather than one. Empty pages are
purged back to the OS eagerly, and the library exposes a deferred-free
hook explicitly for reference-counted runtimes.

That our per-block `owner`, per-block remote-free stack, and hand-over of
abandoned blocks at thread exit match a published and measured design is
the useful part: the shape has a name and a comparison point. Reading the
source would settle two open questions we answered by construction — when
a page is returned to the OS, and how adoption avoids unbounded growth of
the abandoned list.

### snmalloc

`github.com/microsoft/snmalloc`, MIT. Takes the other route for
cross-thread frees: instead of one atomic operation per free, the freeing
thread batches frees into a message queue owned by the allocating thread,
so thousands of remote deallocations cost one atomic operation. The
README names batch deallocation as the workload other allocators handle
worst.

This is the alternative to our per-block remote stack. What we pay today
is one CAS loop per freed item, on both cross-thread paths —
`heap.rs:967` (`free_remote`) and `buffer_arena.rs:733` (`post_remote`) —
so the cost of freeing another thread's memory is linear in the number of
items, with the contention spread across blocks rather than gathered on
one queue. snmalloc gathers it on one queue per owning allocator and wins
back the linearity by batching.

Which side is better depends on a workload we do not have. Release
batching already exists on the caller's side (`ll_release_vector`,
`ll_release_batch`), so the batch is available at the point where the
frees are issued; nothing downstream of it batches. Not evaluated, and it
should not be until a program exists that frees another thread's objects
in bulk.

`deferred_free` is not that batch and should not be mistaken for it. Its
parked list is thread-local and exists for identity — a slot must name
one entity from walk to drain — so the flush replays the records one at a
time through `ll_free`, and a record whose block belongs to another
thread pays its own CAS there. The list is still where snmalloc's shape
would land when actors arrive: a per-thread queue of pending frees
already exists, and grouping its records by owning block before the flush
would turn a chain into one CAS per block without new machinery.

### Named, not read

`tcmalloc` (per-CPU caches over restartable sequences), `jemalloc`
(arenas and extent trees), and MMTk, which is a Rust framework of
collectors rather than an allocator and is the closer comparison for the
collector side. `rpmalloc` was read on 2026-08-10; its entry is below.

## 2026-08-10 — rpmalloc

`github.com/mjansson/rpmalloc`, read at `5dacae8`, version 2.0.1,
Unlicense OR MIT. Read from `rpmalloc/rpmalloc.c` and the README design
section; every line reference below was opened, and nothing here comes
from the changelog alone.

2.0.0 replaced the core of the 1.4 series, so anything recalled from the
older design describes different code. Memory is now span (256 MiB,
fixed alignment) then page (64 KiB, 1 MiB, 4 MiB or 16 MiB by block
size) then block, both headers found by masking the block address; the
per-thread and global span caches are gone, replaced by reserving
address space and committing per page on demand.

Five mechanisms apply to `src/memory/` and one that looks applicable is
not. None of them is measured on our side, so each is a proposal and is
written as one.

### The cross-thread free list carries its own length

`page->thread_free` is one `atomic_ullong` holding the head block's
index within the page in the low half and the list length in the high
half (`rpmalloc.c:1213`, `rpmalloc.c:1221`). A remote free reads the
previous length out of the token it observed and CAS-es the new pair
(`rpmalloc.c:1404`). The owner takes the list with one CAS to zero and
gets the count with it, so the used-block counter is corrected without
reading a single link (`rpmalloc.c:1381`).

Our `collect_remote` (`heap.rs:995`) swaps the list out with one atomic
and then walks it end to end for one reason: to learn how far `used`
must drop (`heap.rs:1009`). Every link on that walk was written by
another thread, so it is a cache miss, and the list is longest exactly
when the block is most contended. Packing the count beside the head
removes the walk. Our slot index fits the low half with room to spare
(4080 slots at the 16-byte class), and the tail is still needed to
splice onto a non-empty local list — rpmalloc sidesteps that by adopting
only into an empty one (`rpmalloc.c:1372`), which is the state
`alloc_block_full` meets by construction, a full block having no local
list.

The win is smaller than a missing walk suggests, and the reason belongs
here before anyone builds it. Both collection sites are cold — `refill`
runs about 0.00003 times per allocation on the steady-state benchmarks —
and the slots the walk chases are the slots the block is about to hand
out, so the misses it pays are misses the allocation path would pay a
moment later. What the count buys is the serial dependency: a pointer
chase cannot prefetch, while the pops that follow can. Measure first.

### Reallocation in place

rpmalloc returns the same block when the new size still fits it
(`rpmalloc.c:2402`), refuses to move a huge block that shrinks by less
than half (`rpmalloc.c:2413`), and on a move that does happen
overallocates to 1.375x when the growth is smaller than that, so a loop
growing by a few bytes at a time stops reallocating at every step
(`rpmalloc.c:2429`).

`ll_realloc` (`stdapi.rs:369`) allocates, copies and frees on every call,
including when the old and the new size share a class: 40 bytes to 48
bytes costs a block, a `memcpy` and a free to move inside one 48-byte
slot. The class size is already recoverable, since `ll_usable_size`
(`stdapi.rs:349`) reads it from the block header, so the test is one
comparison on a path that is cold anyway. No entity is involved:
`realloc` serves the raw C surface, and the walker reads no block of
that kind.

No benchmark covers it. `rptest` in `benches/standard.rs` frees and
allocates rather than reallocating, so this path has no measurement at
all, in either shape.

### The band between the largest class and one whole block

Our classes stop at 8 KiB (`heap.rs:102`), and everything above that up
to a block payload takes a whole 64 KiB block (`stdapi.rs:154`), so a
9 KiB request holds 64 KiB — about eight times what it asked for,
against the "under 25%" that `docs/memory-manager.md` states for the
classes below. rpmalloc holds a step of roughly 25% up to 128 KiB by
giving the larger classes their own page sizes (`rpmalloc.c:687`). The
same band here needs no second page type: classes of two to five slots
per 64 KiB block keep the stride uniform, which is what the walker's
stride and the header layout depend on.

Alignment reaches the same path from the other side. A request with
`align > 16` is routed there whatever its size (`stdapi.rs:147`), so
`aligned_alloc(64, 40)` costs a 64 KiB block. rpmalloc allocates
`size + alignment` from the ordinary classes, offsets the pointer and
marks the page as carrying aligned blocks (`rpmalloc.c:2376`); free
realigns by the block size only in pages holding that flag
(`rpmalloc.c:1759`). Worth building only for a caller that wants
over-16 alignment, and the runtime has none today.

### Knowing a block is already zero

`page->is_zero` records that a page's blocks read zero, and the
allocator uses it to skip the memset in `rpzalloc` (`rpmalloc.c:1491`).
The flag is set where the knowledge is free: a page recommitted after
decommit comes back zeroed by the kernel, so only the header prefix is
cleared by hand and the rest is declared zero (`rpmalloc.c:1281`).

`Heap::refill` writes eight bytes into every slot of an entity block,
unconditionally (`heap.rs:1115`). Up to 4080 stores at the 16-byte
class, and because the stride is 16 bytes it dirties every cache line of
the 64 KiB block, which is one refill costing the write traffic of the
whole block. The rule it enforces is narrower than the pass: the walker
tests one field, `refcount != 0` (`heap.rs:2020`), and reads only slots
below `bump`.

Two sources of the same knowledge exist here and neither is used. A
block carved from a fresh region is untouched memory, and regions come
from `alloc` (`block_pool.rs:501`); `alloc_zeroed` for a 2 MiB
block-aligned region is served by a fresh kernel mapping, so the
guarantee costs nothing. A block returned empty from an *entity* heap
still satisfies the invariant: `FreeSlot` deliberately preserves the
first eight bytes, holding the dead entity's final header
(`heap.rs:175`), and an entity dies at refcount 0. What breaks the
invariant is a block that served as raw or arena memory in between, or a
recommissioning at a different stride, so the flag has to name the
stride it holds for.

### Commit on demand, decommit on a threshold, and a huge-mapping cache

Free pages accumulate per page type until the count crosses 16, 8, 4 or
2 (`rpmalloc.c:712`), at which point the excess is decommitted down to a
retained 4, 2, 1 or 1 (`rpmalloc.c:715`, applied at `rpmalloc.c:2003`).
The page header prefix stays committed so the metadata survives, and the
prefix size is the page size captured at map time, so commit and
decommit always name the same range (`rpmalloc.c:1249`).

Freed huge mappings go to a 32-slot cache instead of straight back to
the OS: bounded by committed bytes rather than count, evicted by age,
and reused when the request fits within a 25% overshoot
(`rpmalloc.c:1600`, `rpmalloc.c:1708`).

Our pool never returns a region (`block_pool.rs:10`, where the lazy
purge is recorded as deferred), and our one-block-per-class
`empty_reserve` (`heap.rs:1031`) is the same hysteresis at a count of
one. `LARGE_RUN` unmaps on every free (`stdapi.rs:24`), which is the
allocation shape a huge cache exists for.

### Routing a free into a full block, and why it stays there

A full page is on no list its owner scans, so a foreign free left there
waits for a local free to touch the page. rpmalloc has the freeing
thread read `is_full` and push into a per-page-type list on the heap
instead (`rpmalloc.c:1391`); the owner drains it on the refill path with
one CAS and frees each block locally (`rpmalloc.c:2133`). We answer the
same problem from the other end: `collect_owned` (`heap.rs:685`) sweeps
every block this heap owns of the class before drawing a fresh one, and
the comment there records what its absence cost (34.2M to 2.3M ops/s on
`mt_bench`). O(blocks owned) on our side against O(1) on the freeing
thread looks like a trade worth taking, and it is not available to us.

Two things block it. The freeing thread reads `is_full` while the owner
writes that packed flag word, which rpmalloc documents as a benign race
and silences in ThreadSanitizer (`rpmalloc.c:539`); for us it is a data
race Miri reports, and the gate takes Miri seriously. And the list it
pushes to belongs to a heap, which in rpmalloc outlives every thread
(`rpmalloc.c:1952`), while ours dies with its thread — a message posted
to a dead heap is stranded, and that is exactly why `remote_free` sits
in the block (`heap.rs:296`).

### What we already do, and rpmalloc does not

Virgin slots. rpmalloc threads a free list through the new blocks of a
page at first touch, bounded to the current OS page so the work stays
inside one fault (`rpmalloc.c:1435`). Our bump cursor makes the same
slots available with no per-slot work at all, which is the trade
`heap.rs` records the bitmap losing.

### What does not apply

The three-level hierarchy with 256 MiB span alignment answers a problem
we do not have: we carry one block size and one mask, and our largest
class is far below the point where a fixed page size wastes. Heap
packing (`rpmalloc.c:1874`) exists because a thread heap is small
against a mapping page; ours comes from the process allocator as one
`ThreadHeaps` pair per thread (`heap.rs:1720`). The spin that escalates
to `sched_yield` after 100 pauses (`rpmalloc.c:409`) is the answer to a
hand-rolled lock under preemption; our cold paths take a
`std::sync::Mutex`, which parks.
