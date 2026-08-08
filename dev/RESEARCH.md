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
(arenas and extent trees), `rpmalloc`, and MMTk, which is a Rust
framework of collectors rather than an allocator and is the closer
comparison for the collector side.
