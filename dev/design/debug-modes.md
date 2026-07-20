# Debug and observability modes

Design pass for what `PLAN.md` calls **layer 2** of allocation telemetry
and `memory/stats.rs` calls "the opt-in event log". Layer 1 (aggregate,
always-on, block-granular) exists; this document specifies everything
above it.

Status: **design, not implemented.** Nothing here is in the code yet.
Sections marked *open* need a decision before that part is built.

---

## 1. What we are trying to see, and why the obvious tools cannot

Four distinct questions, deliberately not conflated:

| Question | Today |
|---|---|
| How much memory does the process hold, and how much is live? | Layer 1, `ll_memory_stats` |
| What objects exist right now, of what class, in which memory? | **nothing** |
| How long do objects live, and which ones die young? | **nothing** |
| Who allocated this, and did it leak? | **nothing** |

The reason we must build this rather than borrow it: **we are the
allocator.** External tooling sees our 2 MB regions and nothing inside
them.

- **Valgrind / ASAN LeakSanitizer** see one big region, not the objects
  carved out of it. An object leaked inside a live block is invisible.
- **Miri** runs the suite clean but is blind to leaks here on purpose
  (`-Zmiri-ignore-leaks`; the immortal region, the carved regions and
  thread heaps are permanently retained by design), and its leak check
  is all-or-nothing at process exit, so it cannot be scoped.
- **Zend MM** solves exactly our problem for exactly our shape: it walks
  the still-live blocks at *request shutdown* and reports leaks with the
  allocation site. We have the same natural boundary — arena reset.

So the design target is: **per-request leak and lifetime reporting,
plus a live object registry, at zero cost when disabled.**

## 2. Levels, and what each one costs

Two axes, kept separate. Conflating "debug build" with "production
metrics" is the classic mistake: the first may cost 2× and change
layout, the second must be affordable on a live server.

### Axis A — build-time levels (cargo features, cumulative)

Cargo features, not runtime flags, for the reason `probe-counters`
already documents: a runtime check on the allocation path is itself the
cost we are trying to measure. mimalloc makes the same call with
`MI_DEBUG=1..3`.

| Level | Feature | Adds | Cost |
|---|---|---|---|
| 0 | *(default)* | Layer 1 aggregates only | zero |
| 1 | `debug-registry` | Live object registry: per class, per category, per arena. Heap walk. Per-request leak report. | counters on alloc/free; walk is on demand |
| 2 | `debug-lifetime` | Birth stamp per allocation, lifetime histograms, shadow metadata | shadow memory (~1/16 of heap), one store per alloc |
| 3 | `debug-integrity` | Fill patterns, canaries, double-free and free-list corruption detection, fault injection | large; development only |

Cumulative: each level implies the ones below.

A level is not only a runtime setting — **it selects which allocation
ABI the compiler targets** (§4). At level 0 the compiler emits calls to
the release entry points and nothing in this document exists in the
binary at all. From level 1 it emits calls to the debug twins and passes
whatever that level needs: site id, then stack id, then arena identity.

### Axis B — production sampling (runtime, any build)

Levels 1–3 are for development. A live server needs *some* of this
without paying for all of it, which is what jemalloc's `prof` sampling
is for. So layer 2 also supports **sampled** operation: record full
metadata for 1 in N allocations, where N is a runtime setting.

Sampling must be **cheap to decide**: a per-thread countdown counter
decremented per allocation, not a random draw per allocation. Refill the
countdown from a geometric distribution when it fires, so the sample is
statistically unbiased. (This is jemalloc's technique and it is the
right one.)

*Open:* whether sampled mode is a fourth cargo feature or is always
compiled in with the countdown check. The check is one decrement and a
predictable branch, but this crate does not accept "probably free" — it
must be benched before the decision (see `dev/BENCHMARKS.md` for how).

## 3. The object registry

### 3.1 Where the metadata lives: shadow memory, not fat headers

**Decision: per-allocation metadata never goes in the object header.**

The alternative — growing `RcHeader` or `Object` — is rejected because
the object layout is a contract with the compiler (it emits inline
allocation and direct slot offsets), it is pinned by layout tests, and
it would make debug builds diverge from release in the one way that
matters most.

Instead, metadata lives in **shadow memory**: a parallel structure
addressed from the object pointer, allocated only when a level that
needs it is on. This is how ASAN and Valgrind work, and it is the right
fit here because our blocks make the address arithmetic trivial.

**Heap blocks** — exact and O(1). A block is `BLOCK_SIZE`-aligned with
one size class, so:

```
block      = ptr & !BLOCK_MASK
slot_index = (ptr - payload_start(block)) / class_size
```

The slot index is the shadow index. One shadow block per heap block,
taken from the same pool, linked from the block header (a debug-only
field, past the header's hot lines — the reserved 256-byte line has
room, see `dev/DECISIONS.md` on the header split).

**Arena blocks** — no stride, so no index. Arena allocation is a bump
pointer over variable sizes, so a shadow *array* is impossible. Use
instead a **side log appended at allocation**, which is exactly the
machinery the arena already has: it self-hosts an escape log, a
destructor log and a release log in its own bump memory. This is a
fourth log of the same kind, and it dies with the arena for free.

That asymmetry is not a wart, it reflects reality: heap objects are
individually freed and need random-access metadata; arena objects die
en masse at reset and only need to be enumerable.

### 3.2 Extensible per-allocation metadata

Requirement: different allocations carry different metadata, and the
system must be able to hold it.

**Decision: a fixed core record plus a tagged extension chain.**

```
core (16 bytes, one per tracked allocation)
    birth_seq   u64   virtual clock, see §5
    site        u32   LLAllocSite, see §4
    ext         u32   offset of the first extension, or NONE
```

Class and category are *not* stored: both are already reachable from the
object itself (`Object::class`, and the category bits in `RcHeader`), and
duplicating them would double the cost and invite them to disagree.

Extensions are variable-size tagged records in a side arena, singly
linked:

```
ext record:  tag u16 | len u16 | next u32 | payload[len]
```

A tag registry keeps tags meaningful across subsystems (GC, actors, the
buffer layer, host code). Unknown tags are skipped by readers, so a
subsystem can attach metadata without every consumer knowing about it.

This is the part that makes the system a *platform* rather than one
fixed report: adding "which HTTP request allocated this" or "which actor
mailbox this came from" is a new tag, not a schema change.

### 3.3 Enumerating live objects

**Heap:** walk blocks (the owner already chains them through
`owned_next`), and within a block, a slot is live if it is below `bump`
and not on the free list. Marking the free list first, then sweeping,
yields the exact live set — the same trick `mi_heap_visit_blocks` uses.
Cost is O(slots) and paid only when the walk is requested.

Note the cross-thread subtlety this crate already has to respect: slots
parked in `remote_free` still count as live to the owner. The walk must
use the same rule, or it will report phantom leaks.

**Arena:** iterate the allocation log from §3.1.

**Immortal / LongLived:** enumerable the same way as heap once those get
their own allocators; today they route through the heap path.

The public shape is a **visitor**, not a returned collection:

```rust
pub fn visit_live_objects(f: impl FnMut(&LiveObject));
```

so nothing allocates during a walk, which matters because a walk may run
under memory pressure or from a leak report at shutdown.

## 4. The debug ABI: the compiler calls different entry points

**Decision: in a debug build the compiler does not call the release
allocation entry points with extra arguments — it calls a parallel set
of debug entry points, which may take whatever arguments the level
needs.**

This is the mechanism that makes everything else in this document free
in release, and it is worth stating before the individual features that
use it, because they all ride on it.

```
release:   ll_object_new(ctx, class, category)
debug:     ll_object_new_dbg(ctx, class, category, site, arena_id, ...)
```

Why a parallel ABI rather than an extra parameter or a runtime flag:

- The release path stays **byte-identical**. No spare argument, no
  branch, no register pressure, no inlining decision changed. This crate
  exists to keep that path fast, and an observability feature that taxes
  it has already failed.
- The debug entry points are free to take anything, and to change, since
  nothing outside a debug build links them.
- Precedent: this is exactly what Zend does. In a debug build `emalloc`
  expands through a macro that appends `__FILE__` and `__LINE__`, which
  is why PHP can report leaks with a file and line at request shutdown.

**The consequence that must not be missed: free gets no arguments.**
`free` is reached from a refcount reaching zero, from the cycle
collector, and from arena reset — none of which know the allocation
site or the arena. So anything the compiler passes at allocation has to
be *stored* to be available at death. That store is the shadow memory of
§3. The two mechanisms are not alternatives; they compose:

> the debug ABI is how metadata gets in, shadow memory is where it lives
> until the object dies.

**Price, stated plainly:** two ABIs to keep in step. A new allocation
entry point means a debug twin, and a mismatch shows up as a debug-only
bug — the worst kind to chase. Mitigation: generate the twins rather
than write them, and have one test per pair asserting the debug entry
produces an object indistinguishable from the release one.

## 4.1. Where an allocation came from: `LLAllocSite`

`PLAN.md` already names this. The design decision it needs:

**Decision: the compiler passes a static site id; we do not capture
stacks.**

jemalloc samples and unwinds because it serves arbitrary C programs and
has no other option. We are the runtime of a compiled language, so the
compiler knows the allocation site at compile time and can pass a
constant. That is cheaper than unwinding, exact rather than sampled, and
survives inlining and optimization — which stack capture does not.

```
LLAllocSite = u32   index into a static, immortal site table
site table entry:   file, line, function, class (all interned)
```

**One `u32`, not `__FILE__` plus `__LINE__`.** Zend passes the string
pointer and the line number as two arguments on every call. An index
into a table the compiler emits once is strictly better: one register
instead of two, no string pointers travelling through the runtime, and
the table can grow richer fields later without touching a single call
site. The file and line are still there — they are in the table, which
is where they belong, since nothing at runtime reads them until a report
is printed.

The table is emitted by the compiler into the module and registered at
load. `0` is reserved for "unknown / host code", so anything entering
through the plain C ABI still works, just without attribution.

## 4.2. Backtraces: who did this, and when

A site id answers "where in the source". It does not answer "how did we
get here", which for a leak is usually the more useful question — the
same constructor called from two paths leaks in only one of them.

**Decision: walk the language-level frame chain, never the machine
stack.**

Unwinding the native stack per allocation is both expensive and wrong
for us: after inlining and optimization the machine frames no longer
correspond to anything a user wrote. jemalloc unwinds because it serves
arbitrary C programs and has no language-level structure to consult. We
do — the runtime has (or will have) a frame chain for exceptions and
stack traces, and that chain is the honest answer in user terms. Walking
it is a pointer chase over a handful of frames, with no DWARF, no
symbolization, and no unwinder.

**Decision: store a deduplicated stack id, not the stack.**

Storing a full chain per allocation is unaffordable in both time and
space. Instead hash the frame chain, intern it in a stack table, and
store the resulting `u32`. This is what heaptrack and jemalloc's
profiler do, and it collapses the storage to four bytes with the full
chain recoverable at report time.

```
LLStackId = u32     index into an interned stack table
stack table entry:  the frame chain, itself deduplicated by suffix
```

Suffix deduplication matters: chains share tails, so interning each
frame with a pointer to its parent turns the table into a tree and makes
a deep chain cost one node.

**Cost tiering, because even this is not free at every allocation:**

| Level | What is captured | Cost |
|---|---|---|
| 1 | site id only | one constant argument |
| 2 | site id + stack id | frame walk, bounded by depth |
| sampled | full stack + wall clock for 1 in N | amortized to nothing |

So `site` is always affordable and `stack` is the thing that gets
sampled — the opposite of jemalloc, which must sample everything
because it has no cheap site id available.

**"When"** is already answered: the birth stamp of §5, on the virtual
clock, with wall-clock time added for sampled allocations.

**ABI impact** is covered by §4: these are arguments to the debug entry
points and do not exist in the release ABI. The compiler must emit the
site table, and — for level 2 — maintain the frame chain in debug
builds. That is a compiler-side commitment as much as ours, so it
*needs an RFC entry in `limelight-lang/rfc` before implementation.*

## 5. Lifetime

**Decision: measure lifetime on a virtual clock — a monotonic allocation
sequence number — not wall clock.**

Reasons: it is one relaxed increment we already want for sampling, it is
immune to clock adjustment and to a thread being descheduled, and
"objects allocated since birth" is the unit allocator literature and GC
tuning actually use (it is what generational hypotheses are stated in).

Wall-clock lifetime is still useful for a service operator, so record it
too — but only for **sampled** allocations, where one timestamp read is
affordable.

```
lifetime_virtual = death_seq - birth_seq       (always, when level >= 2)
lifetime_wall    = death_time - birth_time     (sampled only)
```

**Aggregation: histograms, never raw samples.** Log2 buckets, per (class,
category). Bounded memory, and it maps one-to-one onto a Prometheus
histogram, so the export in §8 is a straight copy rather than a
computation.

Average lifetime is derived from the histogram (`sum / count`), and both
are kept, because the average alone is misleading for allocation
lifetimes — the distribution is heavily bimodal (most objects die at
arena reset, a few survive by promotion). Report the histogram; the mean
is a convenience.

**Where the counters live: on the `Class` descriptor.** Class descriptors
are immortal, one per class, and reachable from every object in one
load. A debug-only trailing block on the descriptor holds, per category:
live count, total allocated, total freed, and the lifetime histogram.
That makes the update on alloc/free an increment at a known offset with
no lookup at all.

*Concurrency:* objects of one class are allocated on many threads, so
those counters must be atomic. Relaxed atomics are enough (they are
statistics, not invariants), but a hot class becomes a contention point.
If that shows up in a bench, shard per thread and sum on read. Do not
pre-optimize it; measure first.

## 6. Which memory an object lives in

The question "arena, actor, or heap?" splits in two, and they have
different answers.

**Which *kind* of memory** is already free: `MemoryCategory` lives in two
bits of `RcHeader` on every entity — `GcHeap`, `RequestArena`,
`LongLived`, `Immortal`. Nothing needs adding.

**Which *specific* arena** (this request's, or actor N's) is not
representable today. Both bits are used, so it cannot be another
category, and it must not become a per-object field.

**Decision: the compiler passes the arena identity to the debug
allocation entry point (§4), and the runtime stores it in the shadow
record.**

The compiler knows which arena it is allocating into — that is the whole
basis of the category system it already drives. In a debug build it
therefore hands the identity over as an argument, at zero cost to
release, rather than the runtime inferring it.

An arena id in the block header was considered and is **not** the
primary mechanism. It answers a different and weaker question: it tells
you where a block belongs *now*, so it cannot distinguish two objects
born in different arenas that ended up in the same one, and it goes
stale the moment an object is promoted. It stays useful as a
cross-check — the walk of §3.3 can compare the recorded birth arena
against the block's current owner, and a mismatch is either a promotion
or a bug.

Which points at the distinction worth keeping explicit in reports:

| Question | Source |
|---|---|
| Where was it born? | shadow record, from the debug ABI |
| Where does it live now? | `MemoryCategory` bits, plus the owning block |
| Did it move? | the two disagreeing — i.e. it was promoted |

That third row is not a curiosity. Promotion at arena reset is the
crate's most intricate machinery, and "which objects were promoted, from
where, and how long they then lived" is precisely the question its
tuning needs.

*Open, and honestly so:* **actors do not exist in this crate yet.** The
only trace is a comment in `barrier.rs` noting that actor arenas are
unreachable from outside. So the argument is specified here but should
not be built until actor arenas are real, or it will be designed against
an imagined shape.

## 7. Integrity checks (level 3)

Straight from mimalloc's debug build, which is the state of the art here
and worth copying rather than reinventing:

- **Fill patterns.** Distinct bytes on allocation and on free. Turns
  use-after-free and reads of uninitialized memory from "usually works"
  into a deterministic, recognizable value. Cheapest high-yield check
  there is.
- **Canaries / padding.** A guard pattern after the requested size,
  verified on free. Byte-precise overflow detection. Our free list lives
  *inside* the freed slot, so an overflow silently corrupts the next
  allocation's link — this check is worth more here than in a
  conventional allocator.
- **Double-free and invalid-pointer detection.** On free, verify the
  block kind, that the pointer is slot-aligned for its class, and that
  the slot is not already on the free list.
- **Free-list corruption checks.** Validate links on pop, since a
  corrupted link is otherwise discovered as a wild write much later.

### Fault injection

**Decision: include it.** "Fail the Nth allocation" plus a probabilistic
mode. It is the only practical way to exercise out-of-memory paths, and
this crate has a known open inconsistency there (the huge path returns
null while the pooled paths abort — `block_pool.rs:283` in the audit).
An OOM path that is never executed is not a path, it is a guess.

## 8. Metrics export and Prometheus

**Decision: the runtime exports a snapshot; it does not speak
Prometheus, HTTP, or any wire format.**

This crate has **zero runtime dependencies today**, and that is a
property worth defending — it is linked as a `staticlib` into the
C++/LLVM layer, and pulling an HTTP stack and a metrics client into the
memory manager would be the wrong direction entirely. Rendering and
serving belong to the embedding layer.

The export is a **streaming visitor over the C ABI**, so a scrape
allocates nothing:

```c
typedef void (*ll_metric_fn)(void *ctx, const LLMetric *m);
void ll_metrics_visit(ll_metric_fn f, void *ctx);
```

Metric set, in Prometheus naming conventions:

```
ll_memory_resident_bytes                        gauge
ll_memory_active_bytes                          gauge
ll_memory_blocks{state="out|free"}              gauge
ll_objects_live{class,category}                 gauge
ll_objects_allocated_total{class,category}      counter
ll_objects_freed_total{class,category}          counter
ll_object_lifetime_allocations{class,category}  histogram
ll_arena_resets_total                           counter
ll_arena_survivors_total                        counter
ll_gc_collections_total                         counter
```

**Cardinality is the real risk**, and it must be designed in rather than
discovered in production: `class` is unbounded and user-controlled, so a
program with 10 000 classes would emit 40 000 series. Mitigation: export
per-class series only for the top N by live count, fold the rest into
`class="__other__"`, with N configurable and a documented default. The
aggregate metrics never carry the class label, so a dashboard built on
totals is always correct regardless of N.

### External tool integration

Also worth having, and cheap: annotate our allocator to Valgrind and
ASAN the way mimalloc does with its `MI_TRACK_*` hooks. That is what
gives those tools back their sight into our carved regions, and it costs
nothing when the tools are absent. It does not replace the registry, it
complements it: they check memory correctness, the registry answers
"what is alive and for how long".

## 9. Build order, and what is deliberately deferred

Ordered by value delivered per unit of work, not by section number:

1. **Heap walk + live registry (level 1).** Answers "what objects exist,
   of what class, in which memory" — the question with nothing behind it
   today. Needs no ABI change, no compiler cooperation, no layout
   change.
2. **Per-request leak report.** Falls out of (1) plus the arena reset
   boundary we already have. This is the Zend MM model and the highest
   value for day-to-day work.
3. **Lifetime histograms (level 2).** Needs the virtual clock and the
   shadow/side-log metadata.
4. **Integrity checks and fault injection (level 3).** Independent of the
   above; can be built in parallel by anyone.
5. **Metrics export.** Trivial once (1) and (3) exist, because the
   histogram shape was chosen to match.
6. **The debug ABI, and the attribution it carries** — site id, stack
   id, arena identity (§4, §4.1, §4.2, §6). Implemented last here,
   because it is the only work that needs the compiler, but its **RFC
   should be opened first**: three separate features depend on it, and
   the shape of the debug entry points is a decision the compiler team
   has to live with. Do not let the last item to be built be the last
   item to be designed.

**Deferred on purpose:**

- **Arena id / actor attribution** — until actor arenas exist in the
  code. Specified in §6 as a rule, not built.
- **Sampled production mode** — until the countdown check is benched.
- **A wire-format metrics endpoint** — not this crate's job, by §8.

**Cross-repo:** §4 changes the allocation ABI and requires the compiler
to emit a site table. That belongs in `limelight-lang/rfc` before any
code here. Nothing else in this document crosses the repo boundary.
