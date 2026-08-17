# selective-rc-walk: omit counts when an anchor proves lifetime

**Status:** design proposal; not implemented
**Recorded:** 2026-08-17
**Base:** `rfc/model/gc/rc-walk.md`

## Goal

Keep rc-walk's root equation and collector protocol, but stop maintaining a
reference count for objects whose lifetime is already proved by another live
object or region. The mutator pays according to the **target's** policy:

```text
store(_, counted target)   -> retain/release
store(_, anchored target)  -> no lifetime operation
```

There is no `gc.active` test and no tracing write barrier. Collection remains
rc-walk; the optimization changes which nodes participate in its count
equation.

## Two GC-heap policies

`GcHeap` remains the allocation category. Lifetime policy is a separate,
immutable property because category answers where memory lives, while policy
answers how reachability is proved.

```text
Counted   — ordinary RC; may be an external root.
Anchored  — no lifetime RC; may be live only through a Counted, Arena,
            LongLived, or Immortal anchor.
```

A `NO_LIFETIME_RC` header flag distinguishes Anchored objects. In an rc-walk
build it takes bit 24, currently free since collector condemnation became
private; the proposal does not consume the epoch byte or the contested
rc-trace candidate-index layout. It is fixed before publication. COW objects
are always Counted because their count also answers uniqueness.
`RequestArena`, `LongLived`, and `Immortal` retain their existing category
semantics; they can act as anchors but are not renamed into lifetime policies.

The existing header word still supplies heap occupancy. A published Anchored
object keeps the low word at the non-zero sentinel `1`; retain and release
never change it. Its death path writes zero before returning the slot. The
collector must test occupancy first and policy second; it must never interpret
the sentinel as a real reference count.

## The anchor invariant

For every live Anchored object `t`, every external strong reference to `t`
also keeps at least one non-Anchored anchor `a` live, and `a` reaches `t`
through the traced strong graph.

Equivalently:

```text
external_root(t) => exists anchor a:
    external_root(a) and reaches(a, t)
```

An anchor may be:

- a Counted GC entity;
- the mounted request arena;
- a LongLived region root;
- an Immortal root.

An Anchored object may point to another Anchored object, but its anchoring path
is **sealed at publication**. After the object can cross a checkpoint, no
store may attach that existing object to a new anchor or replace its last
anchoring path while an external reference survives. Sharing is allowed only
when all anchors and paths were established before publication and the
compiler keeps at least one of them live with every external reference.

New Anchored objects may be initialized and attached because they are
allocate-black for an epoch already in flight. Re-attaching an old Anchored
object requires a fresh Anchored copy under the new anchor, or a Counted
representation chosen before publication.

The anchor and sealing invariants together are why removing RC needs no mutator
GC barrier. The collector does not need to discover external roots of Anchored
nodes: their anchors are already roots, the anchoring path cannot appear behind
an already-scanned source, and tracing the anchor discovers the nodes.

## Collector algorithm

Let `C` be Counted nodes and `T` Anchored nodes in the walked population. The
collector snapshots every node and every strong edge exactly as rc-walk does.
For each `c in C` it computes:

```text
external(c) = RC(c) - IN(c)
```

`IN(c)` counts all snapshotted incoming edges whose target is `c`, including
edges from both `C` and `T`. An edge targeting `T` contributes to no count
equation.

Initial mark roots are:

```text
{ c in C | external(c) > 0 }
+ roots supplied by Arena, LongLived, and Immortal populations
```

The second line is new work for the collector. Current rc-walk can treat an
edge from an unwalked population as a root because it contributes to the
target's RC; an Anchored target has no such count. The collector must therefore
enumerate registered anchor objects/regions and trace their outgoing graph.
This is collector work, not a mutator write barrier. The same sealing rule
applies: after an anchor has been scanned, it may attach only fresh
allocate-black Anchored storage, never an old Anchored object.

Marking then follows all strong edges without regard to policy:

```text
mark(C -> C)
mark(C -> T)
mark(T -> C)
mark(T -> T)
```

Every unmarked `C` or `T` node is a candidate. The existing rc-walk snapshot
recheck, component confirmation, corpse rule, weak severing, destructor and
resurrection protocol apply to the mixed component.

## Why the root equation remains valid

For a Counted node, the mutator maintains one count for every incoming strong
edge, including an edge from an Anchored holder. Therefore subtracting the
walked incoming edges leaves exactly references originating outside the walked
population, as in rc-walk today.

Anchored nodes have no count equation. By the anchor invariant, a live
external reference to an Anchored node implies a marked anchor, and the trace
from that anchor reaches the node. Therefore an unmarked Anchored node has no
live external reference. It is safe to include it in a condemned component.

The sealing rule closes the concurrent insertion race. Without it, the
mutator could attach an old Anchored `t` to a live Counted `a` after the
collector had scanned `a`; no target count or write barrier would report that
new edge, and `t` could be freed live. A deletion of an anchoring edge can only
retain stale garbage, but an insertion can kill safety, so the compiler must
reject or copy on that operation.

Uncertain snapshots keep rc-walk's conservative direction: any changed
Counted count, edge, layout, or storage version acquits the whole affected
component. The sentinel of an Anchored node is checked only for occupancy, not
for liveness.

## Mutator operation matrix

The target determines the lifetime operation; the owner policy is irrelevant
except for arena boundary rules already present today.

| Old/new target | On publication | On removal |
|---|---|---|
| Counted entity | retain | release |
| Anchored entity | nothing | nothing |
| COW value | retain | release |
| Immortal | nothing | nothing |

The Anchored row applies only to initialization with a fresh target and to
copies of references whose existing anchor remains live. Publishing an old
Anchored target beneath a new anchor is not a free store: it follows the
copy-or-Counted escape rule below.

For example:

```text
Counted  -> Anchored : zero count traffic
Anchored -> Anchored : zero count traffic
Anchored -> Counted  : retain/release the Counted target
Counted  -> Counted  : current path
```

The hot test can share the flags/category load the store barrier already
performs:

```text
if target.flags has NO_LIFETIME_RC:
    skip lifetime count
else:
    retain_or_release(target)
```

Compiler-known policy removes even that branch. No operation depends on
whether a GC epoch is active.

## How an object qualifies

The compiler may allocate an object as Anchored only when it can name the
anchor and prove all of the following:

1. Every escape of the object carries or retains that anchor.
2. The anchor reaches the object through a traced strong slot before the
   object can cross a checkpoint.
3. A raw pointer, FFI handle, weak upgrade, static, closure, coroutine frame,
   or VM register cannot retain the object after dropping the anchor.
4. All layouts on the path from the anchor are visible to `trace_entity`.
5. The object is not COW and does not require last-release destruction.
6. After publication, no operation can attach the existing object below a new
   anchor or remove its last anchoring path while an external reference lives.

The simplest qualifying cases are compiler-created helper objects dominated
by a Counted owner, request-local subgraphs dominated by the mounted arena,
and immutable implementation nodes whose handles carry their public owner.

If proof is unavailable, allocation is Counted. This is a local missed
optimization, not a correctness fallback executed during collection.

## Escape and promotion

Policy and anchoring topology never change in place after publication.
Converting an Anchored object
to Counted would require reconstructing its exact incoming count while the
graph is changing; merely writing `refcount = 1` is unsound.

When an Anchored object escapes without its anchor, the store path must copy or
promote the reachable value into a newly allocated Counted representation and
publish that copy. This is analogous to arena escape copying. Alternatively,
the compiler can choose Counted at the original allocation when such an escape
is possible.

Likewise, publishing a pre-existing Anchored object beneath a different anchor
copies it as fresh Anchored storage or uses a Counted copy. A plain pointer
store of the old object is not an allowed zero-cost operation.

Moving an arena survivor to `GcHeap` is not enough by itself: the promotion
must select Counted unless it also transports a valid non-arena anchor proof.

## Death, destructors, and weak references

- Counted objects keep current last-release death and deterministic destructor
  behaviour.
- Anchored objects never die on a release; rc-walk reclaims them in a mixed
  condemned component.
- An Anchored object requiring user destruction runs it in the collector
  drain, with rc-walk resurrection rules. Code requiring last-release timing
  is therefore ineligible for Anchored policy.
- Weak references do not prove an anchor. Upgrading a weak reference to an
  Anchored object must return a pair/handle that retains its anchor, or produce
  a Counted copy. Returning the naked object violates the invariant.
- When an Anchored holder dies, its Counted children are released during the
  drain. Its Anchored children require no action.

## Failure examples the verifier must reject

### Naked stack escape

```text
root = anchored_child
drop(counted_owner)
```

The child now has an external root without an anchor. Its sentinel cannot tell
the collector this, so it could be freed live.

### In-place policy change

```text
anchored.flags -= NO_LIFETIME_RC
anchored.refcount = 1
```

There may already be several incoming edges. A later release can kill the
object early.

### Uncounted edge into a Counted target

```text
anchored_holder.child = counted_target  // retain omitted
```

`RC - IN` becomes negative or the target dies while the edge remains. Only the
target's policy decides whether counting is omitted.

### Naked FFI handle

An FFI table stores an Anchored pointer but not its anchor. Heap tracing cannot
derive that external root. The ABI must store an anchor-bearing handle or a
Counted copy.

### Re-anchoring behind the collector

```text
collector already scanned live_owner
live_owner.child = old_anchored
```

No count changes and no barrier reports the edge. The object may be condemned
from its former dead anchor and freed live. The compiler must copy it under
`live_owner` or use Counted policy.

## Interaction with current memory categories

The repository already avoids lifetime RC for `RequestArena`, `LongLived`, and
`Immortal`; selective-rc-walk generalizes that idea inside `GcHeap` without
overloading allocation category:

| Category | Default policy | Reclamation |
|---|---|---|
| `GcHeap` | Counted or proven Anchored | last release or rc-walk |
| `RequestArena` | region/escape accounting | reset/promotion |
| `LongLived` | uncounted region | region owner |
| `Immortal` | uncounted | process exit |

Keeping policy separate avoids consuming the four-value category field and
allows Counted and Anchored entities to occupy the same heap blocks.

## Cost model

The optimization saves two target-header writes for each replacement of one
Anchored target by another: no retain of the new target and no release of the
old. It also removes release cascades through Anchored-only subgraphs.

It does not make every graph operation free:

- stores targeting Counted or COW values still count;
- an Anchored holder storing a Counted target still counts the target;
- Anchored garbage floats until rc-walk runs;
- the collector still snapshots and traces Anchored nodes;
- anchor-preserving handles or escape copies can add cost elsewhere.

The useful metric is therefore the fraction of dynamic target publications
whose target is provably Anchored, not merely the fraction of allocations that
receive the flag.

## Implementation stages

1. Add `NO_LIFETIME_RC` and make `lifetime_counted` depend on policy as well as
   category; forbid it on COW.
2. Preserve occupancy with sentinel 1 and teach all collector phases not to
   treat that sentinel as RC.
3. Extend the private snapshot row with policy and compute `RC - IN` only for
   Counted nodes while counting every edge into a Counted target.
4. Collect mixed Counted/Anchored components through the existing Phase 3 and
   Phase 4 rechecks.
5. Add layout/compiler metadata naming the anchor; reject naked escapes and
   post-publication re-anchoring.
6. Implement counted-copy promotion for escapes whose anchor cannot travel.
7. Add weak, FFI, static, coroutine, arena-promotion, destructor, and COW
   conformance tests.
8. Benchmark the store matrix and end-to-end target-policy distribution before
   enabling compiler selection by default.

## Decision gate

Do not ship Anchored policy until all are true:

1. A machine-checkable rule establishes both the anchor and sealed-topology
   invariants at every escape and publication.
2. The collector model proves the mixed-node root equation and component
   confirmation, including concurrent uncertainty.
3. Every edge into a Counted target is audited to retain and release regardless
   of holder policy.
4. Occupancy, zero-count corpses, slot reuse, and the Anchored sentinel are
   distinguished in all heap enumerators.
5. Weak upgrades, FFI handles, stacks/registers, statics and coroutine frames
   cannot create naked Anchored roots.
6. Arena promotion either selects Counted or transports a valid anchor proof.
7. Benchmarks show that saved count traffic exceeds extra floating garbage,
   collector work, proof metadata, and escape-copy cost.
