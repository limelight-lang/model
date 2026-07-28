# Architecture, visually

A visual companion to `dev/ARCHITECTURE.md`, which is the source of
truth: the knowledge table with the full "knows / does not know"
contract, the shared-resource ledger and the invariant list live
there. This file shows the same structure as diagrams — who knows
whom, who is responsible for what, and the key use cases as
sequences. When a boundary changes, both files change in the same
commit (`dev/WORKFLOW.md`).

Diagrams are PlantUML, embedded as fenced blocks; render on demand
(IDE plugin or any PlantUML processor). No generated images are
committed.

## Layers and who knows whom

Knowledge flows downward: a module may use anything at or below its
own layer. Exactly seven upward edges exist (dashed, red) — all of
them entity death or GC scheduling, each entered at a named point.
Anything new pointing up is a design event, not an edit.

```plantuml
@startuml
skinparam shadowing false
skinparam defaultTextAlignment center

rectangle "**L4 — collectors**\ngc · walk · epoch° · collector° · promote" as L4
rectangle "**L3 — object model**\nobject · class · reference · weak · intern" as L3
rectangle "**LB — mutation**\nmemory/barrier" as LB
rectangle "**L2 — memory manager**\ncontext · arena · heap · immortal · buffer\nbuffer_arena · reserve · stats · stdapi · deferred_free°" as L2
rectangle "**L1 — entity substrate**\nrefcount · value" as L1
rectangle "**L0 — block supply**\nblock_pool" as L0

L4 -down-> L3
L3 -down-> LB
LB -down-> L2
L2 -down-> L1
L1 -down-> L0

L1 .up.> L4 #red : ""refcount -> gc"" arm candidate (rc-trace)\n""refcount -> epoch"" death-branch checkpoint (rc-walk)
L2 .up.> L4 #red : ""context -> promote"" ll_arena_reset
L2 .up.> L3 #red : ""arena -> weak""\nreset drains the weak log
LB .up.> L3 #red : ""barrier -> object""\ndrop_ref cascade
L3 .up.> L4 #red : ""object -> gc""\nforget candidate at death
L3 -> L3 #red : ""class -> object""\ndispose default (data, not a call)

note right of L0
  ° = rc-walk feature only
  solid: knowledge flows down
  red dashed: the only calls up
end note
@enduml
```

### Full wiring

Every structural production edge between modules (ubiquitous hubs
omitted: everyone → `refcount`/`value`, context resolution, the
`stdapi` free funnel; block supply drawn once, package-level). Dense
by nature — use the layer picture above for orientation and this one
for lookup.

```plantuml
@startuml
skinparam componentStyle rectangle
skinparam shadowing false
skinparam linetype ortho
skinparam nodesep 30
skinparam ranksep 35

package "L4 - collectors" as P4 {
  [gc] as gc
  [walk] as walk
  [epoch] as epoch #E6E0F8
  [collector] as collector #E6E0F8
  [promote] as promote
}
package "L3 - object model" as P3 {
  [object] as object
  [class] as class
  [reference] as reference
  [weak] as weak
  [intern] as intern
}
package "LB - mutation" as PB {
  [memory/barrier] as barrier
}
package "L2 - memory manager" as P2 {
  [context] as context
  [arena] as arena
  [heap] as heap
  [immortal] as immortal
  [buffer] as buffer
  [buffer_arena] as buffer_arena
  [reserve] as reserve
  [stats] as stats
  [stdapi] as stdapi
  [deferred_free] as deferred #E6E0F8
}
package "L1 - entity substrate" as P1 {
  [refcount] as refcount
  [value] as value
}
package "L0 - block supply" as P0 {
  [block_pool] as pool
}

' structural production edges (hubs omitted: everyone -> refcount/value,
' context resolution, the stdapi free funnel, per-module pool supply)
collector --> epoch
collector --> walk
collector --> heap : snapshots
collector --> class
epoch --> walk
epoch --> deferred
gc --> walk
gc --> object
gc --> weak
gc --> barrier
gc --> reserve : refill at poll
promote --> arena
promote --> object
promote --> weak
walk --> heap
walk --> object
walk --> reference
walk --> weak
walk --> barrier
object --> class
object --> reference
object --> barrier
object --> heap
object --> immortal
object --> weak : notify
class --> intern
class --> immortal
intern --> immortal
reference --> object
reference --> barrier
weak --> object
weak --> heap
weak --> arena : weak log
barrier --> arena : logs
context --> arena
context --> heap
context --> immortal
buffer --> arena
buffer_arena --> buffer
heap --> reserve
arena --> reserve
stdapi --> heap
stdapi --> deferred
value --> refcount
P2 --> pool : get / put blocks

' vertical layer stacking
P4 -[hidden]down-> P3
P3 -[hidden]down-> PB
PB -[hidden]down-> P2
P2 -[hidden]down-> P1
P1 -[hidden]down-> P0

' the seven sanctioned upward edges
refcount .up.> gc #red : arm candidate
refcount .up.> epoch #red : death checkpoint
arena .up.> weak #red : reset drain
context .up.> promote #red : ll_arena_reset
object .up.> gc #red : forget candidate
barrier .up.> object #red : drop_ref cascade
class .up.> object #red : dispose default (data)
@enduml
```

Violet components exist only under the `rc-walk` cargo feature.
`block_pool` knows nothing above itself; its references to
heap/reserve are the shared test-lock harness.

## What each component is responsible for

One line each; the full contract (the "knows" column, shared
resources, invariants) is in `dev/ARCHITECTURE.md`.

| Module | Responsible for | Notably does NOT know |
|---|---|---|
| `block_pool` | 2 MB OS regions → aligned 64 KB blocks; global chain + thread caches; region registry | what any payload contains |
| `arena` *(hot)* | request arena: bump alloc; self-contained logs (destructor / escapee / release-at-reset); drain primitives for reset | the reset discipline itself — promote drives it |
| `heap` *(hot)* | small-object heap (mimalloc model), raw + entity populations, remote free, abandonment, slot enumeration | entity kinds, verdicts, classes |
| `immortal` | global bump region, never freed | contents of what it hosts |
| `buffer` | growable headerless payload over the mounted arena | entity lifecycle |
| `buffer_arena` | long-lived buffer blocks, per-block free lists, pressure modes | the object heap, entities |
| `reserve` | two blocks per thread so barrier log growth cannot fail | what a log records |
| `stats` | block-granular telemetry, zero hot-path tax | per-object events |
| `stdapi` | size-less malloc/free front door; routes by block kind | entity semantics |
| `deferred_free` *(rc-walk)* | activity flag; parked-free lists through bytes 8–15; post-epoch flush | which kinds park (stdapi filters), verdicts |
| `context` | `LLContext` + TLS current context; composition root; `ll_arena_reset` ABI | class layout, GC strategy |
| `barrier` *(hot)* | store-barrier micro-ops: publish (`store_ptr`/`store_box`), `drop_ref`, escape recording | per-site composition (lowering's) |
| `refcount` | the 8-byte header at offset 0: refcount + flag word; retain/release | entity bodies, blocks, when to collect |
| `value` | the 16-byte Box: payload + tag + flags | unboxed representations (compiler contract) |
| `intern` | interned names as immortal string entities; lookup table | classes — it only serves them names |
| `class` | descriptors: inline vtable train, itables, Cohen display, layout runs, trace lists | instance state, categories, GC |
| `object` | factory, constructed hook, three-phase death, kind-switched `ll_entity_die` | collector internals, block internals |
| `reference` | the `&` reference box, entity kind 3 | classes |
| `weak` | weak cell (kind 5) = canonical `WeakReference`; per-thread weak table; every notification rule | *when* to notify — the death sites' duty |
| `gc` | rc-trace cycle collector (Bacon–Rajan); arm-vs-fire; the `ll_gc_maybe_collect` poll | arming policy (compiler's) |
| `walk` | kind-dispatched tracer, census, whole-heap collection, Phase-4 drains | slots and occupancy (heap's side) |
| `epoch` *(rc-walk)* | mutator side: handshake ack, verdict queue, non-reentrant checkpoint | collector phases |
| `collector` *(rc-walk)* | collector side: steppable epoch state machine, Phases 1–3 | freeing (never frees), the weak table |
| `promote` | arena death with promotion: fixpoint, edge count, retain blocks, release log | copying/evacuation (future) |

## Use cases

### UC1 — Object allocation

```plantuml
@startuml
skinparam shadowing false
participant "generated code" as caller
participant object
participant arena
participant heap
participant immortal
participant epoch #E6E0F8

caller -> object : ll_object_new(ctx, class, category)
alt category = RequestArena
  object -> arena : bump alloc
else category = GcHeap
  object -> heap : entity_alloc
  note right : entity blocks only —\nnever ll_malloc's raw blocks;\nno GC test on this path
else category = Immortal
  object -> immortal : immortal_alloc
end
object -> object : zero body, stamp RcHeader\n(category, kind, refcount 1)
caller -> object : ll_object_constructed(obj)
alt RequestArena
  object -> arena : track destructor (log record)
  note right : refused record fails\nthe creation
else GcHeap / Immortal
  object -> object : set DESTRUCTOR_PENDING only
end
@enduml
```

### UC2 — Reference store (overwrite)

```plantuml
@startuml
skinparam shadowing false
participant "compiled site" as site
participant barrier
participant refcount
participant arena
participant reserve

site -> barrier : store_box(owner_cat, slot, value)
barrier -> refcount : retain(value)
opt categories differ (escape / release-at-reset)
  barrier -> arena : append log record\n(the mounted arena, via context)
  opt log page full
    arena -> reserve : draw a reserve block
    note right : failure becomes a flag;\nthe next poll raises\nmemory-exhausted
  end
end
barrier -> barrier : write the slot (publish)
site -> barrier : drop_ref(displaced)
barrier -> refcount : release
note over barrier : publish before teardown,\nalways in this order
opt refcount reached 0
  barrier -> barrier : ll_entity_die -> UC3
end
@enduml
```

### UC3 — Entity death, refcount path

```plantuml
@startuml
skinparam shadowing false
participant "release site" as site
participant refcount
participant gc
participant object
participant weak
participant barrier
participant stdapi
participant deferred_free as deferred #E6E0F8

site -> refcount : ll_release(entity)
alt refcount still > 0 (GcHeap object, rc-trace)
  refcount -> gc : buffer_candidate — arm only
  note right : collection fires later,\nat a clean point
else refcount reached 0
  note over refcount : rc-walk: the 1 -> 0 branch acks the\nepoch handshake before any teardown;\npickup rides the outermost dispose's exit.\nBatched runs (2026-07-28 split):\nll_gc_checkpoint_ack, then ll_release_batch\nper reference, ll_gc_checkpoint after
  site -> object : ll_entity_die (kind switch)
  object -> object : phase 1 — pre-destructor,\nresurrection check
  object -> gc : forget_candidate (rc-trace)\nbefore any child drops
  object -> weak : notify_death (if bit 7)
  note right : first act of phase 2
  loop counted children (class runs)
    object -> barrier : drop_ref(child)
  end
  alt GcHeap
    object -> stdapi : ll_free (size-less funnel)
    opt rc-walk epoch active
      stdapi -> deferred : park — recycle waits,\nidentity holds for the walker
    end
  else RequestArena
    note over object : memory stays;\narena reset reclaims
  else Immortal
    note over object : free is a no-op
  end
end
@enduml
```

### UC4 — Arena reset

```plantuml
@startuml
skinparam shadowing false
participant host
participant context
participant promote
participant arena
participant object
participant refcount
participant weak
participant block_pool as pool

host -> context : ll_arena_reset(ctx)
context -> promote : arena_reset_full(arena)
loop fixpoint (destructors may create\nnew escapes and destructors)
  promote -> arena : drain destructor log
  promote -> object : run pre-destructors of\ndying, unescaped objects
  note right : survivors marked via\nARENA_RESET_MARK + hold-counts
end
promote -> promote : count internal edges\namong survivors
promote -> refcount : rewrite survivor category\nto GcHeap, in place
promote -> arena : stamp carrier blocks\nBLOCK_KIND_RETAINED
promote -> arena : drain release-at-reset log
promote -> refcount : one release per record\n(real deaths -> UC3)
arena -> weak : drain_arena_weak_log
arena -> pool : return every other block\n(reserve-drawn included)
@enduml
```

### UC5 — rc-walk collection epoch

```plantuml
@startuml
skinparam shadowing false
participant "collector thread" as collector #E6E0F8
participant epoch #E6E0F8
participant "mutator thread" as mutator
participant heap
participant walk
participant deferred_free as deferred #E6E0F8

collector -> epoch : open — publish activity flag,\nsoft handshake
mutator -> epoch : checkpoint acks the handshake
note right : flag observed before\nany snapshot is taken
collector -> heap : snapshot_entity_blocks\n(no bump cursor)
collector -> collector : phase 1 — walk,\nthree-way classification
collector -> collector : phase 2 — judge
collector -> collector : phase 3 — condemn (collector-private),\nsnapshot-compare re-check
collector -> epoch : post confirmations\n(acquittals are dropped in private —\neager death, 2026-07-27)
mutator -> epoch : next checkpoint drains
epoch -> walk : drain_confirmed —\ncorpse rule, phase 4 exact test,\nthen die
collector -> deferred : epoch closes —\nclear activity flag
mutator -> deferred : flush parked frees\n(owning thread only)
note over collector, deferred : the collector never frees;\nits only shared writes\nare epoch stamps
@enduml
```
