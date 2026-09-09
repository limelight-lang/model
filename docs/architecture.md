# Architecture, visually

A visual companion to `dev/ARCHITECTURE.md`, which is the source of
truth: the knowledge table with the full "knows / does not know"
contract, the shared-resource ledger and the invariant list live
there. This file shows the same structure as diagrams — who knows
whom, who is responsible for what, and the key use cases as
sequences. When a boundary changes, both files change in the same
commit (`dev/WORKFLOW.md`).

The diagrams show the implementation, not the destination. The in-line
`rc-cycle` path is built; its future collector-thread accelerator is not and is
therefore absent. The deleted `rc-walk` and `rc-trace` structures remain on
`archive/pre-rc-cycle`, not in this picture.

Diagrams are PlantUML, embedded as fenced blocks; render on demand
(IDE plugin or any PlantUML processor). No generated images are
committed.

## Layers and who knows whom

Knowledge flows downward: a module may use anything at or below its own layer.
The sanctioned upward edges are dashed and red; their complete entry-point
list is in `dev/ARCHITECTURE.md`. Anything new pointing up is a design event,
not an edit.

```plantuml
@startuml
skinparam shadowing false
skinparam defaultTextAlignment center

rectangle "**L4 — collectors**\ngc (ABI) · cells · cycle · promote" as L4
rectangle "**L3 — object model**\nobject · class · reference · weak · intern" as L3
rectangle "**LB — mutation**\nmemory/barrier" as LB
rectangle "**L2 — memory manager**\ncontext · arena · heap · immortal · buffer · buffer_arena\nreserve · critical · retained · stats · stdapi · routing · large_entity · reset_window" as L2
rectangle "**L1 — entity substrate**\nrefcount · value" as L1
rectangle "**L0 — block supply**\nblock_pool" as L0

L4 -down-> L3
L3 -down-> LB
LB -down-> L2
L2 -down-> L1
L1 -down-> L0

L1 .up.> L4 #red : ""refcount -> cycle/queue""\nregister a non-final decrement
L2 .up.> L4 #red : ""context -> promote"" ll_arena_reset
L2 .up.> L3 #red : ""arena -> weak""\nreset drains the weak log
L2 .up.> L4 #red : ""heap -> cycle""\nthread init / exit
LB .up.> L3 #red : ""barrier -> object""\ndrop_ref cascade
L3 .up.> L4 #red : ""object -> gc"" checkpoint bracket\n""object kinds -> cells"" trace adapters
L3 -> L3 #red : ""class -> object""\ndispose default (data, not a call)

note right of L0
  solid: knowledge flows down
  red dashed: sanctioned calls up
end note
@enduml
```

### Full wiring

Principal structural production edges between modules; the exhaustive table is
`dev/ARCHITECTURE.md`. Ubiquitous hubs are omitted here: everyone →
`refcount`/`value`, context resolution, the `stdapi` free funnel, and
package-level block supply. Use the layer picture above for orientation and
this one for the collector boundary.

```plantuml
@startuml
skinparam componentStyle rectangle
skinparam shadowing false
skinparam linetype ortho
skinparam nodesep 30
skinparam ranksep 35

package "L4 - collectors" as P4 {
  [gc ABI] as gc
  [cells] as cells
  package "cycle" as cycle {
    [queue] as cycle_queue
    [collect] as cycle_collect
    [arena + rows + deferred reuse] as cycle_rows
    [mark + scan] as cycle_trace
    [membership + validation] as cycle_validate
    [finalization + reclamation] as cycle_commit
  }
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
  [critical] as critical
  [retained] as retained
  [stats] as stats
  [stdapi] as stdapi
}
package "L1 - entity substrate" as P1 {
  [refcount] as refcount
  [value] as value
}
package "L0 - block supply" as P0 {
  [block_pool] as pool
}

' principal structural production edges (hubs omitted as above)
gc --> cycle_collect : collect / poll
gc --> reserve : refill at poll
gc --> critical : refill at poll
gc --> cycle_queue : refill + drain overflow
cycle_collect --> cycle_queue : detach / merge / retire
cycle_collect --> cycle_rows : trace window + scratch
cycle_collect --> cycle_trace
cycle_collect --> cycle_validate
cycle_collect --> cycle_commit
cycle_trace --> cells : counted children
cycle_trace --> cycle_rows : shadow rows
cycle_validate --> cells : exact edge reading
cycle_commit --> cells : sever children
cycle_commit --> weak : null cells
cycle_commit --> object : destructors + death
cycle_rows --> heap : slot arithmetic
cycle_rows --> retained : survivor positions
cycle_rows --> critical : overflow blocks
promote --> arena
promote --> object
promote --> weak
promote --> retained : publish survivor lists
cells --> heap
cells --> object
cells --> reference
cells --> weak
cells --> barrier
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
weak --> buffer_arena : table storage
barrier --> arena : logs
context --> arena
context --> heap
context --> immortal
buffer --> arena
buffer_arena --> buffer
heap --> reserve
arena --> reserve
stdapi --> heap
stdapi --> cycle_rows : defer reuse in trace
value --> refcount
P2 --> pool : get / put blocks

' vertical layer stacking
P4 -[hidden]down-> P3
P3 -[hidden]down-> PB
PB -[hidden]down-> P2
P2 -[hidden]down-> P1
P1 -[hidden]down-> P0

' sanctioned upward edges
refcount .up.> cycle_queue #red : register candidate
arena .up.> weak #red : reset drain
context .up.> promote #red : ll_arena_reset
object .up.> gc #red : checkpoint bracket
object .up.> cells #red : trace adapter
barrier .up.> object #red : drop_ref cascade
class .up.> object #red : dispose default (data)
heap .up.> cycle_queue #red : thread init / exit
@enduml
```

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
| `reserve` | two blocks per thread for store-barrier log growth | what a log records |
| `critical` | eight blocks per thread shared by candidate-queue and collection overflow | what either consumer stores |
| `retained` | survivor-list lookup and held-occupant accounting for retained arena blocks | entity kinds and verdicts |
| `stats` | block-granular telemetry, zero hot-path tax | per-object events |
| `stdapi` | size-less malloc/free front door; routes by block kind | entity semantics |
| `context` | `LLContext` + TLS current context; composition root; `ll_arena_reset` ABI | class layout, GC strategy |
| `barrier` *(hot)* | store-barrier micro-ops: publish (`store_ptr`/`store_box`), `drop_ref`, escape recording | per-site composition (lowering's) |
| `refcount` | the 8-byte header at offset 0: refcount + flag word; retain/release | entity bodies, blocks, when to collect |
| `value` | the 16-byte Box: payload + tag + flags | unboxed representations (compiler contract) |
| `intern` | interned names as immortal string entities; lookup table | classes — it only serves them names |
| `class` | descriptors: inline vtable train, itables, Cohen display, layout runs, trace lists | instance state, categories, GC |
| `object` | factory, constructed hook, three-phase death, kind-switched `ll_entity_die` | collector internals, block internals |
| `reference` | the `&` reference box, entity kind 3 | classes |
| `weak` | weak cell (kind 11) = canonical `WeakReference`; per-thread weak table; every notification rule | *when* to notify — the death sites' duty |
| `gc` | GC ABI, per-thread due flag, poll refills and dispatch into `cycle::collect` | collector internals and arming policy |
| `cells` | kind-dispatched counted-child trace and sever adapters | slots and occupancy (heap's side) |
| `cycle` | candidate queue; in-line mark/scan over shadow rows; exact validation; finalization, reclamation and owner retirement | entity-kind layout and size-class arithmetic |
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
participant "cycle/queue" as queue
participant object
participant weak
participant barrier
participant stdapi
participant "cycle/deferred_slot_reuse" as reuse

site -> refcount : ll_release(entity)
alt non-final decrement admitted by candidate gate
  refcount -> queue : register_candidate
  note right : append only; collection fires\nlater at a safepoint or pressure
else refcount reached 0
  site -> object : ll_entity_die (kind switch)
  object -> object : phase 1 — pre-destructor,\nresurrection check
  object -> weak : notify_death (if HAS_WEAK_REFERENCES)
  note right : first act of phase 2
  loop counted children (class runs)
    object -> barrier : drop_ref(child)
  end
  alt GcHeap
    object -> stdapi : ll_free (size-less funnel)
    alt a candidate record still names the slot
      stdapi -> stdapi : mark dead in place;\nowner retirement returns it
    else an active trace has stamped its block
      stdapi -> reuse : push the return on the\ndead-entity stack
    else neither condition holds
      stdapi -> stdapi : return slot now
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

### UC5 — In-line cycle collection

```plantuml
@startuml
skinparam shadowing false
participant "mutator safepoint" as mutator
participant "gc ABI" as gc
participant "cycle/collect" as driver
participant "cycle/queue" as queue
participant "trace window + arena" as arena
participant "mark + scan" as trace
participant "validation + finalization" as finalization
participant reclamation

mutator -> gc : ll_gc_collect_cycles / armed poll
gc -> driver : collect_off_the_poll
driver -> arena : open trace window over\nresident workspace
driver -> queue : detach candidate lane
queue --> driver : roots
driver -> trace : mark every root, then scan every root
trace -> trace : shadow counts -> live /\npotentially unreachable rows
driver -> finalization : membership from rows
finalization -> finalization : exact validation; guards;\nnull weak cells; destructors; revalidate
alt confirmed unreachable
  finalization -> reclamation : sever internal edges; free members
  reclamation -> reclamation : drop displaced external children
else live / refused / resurrected
  finalization --> queue : candidates remain registered
end
driver -> arena : close after commit
arena -> queue : sweep rows; return deferred slots;\nmerge detached records
driver -> queue : retire completed candidate deaths
note over mutator, reclamation : the ordinary path keeps rows through teardown;\nthe pressure path harvests a bounded member list\nand returns trace blocks first
@enduml
```
