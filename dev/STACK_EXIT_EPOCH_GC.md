# GC without mutator-side RC: stack-exit epoch protection

**Status:** model design; central safety invariant accepted, production protocol unproved
**Recorded:** 2026-08-18

## Decision in one sentence

A concurrent walk may miss a newly published edge `A -> B`, but it may not
reclaim `B` if the mutator used `B` locally during that walk; local use is
represented logically by `epoch(B) == 0`, and leaving the last local use stamps
`B` with the current acknowledged GC epoch.

This removes mutator-side reference-count maintenance. It does not remove
mutator/collector cooperation: a real implementation needs root or hazard
publication, two epoch handshakes, and delayed physical reclamation.

## Ownership-barrier refinement

The compiler need not independently protect every local reference. A local may
instead be an **anchored borrow** when the compiler proves all of the following:

```text
the anchor cannot be destroyed during the borrow
the strong path from the anchor to the target cannot be severed or redirected
the target does not escape beyond that proof scope
every operation that may invalidate the proof is visible in the IR
```

An anchored target may keep a non-zero epoch. Its liveness is derived:

```text
live(anchor) and stable_path(anchor, target) implies live(target)
```

Reading such a target is a plain pointer load:

```text
x = borrow a.child
```

It performs no RC update, epoch store, hazard publication, or GC read barrier.
The borrow is valid only inside the compiler-proved scope.

When a value must become independently held by generated code, the compiler
emits an **ownership barrier**:

```text
ownership_barrier(x):
    atomically establish x as LIVE and independently protected
    x.touch_epoch = 0
```

The barrier must precede the operation that destroys the last anchor or severs
the last stable path. For example:

```text
x = borrow a.child
ownership_barrier(x)
unset(a)
use(x)
```

If `x` has no uses after `unset(a)`, no barrier is needed. A heap publication
can conversely transfer an independent root into an anchor: publish the edge
first, then replace zero with the current acknowledged epoch. The target stays
protected for the concurrent walk that may have missed the new edge.

The required ordering is therefore:

```text
acquire independent ownership: ownership barrier before first unanchored use
release independent ownership: publish stable anchor before epoch stamp
destroy or mutate anchor: promote every surviving borrow before severing edge
```

The compiler IR should represent `owned`, `anchored(anchor, path)`, and
`unmanaged` references explicitly. Joins conservatively choose `owned` unless
the same live anchor and stable path dominate every incoming edge. Unknown
calls, reflection, by-reference aliases, FFI, and re-entrant destructors are
invalidation points unless summaries prove otherwise.

## Five-axis critical review

### 1. Proof soundness: liveness of the anchor is not enough

The proof needs both a live anchor and a stable retaining path. This is unsafe:

```text
x = borrow a.child
a.child = other
use(x)
```

`a` remains live but no longer retains `x`. Every write to any edge in the
anchor path, destruction of an intermediate node, and ownership-changing call
must end or promote the borrow. Derived chains (`a.b.c`) carry the whole path,
not merely the first object. A missed invalidation is a use-after-free, so
analysis failure must select `owned`, never guess `anchored`.

### 2. Language semantics: PHP aliases make proof scopes short

Dynamic properties, references, callbacks, virtual calls, exceptions,
reflection, and destructors can mutate or expose an anchor indirectly. The
first implementation must borrow only across closed, effect-summarized code
with known layouts. A call without a trusted summary ends all borrows whose
paths it may reach. This preserves correctness but may reduce the optimization
rate enough to erase its value; the compiler must report refusal reasons and
measured borrow-scope coverage.

### 3. Concurrency: the destruction barrier must arbitrate with retirement

`touch_epoch = 0` followed by edge removal is safe only if the collector cannot
win reclamation between those actions. Ownership acquisition and collector
retirement must update the same atomic lifecycle state, so either the barrier
linearizes first and the retirement CAS fails, or the barrier observes
`RETIRING` and retries from a still-live anchor. Promotion is published before
the severing store; release into an anchor publishes the edge before replacing
zero with an epoch. A final handshake or grace period remains mandatory before
physical reuse.

### 4. Representation: one zero cannot count several independent owners

Zero expresses a logical predicate, not the number of roots. If two threads or
two independently ending scopes hold `x`, one release must not replace zero
with an epoch while the other still owns it. The implementation therefore
needs unique ownership, per-thread root slots, an owner bitmap/count, or a
compiler proof that releases coalesce at one last-owner point. Anchored borrows
need no entry, but every independently held value must participate in this
aggregation. A single uncoordinated header byte is insufficient.

### 5. Economics and completeness: cold barriers may become wide barriers

The scheme trades frequent retain/release operations for work at invalidation
points. Destroying one anchor may require promoting many live derived borrows;
exceptional exits and re-entrant destructors can make that path latency
sensitive. Compiler metadata, effect summaries, code size, epoch traffic, GC
bandwidth, and objects retained for an extra epoch all count against the saved
barriers. Benchmarks must measure eliminated RC pairs, ownership-barrier count
and fan-out, refusal causes, promoted bytes, peak retained memory, and p99
destruction latency. The design is justified only if the whole-program result
beats precise RC with Lean-style borrow inference and ownership transfer.

The five reviews leave a viable but narrower claim: an anchored borrow can make
field reads free, and the ownership barrier can concentrate protection work at
proof invalidation, provided the compiler proves stable paths and the runtime
linearizes promotion against retirement.

## Scope

The first model has one mutator, one collector, a non-moving managed heap,
atomic managed pointer slots, no raw or interior pointers, and no weak
references, finalizers, resurrection, or address reuse within a grace period.
Multiple mutators are a later extension and may not encode local ownership in
one object header word.

Two collector backends use the same epoch invariant:

- **Census baseline:** scan every object and reconstruct temporary heap
  indegrees. It establishes the new invariant with the smallest conceptual
  change, but does not collect cycles.
- **Recommended backend:** trace from roots and every target protected during
  the epoch. It collects cycles and avoids temporary counters.

## Logical object state

The model exposes one `touch_epoch` value:

```text
touch_epoch == 0   the object is currently held locally; never reclaim it
touch_epoch == G   the last local use ended during collector epoch G
touch_epoch < G    walk G started after the last protected publication
```

On entry to local state:

```text
touch(B):
    B.touch_epoch = 0
```

On departure from the last local state:

```text
untouch(B):
    B.touch_epoch = acknowledged_gc_epoch
```

The stamp is deliberately retained after the local disappears. Clearing it
would reopen the missed-edge race. A long-lived local remains zero across any
number of epochs. A new allocation starts protected and is outside the
reclamation set of its birth epoch.

`0` is a logical value, not the recommended multi-mutator representation. In
production, current locals live in per-thread root/hazard tables; the object
stamp records the latest completed local-use epoch.

## Central invariant

For every edge `A -> B` that walk `G` may fail to observe, at least one of the
following holds until walk `G` has passed its reclamation boundary:

```text
the walk observed A -> B
or B.touch_epoch == 0
or B.touch_epoch >= G
or B is present in a published root/hazard set
```

This is the whole safety argument for an inconsistent graph scan. Moving the
only edge from `X -> B` to `Y -> B` may make the census see neither edge, but
the move necessarily obtains `B` locally before publication, so `B` remains
protected for that epoch.

Compiler and runtime transformations may not copy a managed pointer from one
slot to another without executing the equivalent local-protection protocol.

## Epoch protocol

### Start handshake

To start walk `G`:

1. The collector publishes `G`.
2. Every mutator reaches a handshake, publishes its current roots/hazards, and
   acknowledges `G`.
3. After acknowledgement, that mutator's `untouch` reads at least `G`; a
   cached older epoch is invalid.
4. Stores completed before acknowledgement happen-before the collector's
   scan. Only after every acknowledgement does the scan begin.

Thus a publication completed before the boundary is visible to walk `G`, and
a publication concurrent with the walk protects its target with zero, `G`, or
a hazard entry.

### End handshake

Before retirement, the collector requests a second handshake. It establishes
that every load/protect operation begun before the request has either
published its hazard or completed, and makes all current roots and hazards
visible. Work discovered by the handshake is drained before any candidate is
retired.

## Safe acquisition

The naive sequence is invalid:

```text
B = load(A.field)
B.touch_epoch = 0
```

The collector may free `B` between those operations. The minimum acquisition
protocol publishes protection outside the target before dereferencing it:

```text
retry:
    B = load_acquire(A.field)
    if B == null: return null
    publish_hazard_release(B)
    if load_acquire(A.field) != B: clear_hazard(); retry
    if load_acquire(B.state) != LIVE: clear_hazard(); retry
    return B
```

Dropping the last local publishes the current acknowledged epoch with release
semantics before clearing its hazard. Multiple local holders require separate
hazard/root entries or an aggregate count; one holder may not replace zero
with `G` while another still holds the object.

This is a local/read barrier. The design eliminates RC updates on every copy
and heap replacement, not all mutator-side GC work.

## Census baseline

Walk `G` takes a stable registry snapshot, initializes collector-private
temporary indegrees, and reads every recorded strong edge. Pointer loads are
atomic; object shape and out-of-line storage require their own coherent-read
contract.

The candidate test is:

```text
temporary_indegree(B) == 0
and B.touch_epoch != 0
and B.touch_epoch < G
and B is absent from every root/hazard set
```

This is only a candidate test. The collector must still win the retirement
protocol below.

A current heap field may differ from the field that contributed to the
temporary count. Consequently, the baseline does not cascade decrements by
reading current fields. It either retains newly exposed zero-indegree objects
until the next census or records and uses the exact adjacency observation
that produced the counts. The first option is the model baseline.

An unreachable cycle has positive internal indegree and survives forever.
The census backend therefore is not the production collector unless paired
with a cycle collector.

## Recommended tracing backend

The production candidate should use the same protection invariant with
concurrent tracing:

1. Complete the start handshake for `G` and seed the mark queue from roots.
2. Treat new allocations as black for `G`.
3. Trace strong edges from marked objects.
4. Treat every target touched or hazard-published during `G` as an additional
   root and enqueue it once.
5. Complete the end handshake and drain all root/hazard and mark queues to a
   fixpoint.
6. Retire unmarked pre-epoch objects through the protocol below.

This gathers cycles naturally. The price is a discovery mechanism for newly
protected targets: a per-thread buffer is preferred; scanning all epoch bytes
again is the slow correctness baseline. Termination is not established until
all mutator buffers have been acknowledged and drained.

## Retirement and physical reclamation

Every object has a lifecycle state independent of its logical touch epoch:

```text
LIVE -> RETIRING -> DEAD
```

For a candidate, the collector atomically changes the packed observed state
from `LIVE(old_epoch)` to `RETIRING(old_epoch)`. A concurrent touch must
arbitrate through the same modification order: it either protects the object
first and makes the collector CAS fail, or observes `RETIRING` and retries its
source load.

After winning the CAS, the collector:

1. waits for the end handshake or grace period;
2. checks every root/hazard table again;
3. cancels retirement if protection appeared;
4. clears registered weak cells when that feature is added;
5. runs the separately specified finalization protocol;
6. removes the registry identity, changes the state to `DEAD`, and only then
   releases the storage.

No address is reused until every scanner and hazard holder from the preceding
grace period is quiescent. Otherwise slot validation and state CAS admit ABA.
Generation-tagged handles remain an optional additional defence.

## Allocation and registry

Allocation during `G` creates `LIVE` storage protected for `G`. Registration
is append-only for the active registry snapshot, or the new object is placed
in a separate birth list that the next walk incorporates. A collector is the
only physical freer. Object bodies, class metadata, and out-of-line pointer
storage remain readable until the grace period ends.

## Memory ordering contract

- Managed pointer publication is a release store; the walk uses acquire
  loads. This orders initialization but does not force a concurrent store to
  be observed.
- Missed concurrent publication is made safe by target protection, not by the
  acquire load.
- Epoch publication and both handshake acknowledgements carry the ordering
  between pre-boundary stores and the scan.
- Touch and retirement update one atomic packed state, or an equivalent
  protocol must prove the same total arbitration.
- Epoch comparison uses a practically non-wrapping `u64`. A formal modular
  comparison requires the live distance to stay below half the number space;
  rollover otherwise requires a drained stop-the-world renumbering.

## Excluded semantics

Weak references require a guarded weak load and atomic clearing before
reclamation. Finalizers require `RETIRING -> FINALIZING -> RETIRED`, exactly-once
execution, and a decision about resurrection. Raw FFI pointers are roots only
if registered as handles. Deterministic destruction and COW uniqueness are no
longer supplied by RC and need separate language/runtime mechanisms.

None of these may be inferred from the core epoch invariant.

## Complexity and expected trade

The census costs `O(V + E)` time and `O(V)` temporary counters per full walk.
Tracing costs `O(V + E_live)` including sweep and collects cycles. Both move
work off heap stores but add safe-acquisition/root-publication work, two
handshakes, retained garbage for at least one epoch, and cache traffic from
the collector.

The design is worthwhile only if the measured local-protection cost is below
the RC retain/release cost it replaces. It must also be compared with the
mature-target insertion barrier in `dev/NO_RC_PUBLISHED_EPOCH_GC.md`: target
shading on stores is likely simpler than a hazard protocol on every managed
load, while stack-exit epochs may win when many stores reuse one already-local
target.

## Model obligations

A bounded model must include heap slots, mutator locals/hazards, object
identity and lifecycle, touch epochs, the global acknowledged epoch, scan
position, temporary counts or mark work, registry membership, and delayed
reuse. Ground truth is reachability from all mutator roots.

Safety properties:

```text
no reachable object becomes DEAD
no managed edge designates DEAD storage
every edge missed by G has a target protected through G
an address is not reused while an old identity is observable
```

Required counterexample scenarios include publication after source scan,
`X -> B` moved to `Y -> B`, epoch flip racing `untouch`, load racing
`LIVE -> RETIRING`, two simultaneous local holders, allocation at the registry
frontier, stale epoch cache, and address reuse. The census model additionally
includes changed adjacency during cascade and an unreachable cycle.

## Prior art and novelty boundary

No exact published match was found for the complete composition: no maintained
heap RC, local sentinel zero, a stamp on the last local departure, retention
through a later complete walk, and reconstructed heap indegrees. This is a
research-search result, not a patentability or freedom-to-operate opinion.

The closest tracing mechanism is the C4 Loaded Value Barrier. It guarantees
that a reference loaded by the mutator is safely marked through before it can
be used or propagated. That is almost the same central observation as this
design, but C4 immediately repairs marking state; this design retains the
target by epoch and lets a later complete walk validate the published edge.

- C4, *The C4 Garbage Collector*:
  <https://lag.net/papers/content/C4-garbage-collector.pdf>
- Related loaded-value-barrier patent US7647458B1:
  <https://patents.google.com/patent/US7647458>

The safe-acquisition sequence is the standard hazard-pointer shape, while the
grace period and delayed reuse are epoch-based reclamation mechanisms:

- Maged Michael, *Hazard Pointers*:
  <https://www.cs.otago.ac.nz/cosc440/readings/hazard-pointers.pdf>
- Keir Fraser, *Practical Lock-Freedom*:
  <https://www.cl.cam.ac.uk/techreports/UCAM-CL-TR-579.html>
- IBM ragged/confirmed epoch handshake, US20100100575A1:
  <https://patents.google.com/patent/US20100100575A1/en>

Dijkstra's incremental-update collector and Go's hybrid barrier close the same
stack-to-heap hiding problem at the write boundary rather than at local
acquisition/departure:

- Dijkstra et al., *On-the-fly Garbage Collection*:
  <https://www.cs.utexas.edu/~EWD/transcriptions/EWD06xx/EWD630.html>
- Go hybrid write barrier design:
  <https://gist.github.com/aclements/4b5e2758310032dbdb030d7648b5ab32>

CIRC is the nearest recent combination of RC and epochs, but it maintains RC
and updates object/link epochs on pointer writes rather than rebuilding a
whole-heap census:

- Jung, Kim, and Brown, *Concurrent Immediate Reference Counting*:
  <https://jhyeon.kim/papers/pldi24.pdf>

US20210141723 also stores a last-access GC epoch in an object header, but uses
it as a recency/soft-memory heuristic, not as a concurrent-reclamation safety
invariant: <https://patents.justia.com/patent/20210141723>.

The potentially distinguishing composition is therefore narrower than any
single mechanism: protect on local acquisition, retain zero while any local
holder exists, stamp on the last local departure, and validate only after a
later complete census, without a heap-store barrier or maintained RC. In
tracing terminology it is closest to a delayed epochal Baker/C4 read barrier;
in census terminology it is reconstructed deferred RC combined with epoch
reclamation. It must not be described as barrier-free.

## Acceptance gate

This remains a model design until all of the following hold:

1. An exhaustive model checks the safety properties under every bounded
   interleaving and retains defective variants as expected counterexamples.
2. Compiler lowering proves complete coverage of managed loads, slot-to-slot
   copies, bulk operations, statics, arenas, and FFI handles.
3. The start/end handshake and retirement CAS have a C/Rust memory-model
   argument and Loom models for their reduced protocols.
4. Weak references, finalization, resurrection, deterministic destruction,
   and COW uniqueness have explicit decisions or remain rejected features.
5. Benchmarks beat current RC and the published-target epoch barrier on
   throughput without unacceptable collector CPU, peak RSS, or tail latency.

Until then the accepted result is narrower: stack-exit epoch protection closes
the missed-new-edge counterexample, provided safe acquisition, handshake, and
delayed reclamation implement the logical invariant.
