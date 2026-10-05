# Research

Notes on code read outside this repository. An entry records what was
read, at which revision, what of it applies here, and which claims were
verified against source rather than taken from a summary. A line number
into this crate's own sources drifts; the symbol beside it is the anchor,
and the external revisions are pinned. The point is
that a reading is done once and that a borrowed idea keeps its origin.

Read-but-rejected belongs here as much as read-and-taken: without it the
same library gets re-evaluated in six months.

## 2026-08-08 — Concurrency Kit

`github.com/concurrencykit/ck`, read at `b5475f5`, BSD-2-clause C. The
library itself is not a candidate dependency, being C and covering
ground `std::sync::atomic` already covers. Its value is the algorithms
and the reasoning written into the source comments.

### The version bracket is half a barrier short

`ck_sequence.h` is the reference seqlock, and against it the array's
version counter ordered one side of each bracket in the wrong direction:
`begin_move` published the odd version with a release store, which orders
what precedes it rather than what follows, and `coherent`'s closing check
re-read the version with an acquire load, which orders what follows
rather than the three data loads before it. ck writes the odd value
plainly and issues `ck_pr_fence_store()`; it puts `ck_pr_fence_load()`
ahead of the closing read.

**Applied 2026-08-11** in `18c585c`, both sides, and `array::head.rs`
cites this entry at the fences. The demonstration, the loom model that
exhibits the accepting execution for the old bracket and for either fence
taken alone, and the aarch64 cost are `dev/DECISIONS.md`, "the table's
version bracket orders both sides with fences".

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

Both modules the comparison was written for are deleted; `rc-cycle` has
no epoch counter, and its window is per thread
(`cycle::deferred_slot_reuse`, `ActiveTrace`). The e+2 argument keeps its value as a reading
record.

### ck_ec: event counts instead of a condition variable

`include/ck_ec.h` implements an event count over futexes: the producer
mutates the structure and then increments the count, which doubles as a
write-write barrier and a wake; the consumer snapshots the value, reads
the structure, and blocks on that snapshot. A wake that arrives between
the snapshot and the block is not lost, because the block is conditional
on the value being unchanged. Neither customer this was read for exists: the event journal was built
without a waiting reader — §9.3 marks a window by snapshotting cursors —
and the handshake the collector would have waited on retired with
`rc-walk`.

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
TSX), `ck_hp` (hazard pointers, where the design uses a trace window),
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
block carved from a fresh region is untouched memory, and since
`8208815` regions come from the OS directly (`memory::os::map_aligned`),
whose mapping the kernel zero-fills — so that half is already paid for
and only carrying the flag across recycling is open. A block returned empty from an *entity* heap
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

## 2026-08-10 — Large objects in eight runtimes, and what PHP allows

Read for S11, the stage that gave an entity larger than one block a
strategy; that stage is closed and the result is
`src/memory/large_entity.rs` (`dev/DECISIONS.md`, the S11 ruling). The
survey and the PHP measurement below are the evidence the built design
rests on and exist nowhere else. The question put to each: how is an object too large for the
ordinary small-object allocator allocated, found by the collector,
reclaimed, and what does the runtime refuse.

**The split that decides everything is how the collector finds an
object.** Where it walks memory by address — HotSpot's G1 and ZGC, V8,
Go, CoreCLR — a large object gets its own aligned chunk and is never
moved, because moving it is what the alignment buys and one object in a
chunk has nothing to compact against. G1 calls it humongous at half a
region and takes a run of contiguous regions, the first marked
StartsHumongous, the tail filled with filler objects so iteration still
strides; ZGC gives an object over 256 KB its own page rounded to the
2 MB granule and never relocates it; V8 cuts at half a page (128 KB of
256 KB), gives a `LargePage` per object, masks the address to find the
header and keeps an ordered set of large pages for interior addresses
that masking cannot resolve; Go's boundary is one 32 KB size class,
above which a span holds exactly one object over ⌈size/8192⌉ contiguous
pages, found through the two-level arena map; CoreCLR sends anything
past 85 000 bytes to the LOH, swept rather than compacted because
"compacting it can be expensive".

Where the collector follows an explicit list or graph — PHP and
LuaJIT — size is invisible to it and no strategy is needed at all.
`zend_gc.c` is refcounting plus trial deletion over the object graph:
the root buffer holds pointers and an object carries its own buffer
index, so nothing is ever looked up by address. LuaJIT threads every
object on a `nextgc` list and marks in the object's own byte.

**Ruby is the closest case to ours and it refuses.** It walks pages by
alignment, so it caps a slot at the largest of twelve size classes
(1024 bytes) and puts everything larger outside the GC heap under a
normal slot that frees it — which is our body rule, arrived at from the
same constraint.

**Two things taken from this into the design.** The threshold everywhere
is a fraction of the page or region rather than an absolute number —
half a region, half a page, an eighth of a page, one span — which is the
form the S11 invariant takes: the category's packing unit. And a refusal
by a per-type cap is normal practice rather than an evasion; V8 has
`FixedArray::kMaxLength`.

**What PHP allows, measured here rather than read** (PHP 8.6.0-dev on
this box, 2026-08-10). A class of 10 000 declared properties compiles
and runs; one instance costs 163 840 bytes — 10 000 zvals of 16 bytes
rounded to a page run inside a 2 MiB chunk. A class of 200 000 works
too, at 3 203 168 bytes, which is past the chunk and takes Zend's huge
path with its own mmap. Dynamic properties are not in the slot at all:
10 000 of them cost 1 299 552 bytes in a hash table beside the object.
On our layout the same two classes are 2.5 and 49 blocks, and that
measurement is why S11 supports large slots instead of capping them —
a cap at one block payload is 4 079 properties, and it would refuse a
program Zend runs.

Sources: OpenJDK `g1CollectedHeap.cpp` and `zObjectAllocator.cpp`;
V8 `globals.h` (`kMaxRegularHeapObjectSize`), `large-spaces.cc`,
`memory-allocator.h`; Go `sizeclasses.go`, `malloc.go`, `mcache.go`;
CoreCLR `regions_segments.cpp` and Microsoft's LOH documentation;
php-src `zend_alloc.c`, `zend_alloc_sizes.h`, `zend_gc.c`,
`zend_object_handlers.c`; ruby `gc/default/default.c`; LuaJIT
`lj_alloc.c`, `lj_gc.c`; CPython `pycore_obmalloc.h` and the garbage
collector's internal docs.

## 2026-08-18 — the uncounted-partition literature, swept for the stack-bit question

Not a repository reading: a web sweep over papers and system
documentation, done while ruling on the uncounted-bit proposal (the
ruling is `dev/DECISIONS.md`, same date). Nothing here was read at a
pinned revision; every claim keeps its source and is re-verified before
it steers a design.

- Deutsch & Bobrow 1976, deferred RC: stack references uncounted, a
  zero-count table, and the stack question answered by scanning every
  frame at reconciliation — never by per-object mutator state.
- Wise & Friedman 1977, and MRB in KL1 (1987–1992): the published
  one-bit counts mean unique-versus-shared and are deliberately
  sticky — the bit is never cleared, because a clearable bit cannot
  survive aliasing; freeing the shared falls to backup tracing.
- Free-threaded CPython, PEP 703: the closest industrial match to the
  hybrid — a permanent per-object deferred-refcount flag, stack
  references skipped, reclamation only by the tracing GC, which walks
  every thread's frames at collection start.
- Ulterior RC (Blackburn & McKinley, OOPSLA 2003), RC Immix (2013),
  LXR (PLDI 2022): counted/traced hybrids partitioned by age or
  region; stack roots come from the VM's precise enumeration or a
  conservative scan, never from object bits.
- Chrome Oilpan and JSC Riptide: a traced heap cohabiting with a
  counted world; stack roots of the traced side by conservative scan;
  cross-world edges by tracing through, or by constraint re-marking.
- Hazard pointers (Michael 2004), epoch reclamation (Fraser 2004,
  crossbeam), V8 HandleScope, JNI local tables: mutator-announced
  possession is always per-slot, per-frame or per-thread —
  single-writer state — precisely because a shared per-object flag
  collides with multi-frame aliasing.
- The static family — Joisha ISMM 2007, Rust borrows (RustBelt, POPL
  2018), Perceus with borrowing, Lobster, Nim ARC/ORC — removes
  counting where an owner provably covers every borrow: the road
  the deleted `rc-walk.md`'s birth count and unique ownership already walk, with
  Nim's `.acyclic` partition as the spiritual match for a purity-gated
  class split.

What targeted searching did not find: any published system using a
compiler-maintained clearable per-object "a local may hold me" bit as
the collector's root oracle, under any language restriction. That is
absence of publication, not proof of impossibility — the impossibility
argument is the ruling's, in `dev/DECISIONS.md`.

## 2026-09-18 — YRC's re-registration saving, the one number behind the deferred lane

Filed on S37.4's deletion, which carried it as a half-sentence: Nim's YRC
reports a 56 % saving on re-registration from keeping a proven-live root
registered instead of registering it again at the next decrement, and that
figure is the whole quantified motive on record for this crate's deferred
candidate lane. **It is borrowed and unmeasured here.** The lane's own price
was taken against this crate's fixtures (`dev/BENCHMARKS.md`, S37.4), the
turnover it re-offers at is provisional after YRC's own values
(`dev/DECISIONS.md`, "maturation is Y9's edge-side prune, and the trace
writes nothing"), and what a real
workload saves is owed at S37.5 on a corpus. Quoted as YRC's number, never as
this crate's.

## 2026-09-21 — the idle-GC timers of five runtimes, for the quiet thread's X

Read at source on 2026-09-21, each file at its `master`/`main` head of that
day, for the question S60 asks: after how long does a runtime collect a
mutator that allocates nothing and triggers nothing, and what does the timer
protect against. Verified by `grep` over the fetched file, not from a summary.

| runtime | timer | default | condition | source |
|---|---|---|---|---|
| Go | `forcegcperiod` | 2 min | "maximum time between garbage collections. If we go this long without a garbage collection, one is forced to run"; sysmon sleeps `forcegcperiod / 2` | `src/runtime/proc.go` |
| HotSpot G1 | `G1PeriodicGCInterval` | 0, off | ms since the previous GC; cancelled when `getloadavg()` 1 m exceeds `G1PeriodicGCSystemLoadThreshold` (0, off) | `gc/g1/g1_globals.hpp` |
| HotSpot Shenandoah | `ShenandoahGuaranteedGCInterval` | 5 min | "useful when large idle intervals are present, where GC can run without stealing time from active application"; young 5 min, old 10 min | `gc/shenandoah/shenandoah_globals.hpp` |
| HotSpot ZGC | `ZCollectionInterval` | 0, off | seconds, "Force GC at a fixed time interval"; `ZProactive` on by default is a heap-growth rule, not a timer | `gc/z/z_globals.hpp` |
| V8 | memory reducer | 8 s after the allocation rate drops (`memory_reducer_delay_ms`), at most 2 GCs (`memory_reducer_gc_count`) 500 ms apart (`kShortDelayMs`); a watchdog GC at 100 s without one (`kWatchdogDelayMs`) | `HasLowAllocationRate()` — the mutator went quiet | `src/flags/flag-definitions.h`, `src/heap/memory-reducer.cc` |

What each protects against. Go's and Shenandoah's timers bound how long a
process that stopped allocating keeps garbage it will never trigger a
collection for — finalizers, memory returned to the OS. G1's and ZGC's are off
by default: their triggers are allocation and occupancy, and a periodic cycle
is the operator's choice. V8's memory reducer is the nearest in shape to S60's
X: it detects the transition from allocating to quiet and collects the garbage
the quiet phase left, on a delay, a bounded number of times, with a long
watchdog behind it.

What applies here. The field's range for "collect a quiet mutator" runs from
8 s (V8) through 100 s (V8's watchdog) and 2 min (Go) to 5 min (Shenandoah);
nothing runs at 1 s. S60's X guards a narrower thing than any of these — a
component that became garbage while its root was parked, on a thread that
registers nothing — and its cost per X is one un-pruned trace of the lane's
closure, so the memory side, not the CPU side, is what a longer X spends. The
figure is Edmond's; the entry records the range and the sources.

## 2026-09-30 — pruning by generation in cycle collectors, and how an interruptible trace completes

Two web sweeps for S67.9 (`dev/plans/S67.md` at `410b856`, S67.9), each claim
with its source as the sweep gave it. Only Nim YRC was read here at source
(`lib/system/yrc.nim`, `devel`, fetched 2026-09-30): its stamp rule, epoch
length, promotion age and `genSuspects` buffer are verified by `grep` over the
file; the rest is the sweeps' reading and is re-verified before it steers a
design.

**Pruning by generation is the standard answer where generations exist.**
CPython up to 3.13 subtracts only references internal to the generation it
collects, so older generations stand as external roots and are never entered;
a full collection waits for `long_lived_pending` to pass a quarter of
`long_lived_total` (`Modules/gcmodule.c`, 3.12; `Python/gc.c`, 3.13).
Age-oriented RC (Blackburn et al.) and Azatchi–Petrank reclaim old cycles by an
infrequent full trace. Nim YRC stamps the cells a commit proved live with the
epoch and a survival age, and a capture does not descend into a stamped
descendant of the current epoch at age `>= YrcPromoteAge` (3) whatever its
root-buffer flag — the test in `claimCell` reads the stamp word alone; a root
bypasses the stamp only at its own scan. The epoch advances every
`YrcEpochLen` = 64 collections; an old cycle whose last external references
were young is kept in `genSuspects` and promoted to the roots at the advance,
"the major-collection half". The source records that epochs shorter than about
4 resonate with the adaptive threshold, and that a work-based clock lost on its
web benchmark.

**Trial-deletion collectors without generations re-trace.** Bacon–Rajan 2001
admits the same live structure may be traversed many times; PHP's `zend_gc.c`
and Nim ORC skip only acyclic types and raise the threshold when a run frees
little; Ulterior RC caps the trial deletion's time and reports that "if the
time cap is too low, some programs may be unable to reclaim cyclic garbage".
Gecko's cycle collector drops known-live objects from the purple buffer
(`CanSkip`, `CanSkipInCC`), treats every purple object as live during an
incremental collection, and lifts the slice budget after 2 s. Paz et al.
(Technion TR CS-2003-10) treat candidates of newer buffers as live on purpose.

**No interruptible trace in the sources completes without a write barrier,
mutator work or a stop-the-world fallback, and none restarts from nothing by
design.** Go's mark assists, V8's allocation-driven steps and atomic finish,
SpiderMonkey's growing slice budget and non-incremental finish, G1's full GC,
Shenandoah's degenerated cycle, Lua's allocation-debt steps: each keeps its
partial mark behind a barrier. CPython 3.14.0's incremental collector ran
without a barrier by taking each increment's whole unscanned closure, and was
reverted in the 3.14 series for memory growth (up to 5× peak RSS, by the
reverting thread; not checked against a tag). LXR (PLDI 2022) derives
snapshot-at-the-beginning from the decrements its coalescing RC barrier
already logs. Measured barrier costs: card marking 0.9 % of execution time
(Blackburn and Hosking, ISMM 2012); G1's unconditional SATB barrier 5.5 % and
all its barriers 12.4 % of mutator time (VEE 2020).

What applies here: this crate borrowed YRC's stamp, epoch and age but added an
exemption YRC does not have — a registered target is never pruned — and has no
old-generation pass; that pair is S67.9's cause. The trace's completion under
the recall keeps its owner question (S67.9's proposal, item 5).

## 2026-09-30 — one graph for all roots, breadth first, and liveness flooded from the roots: Firefox, CPython, Lins

A third sweep for S67.9, on Edmond's question whether flow and shortest-path
algorithms hold a hint (`dev/plans/S67.md` at `410b856`, S67.9, the review's
round 1). The sweep read Firefox at source (`xpcom/base/nsCycleCollector.cpp`
at `e68e3aea`, 2026-09-01; `dom/base/CCGCScheduler.cpp`,
`nsCCUncollectableMarker.cpp`, `FragmentOrElement.cpp`,
`js/xpconnect/src/XPCJSRuntime.cpp` on main, fetched 2026-09-30) and CPython
at tags v3.14.0, v3.14.4 and v3.14.5 (`Python/gc.c`,
`Python/gc_free_threading.c`); the papers are cited as the sweep gave them.
The line numbers are the sweep's and are re-read before a design rests on one.

**Lins traced one root at a time, which is quadratic; Bacon–Rajan run each
phase over all roots, which is linear** (US6879991B2; Frampton et al.,
"Efficient Concurrent Mark-Sweep Cycle Collection", §2). This crate's parts,
each resetting its rows (`TraceScratchArena::reset_to_the_watermark`), repeat
Lins' pattern over the shared live state. Firefox proposed the same one-root
"soft incremental" scheme and closed it WONTFIX: almost everything is
reachable from a root, and the graph is very connected (bug 701878). What it
ships is one graph over all purple roots, deduplicated per object, built in
slices that resume where they stopped (`BuildGraph`), with a slice budget of
3 ms that grows with elapsed time and turns unlimited after 2 s
(`CCGCScheduler.cpp`, `kMaxICCDuration`); an object retained during building
is treated as live afterwards.

**Both production collectors build breadth first**: Firefox walks its node
pool in insertion order, CPython's `move_unreachable` scans a list the
traversal appends to. This crate's mark is depth first (`pop_work` over a
stack).

**Liveness is flooded from the true roots so the cyclic pass skips it**:
CPython's free-threaded build marks what the sysdict and the thread stacks
reach as alive before the cyclic pass (`gc_mark_alive_from_roots`), and a miss
falls through to the ordinary computation; Firefox marks documents reachable
from live windows at each collection's start and skips their nodes
(`CanSkipInCC`), and black JS objects never enter its graph.

**A distance estimate orders, it does not cut** (Maheshwari–Liskov, as the
Terriberry survey cites them): live objects settle at their distance from the
roots, garbage cycles' estimates grow. **A futile local search is ended by an
exhaustive pass**, as push–relabel's global relabeling and Firefox's unlimited
budget after 2 s do; Ulterior RC warns that a time cap set too low leaves some
programs' cyclic garbage unreclaimed (Blackburn–McKinley, OOPSLA 2003, §3.2.3).
CPython's incremental collection scaled its work by new objects, took each
object's full closure, and is present at v3.14.4 and absent at v3.14.5.

**Not found:** a production collector that posts partial results from a
truncated trial deletion; the soundness of S67.9's (6′) rests on this crate's
own reading. Not read: Pony ORCA, Paz–Petrank, Oilpan, PHP, Perl.

## 2026-10-04 — immediate RC with a concurrent or incremental cycle collector: who built the hybrid

Asked by Edmond after the Recycler design rounds: has anyone combined
owner-written immediate reference counting with a cycle collector that runs
beside the mutator, and how did each make mutations during the collection
safe? A web sweep by a research agent; WebFetch could not read the PDFs of
Bacon–Rajan, Formiga–Lins, Joisha and Frampton, so those are taken from
abstracts and secondary pages and marked so. Nothing here was run.

### What each system does (sourced)

- **Firefox, the XPCOM cycle collector.** Immediate RC, a purple buffer, an
  incremental collector on the main thread. Only graph building runs in
  slices; `ScanRoots` and `CollectWhite` run in one slice with no mutator
  between them ([nsCycleCollector.cpp](https://raw.githubusercontent.com/mozilla/gecko-dev/master/xpcom/base/nsCycleCollector.cpp)).
  During an incremental collection both `AddRef` and `Release` put the object
  in the purple buffer with a dirty bit; every dirty object is treated as
  live, and what the buffer gains during the collection is a suspect only for
  the next one ([bug 850065](https://bugzilla.mozilla.org/show_bug.cgi?id=850065)).
  Debug builds run a synchronous collection beside it and check that both
  find the same garbage. A ~200k-object teardown went from ~600 ms to ~200 ms
  in 40–47 ms slices. A count reaching zero sends the object to the buffer as
  "snow-white" rather than deleting it, and snow-white objects are not freed
  while a scan runs ([nsISupportsImpl.h](https://raw.githubusercontent.com/mozilla/gecko-dev/master/xpcom/base/nsISupportsImpl.h)).
  The use-after-free of [bug 1023758](https://bugzilla.mozilla.org/show_bug.cgi?id=1023758):
  a node traversed earlier died during the collection, was ignored, and a live
  object it pointed to was unlinked; the fix treats dead traversed nodes as
  roots. In production.
- **Kotlin/Native's first memory manager.** Deferred RC with a
  trial-deletion cycle collector, and a "shared cyclic collector" on a
  background thread over frozen objects reachable from atomic references,
  restarting with backoff when it saw a count change; review noted it was
  "not properly synchronized with refCount() mutation"
  ([roadmap 2020](https://blog.jetbrains.com/kotlin/2020/07/kotlin-native-memory-management-roadmap/),
  [update 2021](https://blog.jetbrains.com/kotlin/2021/05/kotlin-native-memory-management-update/),
  [PR 3742](https://github.com/JetBrains/kotlin-native/pull/3742)). Replaced
  by a tracing collector in 2021–22 for low throughput, "pauses become rarer
  but longer", the freezing model, no code sharing with the JVM, leaks of
  cycles through atomic references, and tracing being "far more flexible and
  tunable".
- **CPython.** The free-threaded build (PEP 703) uses biased RC and a
  stop-the-world cycle collector ([PEP 703](https://peps.python.org/pep-0703/)).
  The incremental cycle collector shipped in 3.14 was reverted in 3.14.5: the
  longest pause fell from 26 ms to 1.3 ms, but memory grew up to 5× and total
  runtime was slower ([discussion](https://discuss.python.org/t/reverting-the-incremental-gc-in-python-3-14-and-3-15/107014)).
  No concurrent-thread attempt found.
- **PHP, `zend_gc`.** Synchronous Bacon–Rajan when its 10k-entry root
  buffer fills ([manual](https://www.php.net/manual/en/features.gc.collecting-cycles.php)).
  No concurrent proposal found.
- **Others.** Nim ORC: synchronous trial deletion in the thread
  ([blog](https://nim-lang.org/blog/2020/12/08/introducing-orc.html)).
  Lobster reports leaked cycles at exit
  ([docs](https://aardappel.github.io/lobster/memory_management.html)).
  Perceus/Koka collects no cycles ([paper](https://xnning.github.io/papers/perceus.pdf)).
  Swift, Objective-C, Perl and Vala have no cycle collector and rely on weak
  references (from memory, not checked). Pony traces inside an actor and
  counts references between actors deferred; a detector actor proposes cycles
  of blocked actors and confirms them by CONF/ACK messages
  ([OOPSLA'13](https://www.ponylang.io/media/papers/opsla237-clebsch.pdf)) —
  the closest "collector proposes, owners confirm" protocol, but over actors.
- **Samsara (Rust), the closest in mechanism.** Mutators do immediate RC on
  `Arc`; a collector thread runs trial deletion concurrently and frees what
  it finds; a visited object that is decremented turns DIRTY and is re-queued;
  the collector holds read/write locks while scanning, so a mutator may block
  on a write; an unsound free panics ([blog](https://redvice.org/2023/samsara-garbage-collector/),
  [repo](https://github.com/chc4/samsara)). A hobby project with no
  benchmarks. `bacon_rajan_cc` and `dumpster` collect in the calling thread.
- **Academic.** Bacon–Rajan's concurrent collector (the Recycler): deferred
  RC, mutators log increments and decrements into epoch buffers the collector
  applies ([PLDI'01](https://dl.acm.org/doi/10.1145/378795.378819)), longest
  pause 2.6–6 ms; Σ-test (no reference from outside the cycle) and Δ-test (no
  member's count rose in the next epoch), from a
  [secondary summary](https://maplant.com/2024-12-13-Scheme-to-the-Spec-Part-I:-Concurrent-Cycle-Collection.html).
  Paz et al.: sliding-views cycle collection over deferred RC, ~1 ms pauses,
  fixing Bacon–Rajan's termination problem
  ([TOPLAS 2007](https://dl.acm.org/doi/10.1145/1255450.1255453)). Lins and
  Formiga–Lins: the mutator sends increment/decrement queues to a collector
  processor; "preliminary" speedups only
  ([VECPAR'02](https://link.springer.com/chapter/10.1007/3-540-36569-9_44),
  [JUCS'07](https://zenodo.org/records/6999854)). Frampton et al.: deferred RC
  with concurrent mark-sweep for cycles; "objects subject to races during
  concurrent tracing are trivially identified using data already established
  by the reference counter"; up to 2× better than trial deletion
  ([TR](https://www.semanticscholar.org/paper/Efficient-Concurrent-Mark-Sweep-Cycle-Collection-Frampton-Blackburn/c153afb4e7ec70d0c7fa272208e671680c07a28e)).
  LXR ([PLDI'22](https://arxiv.org/abs/2210.17175)), RC Immix and Ulterior RC
  are deferred RC with backup tracing. Joisha's Bartok: immediate RC with
  compiler optimisations ([TR](https://www.microsoft.com/en-us/research/wp-content/uploads/2016/02/tr-2007-104.pdf));
  its cycle handling not confirmed.

### What applies here (inference, not sourced)

- No published or production system was found with owner-written,
  non-atomic immediate RC plus a concurrent collector thread over shadow
  state; the combination appears new. Closest: Firefox (the same mutation
  rule in one thread), Samsara (a concurrent thread over immediate RC, with a
  DIRTY flag), Pony (proposal and confirmation).
- Firefox's rule — any count write during the window means live, deferred to
  the next round — is the window-tag variant (R2) run in one thread, and it
  holds only with increments marking as well as decrements, with objects that
  die mid-scan treated as roots (bug 1023758), and with no mutation between
  the final scan and the free; on a separate thread the last is the handshake
  that publishes the owner's tag bytes.
- Bacon–Rajan's Σ/Δ and Lins' design assume deferred counts the collector
  owns; over owner-written counts the window tags stand in for the Δ-test.
- Deferring suspects to the next round can inflate memory (CPython 3.14,
  up to 5×); Kotlin/Native dropped its concurrent RC cycle collector for
  races, pauses and inflexibility. A shadow synchronous checker in debug
  builds, as Firefox runs, is the soundness check to copy.

## 2026-10-04 — why the Recycler did not become a mainstream collector

Asked by Edmond with the Recycler designs on the table. A research agent read
the full texts of the PLDI'01 paper, Paz et al., Levanoni–Petrank (TOPLAS),
Ulterior RC, "Down for the Count", RC Immix, LXR and Biased RC, and the
production sources below; the ECOOP'01 full text was not reachable (abstract
only). The UCSB mirror "bacon-concurrent.pdf" is the PLDI paper, so section
and table numbers are the PLDI paper's. Nothing here was run.

### Bottom line: why it did not spread

1. Its 2.6 ms pauses were bought with a spare CPU for every ~3 mutator CPUs,
   an atomic exchange on every heap pointer store, and collector work many
   times mark-sweep's (javac 104.1 s against 2.8 s); its one collector thread
   did not scale, in the authors' own words, and mutators blocked when memory
   or the mutation buffers ran out (up to 43 MB).
2. Its concurrent cycle collector rescanned candidates repeatedly and had a
   liveness race that could leave a garbage cycle unreclaimed for ever
   (Paz, Bacon et al.); even the repaired collector trailed backup tracing by
   5–10 %.
3. Its successors kept deferred or coalesced counting but found cycles by
   tracing (Levanoni–Petrank, Ulterior RC, RC Immix, LXR); only the
   synchronous Bacon–Rajan algorithm reached production (PHP, Firefox, Nim).
4. Here: most of its cost bought concurrent counting, which per-thread
   non-atomic counting avoids; what applies is trial deletion's walk over the
   live data reachable from candidates (Composer disabled PHP's collector for
   it).

### What the Recycler measured ([PLDI'01](https://dl.acm.org/doi/10.1145/378795.378819); [mirror](https://sites.cs.ucsb.edu/~ckrintz/racelab/gc/papers/bacon-concurrent.pdf))

- A 24-way 450 MHz RS/6000 with one more CPU than mutator threads (§7).
  Longest pause 2.6 ms, end-to-end time "usually within 5%" (abstract); the
  [ECOOP abstract](https://link.springer.com/chapter/10.1007/3-540-45337-7_12)
  says 6 ms.
- Under limited CPU or memory "about 90%" of mark-sweep's speed; on one CPU
  jess 166 s against 108 s, javac 249 s against 127 s (Table 6).
- The collector spent "far more time performing collection" than mark-sweep
  (§7.4): javac 104.1 s against 2.8 s, specjbb 136.7 s against 4.7 s
  (Table 3). On javac over half the collector's time went to Mark/Scan for
  under 4,000 cycles collected (§7.3, §7.6).
- Mutation-buffer high-water mark 128 KB–4.8 MB, and 43 MB for mpegaudio at
  about 60 mutations an object (Table 4, §7.5); one extra header word an
  object (§5); a free-list allocator "less than ideal" (§5.1); an atomic
  exchange on every heap pointer store (§8); one collector CPU per about three
  mutator CPUs, "their collector is scalable while ours is not" (§2, §8);
  mutators block when memory or buffers run out (§1).

### Problems reported later

- Paz, Bacon, Kolodner, Petrank, Rajan ([CC'05](https://csaws.cs.technion.ac.il/~erez/Papers/CycleCollection.pdf),
  [TOPLAS'07](https://dl.acm.org/doi/10.1145/1255450.1255453)): the original
  concurrent collector "makes many repeated scans over the candidates", and
  "liveness cannot be guaranteed. A rare race condition may prevent an
  unreachable cyclic structure from being ever reclaimed" (§1.2) — a leak,
  not a safety bug. Their improved collector still "falls behind a backup
  tracing collector by 5-10%", matching it only once young objects were
  traced (the age-oriented collector, §1.4).
- [Levanoni–Petrank](https://csaws.cs.technion.ac.il/~erez/Papers/refcount.pdf)
  (§1.8.1): the Recycler's barrier "still contains a compare-and-swap for each
  reference slot update"; sliding views with coalescing remove all barrier
  synchronisation, and cycles go to a backup on-the-fly mark-sweep.

### What replaced or built on it

- [Ulterior RC (2003)](https://users.cecs.anu.edu.au/~steveb/pubs/papers/urc-oopsla-2003.pdf):
  pure RC 12.7 s on jack against 7.2 s for generational mark-sweep, "due to
  the pointer tracking costs in RC" (Table 1); trial deletion struggles with
  large cycles such as javac (§5); young objects copied, only old ones counted.
- [Down for the Count (2012)](https://users.cecs.anu.edu.au/~steveb/pubs/papers/rc-ismm-2012.pdf):
  "not aware of any high performance system that relies on reference
  counting"; standard RC about 30 % slower than mark-sweep; backup tracing
  "performs substantially better than trial deletion and has more predictable
  performance" (§2.3); immediate RC is popular in PHP, Perl and Python for its
  minimal runtime support; cyclic garbage averages 16 % of objects, up to 73 %
  in hsqldb (Table 7).
- [RC Immix (2013)](https://users.cecs.anu.edu.au/~steveb/pubs/papers/rcix-oopsla-2013.pdf):
  the free-list heap is "the principal source" of the remaining ~10 % gap
  (+26 % L1 data-cache misses, +7 % instructions, Table 1), closed by
  line/block allocation, copying and backup tracing.
- [LXR (2022)](https://users.cecs.anu.edu.au/~steveb/pubs/papers/lxr-pldi-2022.pdf):
  coalescing RC in brief stop-the-world pauses plus concurrent SATB tracing
  for cycles; a 1.6 % write barrier; 4 % faster than G1 and 7.8× Shenandoah's
  throughput on Lucene. In this lineage cycles go to tracing, not trial
  deletion.

### Production

No production system shipped the concurrent Recycler; the synchronous
Bacon–Rajan algorithm did:
- PHP since 5.3 ([zend_gc.c](https://github.com/php/php-src/blob/master/Zend/zend_gc.c)
  cites the paper; the green/red/orange colours are unused); a 10,000-root
  buffer ([manual](https://www.php.net/manual/en/features.gc.collecting-cycles.php));
  the manual's benchmark about 7 % slower and 931 MB → 10 MB
  ([performance](https://www.php.net/manual/en/features.gc.performance-considerations.php));
  Composer [disabled the GC](https://github.com/composer/composer/commit/ac676f4)
  because it kept rescanning live objects; PHP 7.3 added an adaptive threshold
  ([PR #3165](https://github.com/php/php-src/pull/3165), 12.75 s → 2.32 s).
- Firefox's [XPCOM cycle collector](https://github.com/mozilla-firefox/firefox/blob/main/xpcom/base/nsCycleCollector.cpp):
  "not using the concurrent or acyclic cases"; incremental, with a
  Levanoni–Petrank trick.
- Nim [ORC](https://nim-lang.org/blog/2020/12/08/introducing-orc.html): trial
  deletion; the `acyclic` annotation "can be crucial".
- Others: CPython subtracts internal references over a whole generation
  ([InternalDocs](https://github.com/python/cpython/blob/main/InternalDocs/garbage_collector.md));
  Swift and Koka have no cycle collector ([Perceus §7](https://www.microsoft.com/en-us/research/wp-content/uploads/2020/11/perceus-tr-v1.pdf));
  Perl uses `weaken` ([perlref](https://perldoc.perl.org/perlref)); Lobster
  reports cycles at exit ([docs](https://aardappel.github.io/lobster/memory_management.html));
  Pony uses ORCA ([tutorial](https://tutorial.ponylang.org/appendices/garbage-collection.html));
  OpenJ9 has no RC policy, Metronome is tracing ([docs](https://eclipse.dev/openj9/docs/gc/));
  [Biased RC](https://iacoma.cs.uiuc.edu/iacoma-papers/pact18.pdf) cut Swift
  client execution time by 22.5 %.
- No statement by Bacon or his co-authors on why they left the Recycler was
  found; his [2007 ACM Queue article](https://queue.acm.org/detail.cfm?id=1217268)
  was not reachable. The agent's inference: Metronome
  ([POPL'03](https://dl.acm.org/doi/10.1145/604131.604155)) guarantees
  utilisation from bounded live memory and allocation rate, which the
  Recycler — a spare CPU, memory headroom, blocking on exhaustion, unbounded
  cycle work — could not.

### What applies here (inference, not sourced)

- Most of the Recycler's cost bought concurrent counting: a CAS on every
  store, mutation buffers, a non-scalable collector thread, a spare CPU.
  Per-thread non-atomic immediate counting avoids it.
- The agent concluded that deferred or coalesced RC is "largely unusable
  here" because COW needs exact counts and destructors need RC. Half of that
  is wrong for this project: destructor timing is no constraint
  (`rfc/model/gc/cycle/questions.md` Y2, ruled by Edmond); COW does need exact
  counts, which keeps COW kinds immediate (`rfc/model/values.md`) but leaves
  objects free to defer.
- What does apply: trial deletion traces the live data reachable from
  candidates (the PLDI javac result and Composer's complaint are the same
  pathology; mitigations are an acyclic filter, purging, an adaptive
  threshold, perhaps tracing the young); the root buffer's memory; unbounded
  synchronous pauses; the free-list allocator's locality cost (RC Immix).

## 2026-10-04 — immediate RC plus a backup tracing collector for cycles, against trial deletion over a root buffer

Asked by Edmond: the papers he read suggest tracing finds cycles more cheaply
than trial deletion over a root buffer; is a hybrid — immediate counting plus
a tracing collector for cycles, with no root buffer — worth building as an arm
to compare? A researcher read the full texts below (nothing run here). The
entries above on the Recycler and on immediate RC with a concurrent cycle
collector are not repeated.

### Bottom line

Worth trying, **as an arm beside the default build, not a replacement**, in its
cheapest form first: a per-thread stop-the-owner mark–sweep triggered by heap
growth, no root buffer, no registration on a decrement, its roots taken from
the counts (CPython's rule: a count above the in-heap references is held from
outside). Every like-for-like comparison found puts backup tracing ahead of
trial deletion (Frampton: trial deletion 1.41–1.94× tracing's time per
collection, on every benchmark). The main risk is the pause: it grows with the
live heap (Nim's old mark-and-sweep, 46–205 ms worst latency on a 135 MB live
heap, against 1.1–6.2 ms for ORC), so a stop-the-owner trace may beat today's
longest owner pause but cannot meet the 5 ms gate of
`dev/design/recycler-over-counts.md`; that needs concurrent marking, which
brings a hook on decrements back, though only while marking runs. The second
risk is memory: "the choice of heuristics dominated results, rather than the
algorithm" (Frampton).

### What the literature measured

- **Frampton's thesis, ch. 4** (Jikes RVM, stop-the-world, a collection per
  8 MB allocated): trial deletion costs 1.69× backup tracing (geometric mean;
  1.43× the nodes visited, 1.19× the cost a visit), "around 70% worse" across
  triggers from 128 KB to 128 MB; backup tracing ≈ 8 ns a visit. A concurrent
  SATB "cycle tracing" is 0.83× backup tracing, skipping acyclic types,
  sweeping only candidates and re-checking only objects whose count fell to
  non-zero during the trace; trial deletion's candidate set "requires that
  this set be continually maintained", a measurable cost. Counterpoint: trial
  deletion won mutator time on jess.
  <https://users.cecs.anu.edu.au/~steveb/pubs/theses/frampton-2010.pdf>
- **Down for the Count? (ISMM'12)** calls backup tracing "substantially better
  than trial deletion", citing Frampton, with no measurement of its own.
  <https://users.cecs.anu.edu.au/~steveb/pubs/papers/rc-ismm-2012.pdf>
- **RC Immix (OOPSLA'13)**: backup tracing needs the stack and register roots,
  hence stack maps — "naïve reference counting implementations usually do not
  perform cycle collection" for that reason.
  <https://users.cecs.anu.edu.au/~steveb/pubs/papers/rcix-oopsla-2013.pdf>
- **LXR (PLDI'22)**: an SATB trace on the counting write barrier finds cycles
  "with no additional mutator overhead" (barrier 1.6 %), triggered by free
  blocks or predicted wastage; "RC may never delete an unmarked object while an
  SATB trace is underway". Worst case: a live singly-linked list.
  <https://users.cecs.anu.edu.au/~steveb/pubs/papers/lxr-pldi-2022.pdf>
- **Fast Conservative GC (OOPSLA'14)**: a conservative stack and register scan
  falsely retains under 0.01 % of objects; conservative RC Immix is within
  2–3 % of exact. <https://dl.acm.org/doi/abs/10.1145/2660193.2660198>
- **Nim's old `refc`** ("Refcounting + Mark&Sweep... Been there, done that,
  didn't work."): deferred counting, a conservative stack scan, a cycle pass
  marking from stack and globals at a threshold of 4 MB then twice the
  occupied memory. ORC (trial deletion) replaced it; against mark-and-sweep:
  worst latency 1.10 vs 46.4 ms, memory 137 vs 333 MiB, throughput 35.0k vs
  39.6k requests/s — mark-and-sweep faster.
  <https://raw.githubusercontent.com/nim-lang/Nim/devel/lib/system/gc.nim>,
  <https://nim-lang.org/blog/2020/12/08/introducing-orc.html>
- **CPython** scans no stack: the working count is the refcount less the
  references from inside the set, and what stays above zero is held from
  outside; containers are tracked at allocation, not at decrement; a full
  collection runs when pending long-lived objects exceed 25 % of them; PEP 442
  runs finalizers, then detects again for resurrection.
  <https://github.com/python/cpython/blob/main/InternalDocs/garbage_collector.md>
- **PHP** runs destructors, sets `IS_OBJ_DESTRUCTOR_CALLED` and collects once
  more. <https://github.com/php/php-src/blob/master/Zend/zend_gc.c>
- **Oilpan** moved Blink from counting to tracing over cycle leaks and
  use-after-free; precise heap, conservative native stack.
  <https://v8.dev/blog/high-performance-cpp-gc>
- **Rust**: `dumpster` is trial deletion over a candidate set filled at drop
  (<https://claytonwramsey.com/blog/dumpster>); `bacon-rajan-cc` collects
  synchronously.
- **2024–26, abstracts only**: Kim et al., partial tracing for C++/Rust without
  stack maps, counts identifying the roots
  (<https://jhyeon.kim/papers/pldi26.pdf>); Arborescent GC (ISMM'25,
  <https://dl.acm.org/doi/10.1145/3735950.3735953>); Verona's SCC-based
  counting (ISMM'24, <https://dl.acm.org/doi/10.1145/3652024.3665507>).
- Not verified in a primary source: the Unified Theory paper read in abstract
  only (<https://doi.org/10.1145/1028976.1028982>); Swift, Objective-C and Perl
  having no cycle collector, from memory.

### What decides the winner

Trial deletion walks each candidate's reachable subgraph two or three times a
round, live parts included, and the same live structure comes back with every
new candidate: its work is the sum of those subgraphs, unbounded by the heap.
Tracing visits each live object once a collection plus a sweep. Tracing loses
where the live heap dwarfs the cyclic garbage and the candidates' subgraphs
stay small (Nim's JSON server), where the pause must stay short without
concurrent marking, where concurrent marking needs a barrier, and where the
roots cannot be found.

### Fit here

- Destructors and copy-on-write are untouched: immediate counting still frees
  acyclic garbage at once; the trace takes only what counts cannot.
- With a growth-triggered stop-the-owner trace the decrement loses its
  registration (S67.5 counted 1.60M registrations in one `web-heap` run; the
  saving is unmeasured).
- Per-thread heaps, no reference crossing threads: a per-thread trace with no
  barrier.
- Roots without stack maps: (a) from the counts, CPython's rule — sound only at
  safepoints where no ARC-elided borrow is an entity's sole reference, the
  compiler's to confirm, at one extra pass over every object; (b) a
  conservative scan of the owner's stack and registers plus an explicit list
  of Rust-side holders, which needs an "is this an allocated object start"
  lookup; (c) request boundaries, where the stack is empty.
- A concurrent stage 2: re-check objects whose count fell to non-zero during
  marking (Frampton, for deferred counting; that it holds with counted locals
  is the researcher's inference, unverified), LXR's rule for an object whose
  count reaches zero during marking (the withheld returns may cover it), new
  objects live.
- Cyclic garbage's destructors as PHP and CPython: called in sweep order, the
  "called" flag set, the objects kept to the next trace and freed then if still
  unmarked — which makes resurrection safe.

### Numbers, all estimates

Our own figures point the same way, by inference: on `web-heap` the collector
spends 46.7–49.9 s of CPU a 116 s run against the mutators' ≈ 82 s (S68.1,
`dev/BENCHMARKS.md`), ≈ 230 whole walks of `web-heap-150k`'s state (1.53M rows,
202 ms a walk). The 400k garbage ring: marking does not touch garbage; ≈ 3 ms a
pass at 8 ns a visit, freeing ≈ 15 ms (the measured acyclic cascade) — an owner
pause of ≈ 20–30 ms against 62–67 ms on one thread and 48–51 ms split today.
`web-heap-150k`: 25–70 ms a trace, ≈ 0.03–0.14 of a core at one or two traces a
second, against ≈ 0.4 today. Caveat: part of any win is representation, not
algorithm — our trial deletion costs ≈ 155 ns a ring member against ≈ 8 ns a
mark visit with a header bit.

### The minimal experiment it proposes

A feature arm `trace-backup`: registration out of the decrement path; a
per-thread trigger on held bytes (twice what the last trace left, 4 MB floor);
at the owner's safepoint, in-heap reference counts, a mark from what is held
from outside, the destructor phase, a sweep; in debug builds a check of its
verdicts against Bacon–Rajan run beside it. Measure: registration's price alone
(on and off, collector off, `web-heap` mutator CPU); the 400k-ring probe;
`web-heap` and `web-arena` (mutator plus trace CPU, the longest owner pause,
mean and peak held garbage, RSS); two loads where tracing should lose (a large
live heap with rare cycles, a long live list). Gate against the default build:
total CPU lower, held garbage no worse than its 71–78 MB mean, the longest pause
under its 67–160 ms; a stage 2 with concurrent marking against the 5 ms gate.

## 2026-10-05 — proving a group of objects out of cycle collection at runtime

Asked by Edmond: "an algorithm, probably from Microsoft, that proves
acyclicity at runtime". Read by a research agent; what it verified by reading
a source is marked, the rest is its inference or memory.

**The answer, most likely** (verified, full text read): Parkinson, Clebsch
(Microsoft Azure Research, Project Verona) and Wrigstad, *Reference Counting
Deeply Immutable Data Structures with Cycles: an Intellectual Abstract*,
ISMM 2024, doi:10.1145/3652024.3665507. At `freeze`, one DFS computes the
frozen graph's strongly connected components (Purdom's path-based algorithm
with union-find, O(|E|·α)); each component's representative holds one count of
references from outside it; the condensation is a DAG, so counting at the
component's grain reclaims cycles with no tracing. Costs: a status field a
object (verona-rt tags the header's low bits: `SCC_PTR`, `RC`, `PENDING`,
`NONATOMIC_RC`), a `find()` on every retain and release, a freeze at about
twice a mark-and-sweep of the same graph. Holds only for deeply immutable
graphs: no write after the freeze. It confines cycles to known units rather
than proving acyclicity.

**Related, same group:**
- *Dynamic Region Ownership for Concurrency Safety*, PLDI 2025,
  doi:10.1145/3729313 (verified for the mechanism): regions with one owning
  entry, a barrier on every store; "cycles in the mutable heap cannot cross
  region boundaries", so each region gets its own cycle detector. No
  performance figures published.
- verona-rt `RegionRc` (source fetched): region-local RC with Lins' lazy
  mark-scan, traces stop at sub-region entries.
- *Dynamically Checked Deep Immutability in Python*, PLDI 2026,
  doi:10.1145/3808352, and PEP 795: not read (no access).

**Elsewhere:** Bacon & Rajan, ECOOP 2001 — statically acyclic ("green")
classes never buffered (secondary sources only). php-src: `GC_NOT_COLLECTABLE`
for strings, resources (#17194), enums and static fake closures (PHP 8.5,
#19866); draft #17130 flags objects whose typed properties can only hold
acyclic scalars (verified on the PR pages). CPython lazily untracks tuples of
untracked members during a collection (verified, devguide). Armstrong &
Virding, IWMM 1995: without destructive update every pointer points to older
data, so the heap is acyclic (abstract). Perceus and Lean's counting do not
handle cycles at all (abstracts).

**What may apply here** (inference, none measured):
1. A class-level "acyclic" bit at allocation (Bacon–Rajan green, php-src
   #17130): never a candidate, a leaf in every trace; cleared by a dynamic
   property. First figure to read: the share of candidates whose class
   qualifies.
2. A depth-1 "holds no collectable value" bit on mutable containers, set
   lazily by a trace that finds no collectable child (CPython's untracking),
   cleared by the slot-write path that already writes the window tag.
3. Freeze and component counting (the ISMM paper) for data built once per
   worker: removed from cycle collection for good.
4. Request regions (Pyrona): traces stop at the region's entry, and a request
   still being built is not traced from outside — the measured waste of the
   Δ-test (72 % of the touched sets are live requests being built). Costs an
   owner word and a barrier on every store.
5. The collector's trace could compute components on the fly; a component
   whose members carry no window tag across epochs keeps its stamp. No paper
   found for this on a mutable heap.
