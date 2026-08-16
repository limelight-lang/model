# GC without mutator-side RC: published-epoch barrier

**Status:** research idea, not a decided design
**Recorded:** 2026-08-16

## Idea

Remove reference-count maintenance from the mutator for ordinary heap
objects. The mutator allocates but never reclaims object memory; reclamation
belongs entirely to the collector. Consequently, a concurrent walk cannot
race slot reuse and does not need rc-walk's deferred-free/parked-list identity
protocol.

When a reference to `B` is published, the mutator records that publication in
`B`'s header. The original sketch was:

```text
store(slot, B)
B.published_epoch.store(current_epoch, Relaxed)
```

The field should be an atomic byte. A concurrent plain write/read is a Rust
data race even if the target hardware makes an aligned byte store indivisible.
`AtomicU8::store(Relaxed)` is expected to compile to an ordinary byte store,
without a locked RMW or fence.

The collector treats an object published during its current epoch as live and
traces it before reclamation. This is an object-level incremental-update
barrier. It is an alternative to marking the containing heap card dirty.

## Central optimization: barrier only for mature objects

The mutator does **not** need to mark every published object. An object
allocated during the current collection is allocate-black: it is live for this
epoch by construction and cannot belong to the pre-frontier garbage set.
Likewise, an old object already shaded in this epoch needs no second enqueue.

The effective barrier is therefore:

```text
store(slot, B)

if gc_active
   and B predates the current epoch
   and B is not marked in the current epoch:
       if CAS(B.mark_epoch, old, current_epoch) succeeds:
           enqueue(B)
```

Conceptually, only second-and-later-epoch objects pay the target-shading slow
path. The expected common cases are cheaper:

- GC inactive: no marking work;
- compiler-known fresh `B`: eliminate the barrier completely;
- current-epoch `B`: one age/mark check, no enqueue;
- already shaded mature `B`: one mark check, no enqueue;
- previously white mature `B`: one successful transition and one work-buffer
  entry, paid once per epoch.

This is stronger than merely deduplicating queue entries. It focuses mutator
and collector work on the only population the current epoch can reclaim. It
also fits ll-model's existing epoch-stamp and allocate-black experience.

The boundary must still be proved. A mutator can read the epoch, be delayed
across an epoch transition, allocate or publish, and then write a stale mark.
The design therefore needs one coherent definition of "predates the epoch":
an allocation-frontier snapshot, a generation/age field, or a mark epoch whose
transition is synchronized with allocation. A wrapping byte must only cause
conservative retention, never make a mature white object look newly allocated.

## Prior art: this is an advancing-wavefront barrier

The closest established family is the Dijkstra/Steele incremental-update
barrier. In tri-colour terms, publishing a reference to a white target shades
that target grey and puts it on the marking worklist. The idea is therefore not
a new GC category; the useful research question is whether ll-model's
single-mutator ownership, stable addresses, layouts, and spare header byte
permit an unusually cheap specialization.

V8's concurrent marker is particularly close to the proposed target-side
operation. Its concurrent fast path performs the reference store, then
atomically changes the target from white to grey and enqueues it. V8
deliberately omits the source-object colour check: retaining more objects is
safe, and avoiding that check avoids a required store-load fence. Merely
writing an epoch byte is not equivalent unless the collector also has a sound
way to discover and drain every newly shaded object before terminating.

Go's production hybrid barrier demonstrates the other major hazard. It shades
both the overwritten reference (Yuasa deletion barrier) and, while the current
stack is grey, the newly installed reference (Dijkstra insertion barrier).
This prevents moving the sole reference heap-to-stack or stack-to-heap from
hiding an object. The proposed ll-model barrier cannot be judged only on
heap-field insertion; its root/stack protocol and deletion behaviour must be
proved together.

WebKit Riptide is the closest source-side alternative. Instead of shading the
new target, it marks an already-visited source object grey and revisits it. It
uses object-granularity `cellState` plus versioned mark state rather than a
card table. WebKit reports approximately 5% mutator overhead while collection
is active and no detected overhead while inactive, but also documents the
store-before-barrier ordering problem, conditional fences, and a possible
"death spiral" when repeated revisits prevent completion and retain memory.

These implementations change the recommendation:

- Do not treat `published_epoch.store(E, Relaxed)` as a complete barrier.
- Prototype a real `shade(B)` operation: atomic white-to-grey transition plus
  insertion into a mutator-local marking buffer.
- Apply `shade(B)` only to the pre-epoch/mature population; current-epoch
  allocations are black and compiler-known fresh publications should have no
  barrier at all.
- Compare it with `revisit(A)` (object-level Steele/Riptide) and card marking.
- Design termination detection and root scanning as part of the barrier, not
  after it.
- Model the protocol before relying on an informal ordering argument. The
  history of the original on-the-fly collector includes a serious ordering
  error that was found only during a state-based proof.

Primary and implementation sources:

- Dijkstra et al., *On-the-fly Garbage Collection: an Exercise in
  Cooperation*: <https://www.microsoft.com/en-us/research/publication/fly-garbage-collection-exercise-cooperation/>
- V8, *Concurrent marking*: <https://v8.dev/blog/concurrent-marking>
- Go runtime hybrid barrier: <https://go.dev/src/runtime/mbarrier.go>
- WebKit, *Introducing Riptide*: <https://webkit.org/blog/7122/introducing-riptide-webkits-retreating-wavefront-concurrent-garbage-collector/>
- Vechev and Bacon, *Write Barrier Elision for Concurrent Garbage
  Collectors*: <https://www.sri.inf.ethz.ch/publications/vechev2004write>

## Expected benefit

The current counted heap publication measures about 2.74--2.82 ns and a plain
pointer store about 0.33 ns on the recorded benchmark machine. A hot
publication plus relaxed byte store might plausibly land around 0.6--1.3 ns,
but that is a hypothesis, not a result. A cold target header can make the
object barrier much more expensive.

Removing RC also removes individual retain/release cascades. Allocation can be
append-only on the mutator side and dead memory can be reclaimed in batches by
the collector.

## Proposed collector shape

1. Start epoch `E` and fix the allocation frontier.
2. Objects allocated after the frontier are young/live for this epoch.
3. Obtain a sound root view.
4. Concurrently trace the pre-frontier graph.
5. Drain the mutator publication buffer and trace every successfully shaded
   object to a fixpoint. A full scan for `published_epoch == E` remains a
   slower baseline, not the preferred design.
6. Enter a final confirmation protocol that closes the race between the last
   publication check and physical reclamation.
7. Process weak references and destructors, including resurrection.
8. Reclaim confirmed dead slots or whole blocks on the collector side.

The ordering shown in the illustrative barrier is not yet proven. V8 shows
that store-then-unconditional-shade can avoid a fence, but only together with
an atomic colour transition, worklist publication, and termination protocol.
Shade-then-store is conservative in a different direction. The final protocol
must make one ordering sound rather than relying on timing.

## Correctness questions to answer

### Publication race

- Which operation comes first: slot publication or epoch-byte publication?
- What prevents reclamation between those two operations?
- Is a release/acquire edge required at the final confirmation boundary, even
  if the hot byte store itself remains relaxed?
- Can an epoch transition race a mutator that read the old epoch number?
- Is the atomic operation an unconditional epoch store, or a white-to-grey
  compare/exchange whose winner must enqueue the object?
- How does termination prove that no successful shade remains buffered?
- Can an allocation racing the epoch boundary be classified as old by one
  participant and new by another?
- Does the age test use the object's header, its allocation block/frontier, or
  generation membership?

### Barrier coverage

- Must every heap-field, array-element, reference-box, static, stack, register,
  arena-to-heap, and FFI-handle publication execute the barrier?
- Can stack/register roots instead be covered by a handshake and exact stack
  maps?
- How are bulk copies, array moves, COW copies, and initialization stores
  represented?
- Which stores can be proven initialization-only or otherwise barrier-free?
- Can compiler analysis remove barriers using the published elimination
  conditions for incremental-update collectors?

### Transitive reachability

- When a published object is discovered, how is it enqueued exactly once?
- Does scanning all published bytes require a second whole-heap pass?
- Can publications continue faster than the collector reaches a fixpoint?
- How is ABA (`A -> B -> A`) handled if the epoch byte already contains `E`?

### Roots

- Without RC, `RC - IN` no longer derives external roots.
- Define exact stack/register maps, statics, arena roots, and FFI handles.
- Decide whether root publication uses a short handshake, a final stop, or a
  different snapshot mechanism.

### Reclamation boundary

- A final check followed by concurrent free is insufficient: the mutator can
  publish the object between check and free.
- Define either a short stop/handshake covering confirmation and reclamation,
  a condemned state observed by the barrier, or another ownership-transfer
  protocol.
- Ensure allocator reuse cannot begin until that protocol completes.

### Destructors, weak references, and resurrection

- Without RC, destructors are no longer triggered at the last release.
- Specify when and on which thread user destructors run.
- Null weak cells before user code.
- Re-trace after possible resurrection before reclaiming memory.
- Decide whether deterministic resource cleanup moves to explicit handles.

### COW and uniqueness

- RC currently answers whether a COW value is unique.
- Evaluate retaining local RC only for strings/arrays, using a uniqueness bit,
  compiler ownership analysis, or abandoning COW for selected values.

### Epoch-byte lifetime

- Define byte wraparound and the meaning of zero.
- Prove that stale and wrapped values only retain garbage, never free live
  objects.
- Define who clears or overwrites the byte and under what synchronization.

## Performance experiments

Benchmark the following under identical layouts and working sets:

1. Plain pointer store.
2. Current RC `heap -> heap` publish.
3. Pointer store plus `AtomicU8::store(Relaxed)` in the target header.
4. Pointer store plus card-table byte store based on the source slot.
5. Header and card variants with the mark already equal to the current epoch.
6. Header `compare_exchange(white, grey)` plus a thread-local work-buffer push.
7. Source-object `revisit(A)` with object state, in the Riptide/Steele shape.
8. Mature-only target shading versus unconditional target shading, including
   the fraction of publications eliminated as compiler-known fresh.

For each variant measure:

- hot L1, L2-sized, LLC-sized, and memory-sized working sets;
- sequential and random targets;
- repeated stores to one object/card and one store per object/card;
- target-header cache misses caused solely by the barrier;
- cache-line contention with a concurrent collector;
- x86-64 and AArch64;
- compiler-generated/inlined code, not only an ABI call.

End-to-end prototypes must additionally report:

- throughput and instructions per request;
- p50/p95/p99 mutator pause;
- collector CPU and memory bandwidth;
- peak RSS and unreclaimed bytes;
- bytes of collector metadata per object and edge;
- publication rate and number of extra objects retained per epoch;
- publication age distribution: fresh, current-epoch, mature-already-marked,
  and mature-newly-shaded;
- percentage of barriers statically elided for fresh values;
- time and number of passes required to reach a publication fixpoint;
- destructor and resurrection latency.

## Alternatives to compare

### Object epoch byte

Precise at object granularity, but writes the target object's header and may
cause random cache traffic. An epoch store alone still needs a discovery and
termination mechanism.

### Target shading (Dijkstra/V8 shape)

Atomically transition the newly referenced target from white to grey and let
the winner enqueue it. This is the closest proven family to the original idea
and is now the leading prototype candidate. The ll-model specialization should
shade only mature/pre-epoch targets; allocate-black objects are outside the
current reclamation set.

### Source revisit (Steele/Riptide shape)

After changing a field of an already-scanned source, turn the source grey and
enqueue it for a second scan. It has better locality than touching the target
header and can share machinery with a generational barrier, but requires care
with store-before-state-load ordering and collector progress.

### Card table

Marks the source region containing the changed slot. Usually has better
locality and naturally covers several nearby writes, but causes conservative
rescanning of all objects intersecting a dirty card.

### Short final stop

No hot-path barrier, but confirmation requires a stop-the-mutator root and
graph pass. This is the correctness baseline against which concurrent variants
should be measured.

### OS dirty-page tracking

Avoids an explicit software store barrier but works at coarse granularity and
may pay expensive page faults/protection transitions. Treat as an experiment,
not the default assumption.

## Decision gate

Do not replace rc-walk until a prototype demonstrates all of the following:

1. A proof or model for publication ordering, epoch transition, roots, and the
   final reclamation boundary.
2. Complete barrier/root coverage in compiler lowering and runtime code.
3. A measured hot-path advantage over both current RC and card marking.
4. Bounded collector metadata and unreclaimed memory.
5. Acceptable p99 pause and destructor behaviour on representative workloads.
6. Evidence that mature-target frequency is low enough for the proposed
   specialization to matter end to end.
