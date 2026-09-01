# Decisions

An architecture changelog: what was decided and why, not what changed
in the code. Routine fixes and renames belong to git, not here.

A superseded decision is replaced by a **new entry**; old entries are
never edited or deleted.

---

## 2026-09-01 — the workspace base is drawn at the first collection, not at thread init

Owner: S36.10, on Edmond's ruling over the Sage gate's first escalation.
Supersedes the 2026-08-26 entry below that made the workspace a second
mandatory block at init.

**Decided:** a thread's first collection draws one ordinary-pool 64 KiB block
through `gc_metadata::acquire`, and the thread holds it until exit as its
trace workspace — rows, worklist and, after S36.11, the withheld-return
records bump inside it, and overflow beyond it draws the pool and then the
critical reserve and returns at every close. A refused first draw is a
collection that does not start, `None` at the open, which is the answer the
open already gives when its first block is refused. Thread init draws nothing
new. The base returns at exit after the queue's blocks and before
`critical::drain`, and it is outside the arena's list of returnable blocks,
so an abort's count-based return cannot hand it to the reserve.

**Why:** the design of record has one mandatory block per thread and says a
thread that cannot obtain it does not start (`rfc/model/memory/critical-reserve.md`,
"Allocation paths"; `rfc/dev/DECISIONS.md`, 2026-08-28: the queue base is the
one stock that cannot be refilled at a later poll without suspending the
guarantee between birth and that poll). A workspace meets neither half of
that reason — a collection that lacks it answers `None` and loses nothing it
had — so a second mandatory block raised the thread-start threshold for a
guarantee the design never made. Edmond's condition is the one that matters
and holds either way: every block is explicitly requested from the memory
manager, which `acquire` is, and nothing is carved from a block that was not.

**The Commit phase moves to S36.12.** "Bytes the commit still names after the
trace close" are the member list S36.12 builds and S36.3 consumes; at this
step nothing names them, so a typed `Commit` state would be built ahead of the
shape it serves and could be seen red only against a test that names bytes no
production path names. S36.10 builds `Idle → Trace → Idle` with the rewind at
the trace close; S36.12 splits the close when it chooses its commit unit.

**Rejected:** the mandatory draw at init, for the reason above; a base
adopted from the critical reserve, which `critical-reserve.md` forbids as an
ordinary bump block; a phase word beside the withheld-return chain's head
pointer as a second representation of "a trace is open", since an unwind
that drops the chain would leave the two disagreeing.

**Cost:** mandatory direct cycle memory stays 65,536 bytes per registered
thread; a thread that has collected once holds 131,072, and 262,144 with both
queue spares present and an empty queue — the figure the 2026-08-26 entry
called a maximum is the empty-queue figure, a polled thread with one candidate
holding five blocks.

---

## 2026-09-01 — a retained block's survivor list lives in the arena's own memory, and the process registry goes

Owner: S36.14, ruled by Edmond over the Sage's ruling, before S36.9 slice (e).

**Decided:** the sorted list of a retained block's survivors is written at the
reset into memory the arena already holds — the retained block's own tail when
the list fits below its last object, otherwise the reset's current block,
which is then retained as the holder of that list, and only when neither has
room a fresh pool block. The retained block's collector line carries the
list's address and length beside the shadow pointer; a null address is a block
retained for bytes alone, and the trace answers *untracked* for an edge into
it. `memory/retained.rs` loses its `Mutex<BTreeMap>`, its `Arc<[usize]>`
and `snapshot`, and keeps the arithmetic over the header and the occupancy
test. `heap::for_each_entity_slot` finds retained blocks by the region scan on
their kind and reads the list from the header, under the quiescent-mutator
contract it already states.

**Why the registry existed, and why it goes:** `rc-walk` walked every block of
the process once per epoch, and a bump-filled block has no stride, so the walk
needed a list of every retained block's occupants; one process table under a
mutex gave a process-wide walker that list (`918cf1d`). The walk was deleted
on 2026-08-26 and the table was kept for its lookup. `rc-cycle` enumerates
nothing: every production reader asks about one block whose address it holds,
and the one enumeration has no production caller (`dev/CYCLE-COLLECTOR-REVIEW.md`,
finding 3, verified on the day). A process-global lock per traced edge, for a
table nobody enumerates, is the cost of a structure that outlived its reader.
Edmond's ruling: a survivor list belongs to the arena that produced it.

**The count word is atomic.** Live occupants and pinned payloads are one
64-bit word on the collector line, decremented with `fetch_sub` by whichever
thread frees; the returned value says who holds the last count and owes the
block to the pool. `ll_free` is an ABI entry and cannot be made owner-only,
and S38.3 parks frees "whichever thread performs them"; two plain words
written from two threads lose an update, which is a block that never returns
or returns under a live occupant. One `lock xadd` on a promoted survivor's
death is cheaper than the mutex and tree lookup it replaces. Disclosed for
Edmond to overturn; not overturned.

**Rejected, with reasons.** Option A, the registry keeping its shape over
manager-backed storage: stable Rust gives `BTreeMap` and `Arc` no allocator
parameter, so it is the registry rewritten by hand with the same lock kept,
and every line of it replaced when the list moves to the block. The Sage's
own form of B, a per-thread chain of fresh pool blocks holding the lists:
the arena already holds the memory the reset is standing in, and a fresh
block is the fallback, not the rule. A block per list, and a full
`HeapBlockHeader` on a retained block: as `dev/design/retained-index-ownership.md`
refuses them. Owner-only plain counters with abandonment and adoption of
retained blocks: unsafe under the ABI free path, and a third population to
adopt that has no free slots.

**What the rfc entry answers before slice (e) writes code:** the header's
words and who publishes them; that a retained block is on no thread's list
and is neither abandoned nor adopted, its last death returning it from any
thread; that a list-holding block returns when its last list and last
occupant are gone; and that the quiescent enumerator reads the list without
a lock. Also the defect the proposal missed: `retain_block` nulls only the
shadow pointer today, and a block retained, returned, drawn by an arena and
retained again would carry a stale list address unless the whole collector
line is cleared before the kind's release store.

---

## 2026-09-01 — the weak table is the mutator's memory, and it comes from the buffer layer

Owner: S36.9 slice d, ruled by the Sage gate before the first edit.

**Decided:** the per-thread weak table becomes an open-addressed table in one
contiguous long-lived buffer payload — `buffer_alloc_longlived_payload` and its
inverse, the storage class an array's table already uses. Its header sits at
the payload's start and thread-local storage keeps one non-owning pointer to
it. A row is sixteen bytes: the target's address, and one subscriber word
tagged in the four low bits an entity slot leaves free. Capacity is a power of
two of rows, starts at 64 and doubles, and the table holds at most half its
rows, which is what makes a linear probe terminate.

**Why the buffer layer and not `gc_metadata`:** the block kind answers whose
memory a block is, and the logical ledger answers how much memory *collection*
holds (decision of the same date). The weak table is the mutator's
death-notification machinery — a thread that never runs a collection fills it,
and S36.3 will have collection read it without owning it. Stamping it GC
metadata would report mutator memory as collection's holding and falsify
`stats()` for every reader. So slice d adds no charge site and no residue, and
two tests assert that no figure of the ledger moves — one over create, growth,
death and disposal, one over an arena reset's weak walk — each against a
snapshot the high-water door has lowered to the current figure.

**The refusal is answered at the ABI entry.** Every fallible step of
`ll_weakref_create` happens before it holds anything: the table's creation or
growth first, the cell second, and then an insert that cannot fail. A refusal
of either answers null, which is what that entry point already answers for out
of memory. The gate bit, the arena's weak log and the table are all written
after the refusal point, so none of them is reached; the tests hold the gate
bit and the table, and the weak log is structural — a refused create returns
before `log_weak`. Removal allocates nothing, so the death path is structurally
incapable of failing rather than promising not to — which is why the process
abort slice c spends on a refused growth is not spent here: `create` has a
caller who can decline and `ll_free` does not.

**Refused, with reasons.** `array::table` reused: its rows carry values, string
keys and insertion order, and its removal relocates another entry, which drags
the collision-defense state onto the sever-to-free path. A cell pointer in the
object header: eight bytes on every object for a minority feature, which the
RFC already rejected. A side array indexed by slot: eight shadow bytes per
sixteen-byte slot, and rows that outlive slot reuse. Leaving the `HashMap` and
proving it off the collection path: it allocates through the global allocator,
and ownership is the question this step asks.

**The arena's weak drain streams.** `drain_arena_weak_log` notifies inside the
callback rather than collecting first. Nothing depended on the two phases: the
log's head is detached before the walk, and the notification reaches no field
of the `Arena` and no log of it — the entity header it writes is memory the
arena holds rather than the structure that describes it. The collect-first pattern stays
where it is load-bearing, in `promote`, whose drains re-enter the arena.

## 2026-09-01 — the trace's withheld returns are manager memory, drawn where a refusal can still be answered

Owner: S36.9 slice c, ruled by the Sage gate before the first edit.

**Decided:** an in-line trace records the physical returns it withholds in a
chain of 64 KiB blocks drawn through `memory::gc_metadata`, ordinary pool
first and the critical reserve second. The chain's control line is the first
64 bytes of the head block's payload, and thread-local storage keeps one
non-owning pointer to that head block; a null pointer is the closed window, so
the separate active flag goes. The first block is drawn when the window opens
and not at the first withheld return. Both doors refusing at the open answers
`None` and the collection does not start; a growth past the first block that
both doors refuse ends the process.

**Why the draw moved to the open:** a collection is ordinarily the standard
in-line form and meets no refusal anywhere, but on the pressure path it is a
refused pool that started it (`rfc/model/gc/cycle/questions.md`, Y14). A draw
at the first withheld return meets that refusal holding a slot whose rows are
live: returning it is the reuse the window exists to prevent, and dropping the
record loses a physical return, which the ruling of 2026-08-28 refuses. Moving
the draw to the open is what turns the same refusal into a collection that
never starts.

**Refused, with reasons.** The trace scratch arena as the store: its own reset
returns the record blocks to the pool before the replay reads them, and a
refusal there means "abort the collection", which a store receiving pointers
inside `ll_free` cannot answer with. The queue's base block: its payload is
exactly full at one control line and 8,152 escrow entries, so room for records
shrinks the overflow capacity and forces a third re-derivation of the poll
stride, and the two lifetimes disagree — the base block lives as long as the
thread, the records as long as one trace. Waiting for S36.10's workspace: the
`Box<Vec<_>>` stays until then, so the slice's own deny gate cannot pass and
S36.9 waits on two later steps.

**The ledger gains a seventh charge site and a third residue.** A chain block
leaving the append position is charged for what it holds; the block under the
cursor is a residue, entered in the high-water figure at the window's close
and never standing in the current one. Charged and marked are the bytes
*written*: the head's control line counts and a later block's reserved one
does not, which is the rule `current_bytes_in_use` already states. The close
marks the arena's residue and the chain's as one sum, before the arena's reset
charges and discharges its own — that instant is the only one at which both
residues of a collection are in use, and a mark after it would enter them
separately and understate the maximum.

**The window is taken down by whoever releases the chain.** The ordered close
takes it down after the row sweep and before the replay; the chain's own drop
takes it down again, which is what an unwind out of the sweep reaches. See
`POSTMORTEM.md` of the same date for the round that established this.

## 2026-09-01 — the logical charge lands at a structural transition, not at a grant

Owner: S36.9 slice b, ruled by the Sage gate before the first edit.

**Decided:** the logical pair moves at six places. Five are charges, each with
one inverse: a queue segment leaving the live position charges its whole
65,280-byte payload; an escrow landing charges one 8-byte pointer; a floor
charges its 64-byte control line; an arena block leaving the bump charges what
it consumed; an arena reset charges the block under the cursor and then
discharges the collection's whole total. The sixth is not a charge — the
owner's drain enters the live segment's fill in the high-water figure alone,
the bytes being released in the same breath. Charging 8 bytes per enrolment is
refused.

**Why:** the enrolment write is the hottest path in the runtime. A per-grant
charge puts a relaxed read-modify-write pair on it for a figure whose residue
is already bounded; at a transition the same pair runs once per 8,160
enrolments and the write itself takes no added instruction. The escrow is
charged per entry and is the exception that proves the rule: it is the tier
both memory doors refused into, not the write, and its drain takes one
discharge for the whole run.

**What this costs:** two residues, each bounded by one block payload — the live
segment's own fill, per thread, and the arena block still under the bump, per
collection in flight. The current figure lags by them, and the high-water
figure carries each only from the transition that ends it: the arena's reset,
the owner's drain. On one thread that is exact, a collection holding one block
for two hundred bytes standing in the figure at two hundred. Across threads it
is not: a segment filled while another thread held ten is entered when its
owner drains, and by then the other thread may have given its ten back, so the
figure can miss a maximum the two stood in together by up to one payload per
thread.

---

## 2026-09-01 — collection's logical bytes are one pair in production, split only in a debug build

Ruled by Edmond, extending the entry below. Owner: S36.9 slice b.

**Decided:** the production build carries one pair of logical figures — bytes
in use inside the blocks collection owns, current and high-water — beside the
physical block count. Which structure holds those bytes is not carried: rows,
worklist, member lists, parking, deferred drops and suspects are one number.
The per-structure breakdown becomes a build-time feature of its own, the
mechanism `dev/design/debug-modes.md` axis A already describes, and it may add
code and cost that production does not pay.

**Why:** the question production has to answer is how much collection holds
against how much it reserved — a bump arena takes a 65 280-byte payload and
may hold two hundred bytes in it, and nothing measured that gap. Which
structure the bytes belong to is an analysis question, and analysis is what a
debug build is for. Carrying six categories on every suballocation is the same
split the entry below removed one level up, paid on a hotter path.

**What this costs:** a growth that comes from one structure cannot be
attributed from a production process; reproducing it needs the feature build.
That is accepted — a runaway is found by size first and by owner second.

---

## 2026-09-01 — GC memory is counted once, and the block kind is the split

Ruled by Edmond. Owner: S36.9.

**Decided:** `memory::gc_metadata` keeps one current and one high-water block
count for everything cycle collection owns, and `GcBlockRole` is deleted with
its per-role counters and its header word. The question the accounting answers
is which consumer holds the memory — collection, a request arena, an entity
heap — and the block kind already answers it.

**Why:** the four roles were a split inside one consumer, and nobody read it.
What it bought was a second check on return, that a floor is not given back as
a segment; the kind check that survives already refuses a block collection
never owned, and the pool and the reserve refuse a GC-stamped block at their
own doors. A measurement that wants the queue-against-workspace division can be
taken on the day it is wanted, from a build that adds it, rather than carried
by every acquisition.

**What this costs:** `stats()` can no longer say how much of the total is the
candidate queue and how much a collection's workspace. S40.3, which sizes the
workspace, takes its own measurement.

---

## 2026-09-01 — the free that reaches a traced address is the concurrent collector's problem

Ruled by Edmond after the review of `52b2cbf` and `0416e83`. Owner: S38.3.

**Decided:** the trace window of S36.2 stays as it is, and covering the
addresses a trace holds outside entity slots — an array's table storage in a
buffer chunk, a retained payload, an OS-direct run — is S38.3's work, to be
designed when the collector and the mutator run on different threads. S36.2
stays closed.

**Why:** on one thread there is no free inside the window. Mark and scan only
read, and the window ends after the final scan read and before exact
validation, so the user-code teardown that could free anything runs outside it
(entry of 2026-08-31 below). What makes the hazard real is a second thread: the
collector reads an entity while its owner frees it, and the block pool is
process-global, so the memory is recommissioned immediately. `rc-walk` paid for
exactly this, which is why its parking held for a whole epoch rather than for a
phase.

**What this does not settle:** which of the three routes park, and whether they
park by address or by block. S38.3 carries the list.

---

## 2026-09-01 — cycle GC owns persistent manager-visible memory and one token per candidate

Plan prerequisites S36.9–S36.13 and S37.4, before S36.3.

**Decided:** every production byte owned for cycle collection is issued by the
project memory manager and remains identifiable there as GC metadata. A new
`BLOCK_KIND_GC_METADATA` distinguishes those blocks from request arenas, and a
physical accounting layer distinguishes queue floor and segments from
workspace base and overflow without double counting. A second, logical axis
counts current/high-water bytes for rows, worklist, members, parking, deferred
drops and suspects within those blocks. Allocator-owning Rust containers
(`Box`, `Vec`, maps, `Arc` backing storage and hidden `GlobalAlloc`) are not a
collection-memory mechanism. Plain fixed layouts and raw links are allowed;
their backing is manager memory. This supersedes the 2026-08-31 acceptance of
the parking `Box<Vec>`.

**Why:** “the manager happened to supply the bytes” is weaker than ownership.
Today queue and shadow blocks read `BLOCK_KIND_ARENA`, so neither a census nor
the manager's counters can say what the collector holds, while parking bypasses
the manager altogether. The rule is executable: current and peak GC bytes are
counted by role, collection tests deny the global allocator, and thread exit
returns the direct count to zero. A source/ownership audit covers allocations
made before that denial begins, including the weak registry/disposal path and
the retained registry; cloning the current retained `Arc` would preserve an
allocation the manager never saw and therefore does not satisfy the rule. The
queue's present TLS `Cell`s move into its manager-issued floor header, leaving
TLS only a non-owning pointer; capacity and poll bounds are re-derived from the
new layout.

**Decided:** every registered owner keeps one ordinary-pool 64 KiB
`CycleWorkspace` block from init to exit. Collection overflow may draw the pool
and then the critical reserve, but returns after commit or abort; a permanent
base never consumes the critical reserve. The workspace moves through typed
`Idle → Trace → Commit → Idle` phases. Ending the trace filters members while
rows are readable, sweeps block shadow pointers, lowers the active flag and
replays parking. Only the later commit/abort rewind may reuse bytes still named
by the member list.

**Cost:** mandatory direct cycle memory is 131,072 bytes per registered thread:
one 65,536-byte queue floor and one workspace. The two best-effort queue spares
make the nominal/maximum direct baseline 262,144 bytes when both are present.
The distinct critical reserve has capacity up to 524,288 bytes and is neither
guaranteed resident nor workspace capacity. One base block is the fixed policy;
a warm-overflow cache needs S40.3's measurement. A calculated dense 381-entity
shape uses 23,568 bytes (about 23.0 KiB) of the present row, stack and member
forms; sparse placement across 381 widest blocks reserves 6,251,448 row-array
bytes, or 6,258,608 bytes (about 5.97 MiB) with that stack and 381 member
pointers. These are bounds from layouts, not workload results.

**Decided:** `ENROLLED` denotes one logical candidate token in exactly one of
three owner states: active queue, detached in-flight batch, or dormant
suspects. Acquittal moves the token to dormant without clearing the bit; epoch
turnover detaches the due dormant lane beside the active lane as a composite
in-flight batch without a second enrolment or a second token. Abort restores
each sub-batch to its source lane without allocation. Repeated decrements while
dormant see the bit and add nothing. An enrolled
death leaves its record and identity standing; only the consumer that owns the
record may observe count zero, clear the bit, physically return the slot and
retire the token.

**Why:** clearing on acquittal is an edge-triggered permanent miss, while
copying into both active and suspects is a duplicate raw pointer whose two
consumers can retire different occupants of a recycled slot. Moving one token
preserves recall and identity together. Current queue chains cannot splice a
partially filled suspect chain because only their live head carries a bound;
the replacement therefore carries explicit `read`/`used` bounds or compacts
back to full segments before any O(1) move.

**Review:** the Sage counted allocation and cache traffic before this plan was
written. Queue entries consume one new 64-byte line per eight enrolments;
widest flat rows reserve 16,408 bytes — 257 line-equivalents and 257–258
physical lines depending on alignment — while first touch is proven only to
write 121 bytes; its distinct addressed lines remain to measure. Persistent
backing removes manager churn, not those cache fills. The Critic required zero-allocation batch restore,
ordinary-pool funding for the permanent base, consumer-owned corpse retirement,
member filtering before the row sweep and an explicit choice between component
and conservative condemned-batch commit. Each implementation step repeats the
Sage-before-code and Critic-after-repair gates; this review does not pre-approve
their code.

## 2026-08-31 — a trace window owns its row arena and physical returns replay through one door

S36.2.

**Decided:** the in-line trace's slot-reuse window and its `ShadowArena` are
one `cycle::parking::TraceWindow`. Its close first resets the arena and nulls
every block shadow, then lowers the owner-local active flag, then replays every
parked return through `memory::stdapi::ll_free`. The guard is must-use,
thread-bound and non-nestable in every build.

**Why:** a row is indexed by address. Returning a slot before the row sweep lets
the next occupant inherit the dead one's working count and verdict; returning
the block before the sweep makes the sweep itself write into recommissioned
memory. Owning both resources makes the order a property of the type rather
than of S36.7's future call site. The trace window ends after the final scan
read and before exact validation; calling it a collection window would invite
holding it across the user-code teardown that the trace token explicitly does
not cover.

**Decided:** the physical-return gate covers every population that can lose a
row address: ordinary entity slots, retained blocks, pooled large entities and
OS-direct entity runs. The header-reading gate is narrower. In particular,
`promote::arena_reset_full` calls `ll_free(block)` for a retained block already
empty at registration; that pointer is a block-return sentinel, not an entity
header, and may be parked but must never be read for a refcount or `ENROLLED`.

**Why:** the last retained occupant returns the whole block even though the
population has no per-slot free list, and a large run is unmapped. Both lose
identity as completely as reusing one slotted address. One predicate for
physical identity and another for legal header access prevents the block kind
at offset zero from being mistaken for a live refcount.

**Cost and boundary:** parked pointers live in an out-of-band owner-local
`Vec`; its cold trace-only allocation is the cost of leaving the intrusive
list of 2026-07-26 below. Bytes 8-15 of an entity slot are the class word the
walker dereferences one pass after the header, so a link threaded there is a
wild read under a walker chasing a stale pointer, while out of band the corpse
stays readable (`docs/memory-manager.md`, "Parking is out of band"). This TLS form is only
the synchronous owner-side substrate. Before the accelerator exists,
S38.1/S38.3 must move the state where a worker can address its owner and solve
RFC audit A3's generation/handoff race; this step claims neither.


## 2026-08-29 — the exact test compares two sums, and a component arrives as its own member list

S36.1, and both choices are about the judgement that stands between a trace and
a free.

**Decided: `cycle::exact::judge` compares the sum of a component's refcounts
against the sum of the edges its members hold of each other, instead of testing
`RC(m) = IN(m) + guard` member by member.**

**Why:** the per-member form needs one in-degree counter per member, and there
is nowhere to keep it. The trace token is released before the exact test of any
component and the arena returns with it, so the allocator that funded every
other collection-private array has gone (`rfc/model/gc/rc-cycle.md`, "The
release obliges a readership rule"). `rc-walk` built a `HashMap` per component;
a door that can refuse leaves the component unjudged exactly when memory is
short, and the sums need no memory at all. They answer the same question,
because `RC(m) >= IN(m) + guard` holds for each member on its own — every
in-component edge is a counted reference a member holds, and the guard is one
more. A debug build checks that premise member by member, quadratically: the
sum cannot see a defect that invents an in-edge into one member and loses a
real one in another, and such a defect frees a component a live reference
holds.

**Decided: the member slice is sorted by address in place, and membership is a
binary search over it.**

**Why:** a linear scan per traced edge costs the component's size per edge, and
a component here can be the whole reachable population — 381 objects of 381 on
the corpus measured 2026-08-25 (`rfc/model/gc/rc-cycle.md`, "What it is").
Neither form allocates, and neither was measured; what decides is the bound.
The price is the caller's order, which the sort destroys: nothing may be held
parallel to the slice by index, and an entry array indexed after the call names
a different member — which would clear the enrolment bit of a live entity, the
permanent miss of `rfc/model/gc/cycle/questions.md`, Y6.


## 2026-08-29 — the scan re-reads a colour it may have written, and its row lookup cannot write one

Two choices S35.2 made, both about the instant a verdict is read rather than
about what a verdict means.

**Decided: the scan carries no colour on its worklist and reads the row's
colour again when it expands an entity.**

**Why:** a condemned entity can be raised to live between the two instants,
and that is the ordinary case rather than a corner. A ring held by one
reference into its middle has the member the trace reaches first condemned
before the held member is found; the raise is what spares the ring, and a
colour copied at push time is stale in exactly that case, so the raised
member's children would keep a condemnation nothing stands behind. The cost is
one row load per popped entity, against a wider worklist entry and a verdict
that would depend on the order the graph was walked in.

**Decided: the scan reaches a row through `cycle::arena::met_row`, a
read-only twin of `meet`, rather than through `meet` itself.**

**Why:** `meet` initialises a row from the entity's refcount when the colour
says untouched, and it reserves a block's rows at the first touch. Both are
wrong after the counting is over: the scan judges what the mark counted, so an
entity the mark never met has no count to judge, and a lookup that allocated
could abort a collection whose counting had already finished. `met_row`
answers `None` for an unenrolled block, an index past the array, a group this
collection never zeroed, and a colour still untouched. The group check is the
one that is not redundant on paper: the arena recycles pool blocks, so the
rows under an unzeroed group are the last tenant's, and one of the colours it
may carry is a verdict.


## 2026-08-29 — the maturation prune sits above the block dispatch, and the mark carries its own worklist

Two questions S35.1 was left holding, both answered while building the mark.

**Decided: the age prune of S37.1 is evaluated at the head of
`cycle::mark::visit_child`, above `cycle::row::edge_to`, and `Edge` grows no
third variant.** A matured child is read as an opaque live external, which is
the answer `Edge::External` already carries — no row, no subtraction, no
descent — so the prune adds a header test and no second dispatch, and the
"one dispatch per child" clause the dispatch was built under stands.

**Why**, and it is an argument the Critic round did not have: the prune is
evaluated on the target of an edge and never on a root
(`rfc/model/gc/rc-cycle.md`, "What it is"), while `edge_to` is asked about a
root as well — `mark::meet_root` asks it to place the root's own row. A prune
inside the dispatch would therefore prune roots, and a ring whose own root had
matured would go uncollected until its epoch turned, which is the failure the
edge-side reading exists to avoid. The alternative kept the touched-block list
able to tell a matured child from a child outside the heap; nothing reads that
distinction, and the retained arm would take the registry lock before finding
out the child was matured.

**Decided: the descent's worklist is a chain of 512-entry segments drawn from
the collection's arena, and a segment that empties is kept rather than
abandoned.**

**Why:** recursion was refused on the closure's own size — the subgraph
reachable from a median candidate root measures at the whole object
population, 381 of 381 — so a chain deep enough to exhaust the native stack is
an ordinary graph rather than an adversarial one. The arena is a bump with no
free, so an abandoned segment is memory the collection never gets back: a
trace whose depth oscillates across one boundary would take a page per
crossing, and nothing but the segment count reports it, the entries coming
back correctly either way. Its refusal is the arena's own, so the mark aborts
at a refused segment exactly as it aborts at a refused row array, and the
heap is byte-identical either way.


## 2026-08-29 — what the first touch of a thread-local with drop glue may cost, and where it is allowed to happen

**Read off this box, not assumed.** The claim the Critic raised on S34.1 — that
registering a TLS destructor allocates and kills the process when it cannot —
holds here exactly, and the mechanism is worth writing down because nothing in
Rust reports it. The test binary carries a weak reference to
`__cxa_thread_atexit_impl`; `std::sys::thread_local::destructors::`
`linux_like::register` calls it and discards the result. In Ubuntu GLIBC
2.39-0ubuntu8.7 that function `calloc`s 32 bytes per registration and, on a
null, jumps to `__libc_fatal` with the string "Fatal glibc error: failed to
register TLS destructor: out of memory". It never returns a failure, so the
discarded result costs nothing: there is nothing to discard.

**So the touch is moved, not made cheap, and moving it does not change the
class of death.** This is the part worth stating precisely, because the
neighbouring ruling invites the wrong reading: a refused floor ends a *thread*
and `ll_thread_init` answers `false`, while a refused registration ends the
*process* through `__libc_fatal` before init can answer anything. No arm
removes that while the exit guard is a `thread_local!`, and the guard has to be
one — it exists to be a destructor. What moving the touches buys is therefore
narrower than a class change: every registration a release would have made now
happens in one call, at a fixed place, before the thread has done any work, so
the death is deterministic in location rather than scattered across the release
path. `ll_thread_init` touches all four: the pool's thread cache at the first
`BlockPool::get`, which is `queue::take_floor`'s; the barrier reserve and the
critical reserve at their own fills; and the exit guard at
`thread_exit_will_run`, which is last, and which is what makes the guard the
first destructor to run under glibc's reverse order.

**What is left on the release path is one population**, the thread the runtime
never registered. Its first registration is the exit guard's, inside
`cycle::queue::draw_floor_or_abort`, and `critical::draw` two lines later is
the second. Both stand beside an abort that thread already takes — the refused
floor — but they are **not the same edge**: the floor's abort answers a block
pool with nothing left, and the registration's fatal answers glibc's heap
with nothing left, and one can be exhausted while the other is not. What is
true is weaker and is what is accepted here: under exhaustion deep enough to
starve either, an unregistered thread doing entity work ends the process, and
this runtime has no reporting path for that thread at all.
`memory::critical::tests::where_the_first_touch_happens` pins the placement,
which is what a test can reach — the registration itself cannot be observed
from inside a process that the failing case has already killed.

**Four is the whole inventory**, and it is small because of the rule of
2026-08-03: every per-thread structure a thread exit can reach was converted to
a raw pointer in a `Cell`, which has no drop glue and so registers nothing. The
four that remain are the two reserves, whose `Drop` is the fallback for a
thread that never ran `ll_thread_exit`; the pool's thread cache, for the same
reason; and the exit guard, which exists to be a destructor. A fifth would put
the ground back in question, so `memory::critical::tests::`
`where_the_first_touch_happens` carries a census of every `thread_local!` in
`src/` against a literal list: adding one fails that test, which is a
convention held by a list rather than a mechanism, and it is written there as
one.

**The other arm was priced in its narrow reading only.** Dropping `Critical`'s
drop glue buys nothing while the guard next door keeps its own, and the guard's
registration comes first on the release path anyway — that much is settled. Not
priced: a guard built on a `pthread_key_create` key taken once at process
start, where the failure is reportable and per-thread arming may allocate
nothing. It would need FFI plumbing and a per-target story, and glibc's
`pthread_setspecific` allocation behaviour was not read, so it stands as an
unexamined option rather than a rejected one.

## 2026-08-29 — `ll_thread_init` answers, and three self-initialising paths are the only callers that may not read the answer

**The status is `#[must_use]`.** The ABI gained a return so that a refused
floor could end a thread rather than the process (`rfc/dev/DECISIONS.md`, "the
escrow's floor is allocator-issued"), and a status nobody is obliged to read
ends nothing: every Rust caller in the tree discarded it, so the ruled soft
path — the task runs elsewhere — was unreachable and the refusal degenerated
into the lazy draw's abort. The attribute puts the choice at each call site;
the thirty-one test and example sites assert it, which is also the premise each
of them depends on.

**It is a Rust lint and stops at the crate boundary**, which is where the
population it was built for lives: an embedder calling through the C ABI is
told nothing by an attribute. The tree's own C callers are the demonstration —
every probe in `bench-external/` declared the new signature and went on calling
it bare — so each of them now reads the answer by hand and refuses to measure a
thread the runtime would not start.

**Three exemptions, and they are the self-initialising paths**:
`stdapi::ll_alloc_init`, `heap::ll_entity_reserve` and `heap::entity_alloc_init`.
Their contract is a null allocation on any refusal, which they report by
reading the heap after the call, and that report covers a refused floor exactly
as it covers a refused heap. A fourth caller, the journal's registration inside
`ring_for_writing`, discards it deliberately: what it needs from the call is
the exit guard, which it asks for on the next line.

**The floor is returned by `release_floor` and not by `drain`.** `drain` is
also how a live thread empties its queue — every test in the module starts and
ends with it — and a live thread stripped of its floor would draw a second one
at its next enrolment. So `retire_the_journal` calls the two in order, the
floor after the segments and before the critical reserve drains, which is the
order every other block of the queue's already takes.

## 2026-08-28 — the bulk release loop polls on its own backedge

**The poll contract binds this loop too.** `ll_release_vector` runs its loop
inside the runtime over a count the caller chose, and the compiler emits its
poll only after the call — so nothing refilled the queue's spare cells or the
critical reserve mid-run, and every iteration could enrol. A clear of some
ninety thousand shared elements therefore exhausted eleven segments of funding
and then the escrow, and aborted with memory free. The loop now calls
`ll_gc_maybe_collect` every `POLL_STRIDE` iterations, which is half the escrow.

**Why the backedge is a legal fire point**, and it is ruled rather than
assumed: iteration `i - 1` has fully returned, its death and destructor with
it, and `entities[i]` has not been read — `rfc/model/gc/strategies.md`'s
"between mutator operations, after the current store or teardown has
completed". It rests on a precondition `rfc/model/memory/bulk-operations.md`
now states, that the caller severs every traced edge to an entry before
submitting the vector. Inside a teardown the gate is closed and the poll fires
nothing, which is the reentrancy guard that document already licenses.

**Cost.** One compare-and-branch per iteration of the bulk path, and a full
poll every 4080 iterations. Unmeasured, and no figure is offered: the crate has
no bench that isolates a vector release.

**Re-derived 2026-09-01.** The stride is 4,076 since S36.9a gave the floor's
first 64 bytes to owner control, which took the escrow to 8,152 entries. The
rule is unchanged and the figure above is what it was on the day.



## 2026-08-28 — an enrolment cannot fail, and the undo of the enrolled bit is deleted

**Superseding the undo the entry below records.** Edmond ruled that nothing may
be lost, so the release path no longer has a branch in which no entry names an
entity: below the live segment, the two spare cells and the critical reserve
sits an **escrow** in the same thread-local — a fixed array of one segment's
capacity, `const`-constructible and never grown — and a refused entry lands
there by a store and an increment. `enrol` answers nothing, and a set enrolled
bit always names an entry. The design side is `rfc/dev/DECISIONS.md`, "an
enrolment cannot fail", which also carries the Sage's mechanism for the poll.

**Why the undo had to go rather than be narrowed.** It left the entity
enrollable at a *later* decrement, which is not the same as keeping this one:
if the decrement that was refused was the ring's last external release, no
later decrement comes and the ring is unreachable garbage for ever. That is
Y6's permanent miss, and it is what the ruling refuses.

**What the poll does with the escrow**, in `gc::ll_gc_maybe_collect` and in this
order: refill the two cells, drain the escrow into the queue as far as the room
allows, then fire if armed. The order is load-bearing — a drain before the
refill puts the entries straight back. A drain that finds no room stops rather
than looping, leaving the entries for the collection the same poll is about to
run.

**Cost, measured.** 65 280 bytes of thread-local per thread, on top of the two
spare segments and the critical reserve's eight blocks — and they are committed
at thread creation rather than on first touch. `readelf -S` on the test binary
at this commit puts `.tbss` at 65 680 bytes, so the escrow is 99.4 % of the
crate's whole zero-initialised TLS image, and that image is what glibc
allocates and zeroes for every thread it starts. The
hot path gains nothing: the escrow sits after the reserve's refusal, which sits
after both cells. Overflowing the escrow aborts, which is the last resort the
funded class already keeps (`rfc/runtime/exceptions.md`) and is reachable only
by a run of enrolments long enough to fill a whole segment with no safepoint
poll in it.




## 2026-08-27 — the enrolment queue is a chain of pool segments, and its live segment is a spare cell

**Decision.** A queue segment is one 64 KiB pool block, the queue is a chain of
them threaded through `BlockHeader::next`, and growth links the full segment in
rather than copying out of it. Only the live segment is partly filled — an
overflow is the one way a segment leaves that position — so the chain carries no
per-segment length and the fill is one cell beside it. A thread holds **no live
segment until its first enrolment**, which finds no room by construction and
takes the overflow path, so the empty-queue case is the overflow case and a
thread that never enrols holds two segments instead of three.

**Why.** The design fixes the segment at one pool block, that being the only
unit both funding doors dispense (`rfc/model/gc/cycle/questions.md`, Y12
clause 3). Everything else follows from wanting the write to be a store: a
per-segment header would put a second load on the hot path, and a lazily taken
live segment removes the arm that would otherwise test for one.

**The undo of the enrolled bit is this step's own, and it decides nothing
beyond itself.** With both cells empty and the critical reserve spent, no entry
lands, and the release path puts the bit back down. That is legal under the law
of 2026-08-26 — the owner reducing its own incomplete enrolment on an exact
reading — and it leaves the entity enrollable at a later decrement rather than
reserved an examination that will never come. What the runtime owes beyond it is
`rfc/dev/PLAN.md` S8.5 and is nobody's ruling yet, so the branch carries a
`#[cfg(test)]` counter and no reporting.

**Cost.** Two 64 KiB blocks resident per thread from `ll_thread_init`, and a
third from the first enrolment. One narrow store into the flags half on the
release path, where the loaded word is reused, plus the queue's own store. Not
measured: what the enrolment adds to a non-final decrement, there being no
benchmark that isolates one.

**Rejected: a `RefCell` around the queue.** The write is the hottest path in the
runtime and the borrow flag buys nothing, the queue having one writer by
contract and no path that re-enters it. **Rejected: keeping the fill inside the
block.** It is a second cache line on the write path for a number only the live
segment has.


## 2026-08-27 — the test binary counts allocations through a global allocator

**Decision.** `test_support::allocation_probe` installs a `#[cfg(test)]`
`#[global_allocator]` over `System` that counts allocations per thread, and
`block_pool` counts requests for a block the same way. A test asserts that a
path allocated nothing by bracketing it with both counters.

**Why.** The clause under test is that enrolment never allocates and never
locks, and the path proves it by calling no allocator by name. A probe threaded
through the call sites would see only the calls somebody remembered to thread it
through — the property would be assumed by the instrument meant to check it.
The pool counter is the second half: a request the thread cache serves allocates
nothing and still takes the pool's word, and a path that may not lock is judged
by requests rather than by allocations.

**Cost.** One thread-local increment on every allocation in the test binary, and
a global allocator where the crate had none. The release build is untouched.



## 2026-08-27 — a block's rows and its place on the touched list are one allocation

**Decision.** The touched list threads through a 24-byte prologue on the row
array itself — `{ block, next, slots, population }` ahead of the rows — so a
collection's first touch of a block reserves its rows and enrols it for the
sweep in one call to the arena. The 512-entry segment chain that the arena
landed with on the same day is gone, and with it `note_touched` and its refusal
path.

**Why.** The enrolment must precede the stamp, because the enrolment can fail
and the stamp cannot: a block stamped with rows an abort then gives back is the
stale pointer the sweep exists to prevent, and it is reached exactly when memory
is short. Two allocations make that an ordering rule anybody can break; one
allocation makes it unreachable — after the rows exist there is nothing left to
refuse. The cost moves the same way: a touched block pays 24 bytes rather than
its share of a 4 KiB segment, and the first touched block of a sparse collection
paid a whole segment for one entry.

**What it costs.** A large entity has no array — its single row is a word of its
own block header — so it takes a prologue with no rows behind it, 24 bytes for
the sake of the sweep alone. The alternative was to leave large-entity blocks
off the list and have the sweep find them another way, which means a second
enumeration of exactly the blocks the list already names.

**Also settled here: a retained block's index space.** It is the block's object
index, so the array holds one row per occupant and a row is found by the
occupant's position in it — the same number `retained::occupant_index` answers
in. The length lives behind the registry mutex, so it is asked once per block at
the first touch (`occupant_count`) rather than once per edge; the per-edge lock
in `occupant_index` stays until S35.1 gives the trace a per-block visit to hold
an `Arc` over. The row lookup bounds-checks the index against the recorded
length and answers "no row" above it, which keeps the referent alive rather than
condemning it on a row the trace guessed.

**And the reserved colour.** A row is two bits of colour over thirty of working
count, and colour zero means "not met in this collection". Without a code
reserved for it, a condemned member — count zero, met — would read exactly like
a slot the trace never reached, and the second edge into it would re-initialise
the row from the refcount and acquit the component. The same zero is what a
group init writes, and what a large entity's block header carries from its
commissioning.

**The meeting hands the reserved colour's answer back before it destroys it.**
`meet` writes the met colour itself, so after the call the row can no longer
say whether this collection had seen the entity before — and that is exactly
the bit the mark's descent turns on, an edge into an already-expanded entity
taking the decrement and stopping. Four colour codes are all two bits hold, so
there is no fifth for "expanded", and the caller cannot pre-read a row of a
block that has no array yet. `Met::Row` therefore carries `first_reach`
(Critic, 2026-08-27).

**The subtraction lives beside the row rather than at the call site.** The
open-coded form is `compose(colour(r), count(r) - 1)`, and at a count of zero it
wraps to `u32::MAX`, which `compose` clamps to `COUNT_MAX` — the value reserved
for "externally referenced, conservatively live". A row that should read
condemned would read maximally live, and the ring it belongs to would survive
every collection. `shadow::subtract` saturates at zero and carries the colour it
found. The floor is a saturation and not an error because a dirty pass may read
more in-edges than the refcount held, which the design permits: the exact test
on the owner's thread is what turns a candidate into a verdict.

---

## 2026-08-27 — the shadow arena asks the pool first and the critical reserve second, and the virtual reservation goes

**Decision (Sage, final).** The cycle collection's working memory is a bump
arena over 64 KiB pool blocks with two doors in a fixed order: the ordinary
`BlockPool`, and on a null from it a new per-thread critical reserve of eight
blocks (`memory::critical`). Every block comes back at the collection's end and
on its abort alike; what the critical door lent goes back to the reserve before
the pool sees a block. The per-block row array is a plain bump allocation of the
whole `slots × 4` plus its met bitmap, unzeroed. Nothing is lazily mapped.

**Why the ordinary door comes first.** Y14's sentence that the ordinary path is
inadmissible is written on the premise that it has already refused, and since
2026-08-26 the in-line collection is the **standard** form rather than the
emergency one. A collection with no refusal behind it may legitimately want
hundreds of megabytes of rows — the measured full-trace case is 717 MiB, about
eleven thousand blocks — which no reserve funds and the pool funds trivially.
Drawing the reserve while the pool serves is what `critical-reserve.md` forbids
in the other direction: it converts the reserve into ordinary memory with extra
steps. On the pressure path the pool's `get` is a fast fail, so every draw that
matters there is the reserve's and the design's sentence stays true exactly
where it was written.

**Why a second reserve rather than a larger `memory::reserve`.** That module is
`exceptions.md`'s log reserve, and its whole guarantee is that the store barrier
cannot fail; its two blocks are sized from the poll contract. A collection that
drained them would convert an aborted collection into an unreportable barrier
failure. `exceptions.md` splits the reserve in three precisely so that no
consumer's worst case is the sum of all three.

**Rejected: an OS-direct mapping materialised page by page**, which is what
`rc-cycle.md` described until today. A page that fails to materialise reports
nothing a caller can catch: under ordinary overcommit the mapping succeeds and
the failure arrives as a kill at first touch, on the path that runs because
memory is short, in a process built `panic = "abort"`. The step's load-bearing
requirement is that a refusal aborts the collection rather than the process, and
only a call that returns null can carry that. Blocks returned at an abort also
re-enter the pool, where the very allocation that triggered the collection can
be served; a mapping would leave the process instead.

**What the bump form pays.** The whole row array of a sparsely touched block —
up to 16 320 bytes for one traced entity at the smallest class. That is the
twenty-fourth entry of 2026-08-25's objection to per-block arrays back at half
its size, the row having lost its captured count. Accepted, and bounded by the
touched-block list. The chunked form stays the recorded alternative and is
revisited only if a measured traced density lands below 29 %; no such
measurement exists.

**The figure.** Eight blocks, 512 KiB, which is `critical-reserve.md`'s 500 KB
read at block granularity — a starting figure, not a derived one. At four bytes
a row it funds about thirty smallest-class blocks, more at the middle classes.
On the pressure path that capacity is the collection's trace budget, and
exhausting it aborts into the retry-then-raise `exceptions.md` promises. No
partition among the reserve's three named customers is built: two of them do not
exist in the crate, and no share is derivable without a workload.

**Both normative documents moved with the ruling**, in `rfc` at `27417f2`:
`rc-cycle.md`'s "The rows are not zeroed greedily" lost the virtual reservation,
and `critical-reserve.md`'s "The three customers" and "Sizing" gained the draw
order and lost the sentence that called the funding open.

---


## 2026-08-27 — the collector's triple sits past the header, at the header's own size

**Decision.** A block's shadow-row pointer, the reciprocal that turns an offset
into a slot index, and a copy of the size class live in `BlockCollector`, laid
over the block at `COLLECTOR_TRIPLE_OFFSET = size_of::<HeapBlockHeader>()` —
line 3 of the reserved 256-byte header line. It is written by `Heap::refill` for
`BLOCK_KIND_ENTITY` blocks alone, and published by the kind's release store like
every other header word.

**Why past the header rather than inside it.** The collector writes `shadow` at
a block's first touch, and it is the only word of a block header a non-owner
writes. Inside `HeapBlockHeader` it would still get its own line under
`align(64)`, but the offset would then be a consequence of field order rather
than a stated place, and the `const` assertion that keeps the header from growing
into the tail becomes circular: `size_of::<HeapBlockHeader>()` would include the
triple. Overlaid, the two are independent and the assertion has something to say.

**Why the offset is the header's size and not 192.** 192 is what `BlockRemote`'s
64-byte alignment produces today, not a decision anybody made. Written as a
literal it would survive a header that grew to 256 — which still satisfies the
existing "the header fits the line" assertion — and the triple would overlap the
cold links. Tied to the size, a header that grows moves the triple and trips the
`const` assertion that it still ends inside 256.

**Why the size class is duplicated.** `HeapBlockHeader::size_class` is four bytes
from `kind` on line 0, so reading it costs nothing extra while the dispatch is
reading the kind. The copy is for the step after: a row array's length is
`BLOCK_PAYLOAD / stride`, and S33.2 needs the stride again after the reciprocal
has already answered the index. Taking it from line 0 there would put the owner's
bump cursor and free list back into a lookup that had left them.

**Rejected: the flat literal, and a fourth struct field.** Both are above. Also
rejected: making the triple's words plain rather than atomic. Two of the three
are written by the owner and read by a collector on another thread, so plain
writes are a data race by the model however constant the values are — the same
medicine `kind` and `size_class` were split out with.

**What is not built.** `shadow` has no writer: nothing reserves rows yet, and the
step that does — `PLAN.md` S33.2 — also owes nulling it at the end of a
collection, on the abort path included, because a stale pointer left in a block
whose arena has been recommissioned makes the next collection decrement live
payload.

---


## 2026-08-27 — a comment names the plan step that owes it, and the stage's deletion sweeps the number

**Decided (Sage):** a comment that states a capability is absent names the
`PLAN.md` step that will build it, by number, as a forward reference only. The
commit that deletes a stage's section from `PLAN.md`, or moves a debt it carries
to another stage, greps that number over `src/` and `benches/` in the same
commit and rewrites every comment it finds. A stage is never cited as history:
what a closed stage did belongs to git or to a journal entry cited by its title.
`dev/WORKFLOW.md`'s "How a reference is written" is amended to match, and gains
"How a debt is written" beside it.

**Why:** the ban rested on "a number that gets reissued or removed", one reason
covering two failure modes it never separated. A line number or a list item is
reissued, so a stale citation points at the wrong thing and the reader believes
it. A stage number is never reissued — that is the plan's own rule and this
crate cannot change it — so a stale citation points at nothing, which the reader
sees and git resolves. The rule condemned stage numbers for the other class's
failure.

Every alternative referent binds the comment to the wrong event. An `rfc/`
question is closed by an answer rather than by a build, and a `dev/DECISIONS.md`
entry outlives the capability being built, so a comment citing either can dangle
while it is still true and resolve while it is false. The step's closing is the
one event that coincides with the fact that falsifies the comment, which is why
the number is also the grep handle: "documentation follows the logic, in the
same commit" already obliges the closing commit to rewrite these comments, and
the number is what lets it find them. Striking the citation would not make the
comments cheaper to keep — they go stale on the same event either way — it would
make them unfindable.

**Measured, 2026-08-27:** 54 lines in 23 non-test files and 9 in 8 test files
name a stage, a step or `PLAN.md`; 21 distinct numbers are cited, of which two
are already deleted — `S30` seven times as the past end of an interval, `S18.3`
twice as the review that found a defect. Both are history rather than debt,
which the ruling forbids, and both are the two sweeps that were skipped. They
are repaired in the same commit as this entry.

**Rejected:** naming nothing, which destroys the handle the closing commit needs
without reducing the comment's maintenance by one line, and erases the
distinction between a scheduled gap and an oversight. An `rfc/` question or
section, which binds to the wrong event and puts build order into a normative
specification, making it a second plan. A `dev/` debt ledger, which is `PLAN.md`
rewritten under another name — two boards to keep in step — and which would need
a new document and the owner's agreement to buy what the plan step already
gives.

**Cost:** the sweep is still an act someone has to perform at stage closing, and
skipping it leaves dangling numbers, as it did twice; what the ruling buys is
that the failure is dangling-never-wrong and that detection is one grep.
Comments in `src/` now lawfully depend on a file outside `src/`, paid for by the
pointer-not-content clause: the sentence says what is absent and what the step
builds, so it survives its number going dead. And a written rule lost to the
practice, which is worth saying plainly rather than letting the amendment read
as tidying — it lost because its stated mechanism is false for this identifier
class, and for every class where reissuance is real the ban stands unchanged.

## 2026-08-27 — a kind's ring classification is written at its declaration, before a factory stamps it

**Decided:** `EntityKind::Lazy` classifies as ring-closing although no factory
stamps its code yet, and a kind added later is classified the same way — at the
declaration, from the slots the kind holds, rather than when a producer for it
is built.

**Why:** waiting for a producer is what left the ReferenceBox outside the
candidate gate until 2026-08-07, and that gap leaked `$a['x'] = &$a`: the box
holds the array, the array's element holds the box, and the frame's release
decrements the box from two to one. Nothing else is decremented and the box was
not admitted, so no candidate exists and the ring is never freed. A Lazy proxy's
slots are already traversed on both paths that would reach them —
`ll_entity_die` sends it through `ll_object_die`, and `cells::trace_cells`
strides it like an object — so the classification states what the layout is, and
only the factory is missing.

**What this replaces:** the fact was recorded on 2026-08-07 under "the candidate
gate is a set of kinds, not a mask over their codes", whose main clause the
renumbering of 2026-08-26 overturned — the codes were reassigned so that the
gate is a mask again — and which states the fact as "kind 6", a code the same
renumbering moved. `EntityKind::closes_a_ring` cites this entry instead, so the
one sentence the code still needs from that day is not read out of an entry
whose argument no longer holds.

**Cost:** a kind classified before its producer exists is a classification no
test drives. The `const` battery in `refcount.rs` ties each kind's answer to its
code and `to_flags`'s `debug_assert!` catches a kind the battery never named;
neither can see whether the slots are really there. Lazy's own test is owed to
the stage that builds the factory.

## 2026-08-26 — the keep-clause of the field-privacy ruling is retired: both `&self` readers are deleted

The same Sage amends the entry below. Its main clause stands — the fields are
private — and its **keep-clause does not**: `RcHeader::memory_category` and
`RcHeader::lifetime_counted` were kept public on the stated ground that
factories call them before publication, and the sources refute it. The only
occurrences of either name in `src/`, `benches/`, `examples/` and the external
bench crate are the two definitions, `lifetime_counted`'s call into its twin,
and the guard's own pattern strings. The population is zero. Both are deleted.

**The deciding consequence is that one binding stops compiling.** `let hdr =
&(*slot).rc` still forms — `rc` is public on every entity struct — but it now
reaches a type with private fields and no methods, so there is nothing to call
and the binding has no motive. That was the last spelling by which a shared
reference could span a published header's eight bytes while both instruments
watched something else, and it is bit-for-bit the defect of 2026-08-15. It
moves into the type, which is the ground the privacy ruling stood on. The type
also reaches where the grep never walked: the guard reads `src/` alone, so a
bench, an example or the external crate could have formed that reference with
no instrument at all.

**What is accepted lost:** the public API can no longer read any part of a
header, `RcHeader` being construct-only from outside the crate. A consumer
calling `.memory_category()` breaks unsurveyed, which is the consumer class the
privacy ruling already accepted breaking on `.refcount`. If the capability is
wanted again it returns as a free function over the flags word — the shape
`is_object`, `is_string` and `may_enrol` already have — and never again as
`&self`.

**The grep stays on the two grounds that remain**, and the third is named as
retired rather than smoothed over. It is the only instrument that fires when
the privacy is reverted: restoring `pub` breaks no build, nothing outside
`refcount` naming either field, so the erosion is silent until the first new
site. And it reads configurations the checking build does not compile, a
`#[cfg]`-disabled branch parsing without resolving a name — an evasion this
guard has already had. All eight literals stay: four against the revert, four
against the deleted pair being reintroduced from habit.

---


## 2026-08-26 — `RcHeader`'s fields go private, and the source grep is re-aimed rather than retired

The Sage ruled it. The fields lose `pub` outright — visible to `refcount` and
its child modules and nothing wider — and
`refcount::tests::who_may_read_a_header` stays, pointed at what a type cannot
reach.

**The reason is this crate's own**, from `dev/POSTMORTEM.md`, 2026-08-10: an
invariant that people who knew it still violated becomes a type rule, that
being the only form which cannot be violated again. The grep is a list of
spellings and its record argues against it — widened twice in eleven days,
twenty-eight sites found by review rather than by the test, three evasions
conceded in its own module doc, one of which stood live in `memory/barrier.rs`
in the shape that is worse than the race it hides. No production code outside
`refcount.rs` names either field, so the fence costs production nothing, and
it closes the class before S38.0 puts a collector thread beside the mutator —
where a missed spelling stops being a hole in a test and becomes undefined
behaviour running in CI.

**`pub(crate)` is refused rather than overlooked.** Every offence on record was
internal, so that middle form breaks the unsurveyed external consumer and
leaves the demonstrated problem standing.

**The price, measured before the ruling:** 187 accesses in 37 test files, none
in production, none in the benches, none in `refcount`'s own tests, which keep
access as a child module. Two constraints come with the conversion, and either
one broken makes it self-defeating. The shorthand the fixtures get takes a raw
pointer and never `&self`, because `fn refcount(&self)` forms the `&RcHeader`
this change exists to ban — the same shape refused for `Class` on the same day.
And its body is the narrow atomic load rather than a plain read: those 187
sites are the population a ThreadSanitizer run reaches first, so a shorthand
re-exporting the plain read buys the compile error and keeps the blind spot.

**The grep is kept because privacy does not subsume it.** `memory_category`
and `lifetime_counted` take `&self`, stay public for the pre-publication
callers, and autoref reaches them past private fields: `(*p).memory_category()`
on a published header forms the forbidden shared reference and compiles. Those
two spellings the guard sees and the type never will. The four field spellings
stay in its list as the tripwire against the privacy being reverted, at the
cost of one array literal, and its two self-tests keep it honest against
passing by finding nothing.

**What the ruling accepts losing:** the fields as public API. `RcHeader` is
re-exported from `lib.rs` and the crate is published, so a consumer outside
this repository reading `.refcount` breaks at its next update, unsurveyed. Such
a consumer holds exactly the race this day was spent removing, and the
`#[repr(C)]` layout, `RcHeader::new` and the retain/release ABI are untouched.

**Two limits are named rather than solved**, and the list is open. A `&mut
Object` formed over a published entity to reach its other fields asserts
uniqueness over the header bytes without spelling a header access at all. And
`core::ptr::read::<RcHeader>(p)` names no field, so privacy does not reach it
either, while it reads eight bytes across byte 6 — the same instinct that
produced three of the wide reads repaired that day. Both stay Miri's,
ThreadSanitizer's and a reader's.

---


## 2026-08-26 — the header guard greps the pointer spelling, and a class descriptor keeps its reference

`refcount::tests::who_may_read_a_header` matched `.rc.flags` and its three
neighbours, which is a header reached through an entity struct. The same read
through a raw pointer of the header spells `(*p).flags`, and the guard never
saw it: twenty-four sites stood in `promote.rs`, `memory/heap.rs`, `cells.rs`
and one fixture. Most touch arena entities, which no collector traces, but
three populations are published headers outright — `memory/heap.rs`'s census
strides GC-heap slots, `promote.rs` rewrites a survivor's category and then
keeps reading the same header, and `count_children` reads every counted child
including the heap ones. The guard now matches `).flags`, `).refcount`,
`).memory_category()` and `).lifetime_counted()` as well, and every site goes
through `mutator_flags`, `header_refcount`, `update_header_flags` or the new
`set_header_refcount`.

**A class descriptor carries a `flags` word of its own**, and five sites read
it as `(*cls).flags`, so the widened grep flagged those too. They are not
headers, and `Class::flags_of` now reads the word at its own offset through
the pointer; the five sites call it, and the guard keeps no exemption list.
Two other forms were tried and refused. A shared reference — `let cls = &*cls;
cls.flags` — was the first, and it asserts `size_of::<Class>()` bytes readable
and `cls` non-null where the raw read needs four bytes, besides being the one
spelling this guard is documented as unable to see. Renaming the field was the
other: `Class` is `#[repr(C)]` and the compiler emits descriptors against that
layout, so the name is published and moving it starts with an amendment to
`rfc/model/classes.md`.

**A reference binding still evades the guard**, `let e = &mut *entity;
e.flags`, and `memory/barrier.rs` had exactly that in `escape_gain` and
`escape_lose`. It is worse than a plain read, because a `&mut` asserts
uniqueness over the whole struct and an atomic field inside it buys nothing
(`dev/POSTMORTEM.md`, "an atomic field does not survive a `&mut` over the
struct"). Both functions now take one word at a time. Reading found them, the
guard cannot, and the guard's module doc says so.

**Three wide reads survived the first pass**, each eight bytes over a header
and none of them spellable by the grep. `retained::is_occupied` was the worst:
`promote::index_retained_blocks` applies it to the survivors promotion has
just rewritten to `GcHeap`, and `heap::for_each_entity_slot` applies the same
test to the same addresses at the narrow width, so one word was being read at
two widths in one walk. `heap::describe_slot` reported the whole header word
as text and now reports the two mutator halves, the collector's bits having no
mutator-side reader to report them. `stdapi`'s `#[cfg(test)]` free-path
assertion read eight bytes to test four, on every entity of every test build.

**The guard's test exemption covers 187 accesses in 37 files** outside
`refcount`'s own tests, counted the same day by the guard's own patterns, and
its stated reason — headers built on the stack — is false for most of them,
which are factory-allocated entities. That population is the one
a ThreadSanitizer run reaches first, so the exemption is a hole in the
fallback instrument rather than in this one. Closing it is the same job as
taking `RcHeader`'s fields private, and that decision is open: a type would
retire all three evasions the guard admits to — a rename, a local, a reference
binding — at the price of every fixture's shorthand.

---


## 2026-08-26 — what the old collectors left behind is deleted, and what is kept is named

Edmond ruled that nothing of `rc-walk` or `rc-trace` this task does not need
stays in the tree. Six limbs went beyond the two collectors' own files, all of
them unreachable from anything the crate runs:

- `CANDIDATE_INDEX_SHIFT`, `_MASK` and `_MAX` — a position in `rc-trace`'s
  buffer. S30.3's criterion had already called them dead and left them
  standing.
- `EPOCH_BYTE_SHIFT` and `_MASK` — `rc-walk`'s eight-bit maturity stamp, whose
  region S31 re-lays as two bits of epoch, two of maturation age and four of
  reserve. Its test group keeps the narrow-mutator and eager-death contracts
  and marks the flags half with a literal instead.
- `KIND_EPOCH_BEGIN` and `KIND_EPOCH_END` — journal kinds with no site.
  `rc-cycle`'s events get fresh codes at S36.
- `leave_the_candidate_buffer` — an empty function called from two places.
  The duty it marked is real and is S34.3's; the two sites carry it as a
  comment.
- The **storage-version answer** threaded through `trace_cells`,
  `for_each_counted_cell`, `CellReader::walk_outside` and
  `OutsideCells::walk_plain`, and with it `CoherentView.version`. Every caller
  discarded it. It was `rc-walk`'s Phase 3 re-check, S30.2's criterion said it
  dies, and S38.0's reader is specified to answer no version and no give-up.
- The epoch vocabulary in 22 comments across 12 files.

**Two things are deliberately kept**, because the reason to keep them is not
the reason the rest went. `CANDIDATE_KINDS` and `kind_may_close_a_cycle` have
no reader either, but the kind set is the gate `rc-cycle` enrols through and
S31.1 rewrites it rather than inventing it. And the version bracket in
`test_support/outside_block.rs` models the array head's publication window,
which survives in `StorageHead`, and is the mutator half S38.0's Miri slice
races against.

**How the sweep was made checkable rather than read for.** Three mechanical
passes, each returning empty at the end: every `rfc/…md` path cited from code
resolved against the `rfc` tree; every `walk::`, `collector::` and `epoch::`
path resolved against the modules that exist; and an orphan scan for `.rs`
files under `src/` that no `mod` declares. The first found three dead
documents a grep for `rc-walk` cannot see, the second nine stale module paths
S30.2 claimed to have repointed, and the third the one file that compiled
nowhere. A grep for the strategies' names finds none of these.


---


## 2026-08-26 — S28 is abandoned rather than closed, and S29 splits

**Decided:** S28 — flat per-row words for the epoch metadata — stops where it
stands; S29.1 closes with the code it lives in; S29.2 is carried as S39.
**Why:** S28 optimises `rc-walk`'s collector, and S30 deletes `rc-walk`. S29.2
is not a defect of the old collector alone: `rc-cycle` parks slots on the same
enrolment bit, so a thread exiting without draining its queue would reproduce
the leak in the new design.
**Cost:** the measurements S28.1 took are kept in `dev/BENCHMARKS.md`; the stage
itself leaves no survivor, which is why the abandonment is recorded here rather
than being read as completion.

## 2026-08-26 — the header's access width is a correctness rule, and no mutator access spans byte 6

**Decided:** every mutator access to a **published** header is narrow — four
bytes for the counter at +0, two for the mutator's half of the flags at +4.
`header_pair` becomes two loads, `update_header_flags` a 16-bit
read-modify-write, and the teardown guard's `+1`/`-1` touch the counter alone.
The eight-byte helpers are deleted; `publish_header`'s single wide store
stays, being the one access made before the entity is published. A `const`
assertion requires every mutator-visible flag constant to sit below bit 16,
which is what makes the 16-bit read lossless.
**Why:** the collector writes byte 6 one byte at a time, and a 4- or 8-byte
mutator access at +4 overlaps that store without covering it. That is a
mixed-size atomic access: undefined in Rust's memory model whatever it costs,
and Miri rejects it. The entry of 2026-08-15 below called the width a
performance rule with a measurement behind it and kept `header_pair` wide on
that argument; the argument was about which access is faster, and this one is
about which is defined, so it supersedes rather than contradicts.
**Rejected:** keeping the wide accesses and narrowing only the write side. The
Critic round of 2026-08-26 found the plan's own clause guarding writes while
the day-one defect is a read — a write buries the collector's byte, a read is
undefined without burying anything, and only the second is invisible to every
test the crate can write.
**Cost:** `header_pair`'s one wide load becomes two narrow ones on
`ll_cow_separate`'s path, and **that is unmeasured** — the 2026-08-15 figures
priced the opposite direction on the store barrier, not this one. The path is
a copy-on-write separation, which allocates, so the two loads are unlikely to
be its cost; unlikely is not measured, and `dev/BENCHMARKS.md` carries no
entry for it.

## 2026-08-26 — the ring-closing reserve is widened to codes 0–7

**Decided:** the kind codes of the entry below become `Object 0, Lazy 1,
Array 2, Reference 3, String 8, StringDynamic 9, Box 10, WeakRef 11`. Codes
0–7 are held for kinds that can close a ring, four of them free; 12–15 are
free for kinds that cannot. "Closes a cycle" is `flags & 0b100000 == 0`, "is a
string" is `flags & 0b111000 == 0b100000`, and the enrolment gate is
`flags & 0x723 == 0`. `RING_CLOSING_KINDS` is replaced by
`EntityKind::closes_a_ring`, an exhaustive `match` with no `_` arm, plus a
`const` assertion that each kind's classification agrees with its code and a
`debug_assert!` of the same equation in `to_flags`.
**Why:** the entry below held codes 0–3 for ring-closing kinds and assigned all
four, so the reserve reserved nothing and the fifth such kind — a closure
entity, 179 of the corpus's 381 objects being closures — would have taken code
8 and been refused by the mask permanently, with nothing red. A list of members
does not close it either: a kind never added to the list passes every
assertion, which is the bitset's missing line under another name. The
exhaustive match stops the build in the file that owns the answer.
**Rejected:** moving Array and Reference to 4–5 to leave room inside the
class-word prefix 0–1. An entity carrying a class word at +8 tears down through
`dispose`, so a new one needs no kind of its own — it is an Object.
**Cost:** the assignment of the entry below is superseded before any of it
shipped, and `EntityKind` is renumbered twice in one day. Nothing in
`rfc/model/lowering.md` moves: its C mirror names bit positions and no codes.

## 2026-08-26 — the flags word is re-laid for one collector, and `EntityKind` is renumbered

**Decided:** category 0–1, kind 2–5 (four bits), COW 6, arena mark 7, acyclic 8,
owned 9, enrolled 10, escapee 11, weak 12, pending 13, ran 14, free 15, epoch
16–17, age 18–19, collector reserve 20–23, byte 3 free. Kinds become `Object 0,
Lazy 1, Array 2, Reference 3, String 4, StringDynamic 5, Box 6, WeakRef 7`, and
`STRING_OUT_OF_LINE` becomes kind code 5 — *bytes outside the body, whatever the
reason*.
**Why:** the order turns three predicates into mask tests and folds the whole
enrolment gate into one `flags & 0x733 == 0`. The category keeps bits 0–1
because more surviving sites read its value than the kind's, while a mask test
does not care where the field sits.
**Rejected:** kind at bits 0–3, which would have given the teardown jump table a
free index at the category's expense.
**Cost:** this supersedes the refusal of renumbering recorded on 2026-08-07 and
confirmed on 2026-08-13. That refusal was right when renumbering bought one test
for a field nobody was touching; here the field is rebuilt anyway.

## 2026-08-26 — `memory/retained.rs` outlives the deletion of `rc-walk`

**Decided:** the occupancy index of retained blocks moves into `rc-cycle` rather
than dying with the collector that built it.
**Why:** a retained block was filled by an arena's bump allocator — mixed sizes,
no stride — so the address-to-slot arithmetic the shadow rows are built on has
nothing to divide by there. The index is how such a block is addressed at all.
**Rejected:** not tracing retained blocks, which would reinstate the limit
`rc-walk` removed in August: a ring living wholly among promoted survivors would
never be collected — and promoted survivors are exactly where a long-lived ring
forms.
**Cost:** the retained path is a binary search, not the measured 2.6 ns
arithmetic, and its cost is unmeasured.

## 2026-08-26 — the mutator's header writes narrow to a 32-bit counter store and byte flag stores

**Decided:** no mutator write spans the collector's byte; `mutator_update_flags`
and its whole-word stores go.
**Why:** the layout gives byte 2 to the collector, and that only means anything
if the mutator cannot bury it. Today's comment promises the opposite in as many
words — "may bury a concurrent collector byte store".
**Rejected:** inheriting the lossy contract explicitly, where a buried stamp
costs one wasted traversal and never a verdict.
**Cost:** promotion now writes two bytes where it wrote one word, and the order
is load-bearing: `IS_ESCAPEE` is cleared before the category, or a reader sees a
GC-heap entity whose `refcount` still holds an escape hold-count.

## 2026-08-20 — proof-horizon is renamed GC horizon and moves to the RFC

**Decided (Edmond):** the name is `gc-horizon`; the design's normative
text is `rfc/model/gc/gc-horizon.md`, and the three files under
`dev/design/proof-horizon*.md` are banner stubs. The two reading aids
were absorbed rather than moved — the structure catalogue and the
lowering overview into `rfc/model/gc/gc-horizon-states.md`, the worked
`total(Cart)` example into `rfc/model/gc/gc-horizon-cases/README.md`.

**Why:** the design contradicted two normative RFC sections that no
design note could amend, and its case book has to cite the entity RFCs
that live there. The RFC already carries designed-but-unbuilt work
behind status banners (`satb.md`), so "closed pending Phase D" is an
admissible status there.

**The old name is not rewritten.** Entries in this file and files under
`docs/history/` keep `proof-horizon` verbatim: the record describes what
was decided on the day it was decided.

**What the move produced:** a sixteen-case book, and five further open
questions in the algorithm, numbered 7 to 11 in the moved document — the
weak cell's uncounted `target` edge, promotion being a no-op in the
immortal and request-arena categories, raise sites missing from the
placement rule, the inconsistent COW-unique intersection, and runtime
entry points read literally as unsummarized calls.

---

## 2026-08-18 — the per-process key is drawn from the OS, in every build, and is not the hash seed

The crate now holds two secrets and they answer different questions.
`hash/seed.rs`'s seed keys the string hash and is a build constant under
`hash-folding`, where the compiler needs it while it compiles.
`hash/process_key.rs` holds 32 bytes drawn from `/dev/urandom` through
safe `std::fs` at first use, in every build, outside `STAMP` and exempt
from folding: every secret the flood ladder draws comes from it, and a
ladder whose salt an artifact-holder can compute defends nothing
(`rfc/model/maps.md`, "What the flood ladder becomes").

Refused, and recorded because both come back: deriving the ladder's salt
from the foldable seed, which is what the crate did until 2026-08-17 and
which `rfc/model/strings.md` forbids for this key; and
`std::collections::hash_map::RandomState`, which caches one `(k0, k1)`
per thread and then increments `k0`, so any number of words carries 128
bits, and those words share the master the string seed is drawn from.

A consumer takes the key whole, as a keyed hash's key, rather than
slicing words out of it — fixed once in that module's doc for every
consumer, because `draw_salt`'s own doc refutes the bijective word mix
the alternative rests on.

The price, paid knowingly: `#[cfg(not(unix))]` is a `compile_error!`, so
the Windows build refuses until a session on that box adds the door
(`PLAN.md` backlog). Edmond deferred it 2026-08-17.

## 2026-08-18 — a copy sizes its storage by its own replay

Amends the 2026-08-16 ruling recorded in `PLAN.md` S27.5, which had a
COW copy presize to the *source's* slot count so that no bucket the
source kept apart would merge under the copy's narrower mask. The
copy now takes the chunk its own live count reaches through the growth
schedule — `cap = pow2ge(live).max(8)`, `nslots = cap * 2`
(`Table::presize_for_replay`) — and a copy with no live entry takes no
chunk unless it inherited a drawn rung bit, which needs an address to
draw a salt from.

The source-width form was withdrawn because a mask cannot supply the
defence it was bought for. Four families can share a slot, and none of
them is answered by width: an integer key and a string key in a
reseeded-but-weak table are scattered by the copy's own drawn salt, a
string key in an escalated table is re-keyed by it, equal cached hashes
collide at every width and are the equal-identity trigger's business,
and a copy with no rung bit has both rebuilds in hand. Where the salt is
recovered — the timing oracle `rfc/model/strings.md` concedes, or the
address recycling `Table::draw_salt` names — a colliding set is forged
against a wide mask as cheaply as against a narrow one, so the two fall
together there too.

What the withdrawn form cost: a copy of a source grown to 40000 entries
and then emptied carried 512 KiB of slots behind a handful of live ones;
`cap` set to exactly the live count made the first insert after every
copy grow and rebuild; and in the request arena, where nothing is
reclaimed until the reset, the wide ask was a refusal of the program's
write rather than a cost. Sizing by the replay inverts the last of those
— one right-sized chunk instead of every chunk of a doubling replay, all
of which the arena keeps.

Kept from the withdrawn form: the presize itself. It saves the
intermediate chunks, and it is what gives a reseeded copy of an emptied
source the address its salt is drawn from.

## 2026-08-17 — rung one salts every kind's slot, under the per-process key

Amends 2026-08-13, "the flood ladder's two rungs answer different key
kinds": the rungs still answer different failures — differing
identities against equal identities — but the first rung's mix now
covers string slots as well as integer ones, and the salt is drawn as
a keyed hash of the storage address under the per-process key
(`src/hash/process_key.rs`) instead of through the foldable seed. Two
reasons forced it. Under `hash-folding` a cached string hash is a
build constant, so the old rung rebuilt an offline-built string chain
into exactly the same chain; and a salt derived from the foldable
seed is computable by anyone holding a folding artifact. `strong_hash`
keeps its placeholder construction, now keyed by the process key
mixed with the table's salt; the HighwayHash long-key slot stays owed
(`PLAN.md` backlog, "The long-key slot itself"). Rejected: salting
only at escalation — it leaves the chain rung a no-op for strings.
Cost: one extra splitmix on a reseeded table's string path,
unmeasured, and this box cannot resolve an effect that size
(`dev/BENCHMARKS.md`, noise floor).

## 2026-08-17 — the no-RC research round closes: unique ownership survives it

Edmond's ruling on the 2026-08-16/17 GC research round. What survives
is in `rfc/model/gc/rc-walk.md`, "The birth count" and "Unique
ownership": a statically known in-degree written by the factory, and a
one-owning-slot policy with no count, eager death, and COW
eligibility. What is rejected: appeal-walk and the published-epoch
barrier as replacements for rc-walk — the armed barrier costs about
what the RC pair it removes costs, the win is confined to the idle
path, and the benefit is bounded by the COW share of publications and
the epoch duty cycle, both unmeasured; the shared-anchor
generalization — superseded by the strict unique-owner form, since
sharing keeps the sealed-topology proof while forfeiting eager death
and COW eligibility; and the deferred-count window — a missed release
is unrecoverable by any later scan, because the overwritten value is
gone, and an understated count lets eager death and the COW uniqueness
test fire on live shared data. The three sketches
(`SELECTIVE_RC_WALK.md`, `APPEAL_WALK_GC.md`,
`NO_RC_PUBLISHED_EPOCH_GC.md`) are deleted with this entry;
`RC_WALK_CRITICAL_REVIEW.md` stays, its findings being implementation
debts rather than rejected ideas. Implementation of the surviving pair
is gated on a Phase D measurement of the provable-target share
(`PLAN.md`, backlog).

## 2026-08-16 — the performance case's external comparand is a canary, not a self-authored floor

The strategy is Edmond's: a naive, clean C or C++ loop does the same
job as fast as the machine allows, compiled into one binary with the
same operation through the real C ABI — `bench-external/canary/` — and
the case's claims are stated against that bracket rather than against a
floor derived from our own contracts. Two Critic rounds on the S26 plan
draft forced the substitution: a floor authored by the party it grades
is pinned by nothing, and every gap it would read on these paths sits
under the instruments' own unexplained terms.

Three rules travel with every canary figure. The ABI call is a real
call in the probe while the production route inlines through merged
bitcode (`README.md`, "LLVM IR export"), so the bias runs against our
arms and a "within X of C" reading is conservative. Every arm passes
disassembly acceptance re-run after every rebuild (`accept.sh` beside
the probe), because an optimizer deletes a naive loop first, and a
deleted canary reads as a floor while pricing nothing. And a bare
canary prices *its loop*, never "naive RC": it carries no flags test,
no immortality gate and no null path, so it is quoted as a bound, not
as a runtime.

The first probe and what it measured: `dev/BENCHMARKS.md`, 2026-08-16,
"the pair against its canaries". The Zend comparison stays out of the
case until Phase D exists; the end-to-end claim is recorded there as
the missing half, not delivered by canaries.

## 2026-08-15 — a header is read as narrowly as it is written, and through the helpers only

Under `rc-walk` every mutator access to a published header goes through
`refcount`'s helpers, and each helper's width matches the width of the store
that precedes it: `header_flags` and `header_refcount` load four bytes,
`header_pair` loads eight and is reached where nothing narrow precedes it.

**The width is a performance rule with a measurement behind it, not a
correctness one.** A load wider than a fresh overlapping store cannot take
the value out of the store buffer, and the crate paid for that once already
(`dev/BENCHMARKS.md`, 2026-07-27, retain/release at 10.2 ns against 2.78).
The same pairing had returned on the store barrier's path, where `ll_retain`
writes the counter half and the category test read all eight bytes after it;
narrowing the two accessors took `heap → arena` from 4.82 to 1.53 ns per
store and put `rc-walk` below `rc-trace` on a direction where it had been
2.2x above (`dev/BENCHMARKS.md`, 2026-08-15).

**`header_pair` stays wide and keeps its one caller.** A predicate over both
halves — `cow_separation_needed`, reached from `ll_cow_separate` and
`array::entity::needs_separation` — takes one load rather than two, because
neither site has a narrow store in front of it. It buys no coherence the two
narrow readers lack: the collector's only claim on a published header is the
epoch byte, which no such predicate reads.

**Reaching past the helpers to the field is what the guard forbids.** Not
because a plain read gives a wrong value, but because it races the
collector's byte store and is invisible to every runtime check;
`refcount::tests::who_may_read_a_header` reads the sources for it, and
ThreadSanitizer is what exhibits it (`dev/WORKFLOW.md`). The `rc-trace` arm
of a `#[cfg]` pair is exempt: that build has no concurrent collector.

**Refused with it, and not to be reproposed without an instrument that
resolves the escape direction to better than a percent:** merging the
barrier's two flag reads into one snapshot. Measured, moved nothing, and
could not have — the second read lives inside the escape branch that neither
measurable direction takes (`dev/BENCHMARKS.md`, 2026-08-15, "one header read
for the store path").

## 2026-08-14 — the reset reads no corpse

A survivor of a reset can die inside that same reset: a heap `&` box
stored into an arena slot is promoted with the entity it holds, and the
box's logged release tears that entity down while the reset still has
passes to run. Two passes then read what died — `retrace_survivors` and
`reconcile_cow_counts`, both through `walk::trace_entity` — and the weak
walk reads one header word of every entity the arena's weak log names.

**Death becomes membership**, and each pass tests it before it walks an
entity. `reset_window::record_death` is called on `ll_object_die`'s
dispose-true arm, which is the one door a resurrection can turn back, and
at the teardown body of each of the other three kinds, none of which runs
user code that could resurrect. The set is shared by the whole window
chain, because a survivor of an outer reset can die inside an inner one
and it is the outer reset's passes that must not walk it.

**The header was the first test and does not decide this.** Refcount 0
alone calls every internally-reached survivor a corpse, `mark_one`
zeroing one before the counting pass rebuilds it, which is what the
category half was added against. The pair fails too: an escape whose last
hold is dropped by a destructor inside the fixpoint is promoted reading
refcount 0 in the GcHeap category while its edges are live.

**The memory stays mapped for the length of the reset.** A shared
retained block recycles nothing inside itself, so a corpse's header
survives where it is; a survivor in a block of its own hands a run back
to the system at its death, and every later reader of that address then
reads memory the process no longer owns. So the window parks the two
large-entity kinds and frees them after it closes, ahead of the epoch's
own parking — parked in `deferred_free` instead, a checkpoint's flush
could free the body while the reset still runs. An inner close hands its
parked bodies to the window outside it, and only the outermost close
frees anything.

**One free is absorbed rather than deferred**: a corpse in a retained
block this reset has not registered yet. `retained::register` declines to
count an occupant whose header reads zero, so no count exists for that
death to spend, and replaying the free after the epoch would take the
block's live count below its true occupancy and hand it to the pool under
living survivors.

### The COW count: `edges_live + (now - at) + D - K`

`reconcile_cow_counts` settles each COW survivor from the edges the walk
finds now, plus what changed since promotion, plus two correction terms.
The skip and the terms are one repair and neither half stands alone.
Skipping a corpse without them settles a live entity one too low: the
corpse's release is already inside `now - at`, and dropping its edge
removes the same event a second time. Carrying the terms without the skip
settles it one too high: teardown leaves a slot readable and stale, so
the walk counts an edge the escrow has already restored.

**The rows themselves are not filtered**, only the walk, because a COW
survivor cannot become a corpse of the reset that promoted it:
`count_children` gives it one retain per holder edge it finds, and each
holder's teardown spends exactly one, so its count never falls below what
it was before the reset began. A COW entity is also never an escapee —
the store barrier copies it out of the arena rather than counting it in
(`barrier::escape_gain`) — so no `&` box can kill one either.

**D is the corpse's promotion-time snapshot**, taken inside
`count_children`'s existing walk and paid into the escrow of the window
that took it when the holder's teardown completes. The instant is the
whole of it, because it decides which column absorbed the retain: an edge
held since promotion has its retain in the discarded `at` and needs a +1
back, while an edge taken after promotion has retain and release both
inside the delta and must not be compensated. A snapshot taken at the
door of death answers neither case — it sees the edges the corpse holds
at the end, and both counterexamples turn on the difference between that
set and the set at promotion.

**K takes back the compensating retain** `count_children` gives an
already-promoted COW child in a later round. That retain lands in the
child's delta while the edge behind it is walked as well, so the pair
would count twice.

**A resurrection earns none of this.** Teardown that does not complete
records no death, so the passes keep reading the entity and its snapshot
stays with the window. The count is a poor witness of that: where the
entity's edges at the would-be death are the edges of its snapshot, an
escrowed edge replaces exactly the edge a skip removes and the figure
agrees either way. So the test reads the record, and the two Miri cases
read the memory.

## 2026-08-14 — the arena keeps its single bump, and two ways of making it walkable are refused

Asked because the arena carries five logs — `escapees`, `destructors`,
`weak`, `release_at_reset`, `larges` — written during the request and
drained by the reset, and three of them name an arena entity plus a
reason that is *also* a flag in that entity's header. The duplication
looks like something an enumerable arena would remove. It is not.

**A walk cannot replace the escapee log, at any price.** An escape is a
state change on an already-allocated entity: `barrier::escape_gain` fires
when a longer-lived container takes the reference, and the entity's
position in arena memory says nothing about when that happened. The
fixpoint drains that log once per round because destructor bodies create
new escapes on old objects, so a walk would have to rescan the whole
arena every round — arena bytes times rounds, against records today. The
same holds for `weak`. Position can only carry what is known at
allocation time.

**A per-allocation tag word is refused with it.** It is the only
mechanism that would cover all four populations the one bump serves —
entities, out-of-line bodies, raw C memory from `ll_arena_alloc`, and the
log segments themselves — because the C ABI door hands out memory with no
header this crate controls. Eight bytes on every allocation and a store
on the bump path, to serve events the design makes rare on purpose.

**Size-class blocks for arena entities are refused separately**, and on a
checked fact rather than on price: `RcHeader::new` writes `refcount: 1`
into every entity of every category, and nothing releases an arena
entity, so a former-arena block's holes do not read empty under
`heap::for_each_entity_slot`'s `refcount != 0` test — a never-escaped
corpse reads 1 and an escapee whose holders let go reads 0. Handing such
a block to the heap would present corpses as live and trace their slots
into blocks already returned to the pool. It could only be handed over
after a reset-time sweep, and the sweep needs the per-block survivor
inventory the promotion loop already builds — so segregation moves that
record's consumer rather than removing it. The one genuine win it offers,
and the reason to reopen this only with a measurement in hand, is that a
swept block's holes would re-enter circulation instead of waiting for the
last survivor to die (`rfc/model/memory/arena-reset.md` accepts that wait
in writing).

**What stands from this.** A new per-entity obligation of the reset gets
a flag for the O(1) "is this one?" test *and* a log for the "which ones?"
enumeration; the pair is the design. The one door that would genuinely
retire the retention machinery is evacuation, and it is closed by
decision already (2026-07-24, the movable proxy; 2026-08-03, the index
over reset-time copying).

## 2026-08-14 — an object owing a destructor is allocated from the far end of its block

**Supersedes** the destructor log's reason for existing. Placement, not a
record, is what tells the reset which objects owe `__destruct`.

`Arena::alloc_entity` gains a second door for classes carrying
`CLASS_HAS_DESTRUCTOR`: the block is filled from both ends, ordinary
allocations bumping up from `bump` and destructor-bearing objects down
from a floor cursor. The front's "block is full" test compares against
that floor instead of the block's end — the same load and the same
comparison against a field that now moves. A retiring block records its
final floor in its header's private half, read by the reset alone on the
same thread.

**The reset walks `[floor, block end)` by stride.** Every object carries
its class pointer at offset 8 and `Class::object_size` gives the stride,
so a contiguous run of objects needs no record and no link word; strings
and arrays never land there, having neither a class pointer nor a
destructor. Per object the walk runs `__destruct` when
`DESTRUCTOR_PENDING` is set, `DESTRUCTOR_RAN` is clear, and the category
is still `RequestArena` — a promoted survivor owes its destructor at its
own death instead. Inside the fixpoint each round remembers the previous
floor and walks only what grew, which is the take-semantics the log gives
today; the final pass is O(destructor-bearing population), the semantic
floor no mechanism escapes.

**An object past `BLOCK_PAYLOAD` lands in neither end**, taking a run of
its own, and is covered by the `larges` log the reset already visits plus
the same flag test. That test lands with the change: without it a
`__destruct` is silently skipped, the failure mode
`rfc/model/memory/large-entities.md` already flags for the `forget_large`
arm.

**What dies with it:** the `destructors` log and its drain,
`Arena::track_destructor`, and `object::object_constructed`'s ability to
fail — a link needs no memory, so the 2026-07-21 rule that a refused
destructor record fails the creation loses its subject and the `bool`
leaves that ABI.

**What was refused in its place.** A singly-linked list threaded through
the objects: eight bytes per object against zero, a stale link word in
every promoted survivor, and — deciding it — a chain that walks into a
large survivor's run already unmapped by a death inside the same reset,
so the list would be sound only on top of a repair the walk does not
need. Reference counting for these classes is a separate question, not
decided here: it changes observable destructor timing, which is the
RFC's to answer, and it needs the `refcount` word split from the
`IS_ESCAPEE` hold count.

**Unmeasured, and named as such:** that the front door's swapped limit
costs nothing is an intention until `cargo bench --bench alloc` says so
back to back (`dev/BENCHMARKS.md`).

## 2026-08-13 — the reset holds a pin of its own, and releases it after the index is real

**Supersedes nothing; it completes** "a pinned block goes home when its
last payload is freed" below, which decided who spends a pin and left
open when a pin may be spent.

**A pin taken during a reset is unspendable until that reset has finished
establishing occupant counts.** The reset raises one payload count of its
own per block it pins and releases it after `finish_reset`. Until then
the block cannot empty, whatever arrives.

The reason is an asymmetry between the two populations that hold a
retained block. The occupant count is built at the very end, at
`index_retained_blocks`, and that lateness is what makes the occupant
side safe: an occupant dying mid-reset finds no entry and reports
nothing. The pin side has to publish at the refusal, which is early, so
between the two moments `live` reads zero for every block — and a
payload freed in that window emptied a block on paper while survivors
were still living in it. The window is not theoretical: the heap box
behind `&` produces a survivor whose logged release kills it inside the
same reset, and its teardown frees the very bytes the refusal pinned the
block for.

**The release leaves the index in place**, which is why it is
`retained::reset_pin_released` and not `payload_freed`. The latter drops
the index when the count reaches zero, and `ll_free`'s retained arm
answers off the index — a block freed through it with no index reads as a
block the registry knows nothing about and never reaches the pool. The
new door leaves the index exactly as `register` leaves it, and a block
that empties on the release joins the `emptied` vector the reset already
drains through `ll_free`, because no later death exists to report it.

**Zero occupants at that release means "nothing indexed holds this
block", not "every occupant died".** A block holding bytes but no
survivor is never registered at all, so its `live` stays zero for good.
Both shapes are correct on the same reading, and the counter is the whole
of the state that distinguishes them.

**The reset asserts the count it spends is its own.** Of the three doors
into these counters this is the one where a miscount ends at the block
pool rather than at a `false`, so the debug assertion sits there rather
than being left to the layer that would discover it half an hour later in
a recycled block.

## 2026-08-13 — the arena carry is the group's sixth member, and a refusal answers the bytes it left behind

**Supersedes** the closing clause of "a hooked class draws its storage
under its own category, and the arena carry waits" below: the refusal in
`ll_object_new` is gone, and a class with cells outside its body may live
in a request arena.

**`OutsideCells` grows a sixth member**, `carry(arena, entity)`, called
from the reset's survivor loop before the category is rewritten — while
the category still describes where the storage lives. It answers three
things rather than two: `Carried`, `Refused { memory }`, and `Nothing`.

**A refusal answers the bytes, not their block.** Only the class knows
where its storage is, so the refusal carries that address and the reset
masks it into a block header itself — `promote::block_holding`, the one
place that mask is written. The first shape had the class answer a block,
and a class handing back the storage pointer it already holds would have
stamped a block kind over its own first word, left the real block
unretained and filed a pin under a key nothing would ever look up.

**Promotion classifies once.** It used to classify a survivor twice —
once to carry, once more to find the address to pin when the carry was
refused — and its own doc guarded against the two disagreeing. Each arm
now produces its own answer out of the classification it already holds,
so the two cannot disagree for any kind.

**Only memory inside a block of this arena may be refused.** An
allocation the arena took directly from the system has no block of the
arena's and the reset frees every one it logged, so such storage is
transferred rather than refused — the arena forgets the record and the
address does not move.

**The corpse owes nothing**, which is what the category rule below bought:
its storage is arena memory and dies with the pages. A per-corpse free at
reset was refused as unbuildable — the reset enumerates only the
destructor log, so it would need a registration of every hooked instance,
paid on every allocation.

**Promotion still learns no layout.** `external_memory` is its sanctioned
kind switch, and the new arm calls one function pointer out of the group
exactly as the string and array arms call `carry_payload_out_of` and
`carry_storage_out_of`. Object **and** Lazy, because a subclass inherits
the group.

**A test asks `retained::pins` rather than the block's kind.** A block
holding a survivor is stamped retained whether or not a refused carry
pinned it, so the kind answers a different question; the first version of
the refusal test passed with the refusal naming no block at all.

---

## 2026-08-13 — a hooked class draws its storage under its own category, and the arena carry waits

**Decided** by the Sage, on a hole a Critic found while S18.3 was being
finished, and it tightens rather than reopens "a class with cells outside
itself carries one flag and one group of five" below.

**The storage of a class with cells outside its body is drawn under the
instance's own memory category**, through `memory::routing::body_alloc`,
the way a table's storage already is. The earlier clause said only "from
the memory manager", which is too weak: it answers the parking question
and leaves the reclamation one open. The category is what decides who
frees the storage of an instance that dies without a teardown — an arena
object gets phase 1 at reset and nothing else, so storage drawn under any
other category is storage nothing ever gives back.

**A per-corpse free at reset is not the answer, and could not be built.**
The reset never enumerates the dying: a bump-filled block has no stride,
and the only inventory it holds is the destructor log, which names
destructor-bearing objects alone. A free per corpse would need a
registration of every hooked instance, paid on every allocation, forever,
for every map.

**The survivor is the array's problem, and it takes the array's answer:**
a carry, the analogue of `array::entity::carry_storage_out_of`, reached
from `promote`'s existing kind switch through `Class::outside_cells`.
That is a sixth member, so it changes the settled group's shape and takes
a stage of its own — S19 in `PLAN.md`, opened before any map stage, with
a superseding entry here when it lands. Dispatching on the kind does not
breach promotion's invariant: the invariant is that promotion learns no
*layout*, and `external_memory` is its sanctioned kind switch already.

**Until then `ll_object_new` refuses the pairing**, with a
`debug_assert`: it is the one door that sees the class and the category
together. Not a returned null, which in this crate means out of memory
and would lie to the creation site; not a build-time refusal, the
category being a runtime argument. `Immortal` is not refused — such an
instance never resets and never tears down — and `LongLived` is already
marked out of use.

**The refusal is temporary by design.** `rfc/model/maps.md` leans on
arena-resident maps in load-bearing places, the escape copy and the
pointer-tag budget among them, so a permanent refusal would refuse the
RFC.

---

## 2026-08-13 — a class with cells outside itself carries one flag and one group of five

**Decided:** the descriptor gains a `CLASS_OUTSIDE_CELLS` flag and one
pointer to an immortal group of five function pointers. Not a family of
independently nullable fields: three nullable pointers make eight states
of which two are coherent, and the incoherent ones fail silently — a walk
without a sever corrupts a table entry at collection, a sever without a
walk makes every child of the table a computed root and leaks the ring
quietly.

```rust
struct OutsideCells {
    walk_plain:   unsafe fn(*mut u8, *const Class, &mut dyn FnMut(Cell)) -> Option<usize>,
    #[cfg(feature = "rc-walk")]
    walk_relaxed: unsafe fn(*mut u8, *const Class, &mut dyn FnMut(Cell)) -> Option<usize>,
    recheck:      unsafe fn(*mut u8, *const Class, walked: usize) -> bool,
    sever:        unsafe fn(*mut RcHeader, &mut Vec<*mut RcHeader>),
    free:         unsafe fn(*mut RcHeader),
}
```

**The flag is the predicate and it is free.** `for_each_counted_cell`
already loads `flags` for `CLASS_TEMPLATE`, so the test rides a word
already in a register and a class without outside cells loads nothing
extra. `Class` stores the group as `*const ()` and transmutes, as it
already does for `dispose`, because `Cell` is crate-private and `Class`
is public.

**One group rather than loose fields also keeps the descriptor's shape
build-independent.** `Class` is `#[repr(C)]` with its vtable starting at
`size_of::<Class>()`, so a field that exists only under `rc-walk` would
move the vtable, every itable and every generated descriptor between the
two strategy builds. Inside a crate-private group the `cfg` costs
nothing.

**Two walk pointers, because a function pointer cannot be generic.**
`for_each_counted_cell` is generic over `CellReader` and monomorphizes to
a bare stride; a pointer in a descriptor has to have its reader chosen
before the call. The class installs both instantiations of one generic
body, and `CellReader` gains the associated function that picks the
member — the trait already being the one place the two readers differ.

**The visitor crosses as `&mut dyn FnMut`, one indirect call per cell.**
A real departure from "why tracing stays data", accepted because only a
class with outside cells pays it; the runs keep their monomorphized
stride.

**`recheck` exists because Phase 3 cannot ask a non-array for a version.**
The re-check finds the version by casting the entity to `LLArray` and
taking the head at +8, which on an object is the class word: a map would
answer `class_ptr != walked`, be acquitted every epoch forever, and
materialise a 40-byte reference over a body that may be 16. So the walk
answers `Option<usize>` and `Epoch::row_still_has_its_cells` dispatches:
Array keeps the head, a class with the flag asks `recheck`, everything
else stays `true`. `None` means no cell came out of versioned storage.

**The sever hook takes the entity and hangs off the drain's arm, not
`sever_counted_slots`.** The object sever the collector actually runs is
`walk::sever_cells`' `OBJECT | LAZY | REFERENCE` arm, which traces and
calls `empty_cell` per cell; `sever_counted_slots` has one caller in the
crate and it is static-block teardown, which has a layout and no entity.
Emptying a table entry cell-wise is wrong twice: a whole-Box store zeroes
the reserved bytes carrying the collision link, which then reads as entry
index 0 and closes a chain on itself, and a null in the key word reads as
an integer key rather than a hole, leaving a live entry and a stale
count. A static block therefore may not be laid out by a descriptor
carrying the flag, and the builder asserts it.

**`free` exists because rc-trace frees the white set itself.** It does
not call `dispose` there by design, and dispatches per entity kind for
kinds owning memory outside their slot; `Object` falls to the default
arm, so without this member a cyclic-garbage map's chunk is never freed
and holds its block's live count above zero for the life of the process.
This is the member S18.3's criterion needs, not the sever.

**The block a hook yields cells from must be parkable.** A block whose
cells the collector recorded may not be freed while an epoch is in
flight, and the parking machinery takes a freeable block kind or a
buffer-arena chunk — a `std::alloc` allocation cannot be parked at all.
So a hooked class draws its outside storage from the memory manager, and
`free` parks rather than frees during an epoch. This is a constraint on
the contract, not a note.

**The hooks supplement the generic stride; they do not replace it**, or a
subclass's own properties go untraced. `for_each_counted_cell` strides
the runs and then calls the walk hook, and the template arm becomes a
third arm rather than an early return, so a template class with outside
cells is not silently skipped.

**Inheritance is not what `dispose` does today, and that is a defect
S18.2 fixes with this.** `ClassBuilder::build` seeds `ptr_runs`,
`box_runs`, `props` and the vtable from the parent and does **not** seed
`dispose`: a subclass declaring no dispose of its own gets
`ll_default_dispose`. For a map subclass that is a leak of the whole
table. The group and `dispose` are both seeded from the parent, and a
subclass that declares either replaces it wholesale.

**A give-up is the racing reader's alone.** The walk may answer `None`
having yielded nothing when it cannot get a coherent reading of its head,
and that is safe for the collector, where a missed edge only pins its
target. It is not safe for the arena reset: `reconcile_cow_counts`
*assigns* a survivor's count from the edges the trace finds, so a hook
that gave up there writes a count below the truth and the next release
frees a live entity. The plain reader has no writer to race and never
needs to give up, so `CellReader` carries an associated constant saying
whether it may be raced and the give-up asserts on it.

**The consumers, counted honestly.** The walk hook serves the quiescent
tracer and the collector's relaxed one from `for_each_counted_cell`, and
teardown's release through the same door — but only for a class that
keeps the default dispose, and a map overrides it, so for the class this
is built for teardown goes through the class's own body. `recheck`,
`sever` and `free` each have one call site of their own. The claim that
one door serves all three consumers was true of the hook S18 was raised
for and is not true of this group.

## 2026-08-13 — the flood ladder gets kind-dispatched triggers, a third rung, and a key it does not have

**Decided, by the Sage, after two Critic rounds on the map design:** the
array table's ladder is wrong in its triggers, not only in the bodies the
map work first narrowed, and the repair is the table's rather than the
map's. Written in full in `rfc/model/maps.md`, "What the flood ladder
becomes"; recorded here because it obliges `array/table.rs` before any
map exists.

**The two triggers measure two failures.** The chain trigger sees
separable identities sharing a bucket, which a salt answers for any kind.
The equal-identity trigger sees identity numbers that agree while the
keys differ, which only a kind whose identity is a lossy hash can
produce. `insert` counts an entry when `!e.is_int_key()`, which under
four key kinds admits an array entry, so eight arrays with equal content
hashes fire the *string* escalation. Both rungs then return early
forever, and the chain grows without bound with nothing left to rebuild.

**The ladder becomes three rungs.** Rung one, the salt, covers every
kind. Rung two, the keyed byte hash, is the string's alone in trigger as
well as body, the string being the one kind with both a colliding
identity and a re-derivation that allocates nothing. Rung three is
refusal: the insert declined with the table unchanged, raised as a
catchable error and distinguishable from an allocation refusal. It fires
where a trigger trips and no rebuild remains, and it is the structural
backstop `rfc/model/strings.md` has promised since before the map design.

**It stands on a key the crate does not have** — 32 bytes from the OS
once per process in every build, outside `STAMP` and exempt from
`hash-folding`. `strong_hash`'s doc names the slot and stands in for it,
and `draw_salt` hashes a recyclable storage address under a seed that
folding turns into a build constant, so today's per-table salt is a
public function of one address. Until the key exists, every rebuild the
ladder performs is aimable in a folding build and only rung three is
real.

**The obligations, in `array/table.rs`:** the trigger becomes a
tag-equality test; both `slot_hash` and `entry_slot_hash` dispatch on the
tag with the byte branch asserted unreachable from any other; `draw_salt`
draws under the per-process key; `strong_hash` becomes the keyed function
its doc promises; and the early returns in `reseed` and `escalate` are
replaced by rung three.

## 2026-08-13 — a map is a runtime class over the generic table, not a new entity kind

**Decided:** `Map` is an ordinary object — entity kind `Object` — of a
class the runtime provides, holding the crate's generic `Table`. It gets
no kind code of its own and it is not a fourth storage strategy of the
array kind. Edmond's, and it is the third answer to a question the plan
had posed as a pair.

**What it buys.** Teardown is the class descriptor's `dispose`, the
candidate gate already admits `Object`, promotion and the walk already
serve objects, and `Table` is reached as it was built to be — it
allocates no entity, calls no store barrier and names no entity kind, so
a second customer costs it nothing. A new kind code would have bought the
opposite: an arm in every kind switch in the crate, each one a place
where an omission leaks rather than fails, out of a code space where `7`
is reserved and `4`–`6` are the family the RFC wants consolidated.

**What it needs, and it is already planned.** A map's cells are its
entries, and they live in a chunk outside the entity, where `ptr_runs`
and `box_runs` cannot describe them. So a map is the second customer of
the optional `walk` hook on the class descriptor, S18, raised for a
coroutine whose waker cells lie outside the object the same way. The
hook's two rules are the map's too: it yields cells rather than children,
because Phase 3 re-reads a recorded cell; and the chunk goes through
`deferred_free` while an epoch is in flight.

**What it forecloses.** Array-like value semantics by default. That is
the entry below.

## 2026-08-13 — a map is a reference by default and a value by attribute

**Decided:** copy-on-write is an attribute of a map's class, not a
property of the type. Without it a map behaves as any object does: `$b =
$a` is a second name, and a write through either is seen through both.
With it, assignment must yield an independent map, which is what the
attribute means and the only reason the copy exists.

**What a copy copies is the container.** Separation on write builds a new
map entity and a new chunk, moves the entries across, and retains every
key and every value — nothing below the entries is duplicated, exactly as
the array's shallow separation works. The deep copy is the other door: a
map taken out of the request arena by a longer-lived holder copies each
arena COW child out, takes the hold-count route for an arena object or
reference box, and merely retains anything already long-lived. Keys and
values go through that door on the same rule; there is no asymmetry
between them.

**The class owes two bodies for it.** A COW map's separation is the
descriptor's `clone` and `deep_clone`, the slots the lifecycle family
reserved and never filled, and the deep one drains an explicit list
rather than the machine stack — nesting depth is the program's input on a
store path, which this crate has ruled on twice.

## 2026-08-13 — Map and MapMixed are two classes, and the split is identity against content

**Decided:** two classes, with no inheritance between them. `Map` admits
object keys only. `MapMixed` admits integer, string, object and array
keys. Integer and string keys with reference semantics are not a gap
worth a third class: an array already serves them.

**The split is where equality comes from.** An object key is equal by
address, so `Map`'s lookup path has no key kind to dispatch on, no
numeric-string canonicalisation, no string-key ownership, and one shape
of counted child. An array key is equal by content, which is what drags
in the content hash and the recursive walk; `MapMixed` pays for that and
`Map` never links it. This is the shape `SplObjectStorage` has, and it
falls out of the split rather than being designed for.

**Why not one class with a set of admitted key kinds:** the admitted set
would be tested on every write, the content-hash machinery would be
linked into maps that never see an array key, and hashing a key would
become a dispatch before every lookup.

**Why not inheritance:** it runs the wrong way. `MapMixed` admits more
keys, so substituting one where a `Map` is expected is safe for writing
and unsafe for reading — a reader of `Map`'s keys may assume they are
objects.

## 2026-08-13 — an object key is indexed by a rotation of its id, and inherits the flood ladder

**Decided:** an object key's bucket comes from its `spl_object_id`
rotated, not from a salted mix and not from the id as it stands. The
rotation is what a fixed slot stride needs: entity slots are aligned and
evenly spaced, so the low bits of an address carry almost no entropy and
neighbouring objects would land in neighbouring buckets. The rotation
amount is `MapMixed`'s and `Map`'s to settle against the stride, which is
arithmetic rather than a decision.

**Against a deliberate flood the rotation does nothing**, being a
bijection with no salt, and it does not have to: an object key is a key
like an integer key, and the table's existing ladder covers both. A table
starts unsalted, counts entries whose full 64-bit hash equals the new
key's, and escalates once to a keyed hash. Nothing new is built for maps.

## 2026-08-13 — an array key's content hash lives in the map's entry

**Decided, by the Sage:** the hash of an array key is stored in the map's
own entry, in `hash_or_key`, and the array entity gains no field, no bit
and no byte. It is computed once on the insert path by a content walk
that works through a `WorkList` in a buffer-arena chunk and refuses like
any allocation, never on the machine stack. A lookup hashes its probe
afresh, which is the O(size) a value key costs anyway, the confirming
comparison being O(size) regardless.

**Nothing invalidates it, and nothing can.** The entry retains the key,
so any prospective writer names an array whose count is at least two, and
`cow_separation_needed` makes that write separate first: the map's key is
never mutated in place. The freeze is transitive — a nested child with
any external name has a count of two itself, and one at count one is
reachable only through the frozen parent. The insert-path window where
the count may still be one is closed by a different argument: hashing
runs no user code, so the walk and the retain are one uninterrupted
mutator sequence. An escape copy out of the arena preserves the number
too, naming content rather than an address.

**The entry survives the table's own moves** because growth and
compaction copy whole entries and `rebuild_index` re-reads the stored
hash rather than the key's bytes — the mechanism a string key's cached
hash already rides.

**The closest alternative was a lazy cache in the array entity**, guarded
by a "not computed" bit as a string's hash is at +16. It died on the
header-bit ledger: no free bit exists in either build configuration —
0–14 are assigned, and 15 with the top half is rc-trace's candidate index
for every kind that can close a cycle, Array among them. Behind the
ledger stood the price it was never worth: an invalidation store on every
array write, a path that has nothing to do with maps, paid by the
overwhelming majority of arrays that never become a key.

## 2026-08-13 — an arm with no producer is not built, and strategy 1 is the standing case

**Decided:** a representation, a kind arm or a numbered vocabulary entry is
built when something produces it, and not before. Storage strategy 1, the
typed vector of `rfc/model/arrays.md`, is the standing case: nothing stamps
`StorageTag::Typed`, so every site that could meet it answers `unreachable!`
(`array/element.rs`, `array/entity.rs`) and the walker refuses it by name
(`walk.rs`). The 1 → 2 transition stays out of the array work for that
reason, and whoever opens it confirms the state against `arrays.md` first,
because the document describes a transition the crate has no source for.

**Why:** an arm nothing produces is covered by no test that runs, so it
rots at the speed of the code around it and is discovered by the first
caller rather than by the suite. The two earlier applications of the same
rule are recorded in their own entries — the candidate gate's kind set
(2026-08-07) and the journal's event kinds, where numbering the whole
vocabulary ahead of the sites was refused (2026-08-08).

**The boundary is a stage, not a commit.** S7 shipped one step in which
`Vector` had no producer, deliberately, and paid it inside the same stage;
the entry below on the factory's stamp says why the order could not be
reversed.

## 2026-08-13 — a factory's default flips after the layer above it dispatches, not before

**Decided:** when a new representation joins an existing one, the layer
above it learns to dispatch on the tag first, and the factory's default
moves second. `ll_array_new` kept stamping the ordered hash while
`array/element.rs` gained `element_at`, `append_cursor`,
`representation_for` and `store_into_vector`, and only then stamped the
mixed vector.

**Why the other order cannot be run:** a fresh array in the new
representation meets every existing test through a layer that does not yet
know the tag, so the suite fails wholesale on a shape that is correct — and
the failures say nothing about the step being made. The cost of the chosen
order is one step in which the new representation has no producer, which
contradicts the entry above only in appearance: it is paid and closed inside
one stage rather than shipped.

**What the flip itself cost:** 137 fixtures, split by the rule in the
2026-08-12 entry on `hash_array`. Count the factory's test call sites before
planning a default flip; the production doors are the small half.

## 2026-08-13 — flipping the factory's stamp is how a representation-blind door is found

**Decided:** before a new representation gets its producer, the stamp is
flipped early on purpose, the suite is run, and the failures are read as the
inventory of doors that name one representation. The flip is then reverted
until the step that owns it.

**What it found in S7.2:** `separate` and `fill_from` reached for the table
through `as_table` and `as_table_mut`, so a vector array meeting either door
asserted in a debug build and read `Table` fields out of `Vector` bytes in a
release one. Seventeen `array::element` tests failed and
`nesting_worked_through_a_list::a_deep_arena_array_is_copied_out_through_the_work_list`
took the run down with a SIGSEGV, the panic crossing the work list's frame.

**Why it is worth keeping as an instrument:** no static check reports a door
that names one representation, and the suite stays green until the stamp
moves, so the defect class is invisible by construction. The next
representations — `Map`, the typed vector, strategy 1 — meet the same doors.
The procedure is in `dev/WORKFLOW.md` under Tests.

## 2026-08-13 — the entity-kind renumbering stays rejected, and what would reopen it

**Decided:** the entity kinds keep their codes. A renumbering buys a
contiguous range that a mask could test, and the candidate gate no longer
wants one — its policy is a set built from `EntityKind`, so a later
consolidation moves the constant's value at compile time and leaves its
meaning alone (2026-08-07). Consolidating the Proxy family reclaims codes
and not a bit, since seven kinds and five kinds both need three.

**What would reopen it:** the next kind that needs admitting. `7` is
reserved and `4`–`6` are the family the RFC wants consolidated, so the
question returns when `resource` needs a code, and the Proxy family is
priced then rather than now. Until then a renumbering is a change to every
hardcoded kind code in every consumer, bought for nothing that is wanted.

This entry exists because `dev/DECISIONS.md` cited `PLAN.md` for these
grounds, and the plan section holding them was deleted when its stage
closed.

## 2026-08-13 — the deep copy's root exemption rests on a premise the lowering was never asked about

**Decided:** the exemption stands, and the premise is recorded as open
rather than settled. `array::entity::separate` records every source past the
root in `CopiesMade` and leaves the root unrecorded, the argument being that
meeting the root again means a descendant names it, that this is a cycle,
and that a cycle cannot close inside a pure-COW subgraph — every entity a
real ring passes through is non-COW and is published by the barrier instead.

**What is not checked:** whether an array whose own entry names the array
itself can be built at all. That depends on the lowering's retain order,
which lives outside this repository. If such an edge can form, the root
re-enters, the debug probe's assertion fires on input a program wrote, and
a release build finishes with one redundant copy of the root.

**Why it is not closed here:** the answer is another repository's, and the
cost of being wrong is an assertion in a debug build rather than unsoundness
in a release one. Whoever asks the lowering closes this entry with a new one.

## 2026-08-12 — a test asks for the ordered hash, or takes what the factory stamps

**Decided:** with `ll_array_new` stamping the mixed vector, a test whose
subject is the ordered hash builds one through `array::testing::hash_array`,
and every other test takes what the factory stamps and fills it through
`array::testing::push`. The rule decides 137 fixtures that broke on the
stamp and every array fixture written after it.

**`hash_array` is the factory plus the migration**, which is the only route
production has to strategy 3 for a fresh array: an empty vector's migration
carries no element and allocates nothing, so it cannot refuse, and a fixture
built this way cannot drift from what the element layer produces. A second
`#[cfg(test)]` factory stamping `Hash` directly was refused for that reason —
it would be a producer no release build has, and the pair `(tag, storage)`
stays assemblable in one place only (`entity::new_with_storage`).

**What moved to the vector is what the runtime now meets there.** The
collector's rings in both configurations
(`walk::what_the_collection_reclaims`, `gc::a_ring_with_no_object_in_it`),
the candidate buffer's nested array, and the carry of an arena array's
storage out of a dying arena hold arrays the factory stamped, because after
the flip that is what an ordinary array is on those paths and no test
reached the vector's arm of them before. Their hash arms keep their own
tests — `walk::the_children_a_kind_has` needs string keys, and the two
block-sized carries measure the table's own growth — so both
representations are covered rather than swapped.

**What stayed on the hash is what the hash is for:** the table's own group,
the element layer's writes, boxes, compaction and growth, the flood ladder,
key ownership, and the COW copy's hash arm. Those tests were written against
entries, holes and keys, and a vector has none of the three.

---

## 2026-08-12 — the termination probe's storage, and the bound that prices its abort

**Decided:** the entered set of `separate`'s debug termination probe stays on
the system allocator and becomes a `HashSet<*mut LLArray>`. This supersedes the
cost paragraph of the entry below, "the deep copy's termination probe, and what
termination rests on", and leaves the rest of that entry standing.

**The abort is priced by a bound the `WorkList` sentence does not reach.** Every
entry in the set is one live source, and every source past the root is a
112-byte arena array entity (`size_of::<LLArray>()`, asserted in
`array/entity/tests/the_entity_around_the_table.rs`). A level of attacker
nesting therefore costs the arena 112 bytes to cost the set a pointer and a
control byte, so the arena refuses the graph before the set reaches the
allocator's abort. `WorkList`'s "a refusal has to be a value rather than an
abort" governs a release structure whose growth has no such cap above it, and it
keeps that sentence: it stays true of the pending list. Nothing in the harness
exhausts the system allocator, so this margin is stated rather than exhibited —
there is no arm to force and no test to write for it.

**Refused: the bare `Table` and the `WorkList` as the entered set.** Both draw
from the pool whose refusals `array::entity`'s tests choreograph, and both
answers a debug probe can give its own refusal are worse than the abort they
replace. A probe that refuses the copy fails
`a_refused_work_list_gives_the_nested_copy_back` and
`a_refused_association_gives_the_nested_copy_back` at the first iteration: those
tests hold `FORCE_OOM` and `FORCE_REFUSE_LONGLIVED` raised across the whole
`separate` call, so the probe refuses before the root's `fill_from` and the
nested copy they reclaim is never built. A probe that skips its check instead
keeps them green while standing disarmed for every forced-refusal run, and its
first insert becomes an allocation each later refusal test has to step around —
the S7.7 class, where a test stayed green measuring an allocation that had moved
beneath it. The `WorkList` is refused a second time over for keeping membership
a scan.

**The scan's price, measured.** "In the tens today" was already wrong when it
was written: `a_deep_arena_array_is_copied_out_through_the_work_list` copies a
chain of 800 distinct entities, which is 319,600 pointer comparisons per run,
and Miri runs that test. One test, `--test-threads=2`, Miri's own clock:
83.38 s with the scanning `Vec`, 36.61 s with the probe compiled out, 45.35 s
with the `HashSet`. The probe accounted for 46.77 s of the first run and for
8.74 s of the third; the native cost of any of the three is unmeasured. A graph
larger still would change the structure and not the allocator — the pool stays
refused at every count.

**The claim is unchanged, so nothing replaces it.** A counter bounded by the
arena's population would assert only that the loop runs no more times than there
are entities, and it would fire at the end of a doubling walk naming neither the
entity nor the cause. The assertion fires at the second entry and names both
causes, which is what it was restored for.

---

## 2026-08-12 — the deep copy's termination probe, and what termination rests on

**Decided:** the debug-only `entered` vector returns to `separate`'s loop
with a rewritten message, and the count gate stands. This supersedes two
sentences of the entry below — "Termination stops leaning on
count-equals-holders" and the paragraph deleting the probe — and leaves
the rest of it standing.

**Why the probe comes back:** it was deleted because it fired on a
diamond, and the same ruling's association made a diamond enter the list
once. The assertion is now a theorem on every graph PHP value semantics
can build, so it holds on healthy input and fires only on a real defect —
the opposite of what was deleted.

**What it now asserts, and what a firing means.** Two causes, and the
message names the observable first because neither can be assumed: a
child's count under-reporting its holders, or a graph that closes on
itself through the root. The root is the one source the association does
not hold, so a ring entered at the root re-enters once and is reproduced;
a ring entered anywhere else terminates in one pass, its entry having two
in-edges and therefore a count above one.

**Termination rests on both halves, and the earlier sentence overstated
it.** For a shared child the association is authoritative. For a
single-named one the argument is "count one implies at most one holder" —
count-equals-holders, read in the one direction, the same reading
`element_for_copy` stakes its reference-box unwrap on. A count that
under-reports for an arena COW array sends a doubling chain back to
2^depth, and the probe is what names that at depth two instead of hanging.

**The vector rather than the association.** The entities that can wrongly
re-enter are exactly the ones the association never records — count-one
children and the root — so a probe reading only what is already there
observes nothing. Recording count-one children in debug builds was
refused: the association's storage comes from the pool whose refusal
`array::entity`'s tests choreograph, so a debug-only insert changes which
allocation refuses, and a refused debug-record has no good answer.
Recording the root was refused in both forms — in release it allocates a
table on every deep copy, and debug-only it would make the debug build
build a different graph than the release one. The `Vec` uses the system
allocator, which no fault injection here names.

**The gate stands.** Dropping it would make the association authoritative
for every nested child and remove the count from the argument, at a table
insert per nested child paid in release forever. The same count direction
is already staked by `element_for_copy`, where an under-report collapses a
reference another holder still names — a dangling reference rather than a
cost — so hardening the cost symptom while the safety symptom stands
unguarded protects the wrong flank.

**Cost:** debug builds pay one system allocation per `separate` and a
linear scan per level, quadratic in the distinct nested arena COW arrays
of one graph, which number in the tens today. Release builds keep no
guard, so a regression reaching only release shows as cost rather than as
an assertion.

---

## 2026-08-12 — the deep copy preserves the source's sharing

**Decided:** one copy per distinct source entity, held once for every
entry that names it. `array::entity::separate` carries a source → copy
association beside its work list, and the branch that copies a nested
arena array consults it. This supersedes the visited-set half of the
2026-08-07 ruling on the escape copy's recursion; the work-list half of
that ruling stands unchanged.

**Why:** the recursion was entered per *entry* rather than per entity, so
a child two entries name was copied twice, and a source whose levels name
each other twice cost 2^depth copies. Nesting depth on a store path is
attacker-shaped input, which is the stated reason the work list exists —
and the list bounds the machine stack while bounding nothing about the
work. The two answers are semantically identical: a COW entity's identity
is not observable (`memory/barrier.rs`, `store_category_barrier`), so a
program cannot tell one copy held twice from two copies held once, every
write going through separation either way. What is left between them is
cost. Zend's `zend_array_dup` retains children and the shallow separation
retains children; the deep copy exists because the destination outlives
the arena, so it owes a lifetime rather than a proliferation.

**The association is the generic `Table` used bare**: a stack
`StorageHead`, category `GcHeap` so its storage is buffer-arena chunks —
the memory the work list uses and for the same three reasons — the source
address as an integer key and the copy carried as a value it never
counts. That table allocates no entity, calls no barrier and takes its
head and category as parameters precisely so a second customer could
exist; this is that design paying out rather than a new structure.

**It is consulted for a shared child only.** A count of one proves this
entry is the only name — in the arena the count is an upper bound on
holders, and two live entries would each have retained — so the ordinary
path is untouched and pays nothing: the kind test and the sharing test
read one header word between them (`refcount::header_pair`). A hit routes
the existing copy through `barrier::publish_child`, which for a heap
child into a heap slot is a retain, so the copy's shape reproduces the
source's and **the copy holds exactly its holders** — its count is the
number of entries in the copied graph naming it, where the source's also
carries external holders and whatever the arena has not given back.

**The root is the only source that can re-enter and is not recorded.**
A count-one child is unrecorded too and cannot be met a second time, so
the root is the one exemption that matters. Meeting it
again means a descendant names it, which is a cycle, and a cycle cannot
close inside a pure-COW subgraph. Recording it would cost a table
allocation on every escape copy to answer a shape nothing can build.

**Termination stops leaning on count-equals-holders.** Each distinct
entity enters the list at most once, so a subgraph that closed on itself
is reproduced and handed to the tracing side rather than walked until
memory refuses. The `entered` probe that stood in `separate` is deleted
with its message: it fired on a diamond, which is a graph the invariant
allows, so it indicted an intact invariant. No substitute is claimed; a
probe for that invariant belongs to the refcount machinery that maintains
it.

**Considered and rejected:** a forwarding pointer written into the source
header, which needs no side allocation. It writes into live entities
whose header words the collector reads at agreed widths, it breaks the
source being untouched — which the refusal path's simplicity rests on —
and undoing it after a refusal means walking an arbitrary prefix of the
graph, which needs a list, funding the removal of one structure by
building the same one. Also rejected: a hand-rolled pointer set beside
`WorkList`, the table having been kept generic exactly so that sibling
would never be written.

**Cost:** the shallow duplication path pays nothing — with an arena
destination the branch is statically unreachable, and the association's
disposal returns before `Table::dispose`'s release fence when nothing was
ever inserted. A deep copy with no shared nested children pays nothing
beyond the header word the kind test already reads, the table allocating
on the first shared child. The path that pays is the one that was
exponential: a thirty-level doubling chain goes from 2^30 array copies to
31, and teardown of the result shrinks by the same factor. A refused
insert into the association unwinds exactly as a refused `WorkList::push`
does, so null keeps meaning out of memory.

---

## 2026-08-12 — what the layout cost once it was applied

**Decided:** the layout of the entry below stands, and its rule about
fixtures gains the clause applying it required. A list keeps a fixture
that more than one group needs **directly or through another fixture the
list keeps**; a fixture with one group behind it belongs in that group's
file. Read as direct use alone, the rule was false of 39 of the 100 names
the lists declared, so those 39 moved down, and `memory/routing`'s list —
83 lines of fixture serving a 61-line group — became three lines.

**A group file is read with its list open, which the first entry did not
name.** 77 of the 141 group files use a name their list declares — `mk`,
`ctx`, `t`, `with_ctx`, `tie`, and the destructor probes that count what
they saw — so reading one test is a two-file operation where the old shape
kept those fixtures at the top of the same file. The 39 moves took that
from 80 to 77, which is all such a move can buy: the rest are shared in
earnest, and the alternative to sharing them is a copy per group.

**The figures the first entry gives are the source's, not the result's.**
Measured at the applied state: the largest group file is 537 lines rather
than the 531 predicted, the median 124 rather than 112, five over 400
rather than four, and the 40 lists hold 1160 lines at a median of 10, the
largest being `array/table`'s at 189. Two things account for it — a
group's `///` was counted where it stood rather than where it landed, and
the 39 fixtures moved after the entry was written.

**`include_bytes!` resolves against the file that spells it**, so
`hash/tests.rs`'s reference to `vendor/rapidhash/rapidhash.h` needed one
more `../` from a directory one deeper. It is the crate's only such
macro — no `include_str!`, no `include!`, no `#[path]` — which is why the
cost is one line rather than a class of work.

**The `macro_rules!` ordering does not bind today, and the reason to hold
the order anyway does.** No group file calls `recording_class!`;
`array/entity/tests.rs` uses it in two fixtures of its own, and
declarations placed above the macro compile and pass. So what holds the
order is the crate's own shape, `#[cfg(test)] mod tests;` being the last
item of every source file, and the macro is why the order must not be
reversed the first time a group needs one.

**A citation of the form `module::tests::<test>` stopped naming the file
that holds the test**, the list having taken that name. 15 of them were
given their group and name a file again. The ones inside closed entries of
this journal keep their old form, this file forbidding an edit to a closed
entry: they resolve by the test's name, through `cargo test <name>` or a
grep, rather than by opening `src/<module>/tests.rs`.

**Two group names repeat across the crate:** `past_one_block` in
`memory/buffer` and `memory/immortal`, `under_concurrency` in `intern` and
`memory/immortal`. A group is named uniquely inside its module, which is
what the test path and the directory require; a search over basenames
alone answers twice.

---

## 2026-08-12 — a test file holds one group, and the group names what it pins

**Decided:** tests stay in a file beside the module they test, and a test
file holds one group. Every group becomes
`src/<module>/tests/<group>.rs`, opening with the `//!` that stands above
its `mod` today; `src/<module>/tests.rs` keeps the fixtures those groups
share and, **after** them, the `mod` declarations. The rule is mechanical
in one grep: a `tests.rs` containing `mod <name> {` is a file that has
not been split. A fixture a second module needs stays in
`src/test_support.rs`, as it is now.

**The list goes last because a `macro_rules!` fixture is textually
scoped.** `src/array/entity/tests.rs` defines `recording_class!`, and a
child module declared above that definition cannot see it — `use
super::*` imports items, not macros. Declarations after the fixtures also
match how the crate already declares its test module: `#[cfg(test)] mod
tests;` is the last item of every source file.

**Why the file beside the module rather than the module body.** The
inline form is what the ecosystem takes: of the 114 crates unpacked on
this box, 67 carry `#[cfg(test)] mod tests { … }` at 319 sites, against
16 crates at 19 sites for `mod tests;` and a file (counting a site as a
`mod tests` declaration with `cfg(test)` within three lines above it).
`std`, `core` and `alloc` go the other way, 57 separate against 2 inline;
the other 228 inline sites in that tree are `stdarch`, `compiler-builtins`
and vendored crates, 106 of them vendored and so partly the same crates
as the registry sample. Neither count decides this crate, because the
input differs: in those 319 inline hosts the test module is a quarter of
the file, median 76 test lines against 407 of code, and tests exceed code
in 7 % of them; here the median module carries 0.96 test lines for every
line of code, and 18 of 40 carry more test than code. Inlining would
double rather than extend it — `heap.rs` 2322 → 3140, `element.rs`
689 → 2947, `table.rs` 1196 → 2724, `entity.rs` 1109 → 2585 — putting 7
files past 2000 lines. Two are past it today, and one of those two is
`element/tests.rs` at 2258, which is the file this stage was opened over.

**Why one group per file.** A test is found by the name of what it pins,
so the name has to be reachable without opening a file: as a file name it
is in the directory listing and in a grep over paths, and it was already
in the failure path. The size follows from that rather than being aimed
at — the 141 groups run a median of 112 lines, and `element/tests.rs`
becomes seven files, the largest 531.

**No line count is part of the rule**, and the precedent is the comment
pass of 2026-08-11: what decided whether a long comment was cut was
whether its argument stood recorded elsewhere, never its length. A
threshold has to be re-judged at every edit and answers a different
question from the one asked, which is what a file is about.

**The directory of group files is rare, and the counts above do not
support it.** They settle only where a test lives relative to its module.
Four of the 114 crates hold a `src/**/tests/` directory of more than one
file — `sharded-slab`, `walkdir`, and `memchr` at two versions — and none
of them splits by group: the directory is the crate's, not a module's.
The second half of the rule rests on the goal alone.

**A test crate beside this one is refused.** It is the third form in
use — `library/coretests` and `library/alloctests` in the standard
library — and it cannot work here: every test reads crate-internal state,
through `pub(crate)` items and through `array::testing`, which exists
because an assertion cannot destructure the head-and-tail pair.

**The move changes no path, measured on `src/refcount/tests.rs` rather
than argued.** Its 3 groups became a 15-line `tests.rs` and files of 79,
112 and 81 lines; both configurations then ran what they ran before, 11
tests under rc-walk and 8 under rc-trace; the probe was reverted. Nothing
had to be re-pointed because a group is already a module at that path —
all 141 open with `use super::*;`, none is nested — and a child module
sees its parent's private items whether that child is a file or a brace.
The macro is the exception, and the paragraph above is what it costs.

**What the move moves.** 40 files become 181: 141 group files and 40
lists. 18 949 body lines are dedented by one level, 761 `///` lines above
the `mod` declarations become `//!` at the head of a file, and 282
wrapper lines are dropped. The lists left behind hold 1538 lines, median
11, the largest being `array/table` at 232, `array/element` at 224 and
`array/entity` at 173 — so `refcount`'s 15-line list is the small end
rather than the shape.

**Four costs beyond the file count.** The group's description leaves the
list: it becomes the file's `//!`, and no copy stays behind, a second
copy of what already stands in a module doc being what
`dev/WORKFLOW.md`'s comment rule forbids. So the list carries names, and
a name is what a reader gets until a file is opened. `rustfmt` sorts each
run of `mod` declarations, so the list is alphabetical and the order the
groups are written in today is not preserved; that order is accepted as
lost, the list being an index. **A test's history is reached by content,
not by rename detection:** `git log --follow` and `git log -C` both fail
here, a 531-line group scoring 22 % against the 2258-line file it came
from and the default copy threshold being 50 %, so 131 of the 141 files
are out of reach of `-C` in principle. What works is `git log -S 'fn
<test name>'`, verified across the move S9.4 already performed — it names
both that move and the commit the test was born in. And `dev/INDEX.md`'s
"Tests" bullet states the present layout as fact, so it moves in the same
commit as the code.

**A gated group states its configuration twice, and both are required.**
The `#[cfg]` goes on the `mod` declaration, where a configuration that
drops a whole group is visible from the list; the file's `//!` says which
configuration it belongs to, because a file whose gate is only in another
file invites a test that is silently gated to one build. Five groups
carry a `#[cfg]` today, and `refcount`'s already states the reason in
prose.

**What the rule does not decide:** four groups are over 400 lines, the
largest being `element`'s `the_writes_and_the_separation_they_share` at
531. A group that size is two subjects, and splitting it is a judgement
about the group rather than about the layout.

**The counts here were re-measured at HEAD `dbc9eb9` and supersede
S16's opening figures.** They differ in three ways: the 42 files there
count the two `loom` models, whose 8 tests no ordinary run executes, so
the file count is 40; the ecosystem figures came from a different matcher
and the one described above is what these numbers rest on; and the S17
commits landed between the two measurements.

---

## 2026-08-11 — releasing the storage takes the same window as moving it

**Decided:** both `dispose` bodies drive `storage`, `nslots` and `used`
to their empty values between `begin_move` and `end_move`, and free the
chunk after the window closes. The vector does it too, although its
`nslots` is zero throughout and no reading of it can mix.

**Why:** a dying array is still read — it dies mid-epoch and the
collector's snapshot holds its slot — so three separate publications
offer readings no array was ever in. What kept that harmless was the
order of the stores, the null pointer going out first, which
`entries_of` short-circuits on. Nothing said so, and the tidier order
publishes a live chunk against `nslots` zero, which strides the index
region as entries and posts phantom in-edges. The window makes the
order free to choose, which is the point: an argument that rests on
statement order is one edit from being false.

**Cost:** two more relaxed stores per teardown, on a path that is
already freeing memory.

**The `used` rule keeps exactly one exemption.** `Vector::sever_entries`
lowers the count in the chunk it keeps, and may: it empties a component
the drain has confirmed garbage, so teardown is the only writer that
can follow, and a vector's elements go out as atomic stores rather than
the plain key word the rule is about. Written where the rule is
(`array/head.rs`), because a representation whose elements are written
plainly may not copy it.

---

## 2026-08-11 — an edge is re-checked by its shape, and its storage first

**Decided:** `collector::Edge` carries the width of the cell it was read
from, and Phase 3 asks a sixteen-byte cell whether the flags beside the
payload still say "entity". The walk also answers with the version of
the storage its cells came from; the epoch keeps one per walked row and
Phase 3 requires the array's counter to still read it, **before** any
cell of that component is re-read.

**Why:** eight bytes are not the cell. A `Value` is two relaxed stores,
payload first, so a walk can pair the payload of the value being written
with the flags of the one being replaced — and the integer the program
wrote is looked up in the census like any other word. And an address is
not a cell either: every move of an array's entries leaves the old chunk
parked, intact and unwritten for the rest of the epoch, so re-reading a
recorded address there answers with the walk's own value.

**What it buys, measured rather than argued:** Phase 3's contract and
one verdict round trip per moved array. Phase 4 re-traces every member
through its current fields before freeing anything, so a component
confirmed on a stale cell is dropped there — measured with the check
disabled: confirmed 1, nothing torn down. The teardown of a live entity
was never reachable through this.

**Cost, measured:** an edge grows from 24 bytes to 32, the shape's one
byte paid for in padding. Rejected for the version: one word per edge,
which the stage proposed. Every cell of one array is read at one
version, so the number is the array's and the list keeps its size.

**Bounded by the handshake.** The re-check runs after the condemn ack,
which orders what the mutator wrote before its checkpoint, so a move
landing after the ack need not be visible to it. What holds is that no
verdict is posted for a component that changed before the ack.

## 2026-08-11 — compaction moves the entries into a fresh chunk

From stage S13.1. **The ordered hash reclaims its holes by copying the
live entries into a newly allocated chunk of the same size and publishing
that**, rather than sliding them down inside the chunk it already has.
`Table::move_entries` is one body for both ways entries move, with
`EntryMove` saying whether the holes travel; `Table::compact` therefore
allocates, can be refused, and returns `Option<usize>`.

**Sliding in place is undefined behaviour, not a torn read.** The
collector reads entries with relaxed atomic loads while the mutator
memcpys thirty-two bytes over them; a non-atomic write racing an atomic
read is UB, which no later phase repairs, and Miri reports it as a data
race at the copy.

**Word-by-word atomic stores were refused, by arithmetic rather than by
cost.** They answer the race and leave the walker reading one entry at two
indices while the slide is in progress, so a child is counted twice. An
in-edge count above the truth is the one direction that frees a live
object; a missed edge only leaks. A destination nothing has published is
written by one thread, so the copy stays plain and the version window
covers only the publication.

**What makes the old chunk safe to free is the epoch, not the bracket.** A
collector walks only inside one, and an epoch parks every buffer-chunk
free instead of recycling it (`memory::deferred_free`), so a walker still
striding the replaced chunk reads intact bytes. Outside an epoch nothing
walks.

**The price is a refusal where there was none.** An insert that would have
compacted now fails under memory pressure, exactly as one that would have
doubled does. Compaction still avoids doubling — the fresh chunk is the
same size — so what it buys against `realloc_storage` is unchanged.

**It also moved the version counter's justification.** The counter was
argued from in-place compaction, which no double read of `storage` can
see. That case is gone; what forces a validated reading now is the 2 → 3
migration, which changes what the bytes mean at an address that need not
move, and the separate publication of `storage` and `used`.

## 2026-08-11 — the storage head is a field of the entity, and no `&mut` may span it

The Sage's second ruling on the same stage, and it **overturns the
placement half of the entry below**: the head stays one struct above the
representations, and it moves out of them into the entity —
`LLArray { rc, head, storage }`, each representation keeping only its
private tail. `src/array/head.rs`, `src/array/entity.rs`.

**A `&mut` covers its whole range whatever the fields inside it are.**
Every mutating table or vector operation is reached through
`&mut (*a).storage`, so a head inside the representation sits inside that
borrow, and the collector's read of it is undefined behaviour rather than
a race the atomics settle — the interior-mutability exemption belongs to
shared references. This is the defect `dev/POSTMORTEM.md` records for
2026-08-10 at a different word, and the crate has now paid for it twice.
Correctness outranks structure, which is why `Table`'s self-containment —
the reason the first ruling gave for the prefix — is the thing that
yields.

**The type rule that follows:** no `&mut` may ever span the head, and the
only one near it is field-precise. `entity::as_table_mut` derives the
pair — `&mut (*a).storage.table` and `&(*a).head` — from the one
`*mut LLArray`, the union's members are private to that module, and
callers destructure what it returns. A shared reference to a struct of
atomics is the exempted case, and it is disjoint from the `&mut` over the
tail.

**Every operation over a walker-visible word takes `head: &StorageHead`
as a parameter**, `category`'s precedent (2026-08-07): threaded per call
rather than stored, so nothing drifts, and `Map` owns a head of its own
the same way, stamped `Hash` and never rewritten. The bracket calls stay
textually inside `grow`, `compact` and `realloc_storage`, because the
entity cannot see when a move happens — growth fires inside `insert`, and
bracketing every insert would leave every insert in an odd window and
starve the walker. Cost on the hot path is one register argument per call
and no extra load; an array is the same 112 bytes, the words having been
the table's before.

**Both `offset_of!(…, head) == 0` const assertions are deleted with
nothing in their place**: the identity they guarded — the union's address
*is* the head's — stops being a fact. `entity::storage_head` names the
field instead of casting.

**The carry out of a dying arena became one body, dispatched on the tag,
and it lives in `array::entity`.** A storage chunk is bytes: the two
representations differ only in where they keep the size they were granted,
so each hands that field out (`granted_capacity_mut`) rather than carrying
a copy of the operation. Before the tag existed the carry could name the
ordered hash outright; with two representations that reading is a
vector's `cap` used as a byte size, which is the class of defect the tag
was introduced to prevent. `entity::storage_address` names no
representation at all now — the chunk's address is one of the words a
walker reads, so it is the head's.

**Two traps closed with it, both one line from being live.** A reference
spanning a whole array entity invalidates any outstanding `&mut` over its
representation, so `needs_separation` takes `*const LLArray` and reads the
header field-precisely, `category_of`'s shape (2026-08-07). And the head
field is private to `array::entity`, so `&mut (*a).head` — the same defect
one level down — cannot be spelled anywhere else in the crate. Neither
visibility nor a signature can stop a caller outside the crate from making
`&mut *ll_array_new(…)`, so that rule is stated on `LLArray` itself, at
the public surface where a consumer reads.

**A walker with no stride for a tag gives the array up.** The dispatch
matches all three values rather than testing for one: `Typed` is a value
`decode_tag` accepts, and falling through to the hash stride let a *valid*
tag select the wrong layout — the defect the read protocol exists to
prevent, one step over from the stale-tag case it was written for.

**The decisive check is Miri, and it was run both ways.** A
walker-against-mutator test
(`array::entity::tests::the_head_a_walker_reads`) reported
"not granting access to tag … because that would remove [Unique for …]
which is strongly protected" at `head.rs`'s first load, naming
`Table::insert`'s `&mut self` as the protector; after the move the same
test is silent. `cargo test` passes either way and can judge nothing
here. The loom model stands verbatim: its argument is about the fences,
not about where the words live.

## 2026-08-11 — the version counter lives above the storage representations, not inside one

The Sage's ruling, from stage S7. **An array's walker-visible words —
the version counter, the storage pointer, the index-slot count, the
element count and the strategy tag — live in a `StorageHead` that every
storage representation begins with**, rather than each representation
keeping its own. `src/array/head.rs`.

**A counter can only bracket memory that outlives what it brackets.**
The 2 → 3 migration replaces the representation under a walker that is
mid-stride, so a counter inside the vector would be validating against
bytes the migration has already reinterpreted as table fields. That rules
per-representation counters out in principle rather than in taste.

**A swap counter above two inner ones was refused for a second reason.**
A walker that read a stale tag still *performs loads* at the other
representation's offsets before its re-check can discard them, so every
offset a walker can touch under either tag has to be written atomically
at a matching width under both. That forces the walker-visible words to
coincide in offset and width — a common prefix by accident. Naming it is
cheaper than discovering it.

**Deriving the tag from `nslots == 0` was refused:** an empty ordered
hash reads that way, and so does every `Map` before its first insert.

**The head is a prefix inside each representation, not a field beside a
union of them.** That keeps `Table` self-contained, which matters because
`Map` is the table's second customer and wants the hash without an
array's storage protocol; the price is one constant byte in every table,
its tag stuck at `Hash`. The union's own address is therefore the head's
address, and `entity::storage_head` casts it without naming a
representation — pinned by a const assertion in each member.

**The walker loads all five words before it branches on the tag.** The
read set does not depend on the tag, so a stale one is discarded with
everything read beside it and can never select a stride. Both ends of the
bracket keep their fences, for the reasons `version_bracket_model.rs`
exhibits; the model's argument does not count the words in the window, so
it stands unchanged.

**What this cost elsewhere:** the strategy tag can no longer share
`Table::flags`, which the flood ladder writes plainly while the walker
would read the tag atomically. Bits 2–3 there are free again, and
`rfc/model/arrays-hashtable.md` still says otherwise — the correction is
S7.4's.

## 2026-08-10 — the entity-slot limit differs by category, and past it nothing is packed

Edmond's ruling and the Sage's shape, from stage S11. **An entity whose
single allocation exceeds what its category's allocator packs into a
block it shares is not packed at all**: it keeps its inline layout whole
as the sole occupant of a block-aligned allocation of its own, first line
a block header of a new kind pair, entity at `+LINE_SIZE`. Field access
is unchanged, and the walk visits exactly one slot there.

**The limit is per category because the allocators differ, and
`routing::slot_limit` is the one place that says so.** Both arenas
bump-pack within a block, so their bound is `BLOCK_PAYLOAD`: a shared
block cannot hold a slot larger than its payload. The entity heap packs
by size class, and its bound is `MAX_SMALL` rather than the block —
past the largest class a packed slot would take a whole block and land
outside the population both enumerators walk, which is a leak no pass
finds. Reading a block size in a factory is what this replaces; six of
them used to.

Past the bound each category answers differently, and none refuses. The
two heap categories and the immortal region lift in `entity_alloc_in`;
the immortal one needed no allocator, `immortal_alloc` having served a
larger request from a run since before the stage. The request arena
lifts on a **door of its own**, `Arena::alloc_entity`, while
`Arena::alloc` keeps refusing — it serves `ll_arena_alloc` from the C
ABI, where an entity and a byte buffer are the same request, and that is
the reason the bound lives there at all.

**Rejected, and not to be reproposed:** capping a class's slot at one
block payload with the limit enforced by the compiler at class layout.
It is cheap and compile-time known, and it refuses a program Zend runs —
measured on this box against PHP 8.6.0-dev, where a class of 10 000
declared properties instantiates at 163 840 bytes and 200 000 works too.
The compiler warns at `MAX_SMALL` instead, where an object stops sharing
a block and starts costing one of its own. Also rejected: an out-of-line
cell vector, chunked entities, reusing the raw large kinds, entering runs
into the block pool's region registry, and raising `MAX_SMALL`.

Design: `rfc/model/memory/large-entities.md`.

## 2026-08-10 — a string's layout is its own header bit

The Sage's, on a collision the stage's own design walked into. `COW`
selected the string layout — set meant inline, clear meant bytes out of
line — and it also decides three other things: whether a write separates
(`cow_separation_needed`), whether an arena entity is counted at all
(retain and release no-op on a non-COW arena entity), and whether an
escaping arena entity is copied or held (`memory/barrier.rs`). A string
whose content exceeds what its category's allocator packs in one slot has
no inline home, so it must be out of line **and** copy-on-write, and one
bit cannot say both. Building it in the existing dynamic form would have
made a large string silently mutable under a second holder.

The layout is `STRING_OUT_OF_LINE` now, bit 15, and `COW` means only
copy-on-write. The bit is free on a string header in both builds: rc-trace
writes the candidate index into bits 15-31 only for the kinds that can
close a cycle, and `String` is not one, while rc-walk's epoch byte starts
at 16. A kind-scoped bit follows `ARENA_RESET_MARK`, which borrows the
GC-state field for arena entities the same way.

What it bought: `ll_string_new` and `new_uninit` choose the layout
against `routing::slot_limit`, so a 9 KiB heap string and a 64 KiB arena
string are served instead of refused, and both keep the value semantics
an inline string has. Priced with the ruling: a by-size arena string
never survives a reset as itself, because a COW entity is copied out at
escape — so the reset gained no rule, and `carry_payload_out_of` still
sees only proved-single-owner survivors.

Rejected, and not to be reproposed: deriving the layout from kind plus
size (it cannot tell dynamic-by-proof from dynamic-by-size, and it puts a
size compare on the hottest path), a third layout for the oversize COW
case (it differs from the dynamic one in no field), and deferring
oversize strings to a later stage (a 9 KiB string is ordinary PHP and no
later stage would know more).

---

## 2026-08-09 — the table is handed its category and reads no header

Edmond's, and it does not overturn the `RcHeader` rule of 2026-08-07 — it
finishes it. The header stays the only authority on which memory an entity
lives in; what changes is who reads it. `Table` read the owner's header at
every allocating call and asserted, while it was there, that the kind was
Array. A generic ordered hash therefore named one entity kind: `Map` is
meant to reuse this structure, and its first storage growth would have
failed that assertion in every debug build.

The category is a parameter now, on `insert`, `grow`, `realloc_storage`,
`alloc`, `free_storage`, `dispose` and `carry_out_of`. That is the shape
the store barrier has had since the beginning and for a reason that
applies here unchanged: `owner_cat` is passed rather than loaded because
a destination need not have a header at all. Nothing in `array/table.rs`
reads a header now, and the kind assertion went with the read instead of
moving.

One reader replaces it, `array::entity::category_of`, which takes a
`*const LLArray` and delegates to `object::header_category`: what the
assertion stated at runtime the parameter type states at compile time.
The obligation the assertion used to carry moves to the callers and is
written where they look — `Table::insert`'s contract — because it is the
fact that drifted in 2026-08-07's entry and cost a use-after-free: the
category passed is the owner's **as it stands at that call**, and one
cached across an arena reset frees to an allocator the storage never came
from.

The invariant is tested a layer up as well, since the table can no longer
break it: `element::tests::a_promoted_array_takes_its_next_storage_from_the_heap`
drives a write on an array whose header promotion has just rewritten, and
fails if the write caches a category.

## 2026-08-08 — the event journal is one ring per thread

Decided while building §9 of `dev/design/debug-modes.md`, and recorded
here rather than only there because it shapes the write path: **each
thread journals into its own ring, and there is no global ring and no
global sequence number.** A window is marked by reading every registered
ring's cursor before and after the interval, and membership follows from
the two readings.

The framing that made a global ring look necessary was that the census
hunt of 2026-08-06 needed a *global order*. It did not. It needed
**membership** in a window — "did any string die between my two
censuses" — and a cursor pair answers membership exactly while costing
the write path no atomic read-modify-write. Two further properties
settled it: a single ring of K records holds the last K of the whole
process, so the thread under investigation loses its history to whichever
thread allocates hardest, which is the condition the journal is switched
on under; and thread identity lives in a per-thread ring's header instead
of in every record, which is what keeps a record at 32 bytes.

What is genuinely given up is order across threads. An investigation that
needs it stamps a shared counter into a payload word on an event kind of
its own, and pays the contended increment on that kind alone rather than
on every record in the process.

Edmond had been asked and had not answered when the session ended; the
answer stands unless he overturns it.

## 2026-08-08 — an event kind gets a number when it gets a site

The journal's kinds are declared in `src/journal/kinds.rs`, and only the
ones a site writes are declared at all. §9.5 of `dev/design/debug-modes.md`
names an on-demand set beside the default one — retain and release,
store-barrier publishes, buffer chunk allocation and free — and none of
those has a number, because none has a site.

The alternative is to number the whole vocabulary up front, so that an
investigator's mask constants stay stable as sites arrive. What that buys
is a reader who can enable a kind, get silence, and have no way to tell a
runtime that did not do the thing from a runtime that never learned to
record it — the false *none* the whole module is built against, arriving
this time through the vocabulary rather than through the ring. It is also
the arm-with-no-producer shape this crate has refused twice already
(`PLAN.md`, on strategy 1 and on the candidate gate).

The cost is that kind numbers are not stable across builds of different
ages, so a mask written down by hand ages badly. The mask is set through
named constants, and an external reader has no ABI yet (§9.8), so nothing
today pays it.

## 2026-08-08 — a block leaving the entity walk's reach is the decommission record

§9.5 lists three block events: commissioning, decommissioning, and a block
leaving the set the region registry's walk reaches. The third has no kind
of its own.

A block leaves that set by exactly one route — its kind stops reading
`BLOCK_KIND_ENTITY` — and every path that does so hands the block to
`BlockPool::put` in the same breath (`heap.rs`'s two `store_block_kind(…,
0)` sites, and `retained::give_block_back` for a retained one). So `put`'s
record carries the kind the block arrived with, and the departure is that
record with an entity kind in `a`. A kind of its own would have fired
exactly where this one does and nowhere else, and would have needed a
second site on the same path to do it.

What this decision would cost if the premise moved: a future path that
changes an entity block's kind *without* returning it to the pool would
leave the walk silently. The premise is stated on
`KIND_BLOCK_DECOMMISSIONED` where anyone adding such a path will read it.

## 2026-08-08 — the pool's thread cache stages its overflow flush

`BlockPool::put` held the thread cache's `RefCell` borrow across
`push_global`, which takes the free list's `Mutex`. That is sound while
nothing else runs there, and the journal's decommission site is something
else: a record that is its thread's first allocates a ring through
`ll_malloc`, which comes back into this pool and borrows the same cell.
The failure is a borrow panic, and a panic in a `try_with` closure does
not unwind — the process aborts. Ruled by the Sage before S5.2 opened,
against the alternative of amending §9.7 to let a site sit under a lock.

So the borrow decides and stages, and the pushes happen after it ends: the
overflow flush copies into a fixed stack array of
`THREAD_CACHE_CAPACITY / 2 + 1`, which is the most one `put` can overflow
by. The width is load-bearing rather than defensive — a path that
overfilled the cache would index past the array, which is the same abort
by another door — so the excess is clamped and the assumption is a
`debug_assert` beside it.

## 2026-08-08 — the journal is complete to the exit's last act and honest past it

Ruled by the Sage after the scoped pass over the retire-last redesign
found the one thing that redesign's premise got wrong: the last act of
`ll_thread_exit` is not the last act of the *thread*. A thread's own
`thread_local!`s are destroyed after the runtime's guard wherever TLS goes
in reverse registration order — which is where this crate already places
glibc — so records raised there arrived on a closed slot, were dropped,
and were counted nowhere: a window over that thread's death answered a
complete list of records where the truth was that some were lost.

**The runtime's own teardown events stop being post-exit at all.** The
barrier reserve and the block pool's thread cache are drained by hand
inside `ll_thread_exit`, before the ring retires, rather than left to
their `Drop` impls. Both structures are alive there, and their handovers
are a default event kind, so once the record sites exist those events land
in the ring. The `Drop` impls stay as the fallback for a thread that never
ran the exit — the pool serves threads this runtime never initialised —
and on the contract path they find nothing left to hand back. This also
retires the standing exception to the 2026-08-03 rule that every
per-thread structure reachable from thread exit is disposed by hand.

**What still arrives on a closed slot is counted**, by one relaxed
read-modify-write on the branch that writes no record, and reported as
`Window::Lost`. Not saved: saving would mean writing into a retired ring,
which the quota may evict and another thread free, and that is the
raw-pointer-across-teardown defect this module has already paid for. The
count is a **difference** between two marks rather than a running total —
a dropped record is a point event inside one window, and a cumulative
count would degrade every later window, converting "can tell" into
"cannot tell" in the mirror direction.

So the contract is two-part and the parts are named: **complete** to the
last act of the runtime's exit, **honest** past it.

---

## 2026-08-08 — the journal's ring retires last, and closing is an instant

Supersedes the same day's "closing is an interval", which this entry
deletes the machinery of rather than refining. Ruled by the Sage after two
scoped critic passes over that machinery each confirmed a contract-class
defect, the second of them a store through a raw ring pointer parked in a
thread-local across the whole heap teardown — reproducible corruption of a
block the allocator had already handed on.

**One misplaced instant explains all of it.** The ring was retired at step
6 of `ll_thread_exit`, *before* the heap teardown whose events it exists
to catch. Everything built afterwards — a second stamp on the ring, a
thread-local carrying its address between the two, a window answer for
the interval between them — was compensation for that ordering, and each
piece of the compensation carried its own defect.

Retirement moves to the exit's last act. Nothing a dying thread does
inside the contract goes unrecorded then, so a window over a thread's
death is *complete* rather than honest about a gap, and there is no name
to carry across the teardown because there is nothing left to stamp after
it. The design's own argument for the early position — that a record
raised later would open a second ring under a thread already gone — does
not survive inspection: the ring stays in the thread's cell until the
retirement runs, and no second ring can open while it is there.

**The exit's phase is three-valued** (`heap::ExitPhase`), because a
boolean conflated a heap rebuilt in the middle of an exit with a heap
built for a new life on a pooled thread. Under the boolean, a first record
raised by a `__destruct` in step 1 called `ll_thread_init`, which lowered
the flag mid-exit and told every later caller the thread could free again
— the same defect the previous entry's repair had just closed, through a
door no pass had opened. A thread inside its own exit now needs neither
initialisation nor an armed guard to journal: the retirement still to run
is its guarantee.

What the position costs, named: the teardown's own events occupy ring
slots and can lap the records before them. The overflow answer reports
that honestly, and the hypothesis this journal was built for is about a
finishing thread — those are the records it wants.

---

## 2026-08-08 — a review cycle ends on scope and class, not on a silent pass

Ruled by the Sage the same day, after step S5.1's three independent critic
passes found 5, 7 and 7 defects, every one real and rounds 2 and 3 finding
theirs almost entirely inside what the previous round's repairs had added.

**A pass over a whole module cannot converge.** Every repair is fresh code
carrying its own defects, so re-offering the module re-offers a surface that
the last round manufactured; the count measures the surface, not the health.
Past the two dispute rounds of the working rules, a cycle therefore continues
only as passes **scoped to the latest unexamined repair batch**, and it ends
at the first scoped pass that confirms no defect of the module's *contract
class* within its scope. The class is named per module and stated to the
critic: for the journal it is an answer that converts loss or silence into
"nothing happened", undefined behaviour, a ring freed wrongly or never freed,
and a ring nothing will retire.

What the two kinds of finding do differs. A contract-class finding is
repaired with a regression test seen failing, and the repair becomes the next
pass's scope. Everything else — wordings, comments, test gaps, hardening — is
repaired too, but renews nothing; the stage-end review carries it.

**And the cycle has an end that is not another pass.** If two consecutive
scoped passes both confirm contract-class defects, passes stop being the
instrument: the module goes back to the Sage with the accumulated findings,
because by then the evidence is against the shape of the thing rather than
against its details, and the answer is a redesign rather than a review.

The alternative was to close by exhaustion — to stop when the reviewer is
tired rather than when the work is sound — which is what this rule exists to
refuse.

---

## 2026-08-08 — a pinned block goes home when its last payload is freed

A block the arena reset kept for bytes it could not carry out returns to the
pool like any other retained block, and what returns it is the payload's own
free. The rule the entry of the same day left owed ("a block retained for a
payload is pinned and never returns") was a leak of 64 KiB per refused carry,
for the life of the process.

The block is held by two populations and goes home when both are empty: its
live occupants, and the payloads it was pinned for. The pin is therefore a
count rather than a flag — one block can hold the payloads of several
survivors, and each is freed on its own.

The payload's death event was said not to exist. It does:
`buffer_arena::buffer_free_longlived_payload` already reads the block kind
under the pointer and recognises `BLOCK_KIND_RETAINED`, where it did nothing
at all. That arm now spends one pin, and hands the block over when it was the
last holder. The bytes are still left where they are — former arena memory
has no free list to take them back — so what is reclaimed is the block.

During a collector epoch the call parks, for the reason `ll_free` parks a
slot in a retained block: the walker holds addresses inside it and a block
handed to the pool is re-stamped as another kind under them. A parked record
now names the free it replays instead of inferring it from a size, because
this one is neither of the two the size chose between.

**The test needed a fault injection of its own.** `block_pool::FORCE_OOM`
refuses the pool, and the buffer arena can serve a carry out of a block it
already owns or adopts, so the refusal did not happen 5 runs in 40 and the
test passed proving nothing. `buffer_arena::FORCE_REFUSE_LONGLIVED` refuses
the two allocations a carry can make and nothing else, which is what "prove
which allocation was refused" costs here.

An array's teardown walks a list instead of recursing, which closes the half
the entry of 2026-08-07 left open. The input is the same one the store side
was rebuilt for — `$deep = [[[…]]]`, then one release instead of one
assignment — and a frame set per level ends in a stack overflow, which the
guard page turns into a dead process: no unwinding, no record, nothing an arm
of this crate can catch.

`array_die` owns the drain. A nested array whose last reference it drops is
pushed onto a list of its own and torn down by the same loop, so the depth
costs one iteration rather than one frame set. What made that expressible is
`barrier::drop_ref_deferred`: the release reports the entity whose count
reached zero and hands the teardown back, where `drop_ref` runs it. Both are
one body, the deferring half being the whole of `drop_ref` up to the call it
does not make.

The list is the copy's `WorkList`, now generic over its element. It lives in
a buffer-arena chunk for the reasons of 2026-08-06, and a chunk it cannot
grow puts the child it could not take back on the recursive path. A teardown
has no channel to refuse through — the array is already at count zero — so
the choice was between the old shape for one subtree and leaking it.

**Destructor order on the refcount death path is Zend's and is a contract.**
Depth first, and inside a level the order the entries were inserted in:
`[[$b], $a]` runs `$b`'s destructor first. The first shape of the drain lost
that — it released each child where it found it and left only nested arrays
for the list, so `[[$b], $a]` ran `$a` first and `[[$b], [$c]]` ran `$c`
first. A program observes this through the bodies it writes, and a runtime
that executes existing PHP inherits Zend's order whether or not the manual
promises it.

Keeping it needs no cursor into the table and no second stride. Children are
released where they are found until the first dying nested array; from there
the rest of the level becomes **held** lines on the list, their release
postponed and the reference the storage held passing to the list, and the
segment is reversed so the LIFO drain returns it in entry order. A held line
carries its owner's category, since that is what settles the escape ledger
and the release-at-reset log and one list mixes several owners' children. A
flat array still pushes nothing and allocates nothing; the cost on that path
is one flag test per child.

**The guarantee stops at the refcount death path.** The cycle collector
orders the white set as it walks it and the arena reset orders its own
destructors, which is the frame PHP itself draws: Zend's GC walks its buffer
and the manual leaves shutdown order undefined.

Under a refused chunk the order degrades with the bound: a held child
released on the recursive path runs its destructors ahead of the subtree
deferred before it. Memory exhaustion is the only way in.

The drain takes one duty over from `ll_entity_die`'s door with the array:
under rc-trace a dying entity leaves the candidate buffer, and a nested array
never passes that door again. Removing the call was verified to fire
`ll_free`'s "entity freed while buffered as a cycle-collector candidate" —
which is a test-build assertion, so a release build has no net under it.

**What this does not bound** is a chain that leaves the array kind between
levels: an object chain (`$a->next = $b->next = …`) recurses through
`dispose`, and so does a chain alternating arrays with reference boxes
(`$b = [&$a]` in a loop), whose box is not an array and whose own teardown
enters `array_die` one frame set down. The step was scoped to the array
chain because an array literal is the cheapest depth a source file can
build; widening it is open and is Edmond's. Whatever shape a wider drain
takes, it owes the same held-sibling discipline, or it gives the order back.

## 2026-08-08 — a copy collapses a reference nobody else names, and the arena reads that count as an upper bound

**Decided (Sage):** duplicating an array unwraps an element's reference
box when the box's refcount is 1, and nothing else collapses a
reference. In the request arena that count is an **upper bound** on the
box's holders rather than a count of them, so the copy errs toward
sharing there. The divergence is accepted and no mechanism is built
against it.

**The rule.** `array::entity::element_for_copy`, reached from
`fill_from`, is the one place a reference collapses — which is PHP's own
rule: measured on php 8.3.6, neither `unset($r)`, nor a write to that
element, nor a write to another element collapses it, and
`zend_array_dup_element` unwraps at refcount 1. Only a **duplication**
does it: an escape copy is a store crossing a lifetime boundary, where
the program duplicates nothing, so it carries the box across unchanged
(`entity::CopyReason`).

**Why the arena's count cannot be exact.** Two things inflate it, and
the second is the deeper one. `barrier::drop_ref` skips the release when
an arena container lets go of a heap entity, because the reset log owns
that release. And an arena COW array whose count reaches zero does not
tear down at all — the reset reclaims it — so `unset($b)` on an arena
copy that shares a box gives nothing back. The root cause is one
sentence: **arena-container death is deliberately not an event in this
runtime, and PHP's collapse condition is a question about exactly that
event.** Every mechanism that makes the count exact makes death an event
again, which is the arena's reason to exist, spent on a rare `&`.

**The mechanisms that were walked to the floor and refused.**
Subtracting the log's records fails because a record is written at
publication, not at death, so it cannot tell a live holder from a dead
one. Eager release with the record cancelled is refused by the log,
which is append-only with no lookup. Eager release paired with a
compensating retain-record balances arithmetically but is sound only
until a box dies mid-request: then the reset's retain fires into a slot
the walker classifies as free. Cascading teardown of arena COW
containers at zero puts `__destruct` bodies behind arena drop paths that
run none today. A second, uncounted hold-count inside the box makes
liveness a two-word question and puts a kind test back on the arena
release fast path — the cost the 2026-08-08 box ruling refused.

**The error is one-directional, and that is what makes it safe.** Every
live holder of a box carries a counted `+1`: a heap slot counts eagerly,
an arena entry counts at publication, a frame binding retains for
itself. So `refcount == 1` with the duplicating entry alive proves the
entry is the only holder — the unwrap never fires on a box another name
still reaches, and nothing dangles. The divergence can only delay a
collapse.

**What a PHP program can observe, stated rather than left implicit.** In
a request-arena array, after the last `&` binding to an element is gone,
a later `$c = $a` can leave `$a` and `$c` aliasing that element —
provided that earlier in the same request another array shared the
element's box and has since been dropped. A write through the new copy
then reaches `$a`. The pinned sequence is
`$a=[1]; $r=&$a[0]; $b=$a; $b[0]=3; unset($b); unset($r); $c=$a;
$c[0]=9;`, which php 8.3.6 answers `(3, 9)` and this runtime answers
`(3, 9)` in the heap and `(9, 9)` in the arena
(`element::tests::the_arena_reads_a_box_count_as_an_upper_bound`).

**The collector's guard window is the same rule, not a second one.**
`gc::run_cyclic_destructors` and the rc-walk drain guard every white
entity with `refcount += 1` before running `__destruct`, so a
duplication inside a destructor reads an inflated count and shares where
PHP unwraps. That guard is memory safety rather than bookkeeping — it is
what stops a destructor's `$this->x = null` from freeing a white the
collector is still iterating — and no adjustment at the read side is
sound, because a destructor also reaches exactly-counted boxes through
live state. The window closes with the collection.

## 2026-08-08 — a retained block goes home when its last live occupant dies

**Decided (Sage):** the second half of the retention mechanism is built.
`memory::retained` counts a retained block's live occupants;
`stdapi::ll_free`'s retained arm reports each occupant's death; at zero
the index is dropped, the block is restamped and returned to the pool.
The block rides the parked-free queue like every other kind whose free
can put memory back in circulation, because its last occupant's death
hands over the whole block.

**Why now, and why inside S3.1.** A reference box behind `&` made this a
blocking defect rather than a future item. Boxing an element of an arena
array makes an arena element an escapee, so the reset promotes it and
retains its block; the box dies one step later, from the release the
array's entry logged, and takes the element with it. Before, the block
stayed out of circulation for the life of the process, so
`$r = &$a[0]` on an array of objects — which `foreach ($a as &$v)` does
once per element — retired 64 KiB per request in a runtime whose premise
is running requests forever. S3.1's own criterion ("a request that takes
a reference and ends leaves no block held") measured exactly that, so
the step could not close over it.

**Why the vain promotion is not the thing to fix.** At settle time the
element's hold-count is 1, held by the box, and whether the box is
doomed depends on whether its arena holder survives — which is what
settle computes. Telling a doomed box from a surviving keeper before the
fixpoint is trial deletion, a reset-time cycle collector. And the
reset's order is load-bearing in the other direction: the survivors'
compensating retains must land before the release-log releases, or a
heap child held only by an arena survivor hits zero and is freed while
the survivor still names it. So promotion is the sound answer at the
only instant the reset can decide; what was wrong is that the block
never came back.

**Two shapes the mechanism had to grow.** A block retained for **bytes**
rather than for occupants — the refused-carry fallback, where a
survivor's payload could not leave the dying arena — is *pinned*: the
payload has no death event, so no occupant count can speak for it, and
such a block keeps permanent retention. The rule that would release it,
the last live payload gone, is owed and not built. And a block whose
every occupant died *inside* the reset has nobody left to report the
last death, so `register` says so and the reset hands the block over
itself — after `finish_reset`, because the arena's block chain is
threaded through the very headers the pool overwrites.

**What it exposed, and it was a real defect.**
`promote::survivor_holding_heap_entity_compensates_the_release_log`
killed a promoted survivor by hand while a live `Slot` object still
named it through a property. The dangling property was invisible while a
retained block was never reissued — it read refcount 0 and was skipped.
With the block recycled it named a live object, and a whole-heap
collection then judged a genuine garbage ring reachable. The test now
kills the survivor through its holder's slot, which is how anything else
in the runtime would.

## 2026-08-08 — a reference box is allocated in the GC heap, always

**Decided (Sage):** every `&` box is a GC-heap entity.
`reference::ll_reference_new` takes neither a category nor a context,
because it has nothing to choose and nothing to resolve, and
`reference_die` frees unconditionally.

**Why:** the rule a copy of an array must apply — share a box that has
two holders, unwrap one that has a single holder — needs an exact holder
count at the instant of duplication, and the heap is where this runtime
already keeps one: a heap non-COW box is counted by `ll_retain` and
`ll_release` with no special case anywhere. Both alternatives make the
box counted **in the arena**, which breaks "counted or escaping, never
both" and makes `Reference` a second everywhere-counted kind after COW.
What that costs is spread over the whole runtime for a rare `&`:
`mark_one` must stop zeroing the box's count, the count must travel in
`cow_at_promotion` and settle by edges with a delta, `escape_gain` must
branch on kind because it writes `refcount = 1`, and the retain/release
fast path gains a kind test on every arena entity. Growing `LLReference`
to 32 bytes buys one thing over that — an escapee released before the
reset is not promoted in vain — and costs eight bytes on every box.

**The price, stated rather than hidden.** Every `&` is a heap
allocation, which is Zend's own cost class (`zend_reference` is always
heap). Boxing an element of an arena array crosses the boundary twice:
the element enters a longer-lived holder, so an arena COW value is
copied to the heap once per boxing and an arena non-COW one counts an
escape; and the box enters the arena entry, so its release is logged
against the reset. What becomes impossible is arena-speed `&` and a box
dying for free with the arena. The lifetime objection is answered by the
box's own life: `reference_die` calls `drop_ref`, which calls
`escape_lose`, so a box whose holders do not outlive the request dies at
the reset from the log.

**What the ruling took with it:** an arena box cannot be built any more,
so two tests lost their instrument rather than their subject.
`a_copy_over_an_arena_source_shares_the_box` now reads the sharing off
the box's refcount, which is the count S3.2 will decide on;
`a_surviving_reference_box_carries_its_referent` still asserts survival
and the holder count, but the referent now survives by its own escape
count rather than by promotion recursing through a promoted box.

## 2026-08-08 — the table's version bracket orders both sides with fences

**Decided:** `begin_entry_move` stores the odd version relaxed and then
runs `fence(Release)`; `Table::coherent_entries` runs `fence(Acquire)`
and then loads the version plainly. `end_entry_move` keeps its release
store, and the reader keeps its acquire load at the opening check.

**Why:** a release store orders what precedes it, so the odd version was
free to become visible after the entry moves it is meant to announce; an
acquire load orders what follows it, so the three words the closing check
validates were free to be read after it. Each half admits the same
execution, in which a walker accepts a storage pointer from one table
state with an entry count from another and strides a fresh count over a
stale chunk. That is an edge which never existed, and no later phase
repairs it. The reference seqlock this counter reimplements,
`ck_sequence.h`, writes the odd value plainly and fences after it, and
fences before the closing load (`dev/RESEARCH.md`, Concurrency Kit).

**Demonstrated rather than argued.** `src/array/version_bracket_model.rs`
is a loom model of the bracket alone, and it exhibits the accepting
execution for the old bracket and for either fence taken by itself,
while the pair holds. It models a copy of the protocol, since the table
cannot run under `--cfg loom`; three `should_panic` tests keep the three
failing configurations from being quietly re-adopted.

**Cost:** none on x86-64, measured with rustc 1.96.0 at `-O`: both fences
emit `#MEMBARRIER`, a compiler barrier and no instruction. On aarch64 it
is one `dmb` per bracket, and a bracket runs around growth, compaction
and a walker's read rather than on the element path.

## 2026-08-08 — the creation reference is spent before the displaced original is dropped

**Decided:** the COW store composition runs store, release the creation
reference, drop the displaced original. This supersedes the order named
in the 2026-08-04 entry below and in the doc comments that carried it,
`object::ll_cow_separate` and `string::separate`, which put the drop
second and the release last.

**Why:** `drop_ref` runs `__destruct` bodies, so the store site is
reentrant between the drop and the release. A destructor reaching the
slot just written displaces the copy; with the creation reference still
outstanding that displacement takes the copy to one rather than to zero,
and the release that follows returns a death verdict the store site
discards. The copy is then never torn down, and every child it holds
keeps a reference nothing will give back.

**Why it stayed invisible:** a displaced string runs no user code, so
both orders measure the same at the only site that existed until now.
`array::element::set` is the first store of an entity whose teardown can
call back into the language.

**Cost:** none at the store. The release now runs while the slot alone
holds the copy, so it cannot be the death — asserted in debug builds
rather than branched on.

## 2026-08-07 — a table starts unsalted; the flood ladder's first rung draws the salt

**Decided (Edmond):** unsalted is the ladder's zeroth rung, not a mode
anyone selects. A fresh table indexes an integer key by its value, as
Zend does; the first long chain fires `reseed`, which *draws* the salt —
the mix of the process seed and the storage address — and rebuilds. The
second firing escalates, so the bound of one rebuild and one escalation
per table stands. `ll_array_new` lost its `salt` parameter (twenty call
sites, every one a test), `Table::empty` takes nothing, and a COW copy
inherits the salt with the rung bits through `adopt_flood_state`.

**Why:** the salt is worth paying for where keys can come from outside,
and nothing can classify arrays up front — a compiler flag has to be
right on every array, fails silently in the unsafe direction, and keys
arrive through `json_decode`, a database row, `array_keys` and any
function argument. The ladder needs no prediction: an integer flood
under by-value indexing builds exactly one long chain, which is the
first rung's own trigger.

**Found while doing it:** escalation firing from an unsalted table must
draw the salt on the way, because `strong_hash` is keyed by the salt and
zero is a key every attacker knows — the design's residual assumption
("a new colliding set costs a break of a keyed PRF") would be false.
`draw_salt` is one idempotent place; whichever rung fires first draws,
and a drawn salt is never moved again. The LCG step over the old salt is
gone with the redraw it defended: a salt is drawn at most once, so there
is no orbit to learn.

**Rejected:** the compiler's "external data" flag as the selector
(above; it stays available later as an optimization on top of the
ladder); keeping the from-birth salt for the strong key alone (it taxes
every honest table for a state almost none reach).

**Cost:** the first integer flood on a table pays one long chain and one
O(used) rebuild before the mix separates it — bounded by the trigger,
and exactly the price the ladder already charged for a string flood.
The trigger reads shape, not intent: an honest table whose keys stride
by a power of two — 33 page offsets is enough — fires the same rung,
pays the same rebuild, and spends `TABLE_RESEEDED`, so a later string
chain on that table escalates directly instead of buying its (useless)
rebuild first. `rfc/model/arrays-hashtable.md` amended the same day.

---

## 2026-08-07 — an entry's collision link lives inside the element Box

**Decided:** the array table's entry is 32 bytes — `hash_or_key`, `key`,
element — and the collision link is a `u32` inside the element Box's
reserved bytes, at entry +28. The `next` and `meta` fields are gone. Per
element of capacity the table now costs 40 bytes against the 48 it cost
before, which is what `zend_array` has cost since PHP 7.3.

**Why:** the eight bytes had to come from somewhere no concurrent reader
looks. `hash_or_key` could not go — the flood backstop counts equal full
hashes during the insert's own chain walk, and without an inline copy
every step of an attacker-chosen chain would dereference a cold string.
The element's reserved bytes are read by the collector only as part of
the eight-byte relaxed load it makes for the refcounted bit, and it
masks them away; so the bytes are free as long as **every** write to that
word is one relaxed atomic store of the same width.

That is the whole cost, and it is enforced rather than remembered:
`Entry`'s element field is private, so no caller can assign a whole Box
over the link; `Entry::store_element`, `store_element_and_link` and
`store_link` compose tag, flags and link and publish the word once; and
`Entry::value` hands the Box out through `Value::without_reserved`, so a
link cannot travel in a copy into another entry. `Table::entry_mut` is
gone with them — nothing hands out a `&mut Entry` any more.

Zend does the same thing with `zval.u2.next` and keeps it honest with a
rule its macros obey. The rule here has to be stronger because the
collector reads that word while a mutator writes it.

**Considered and rejected, both attacked before being dropped:**

- The link in the element's reserved bytes with the **store barrier**
  narrowed so it stops writing them. The barrier serves every Box slot,
  so the narrowing would reach object property slots, where the collector
  loads the second word as eight bytes (`walk.rs:217`, `walk.rs:257`,
  `object.rs:410`, `object.rs:443`). A four-byte atomic store against an
  eight-byte atomic load on the same bytes is undefined, and it breaks on
  `$o->p = 5` before any array is involved.
- The entry as **two `Value`s**, key and element, with the link in the
  key half's reserved bytes. A key Box's payload does not classify it —
  an integer key is an arbitrary `u64` — so the collector has to read the
  key's tag, which puts the link back in a word it reads. And a `Map`'s
  object key must be published through the category barrier, whose only
  Value-slot store writes all sixteen bytes and would zero the link;
  zero is a legal entry index, so the chain would fold onto entry 0
  rather than end.

**Cost:** `Table::get` returns a `Value` rather than a `&Value`, so a
caller holds no borrow of the table and owes itself a reference to
anything it keeps; `for_each_value_mut` takes and returns a `Value`
instead of handing out `&mut Value`. Two bytes of per-entry reserve
remain, at entry +26, inside the same atomic word. The layout test that
pinned "a full Value write cannot reach the key or the link" lost its
subject — the link is inside the Value now — and was rewritten to check
the store the table performs, which the old shape could not see.

---

## 2026-08-07 — the `RcHeader` is the only authority on which memory an entity lives in

Edmond's ruling, and it is a rule rather than a repair: **an entity's `RcHeader`
says which memory that entity lives in, and everything it owns out of line lives
in the same memory.** A body is allocated from the category its header names and
is freed to it; the two cannot differ, and no structure keeps a second copy of
the category to be asked instead.

What it overturns is the 2026-08-06 decision that the table keeps its own copy so
it can be used without an entity around it. A copy is a fact that can drift from
the fact it copies, and this one already did: a refused promotion left
`Table::category` reading `RequestArena` under a heap array, so the next storage
came from whatever request arena was mounted and its reset returned the chunk
under a live holder (`2e55036`). The repair then was to rewrite the copy in every
outcome — which is the maintenance a copy demands, paid forever, on every path
that touches the header.

The consequences, in order of who owes them. `Table` loses its `category` field
and reads the owning entity's header instead. `Table::carry_out_of` loses its
four rewrites of the copy: promotion rewrites the header and the table follows by
construction. `Table::empty` loses its category parameter. Tests that build a
bare table build an array instead — a headerless table has no memory identity to
answer with, which is exactly what the rule says.

## 2026-08-07 — the deep copy walks a list, and teardown is the half still on the stack

The escape copy of a nested arena array no longer recurses. A nested arena COW
array is copied *empty*, published into its parent's entry, and the filling of
it pushed onto a work list; the copy loop drains that list. Every destination is
therefore reachable from the root the moment it exists, which is what makes the
refusal path a cascade: releasing the root's children frees every copy the call
published, at whatever depth.

The list lives in a buffer-arena chunk, per the ruling of 2026-08-06. The
machine stack is what it replaces. Arena bump memory would hold the list to the
reset for no reason. A `Vec` aborts the process when it cannot grow, and growth
here is driven by the attacker's nesting depth, so the refusal has to be a value.
An ordinary copy allocates nothing: the list is empty until the first nested
array.

Termination needs no visited set, and the reasoning is the ruling's: the list is
entered only by an arena COW child, and a cycle cannot close inside a pure-COW
subgraph while count-equals-holders holds, because every entity a real ring
passes through is non-COW and is published by the barrier rather than entered. A
debug build keeps the visited set and asserts it; a release build pays nothing.

**What this does not fix, and it is the same attacker's input:** teardown of the
copy is recursive — `array_die` releases a child, the child dies, its own
`array_die` runs — one nested set of frames per level. The depth reaches the
machine stack at the free instead of at the store. The plan carries it as the
other half rather than as a separate finding.

One defect found by the test rather than by reading: the list's growth freed the
old chunk through `dispose`, which also empties the list, so a copy deeper than
the first chunk silently lost every pair it had queued — whole subtrees copied
empty with no assertion anywhere. Growth frees the chunk directly now.

## 2026-08-07 — category routing lives in one module, and the free still needs the category

`memory/routing.rs` answers "which allocator serves this memory category" for
the whole crate: `entity_alloc_in` for the seven factories that build an entity,
and `body_alloc` / `body_ensure` / `body_free` for the bytes an entity owns
outside its own slot. The compiler assigns a category to an owner without
knowing what kind of entity will live there, so the question belongs to the
memory layer; it had been answered by the same `match` written out eight times,
and the two body copies had already drifted apart.

What stays at a call site is what belongs to that caller rather than to the
routing: `ll_string_new_dynamic` refuses the two long-lived categories before it
allocates, because an immortal-flagged dynamic string in a GC entity block is
walked by the census and never released; and `Table::carry_out_of` names its
destination, because `self.category` still reads `RequestArena` when the copy is
made and is rewritten only once the outcome is known.

**The free cannot dispatch on the block kind alone**, which is what the plan
asked for and what the first attempt did. The kind looks like the better source
— it is what the bytes are, while the category is a field promotion rewrites —
but the two populations share a kind: a body over a block payload is an
OS-direct run in *both* arenas, so `BLOCK_KIND_LARGE_RUN` names a run the
request arena logged and frees at its reset just as readily as one the caller
owns. Freeing by kind double-freed arena storage and aborted the suite with
`corrupted size vs. prev_size`. So `body_free` takes the category, which
separates the two populations, and `buffer_free_longlived_payload` keeps
dispatching on the kind inside the long-lived one, where retained blocks, parked
chunks and OS-direct runs genuinely differ.

## 2026-08-07 — the flood ladder's two rungs answer different key kinds

The chain trigger redraws the table's salt once and escalates on the second
firing, which is the bound `rfc/model/arrays-hashtable.md` states and the code
did not have: `reseed` returned early only on `strong`, so an attacker could
make every insert redraw, each redraw an O(`used`) rebuild against a document
promising O(n) twice.

**What the work found is why both rungs exist.** A redraw moves integer keys and
no others, because an integer's slot is `mix_int(k, salt)` while a string's slot
below `strong` *is* its cached rapidhash, which no salt enters. Escalation is the
mirror image: it rehashes string keys with a keyed function over their bytes and
leaves `mix_int` untouched. So the ladder is not one defence escalating in
strength — it is two defences for two key kinds, tried cheapest first. The
consequences are worth stating rather than rediscovering: a pure string flood
spends one rebuild that provably separates nothing before reaching the rung that
answers it, and a pure integer flood is answered by the redraw or not at all,
since escalation gives it nothing. `arrays-hashtable.md` describes the rungs as
an escalation and owes this correction.

**The redrawn salt takes entropy.** The step was a public LCG over the old salt,
so an attacker who learned the initial value knew the whole orbit offline and
could aim the redraw as easily as the original — the one rebuild the bound allows
would then be spent for nothing. The new salt mixes the process seed
(`hash::seed::raw`) and the storage address into the step. Under `hash-folding`
the process seed is a build constant, so such a build's redraw is only as
unpredictable as its storage address, which is ASLR's to give.

`strong` and `reseeded` are bits in one `flags` byte rather than a `bool` each,
because the strategy tag joins them there: `rfc/model/arrays.md` gives an array
three storage strategies and two bits to name the current one, and the entity's
flags word has no free bit for either.

## 2026-08-07 — a fire point inside a teardown collects nothing, and the runtime enforces it

Edmond's ruling: the compiler may put `ll_gc_maybe_collect` inside a destructor
body, and it must return there without collecting. The runtime enforces it
rather than trusting the emitted code — `gc::TEARDOWN_DEPTH` counts teardowns in
flight on the thread, the two doors bracket it (`ll_entity_die`,
`ll_object_die`, which nest), and `collect_cycles` returns zero while it is
non-zero, beside the `GC_ACTIVE` reentrancy test it already had. `COLLECT_PENDING`
is untouched by the refusal, so the arming survives and the next poll at a clean
point collects.

**This supersedes the three-call shape recorded below on the same day**, which an
independent review refuted: with the forget standing before `dispose` and again
after it, an object was still a buffered root at refcount zero for the whole of
phase 2, and phase 2 releases children whose destructors are user code. A
collection fired there computes the dying object garbage — its slots are still
populated, so `mark_gray` trial-deletes through them, and `scan` sees refcount
zero — frees it, and the teardown that was interrupted frees it again. The only
point that closes the window by ordering alone lies between phase 1 and phase 2,
which is inside `dispose`, where the runtime has no call site. The guard closes
it without one, so the forget stays at a single place per door: after `dispose`
returns in `ll_object_die`, and before the kind switch in `ll_entity_die` for
the kinds that run no `dispose`.

Regression: `gc::tests::a_collection_fired_from_a_destructor_does_nothing_and_
defers`, seen returning 2 without the guard — the two objects it would have
freed being the two already dying.

The rc-walk build has carried the same bracket since 2026-07-27 for a different
obligation (no message pickup between a committing zero store and the end of a
dispose), so both configurations now count teardown depth, each for its own
strategy.

## 2026-08-07 — this crate's `EntityKind` is the normative kind assignment

Edmond ruled the entity-kind codes out of the RFC: a design document names
a kind and never prints its number, because the number is a detail of the
encoding (`rfc` `f170662`, and `rfc/dev/DECISIONS.md` of the same date).
The assignment therefore has one normative home, `EntityKind` in
`refcount.rs`, and this repository is free to print numbers — the ban is
on the design, not on the implementation of it.

**What that obliges, and it is not built:** a foreign consumer — the
compiler, when its repository exists — must take the codes from an
exported ABI surface rather than transcribe them. Building that export
now was rejected: it has no consumer, and the kind codes are one row of a
surface whose shape belongs to its main load (`ll_retain`, `ll_release`,
the `RcHeader` offsets, the ValueBox layout). When it is built it is
pinned to the enum by `const` assertions, so agreement is checked by the
compiler rather than by eye. Until then a hardcoded kind code in a
consumer is a defect even when its value is right.

The gate change of the same date is what made the coupling visible, and
`rfc/model/classes.md` carried the same defect in prose — a parenthetical
claiming the candidate buffer holds objects and arrays, separated by bit
13. Corrected in `rfc` `cc50370`, together with a second error the pass
found: consolidating the Proxy family reclaims **codes and not a bit**,
since seven kinds and five kinds both need three.

## 2026-08-07 — the candidate gate is a set of kinds, not a mask over their codes

`rc-trace` admits an entity to the candidate buffer when its kind is in
`refcount::CANDIDATE_KINDS` — `{Object, Array, Reference, Lazy}`, a set
indexed by kind code and tested as `(CANDIDATE_KINDS >> kind) & 1` beside
the existing buffered-bit test. **A kind belongs to the set exactly when it
holds counted slots a cycle can close through.** That sentence is the whole
policy, and it is what the constant carries: String, Box and WeakRef own
nothing a ring passes through, so they stay out by the same test that lets
a Lazy proxy in.

**No mask can express this set**, which is why the shape changed rather
than the constant. A subset compare `flags & MASK == 0` admits only kind
sets closed under clearing a bit; admitting `Reference 011` needs bit 0
clear in the mask and excluding `String 001` needs it set. The entry below
read that impossibility as a reason to wait for a renumbering. It is a
reason to stop deriving the policy from the codes: the mask could say
"which kinds" only by leaning on "which numbers they were given", and that
coupling is what produced the leak. The set is built from `EntityKind`, so
a later consolidation of the codes moves its value at compile time and
leaves its meaning alone.

The leak it closes: `$a['x'] = &$a` — the box holds the array, the array's
element holds the box, and the frame's release lands on the box at count
two. Nothing else is decremented, the box was not admitted, so no candidate
existed and the ring lived to process exit. `$a->next = &$a` never showed
it, because there the last release lands on the object.

**`Lazy` is admitted although no factory stamps kind 6 yet.** Waiting for a
producer before admitting a kind is exactly what left the ReferenceBox
outside; its test is owed to whichever stage builds the Lazy factory.

Three alternatives were rejected, and the reasons matter more than the
choice. Buffering the box's *target* instead of the box keeps boxes out of
the buffer, but pays a data-dependent load on the mutator's hot path, moves
knowledge of the Reference layout into `ll_release`, and still leaves Lazy
needing to admit itself — so it ends as this test plus a special case. A
second compare (`masked == 0 || kind == Reference`) grows one term per
admitted kind and leaves Lazy out on the same argument. The entity-kind
renumbering stays rejected on the grounds recorded in `PLAN.md`, and this
removes its last motive.

Cost, accepted on reasoning because `dev/BENCHMARKS.md` puts this box's
noise floor at 1.5–3 % and the effect is smaller: roughly three ALU
instructions where there were two, on the rc-trace non-final-release tail
only — the `rc-walk` build compiles the branch away entirely. Buffer
pressure grows by every heap ReferenceBox decremented to non-zero,
scalar-valued ones included, so the candidate threshold arms marginally
sooner; that threshold is already unmeasured.

Nothing else needed changing, which was verified rather than assumed:
`ll_entity_die` already forgets a buffered candidate of any kind before its
kind switch, `gc.rs`'s white-free default arm already frees a box, and
`walk::trace_entity` already traces through one. Regressions:
`gc::tests::a_ring_whose_last_release_lands_on_a_reference_box_is_collected`,
seen failing on the old gate, and its rc-walk twin in `walk.rs`, which turns
"the whole-heap walk needs no candidate" from an expectation into a fact.

**Owed to the RFC and not yet made:** `model/classes.md` says the buffer
holds objects and arrays, and `model/lowering.md`'s pseudocode comment names
a heap *object*. Both now describe a gate the crate no longer has.

## 2026-08-07 — the candidate buffer admits arrays, and leaving it belongs to the runtime

`rc-trace`'s candidate gate buffers objects and arrays:
`flags & ((0b101 << ENTITY_KIND_SHIFT) | CYCLE_COLLECTOR_BUFFERED) == 0`,
which is the one masked compare the object-only test already was, because
`Object` is `000` and `Array` is `010` and masking bit 1 away leaves that
pair standing. `rfc/model/classes.md` fixes the pair; the crate admitted
only objects, so a ring closed through a heap array produced no root and
was never collected. Both configurations are required legs of the gate, so
rc-trace stayed green over a systematic leak.

A ring that passes through neither kind was still out of reach: an array
holding a ReferenceBox holding the array (`$a['x'] = &$a`) takes its last
external release on the box, and kind `011` does not join `000` and `010`
in one compare. That is answered by the entry above, the same day: the
compare stopped being a mask.

**Forgetting a candidate is the runtime's duty at every door into
teardown**, where it used to be `ll_default_dispose`'s. A `dispose` is class
code and the compiler emits one per class, so the duty was owed forever by
code this crate does not write, and an array runs no `dispose` at all. The
call sites are `ll_entity_die` before the kind switch, which covers every
kind the gate admits, and `ll_object_die` twice: once before `dispose`,
because a caller that statically knows the object takes that door instead of
the switch, and once after it returns, because `__destruct` can buffer the
object afresh — a transient `$this` taken inside it is a retain and a
release, and that release is a non-zero decrement. The order is the whole
point: a collection fired by user code between the buffer entry and the free
traces the dying entity as a root and frees it, and the teardown then frees
it again.

`ll_free` asserts in test builds that an entity slot arrives unbuffered,
beside the refcount-0 assertion that closed the census flake. A door that
forgets to forget then fails at the free that dangles the root, rather than
in an unrelated test half an hour later, which is where the same class of
defect surfaced before.

## 2026-08-06 — `LongLived` goes out of use, and its rename waits for a mechanism

**As the category of an entity the code does nothing.** It is not counted
(`ll_retain` and `ll_release` return early on any non-zero category that is not
COW), not collected (the census enrolls only `GcHeap`, `walk.rs`), and not
freed by any reset or teardown pass — `rfc/model/memory/arenas.md` still records
the reclamation strategy as undecided and no long-lived arena exists here. Its
memory comes from the same entity blocks as `GcHeap`, so an entity marked
long-lived is an immortal entity housed in the collected heap: it survives to
process exit like an immortal one while occupying a slot the collector strides
over on every walk. Nothing new is stamped with it until that changes. The
`owner_cat` use is untouched and stays — a static block or a global has no
owning entity to say how long the receiving slot lives, and that is a comparison
rather than an allocation.

**The rename was considered the same day and deferred.** Edmond's diagnosis is
right: the category is named after a duration rather than an owner, which is
exactly why its reclamation was never decided, and naming the owner would settle
it — the memory would die with the owner in O(1), like an arena reset. The
proposed names were `Region` for it and `Arena` for `RequestArena`.

`Region` does not fit, and the reason is worth keeping. A class declared
`#[Region]` **owns arenas** (`rfc/model/memory/regions.md`: "owns arenas,
exactly like an actor owns arenas", and its example is annotated "lives in this
region's arena"), so the entities a region owns carry the *arena* category. The
population called long-lived today belongs to no region at all. Naming the code
`Region` would therefore mark precisely the entities no region owns. The other
reading — give a region's contents this code — fails against the store barrier,
which counts escapes on `RequestArena` exactly (`barrier.rs`): a reference from
a heap container into a region would take no hold-count, the reset would promote
nobody, and the first reset would leave a dangling pointer in a live container.

`RequestArena` → `Arena` was deferred with it. The invariant "between two
request arenas: forbidden" (`rfc/model/memory/arenas.md`) holds *because* at
most one request arena is mounted in a context; regions break that construction,
and the general name makes the sentence false before the mechanism that would
justify it exists. Both renames wait on the region reset, which is where it will
be decided what category a region's contents carry.

The objections came from an independent critic pass over the plan and were each
verified against the code before being accepted.

## 2026-08-06 — the array entity, and separation is the shallow copy

`LLArray` is `RcHeader | Table`, with **no per-instance class pointer** — the
same construction as a string. `rfc/model/arrays.md` says it directly ("a single
final class … no per-instance class pointer, devirtualized methods"): `array` is
final, so the entity kind already says what this is, and the storage-strategy
tag is an internal bit invisible to `instanceof`. My first version carried a
class word; Edmond caught it. Eight bytes holding the same value in every array
ever allocated is the trade the string layout already refused, and a layout test
now pins the table at +8. The table holds no header of its own, so it stays
testable without an entity around it, and the wrapper supplies the refcount, the
memory category and the COW flag.

**Separation is shallow, and that is a decision rather than a shortcut.** The
copy gets its own storage and index, replays the source in order so insertion
order survives, and retains each counted child once — elements and string keys
alike. Nothing recurses: both arrays share the children until one is written,
which is PHP's semantics. This is *not* the store barrier's escape copy, which
is deep and category-driven; `rfc/model/arrays-hashtable.md` had to separate the
two because `arrays.md` said shallow and `values.md` said deep about what read
as one operation, and an implementer who sees only the word "copy" builds the
wrong one. A refusal part-way releases what the copy retained, disposes the
private storage and reports — the source is untouched, because nothing was
published.

**A reference into an element is a `ReferenceBox`, never a pointer to the slot.**
The other form `values.md` offers retains an owner and points at a slot, which
is right for slots that never move; an element moves on every growth and every
compaction. Two tests pin it by doing the thing that would break a slot pointer:
take the reference, then insert five thousand keys, then write through it.

**Complete enumeration is the tracer's, and the hole marker is why it works.**
`for_each_value`, `for_each_value_mut` and `for_each_string_key` scan the dense
prefix and skip holes, which the arena reset requires to be complete rather than
conservative. A test writes a full sixteen-byte value straight into a dead slot
and checks the hole survives — that is the whole reason the marker lives in the
`key` field rather than inside the value, and the reason the `Value` sits last
in the entry.

---

## 2026-08-06 — the array table's flood backstop counts equal full hashes, and a latent test defect surfaced with it

The backstop from `rfc/model/arrays-hashtable.md` is built. Per insert and
against current state, the walk counts two things: entries whose full 64-bit
hash equals the incoming key's, and the chain length. Nothing accumulates
between operations, which is what makes it survive deletion — a running maximum
would stay high on an emptied table forever. Eight equal full hashes escalate
the table once and one way to a keyed hash over the key's bytes; a long chain of
keys whose hashes *differ* redraws the per-table salt instead, and a second
firing escalates. Redrawing the salt in response to equal hashes is precisely
what made Perl's REHASH exploitable, and a test pins that it does not happen.
The string's cached hash at +16 is never touched, being shared with every other
table holding that string; a test pins that too. Integer keys go through a
salted avalanche mix rather than being indexed by value, and a test pins that
512 keys on a 1024 stride do not share one bucket.

**Two defects of my own, both found by tests rather than by reading.** The
entry's `hash_or_key` holds the key's own identity — the raw integer or the
string's cached hash — while the index slot comes from a *different* number, the
salted mix or the keyed hash. I conflated the two twice, once in matching and
rebuilding and once in what `insert` stored, and each time the symptom was that
insertion succeeded and lookup lost every key. Second: table storage was going
through `entity_alloc`, and the cycle collector reads the first eight bytes of
every occupied slot in an entity block as an `RcHeader` — storage has no header,
so that would have been corruption at the first walk. It goes through the
ordinary allocator now.

**An old test was corrected, not muted.**
`reserved_cells_are_accounted_returned_cells_recirculate` asserted `n >= 1` and
then read `cells[1]`, and took the stride unsigned although the free list is
LIFO and returns cells in descending address order. Both were latent: the array
tests changed pool pressure enough to make a one-cell reserve real, and the
subtraction underflowed. The run's length is `contiguous`, not `n`, and the
stride is signed. No assertion was weakened.

---

## 2026-08-06 — an oversized immortal allocation takes an OS-direct run instead of aborting

`immortal_alloc` routed anything above one block payload into an `assert!`,
justified as a caller bug: immortal entities are class metadata and interned
strings, and those are small. That reading holds only while no caller forwards
input, and the release profile is `panic = "abort"`, so the assert kills the
worker rather than raising. Two callers falsify the premise. A class's
`[Class][vtbl][itables]` train has no size bound. And `intern` would forward an
attacker-shaped length the moment the RFC's runtime-interning arm were taken —
that arm is now dropped (`rfc/model/classes.md`, "A runtime-built name is
matched, never interned"), but this crate must not depend on a document to stay
memory-safe.

**The shape.** A request above `BLOCK_PAYLOAD` takes an OS-direct run aligned to
`BLOCK_SIZE`, with its payload at `+LINE_SIZE` like every other block, so
`BlockHeader::of_ptr` still finds a header carrying `BLOCK_KIND_IMMORTAL`. It
carries no size field: it is never freed, `ll_free` on an immortal pointer is
already a no-op, and nothing enumerates immortal blocks. It does not touch the
bump region either — a huge entity must not abandon the remainder of the current
block behind it.

**An old test was replaced, not muted.** `oversized_immortal_is_a_caller_bug`
pinned the abort and is gone, because the behaviour it pinned is the defect.
Two tests replace it and both were seen failing on the old `assert!` first: the
run is readable and writable end to end under the right block kind, and it leaves
the bump cursor undisturbed. Gate green in both configurations, three threaded
runs each; Miri silent under the workflow's `-Zmiri-ignore-leaks`, which the
immortal region needs by construction.

**Cost.** An oversized immortal entity rounds up to whole blocks, so a 65 KB
entity occupies 128 KB. Immortal memory is never reclaimed, so that waste is
permanent — acceptable for metadata allocated once at link time, and the reason
the path is `#[cold]`.

---

## 2026-08-05 — one template class, and the site's identity is a static shape

`rfc/model/strings.md` rule 3 gave every interpolation site its own
generated class carrying the parts table. **Edmond's call, this date:
one class for all of them**, and what differs per site is a plain
structure in memory that is never allocated and never freed — a count and
a pointer to that many interned immortal parts. A class per string
literal buys nothing: the consumer's declared type is the same interface
either way, and the site's identity is its parts.

The instance stays an ordinary entity, `RcHeader | class | shape |
Value[n]`. Its values are exactly what the substitutions produced, so
they are `Value` boxes rather than pointers: an int or a bool sits in the
box, a string or an object is a reference the collector must see, and
Edmond confirmed the second half explicitly — once the template exists it
is an ordinary object and the collector reads its values.

**What it costs, and it is three places rather than one:** the number of
values is a property of the instance, not of the class, so the class's box
runs cannot describe the body, and every walker that strides an object's
slots needs the branch. `for_each_counted_child` (which teardown,
promotion and the synchronous collector share) and `sever_counted_slots`
(the drain's, which needs the lvalue) were the two the first version
found. The third is `collector::trace_mature`, the concurrent collector's,
which reads cells relaxed-atomically and therefore keeps its own copy of
the stride — an independent review found it missing, with a ring through a
template surviving three epochs while the control ring died. The error was
conservative (an under-counted in-degree reads as rooted), so it leaked
rather than freeing early, but it leaked forever.

**What that says about the shape:** a fourth copy of the stride is one
`git grep` away from existing, and nothing in the type system stops it.
Two `debug_assert`s now close the states that would make such a walk read
past the end of an instance — a template class carrying a property, and a
template built through `ll_object_new`, whose `object_size` is 16 bytes
and whose shape word would be past the body.

**The factory takes its own references** rather than consuming the
caller's, and publishes each value through the store barrier instead of
writing the slot: that is what applies the escape and COW-copy rules, and
what makes a refusal (a copy that cannot be allocated) reportable. A
refused build releases what it had already stored and returns null.

**Flattening measures before it allocates**, so a value with no text yet
stops the whole thing rather than producing a wrong string. Two such
values exist and each waits on something outside this crate: a float
needs the language's precision rules, and an object needs `__toString` —
user code, which rule 3 requires to complete in the measuring pass,
before the result is allocated, so the call site is already where it will
have to go. `new_uninit`/`finish_uninit` in `string.rs` are what let the
result be allocated once and filled in place instead of assembling a
buffer and copying it in.

**Not built, deliberately:** the C ABI a foreign consumer would read the
structure through. Edmond's call — there is no consumer until the
compiler exists, and the signatures would be a guess.

## 2026-08-05 — a buffer block carries its own cursor, so an adopted block is reused and not just held

The entry of 2026-08-04 below named the debt: an adopted buffer block
never became current and `pop_fit` consulted only the current block, so
neither its tail nor its inherited free list served an allocation, and a
block abandoned with one 16-byte chunk held the other 63 KiB until that
chunk died. Both halves are paid here, and the objection that entry
raised — "resuming a foreign bump would mean storing the cursor in the
header and trusting it across an owner's death" — turned out to be
answered already. The adopter reads `live`, `free` and `owned_next` of
the same header across the same death; the cursor is one more field of
the same kind, and what orders it is the abandoned list's lock — the
dying owner settles the cursor before taking the lock to post the block,
the adopter reads it after taking the same lock. (The `Release` store of
`owner` in `adopt` is the adopter's own and orders nothing against the
previous owner; an earlier draft of this entry offered it as half the
proof.) `heap.rs` has kept its blocks' cursors this way since
2026-07-26, and mimalloc, whose model that heap follows, keeps a page's
`capacity` in the page for exactly this reason: a reclaimed page is
pushed straight into the new owner's queue and extends further
(`src/page.c`, `_mi_page_reclaim`; verified against the vendored source
of `libmimalloc-sys` 0.1.49).

**Decided:** `BufferBlockPrivate` gains `bump`, the block's own cursor.
The arena keeps caching it while the block is current — the allocation
path still touches one line — and `settle_cursor` writes it back wherever
the block can change hands, which is rotation and hand-over. Adoption
then takes the request's size: if the adopted tail fits, the block
becomes current and the pool is not asked at all.

**A second look at an owned block was needed:** `resume_owned`, which
goes back to any owned block whose tail fits. Without it a block adopted
for a request its tail could not serve is looked at once and never again,
and the 63 KiB stays out of circulation exactly as before — the first
version of this change had that hole, and the test written for the
ordinary case (adopt on a large request, need a small one next) is what
showed it.

**The order is adopt, then own tails, then the pool — the opposite of
`heap.rs`, deliberately.** `heap.rs` finds a block with room in
`available` and reaches the abandoned list only when there is none;
stated the other way round in an earlier draft of this entry, which read
`alloc_no_block` as the whole path instead of its cold tail. The reason
to differ here: an ownerless block has nobody to collect the frees still
being posted into it, and one pickup per rotation keeps that population
from growing while a thread keeps finding room in its own blocks. The
price, worth naming because nothing measures it yet: a busy arena
accumulates foreign blocks it can never empty, and a rotation walks the
owned chain three times — `collect_owned`, `resume_owned`, and `pop_fit`
in `critical`.

**The free list follows the same rule:** `critical` mode searches the
lists of all owned blocks, current first, with `CRITICAL_SEARCH_BOUND`
misses as one budget for the whole chain rather than per block, so the
bounded walk `buffers.md` promises stays bounded as the chain grows.
`plenty`/`tight` still never consult holes; the tail, unlike a hole, is
served in every mode, because bumping into it is what bump allocation
already is.

**Regressions**, one per claim, each seen failing with its own branch
neutered and only its own: the adopted tail serves the request that
adopted it (`adoption_resumes_the_tail_when_it_fits_the_request`); the
tail of a block adopted for a request it could not serve serves the next
one (`an_adopted_tail_serves_the_request_after_the_one_that_adopted_it`);
an inherited hole in a non-current block serves a fitting request in
`critical` (`critical_mode_reuses_a_hole_in_an_adopted_block`); and a
hole behind a current block whose list has spent the budget is not
reached (`the_critical_search_budget_covers_the_whole_chain`).

**A leak in the tests surfaced with it.** Allocation addresses now depend
on what other tests left on the global abandoned list, and two tests that
follow one named block started failing in the suite while passing alone.
The cause was one missing `free` in a third test, whose arena died
holding a chunk; the first repair was scaffolding that drained the list
for the affected tests, which would have absorbed the next leak just as
quietly. The leak is fixed instead, and the two tests that read the list
directly now fail when one appears — the only place in the suite where a
stranded buffer block is visible at all.

## 2026-08-04 — the critic pass on the hash stage: the stamp did not separate the pair it existed for

Seven findings against the entry below, all above the hash function
itself — the transcription was checked against the vendored header line
by line and holds, including the cascade's non-obvious `2,2,1,1,2,1`
secret sequence and the `position + remaining == len` identity the tail
read depends on.

**The one that mattered.** `STAMP` mixed in the seed only under folding,
and an unset `LL_HASH_SEED` made that seed zero — so a folding build
without a seed and a non-folding build produced **the same stamp**. A
program folded under seed zero, run against a runtime drawing per
process, passed the check and missed every folded lookup. Verified by
building both and printing the stamp: `0x8418e9313e109773` from each. The
stamp failed at precisely the pair it was built to catch, and the only
thing standing in front of it was a `#[cfg(test)]` assertion that
`cargo build --features hash-folding` never runs.

**Closed twice over**, because one fix leaves the class open. A `const`
assertion beside `BUILD_SEED` refuses a seedless folding build at compile
time; and `FOLDS` puts the arm itself into the stamp, so even an explicit
`LL_HASH_SEED=0` — legal as a bit pattern — cannot alias.

**`FUNCTION_VERSION` was a promise nobody could keep.** It had to be
bumped by hand for a changed secret, a changed zero remap or a changed
vendored version; nothing checked. Regenerating `vectors.rs` against a
header with one different secret word left the suite green, the stamp
unchanged, and a shipped folded program silently wrong. It is now
`FUNCTION_IDENTITY`, derived in a `const` block from the secret array,
the zero remap and the bulk stride, with a hand-bumped `FUNCTION_REVISION`
left for the one case constants cannot show — a different upstream
version with identical constants.

**The vendored header was checked by nothing.** `README.md` records a
sha256 that no test and no build step compared against anything, and the
generator and the port both read that same file, so an edited header
yields a self-consistent table and a green suite. A test now pins the
crate's own hash of the header — a weaker digest than sha256, and the
right question ("is this the same file") without carrying a sha256
implementation for one assertion.

**`parse_seed` accepted four inputs it documented as build failures**,
and all four evaluated to zero: `_`, `___`, `0x_`, and `2^64` wrapping
around. `0x_` is the shape a typo in a CI file takes. It now requires at
least one digit and rejects anything past 64 bits; the earlier argument
for wrapping — "refusing a large seed refuses half the space" — was
wrong, since the whole space is expressible.

**Two tests asserted things they could not observe.** `raw() != 0` cannot
fail, because the draw remaps zero away. And two `RandomState::new()`
calls on one thread differ because std bumps a thread-local counter
between them, whatever the entropy source did — so the test that "a stub
returning a constant fails here" would have passed on a platform whose
randomness was fixed, which is the failure that arm exists to prevent.
The draw is now compared across threads, where the keys really are
initialized from the source.

**`build.rs` is gone and its stated reason was false.** It claimed cargo
cannot see `option_env!`. Cargo can: rustc records `# env-dep:LL_HASH_SEED`
in dep-info and cargo consumes it. Verified on this toolchain across
every transition including unset→set — the seed changed, the artifact
rebuilt, with no build script present.

**Also:** `ll_hash_stamp_matches` returns `u32` rather than `bool`, since
Rust lowers `extern "C" -> bool` to `i1 zeroext` and this crate is
*merged* with compiler-generated IR rather than linked, where a
declaration saying `i8` would take a wrong answer from the one function
whose job is to be right. `ll_hash_seed_init` lets runtime startup draw
the seed instead of leaving a `getrandom` syscall at whichever
`LLString::hash` happens to run first; the lazy path stays as the
backstop, because a late seed is silent and a late init is not. `init_at`
gained a `debug_assert` that a supplied hash is one this build computes —
`intern` is the only caller and it is right, but nothing checked.

**Left open, named rather than fixed:** a thread inside `LazyLock::force`
at `fork` time leaves the child blocked on a `Once` nobody will wake.
Structural rather than demonstrated, and it needs a runtime that forks
with threads live, which does not exist yet.

## 2026-08-04 — folding a literal key's hash is a build option, and it is off by default

`rfc/model/strings.md` left it open whether the compiler folds the hash of
a literal key at all, and defaulted to not folding while saying elsewhere
that in the AOT modes "the short path's seed cannot be" per-process
random — a sentence that only holds if it does fold. The two are one
question, because a compiler that folds has to know the seed while it
compiles, and a seed drawn when the process starts is not knowable then.

**Decided (Edmond's call):** make it optional rather than settle it. The
`hash-folding` cargo feature selects the pair.

- **Off, the default.** The seed is drawn from the OS once per process
  (`std::hash::RandomState`, the only portable source in the standard
  library and not worth a dependency). The compiler emits no hash
  constants.
- **On.** `LL_HASH_SEED` fixes the seed at build time, the compiler is
  given the same value, and it folds. The seed then travels inside the
  artifact.

**What folding buys is one load, not the multiplies the RFC priced.** A
literal key is interned, and an interned name is hashed once at creation
(`intern.rs`, `string::init_at`), so the hash of a literal is already
computed once per process. Folding replaces a load from a permanently hot
address with an immediate, and generated code needs the interned pointer
anyway for the identity compare. That gain has not been measured, and
this crate does not land a speed change on reasoning — which is the
second reason the option is off by default rather than on.

**What it costs is the seed's secrecy**, and the threat is concrete:
array keys in a web request are attacker-supplied, so an attacker who
knows the mapping from string to bucket sends N keys that collide and
turns insertion into roughly N²/2 comparisons. With the seed inside the
artifact, that set is computed once, offline, against every deployment of
that build.

**Neither arm is a defence.** A per-process seed raises the attack from
reading a constant to mounting a timing attack, and no further: rapidhash
descends from wyhash and claims no resistance to key recovery from
observed collisions, and seven of its eight secret words are published
constants. Bounding the worst case is the hash table's job — the
probe-length counter with an escape hatch named in `strings.md` — and
that table has no design yet (`rfc/model/arrays.md`). **This entry must
not be read as closing hash flooding.**

**The seed is expanded once.** The reference opens with
`seed ^= rapid_mix(seed ^ secret[2], secret[1])`, which folds away under a
constant seed and would otherwise run per call, ahead of the first byte
and on the chain feeding the finalizer. `expand_seed` and `hash_expanded`
split it so the static holds the expanded value; `hash` keeps composing
the two, so the vector test still exercises the reference's entry point.

**The stamp exists because the alternative failure is silent.** Folded
constants live in the compiled program and the function that must agree
with them lives in the runtime; nothing in the linker compares them, and a
disagreement produces lookups that miss with no crash and no failing
test. `hash::seed::STAMP` identifies the function and — only when folding
— the seed, `ll_hash_stamp_matches` compares it, and generated code is
required to call it at startup. Emitting the program's half is owed by the
compiler and does not exist yet.

**`build.rs` arrived with this** for one line:
`cargo::rerun-if-env-changed=LL_HASH_SEED`. `option_env!` is invisible to
cargo, so without it a rebuild after changing the seed silently reuses the
artifact built under the old one.

**"Per process" is per address space, and a pre-forking server has one**
(added 2026-08-11, from the module doc this entry now carries). A seed
established before `fork` is inherited by every worker, so in the
deployment shape this language is aimed at — a master that forks workers,
as php-fpm does — the guarantee degrades from per-process to
per-deployment: one recovered seed serves every worker for the life of the
master. Drawing on first use rather than at startup does not fix it, since
the master hashes at least the interned names before it forks. Fixing it
means redrawing after `fork` and rehashing everything already cached,
which no caller can do today. This is a limit of the arm rather than a
defect in it.

**Rejected: making a default seed fail the whole suite.** The literal
reading of the plan's "a test that fails when a seed is left at its
default" would turn the documented gate — plain `cargo test --lib`, no
environment — red by construction and leave the default build path the
one nobody runs. Each arm tests its own mechanism instead: with folding,
that `LL_HASH_SEED` was supplied; without it, that two draws from the OS
differ. A third test checks what neither would catch, that the installed
seed reaches `hash_bytes` at all.

---

## 2026-08-04 — the rapidhash port is checked against vectors we generate, because the author publishes none

`rfc/model/strings.md` makes the hash a compiler/runtime contract: the
runtime computes a string's hash and the compiler is meant to fold the
same value for a literal key. A single constant transcribed wrong
breaks that contract without breaking anything a test would normally
notice — the result is still a well-distributed 64-bit hash, stable
within the process, never zero, and every existing test in this crate
passes on it. The symptom appears later and elsewhere, as table lookups
that miss.

The plan's task said to run the author's reference test vectors in CI.
He has none: `github.com/Nicoshev/rapidhash` holds `rapidhash.h`,
`secret.h`, `bench/`, `collisions/` and `old_version/`, and nothing that
pins an expected value.

**Decided (Edmond's call):** vendor the reference header at one pinned
commit and generate the vectors from it once —
`vendor/rapidhash/generate_vectors.c` compiles the header, hashes a
fixed input list under four seeds, and prints `src/hash/vectors.rs`.
The table is committed, so running the suite needs no C compiler;
regenerating it does.

**Rejected: building the C reference at test time** through `build.rs`
or a `cc` dev-dependency and comparing live. It proves the same thing
and demands a C toolchain everywhere the suite runs — the position the
`mimalloc` dev-dependency is already in, which is why it sits behind
`cfg(not(miri))` in `Cargo.toml`.

The inputs are chosen at the reference's branch boundaries, not for
realism, and the long ones are not written out byte by byte: the
generator fills them from `index * 167 + 13` and the consuming test
reproduces that. The two definitions are a pair, and the file that
holds each says so.

**What the table cannot check:** that the vendored header is the one
the compiler side will use. Nothing enforces that yet, because the
compiler side does not exist; when it does, the pin in
`vendor/rapidhash/README.md` is what both sides have to name.

---

## 2026-08-04 — the COW reconciliation carries a delta, because promotion is not the end of the reset

`reconcile_cow_counts` assigned each COW survivor's count from the edges
between survivors, on the argument that the holders dying with the arena
never release and there is no list of them to subtract. The argument
covers holders that existed at mark time and no others, and the reset
does not stop there: promotion rewrites categories **inside** the
settling loop, and the release-log drain runs `__destruct` bodies after
it. A destructor that hands an already-promoted string to a heap object
adds a legitimate reference that no edge between survivors can see —
and it takes the ordinary store path, since the string reads `GcHeap` by
then and the barrier has nothing to copy.

Found by the critic pass on this day's own work, with the failing
sequence spelled out. Both outcomes are silent: the cache dies first and
the string is torn down under a live holder, or the keeper dies first and
`ll_release` runs at zero.

**Decided:** each COW survivor's count is recorded at the instant its
category is rewritten, and the reconciliation settles it as
`edges + (count_now − count_at_promotion)`. Edges replace what the count
said about arena holders; the delta carries across whatever happened
after, whoever did it.

**A second finding closed with it:** `walk::trace_entity` documents its
skipped kinds as conservative, which is true for the collector — an
omitted source only removes in-edges and pins its targets — and false for
a pass that decides a count from the edges it finds. An array survivor
(Phase C) would have its elements' references erased rather than ignored.
`traceable_in_full` asserts the survivor's kind is one the tracer
enumerates completely.

**And a third:** the escapee branch of the old reconciliation was
unreachable — every survivor passes the promotion loop first, which
clears `IS_ESCAPEE` unconditionally. The delta form has no such branch.

**Regression:** `promote::tests::`
`a_holder_acquired_after_promotion_keeps_its_count` — a heap entity torn
down by the release drain stores the promoted string into a cache, and
the count has to come out 2.

## 2026-08-04 — a COW value is copied out of the arena, and the store barrier can say no

`values.md` asserted two invariants over the same four bytes: on a COW
entity the refcount equals the number of holders, in every category; and
while `IS_ESCAPEE` is set, the field holds the arena escape hold-count.
Both applied to a request-arena COW entity taken by a longer-lived
holder — `$o->name = $s` with `$o` in the heap, the most ordinary string
store in the language. A `debug_assert` in `escape_gain` hid it in debug
builds; release builds overwrote a live holder count with a hold-count of
one.

**Decided (Edmond):** the store barrier copies. A COW value has no
identity a program can observe, so a longer-lived slot takes a copy in
the GC heap rather than a hold on arena memory — the deep copy
`arenas.md` already named for value-like data, and the reason an object
is promoted instead: an object's identity *is* observable. A COW entity
therefore never becomes an escapee, and the collision disappears rather
than being arbitrated.

**The COW write rule loses its `IS_ESCAPEE` arm.** It tested a bit that
nothing can now set, on the write path.

**A publish can fail, and says so.** The copy is an allocation, so
`store_ptr`, `store_box` and `ref_store` — and the three C ABI entries —
return whether the store happened. A refusal leaves the slot and every
count exactly as they were, which is what makes it reportable rather than
a broken invariant: generated code raises memory-exhausted where it
already checks a factory's null. `exceptions.md` is amended, because its
"the barrier is funded, not checked" paragraph was written when the
barrier's only failure was a fixed-size log record. A reserve cannot fund
a copy the size of the value.

**Rejected:** emitting the copy in generated code before the store, so
the barrier could stay `void`. It works and it was the recommendation
here; Edmond chose the channel, and it is the better half of the two —
the runtime keeps the decision about what a store means, and the compiler
keeps only the raise.

**Regression:** `barrier::tests::`
`a_cow_value_leaving_the_arena_is_copied_rather_than_counted`, seen
failing on the old `debug_assert`. The old predicate test that asserted
the dead arm moved with the contract rather than being weakened:
`string::tests::the_rule_reads_the_flag_before_the_category_and_the_count`
now records where the invariant is enforced.

## 2026-08-04 — the buffer arena is a heap like the other one, and gets the same ownership rules

A string is an object of reduced form, and an array will be one too
(Edmond, this date). Its payload is therefore an entity's **body**, and a
body is freed by whichever thread drops the last reference — the rule the
object heap already implements. The buffer arena implemented the opposite
one: thread-local, frees from the owning thread only, a Phase-1 note
deferring the rest until "a real consumer needs it". The consumer arrived
with the GC-heap dynamic string and the note was not revisited, so a
string created on one thread and released on another decremented a live
count and pushed a free-list link across threads, and could hand a block
to the pool while its real owner was still bumping into it.

**Decided:** the buffer arena carries what `heap.rs` carries — a
per-block `owner`, a per-block lock-free MPSC stack for foreign frees,
`live` written only by the owner, hand-over at thread exit with a global
abandoned list, and adoption on the refill path. The header had room: the
block's header line is 256 bytes and the old header used 16, so the
owner's fields sit on the first cache line with `owner`, and
`remote_free` alone on the second.

**Two departures from the heap's version**, both because a buffer block
is bump-filled rather than slotted. The owner keeps a chain of its blocks
— a block the bump has moved past is reachable no other way, and its
posted frees would sit there forever. And **an adopted block is
reclaimed, not reused**: it never becomes current, `pop_fit` only
consults the current block, so neither its bump tail nor its inherited
free list serves an allocation. A block abandoned with one live 16-byte
chunk is held, swept on every rotation, and returns whole when that chunk
is freed — while the adopter takes a fresh pool block for the allocation
that triggered the adoption. That is the weaker half of what `heap.rs`
gets from adoption, and it is stated here rather than as "the tail of one
block", which is what an earlier version of this entry called it
(corrected 2026-08-04 by the second critic pass). Resuming a foreign bump
would mean storing the cursor in the header and trusting it across an
owner's death.

**Rejected:** moving GC-heap payloads to `ll_alloc`, which answers
cross-thread free for free. It puts continuously varying, realloc-heavy
churn into size classes and back into the object heap, which is the
isolation `buffers.md` exists to state. One protocol is cheaper than two
ownership rules.

**Regressions**, both seen failing: a foreign free that sent the owner's
block home, and a block dropped on the floor by an arena that died
holding a chunk.

## 2026-08-04 — the epoch test moves to the free that does not pass `ll_free`

`ll_free` is described as the single funnel every ordinary local free
passes, and the deferral window's test sits there. A buffer-arena chunk
does not pass it: `buffer_free_longlived_payload` reads the block kind
and calls `BufferArena::free` directly.

**Decided:** the branch makes the test itself, rather than routing the
call through `ll_free` to reach the existing one. `ll_free` cannot free a
chunk anyway — `BufferArena::free` needs the granted capacity, and a live
chunk carries no metadata to read it from.

**The whole call parks, not the link write.** `free` writes
`{ next, size }` into the chunk *and* decrements the block's live count,
and an emptied non-current block goes back to the global pool, where it
is re-stamped as another kind. Parking only the write would leave the
second half live.

**The parked record widens to `(pointer, size)`**, with size zero meaning
the `ll_free` route. The alternative — reading the capacity back at flush
time — has nowhere to read it from, which is the same zero-metadata
contract that makes `free` size-carrying in the first place.

**Regression:** `deferred_free::tests::`
`a_buffer_chunk_parks_instead_of_being_written_into` — a pattern written
into the chunk survives a free made while the epoch is open.

## 2026-08-04 — the reset traces through one tracer, and a COW count is settled after the fixpoint

Promotion kept its own idea of which entities have children: `is_object`
on the flags, then `object::for_each_counted_child`. Every other kind was
a leaf. A reference box (kind 3) has one counted child and `walk.rs` has
known it since A2 — so a surviving arena `&` was promoted with its
referent left in the dying arena. `ll_reference_new` accepts
`RequestArena`, so the shape is producible today.

**Decided:** the three traversals of the reset — `mark_subgraph`,
`retrace_survivors` and `count_children` — go through
`walk::trace_entity`. One tracer, one place for the array kind to arrive
in, and no second authority to drift from the first.

**The count model splits on the COW bit.** For a non-COW arena entity the
count field is idle — retain and release return early on it — so the
reset's zero-at-mark and rebuild-from-edges is free to use it as scratch.
A COW entity is counted in every memory category (`values.md`), and the
fixpoint is where destructors run: `unset($this->s)` reaches `ll_release`
on the same string a survivor holds, and a count zeroed at mark time
underflows there. Marking now leaves a COW count alone, and
`reconcile_cow_counts` settles every COW survivor once, after the last
destructor, from the edges that remain.

**It assigns rather than adjusts.** The holders that die with the arena
never release, so the count carried out of the fixpoint is too high by
exactly their number and there is no list of them to subtract. The
surviving edges are enumerable, and they are the answer. Between the mark
and the reconciliation the count is too high, which is the safe
direction: it can only cause a separation that was not strictly needed.

**Rejected:** keeping the true count and subtracting the dying holders —
that needs a walk of the whole dying population, and the trace is bounded
by the escaped subgraph on purpose (`arena-reset.md`). Also rejected: a
private kind switch inside `promote`, which is the second authority the
first paragraph is about.

**Regressions**, both seen failing first:
`promote::tests::a_surviving_reference_box_carries_its_referent` (the
referent still reads `RequestArena` after the reset) and
`promote::tests::a_destructor_may_release_a_cow_survivor_during_the_fixpoint`
(a subtract overflow inside the reset, which aborts rather than unwinds).

## 2026-08-04 — the buffer arena joins the hand-freed structures, and the exit order gains a fifth step

`buffer_arena.rs` carried a note from 2026-08-03: its `THREAD_BUFFER_ARENA`
was a `RefCell<BufferArena>`, therefore had drop glue, therefore had a TLS
destructor registered after the exit guard's and running before it — safe
only while nothing on the thread-exit path reached it, and to be converted
"the moment a teardown frees a long-lived buffer, which strings and arrays
will do". The dynamic string is that moment: static-block teardown runs
user code, that releases entities, and `string_die` returns a payload
through `with_buffer_arena`. The panic would be an `AccessError` inside a
TLS destructor, which cannot unwind — an abort of the whole process on a
worker thread ending normally.

**Decided:** the same shape as the other three conversions — a
`Cell<*mut BufferArena>` with no drop glue, allocated on first use, freed
by an explicit `dispose` that `ll_thread_exit` calls. `Drop` stays on
`BufferArena` itself: stack-built arenas in tests still need it, and it is
now run by hand rather than by TLS glue.

**The order in `ll_thread_exit` becomes:** static blocks → candidate
buffer → parked list → weak table → **buffer arena** → the heaps. The
arena is fifth because every step above it can still free a buffer: the
static blocks through user code, and the parked backlog's flush through
the same payload route. It is before the heaps only because nothing after
it needs an arena; the blocks it hands back go to the process-global
pool, which outlives every thread. Disposing earlier is not detected — a
later free would build a second arena through the lazy path and leak it —
which is why the position is written down rather than inferred.

**Regression:** `static_block::tests::`
`a_thread_that_just_ends_frees_a_static_strings_payload`, a worker that
puts a dynamic string in a static and simply ends. Verified by restoring
the `RefCell` and watching the process abort.

## 2026-08-04 — promotion carries a survivor's payload, and the two routes are not the same route

A dynamic string that survives an arena reset keeps its entity — the
block holding the header is retained — but its payload is arena memory
and would go back to the pool. Promotion now asks one kind-dispatched
question, `carry_external_memory`, so the reset still knows nothing about
any layout: it holds blocks by the address of a header and does not
otherwise look inside an entity.

**An OS-direct payload transfers; an in-block payload is copied.** Not
symmetry for its own sake — the difference is what removes the failure
mode. A reset runs at the end of a request, where there is no caller left
to report a refusal to, and a transfer allocates nothing: `forget_large`
zeroes the arena's record of the run and the drain skips zeros. The
in-block route has to copy, since the block itself is going home, but it
is bounded by a block payload.

**When the copy is refused**, the payload's block joins the retained set
— the same mechanism that keeps the survivors' own blocks out of
circulation, for the same reason — and the string reads its old bytes for
the rest of its life. `buffer_free_longlived_payload` gained a
`BLOCK_KIND_RETAINED` arm that does nothing, which is that fallback's
other half. **Rejected: failing the reset**, which would introduce a
class of failure nothing above it can handle.

**Superseded:** the escape ban of the entry below, which lasted a day.

## 2026-08-04 — a dynamic string may not escape its arena, and the escape barrier says so out loud

`escape_gain` asserts the escaping entity is **not** COW, an assert
written to keep COW values out of the hold-count path. The dynamic string
is the crate's first arena-allocatable non-COW entity, so that assert
admits it by construction.

It must not be admitted. Promotion keeps the survivor's header and
rewrites its category but knows nothing about the payload, which is arena
memory: retention keys on the block holding the *header*, so the
payload's block goes back to the pool at the reset and the survivor reads
recycled memory. Worse than the dangle, the block is later re-stamped
`BLOCK_KIND_BUFFER`, and `buffer_free_longlived_payload` routes by
reading that kind — so the eventual free decrements a live count and
writes a free-list link into a chunk the buffer arena never granted.

**Decided:** a real `assert!`, not a `debug_assert`, refusing an escaping
entity of kind String with `COW` clear. In release the alternative is
silent corruption several allocations later, and the compiler allocates a
dynamic string only where it proved a single owner — so reaching this is
already a broken contract, and an abort naming the cause beats a heap
whose free lists disagree with it.

**Rejected: banning `RequestArena` for dynamic strings.** That taxes the
common and perfectly safe case — an accumulator that dies with the reset —
to prevent the rare one.

**Lifted when** promotion carries the payload (`rfc/model/strings.md`,
"An arena string that survives the reset takes its payload with it";
`PLAN.md` task 13).

## 2026-08-04 — a COW copy is sized by the holder's lifetime, and comes back at +1

`values.md` gives the separation rule and says the holder stores what the
barrier returns, but leaves two things to the implementation, and both
are the kind that corrupt quietly when guessed wrong.

**Where the copy lives.** `ll_cow_separate` takes `owner_cat` — the
**holder's** category, supplied by the compiler as it is to every other
store-side barrier — and the copy goes to the request arena when the
holder is arena-category, to the GC heap otherwise. The original's
category does not enter into it. A copy is a fresh entity nothing has
registered anywhere, so the only way it goes wrong is a holder outliving
it; an arena holder cannot, and every other holder needs something that
survives the reset. This also keeps a copy out of the two categories that
cannot own a written string at all — immortal is shared process-wide, and
`string_die` frees only `GcHeap`, so a long-lived copy could never be
reclaimed.

**Rejected: deriving the category from the original**, which is what the
first version did. Arena original plus longer-lived holder then produces
an arena copy under a heap slot — a cross-lifetime reference the barrier
was called to prevent — and interned original plus arena holder produces
a heap allocation and a release-at-reset record for a value the reset
would have reclaimed for free.

**The reference the copy comes back with is +1, owned by the caller**,
like every other factory here, and the composition is written out in
`string::separate`: separate, store (which retains), drop the displaced
original, release the creation reference. The last step is the one worth
naming — skipping it leaves the copy at two for one holder, and that is
worse than a leak: the sharing test reads `2 > 1` on every later write,
so the value separates forever and COW is off for the rest of its life.

**The COW flag is tested before the category**, although `values.md`
lists the category first and mentions the flag only in its third line.
For every entity this crate produces the answer is the same, since every
string is COW; the reorder buys two things. A dynamic string
(`COW = 0`, bytes out of line) that reached the barrier by mistake writes
in place as its layout demands instead of having its header read as
inline bytes. And a **non-COW plain object** that is immortal or escaped
is no longer copied — under the document's order it would be, and copying
a plain object breaks reference identity.

**Found while reviewing this, not fixed here:** `values.md` justifies the
category test with "the count is pinned", which describes immortal and
not long-lived — a COW entity takes neither early return in `ll_retain`,
so its long-lived count is fully maintained. The arm is kept for the two
reasons that do hold, recorded at `cow_separation_needed`.

## 2026-08-04 — the string header follows the design, and the design is what the test pins

`LLString` carried `hash` at +8 and `len` at +16; `rfc/model/strings.md`
specifies the opposite, and names +8 and +16 explicitly because the
dynamic layout has to put the same two fields in the same places. The
code had matched `zend_string`'s order instead, which is where it
presumably came from. Nothing was broken by it — one string kind exists,
it is immortal, and only `intern.rs` reads those fields — and nothing
would have been broken until the dynamic layout arrived and started
reading a hash as a length.

**Decided:** the fields are reordered to `len`, then `hash`, and
`intern::tests::layout_matches_the_string_design` pins all four offsets
plus the struct size. Verified by putting the old order back and
watching the test fail (`left: 16, right: 8`); the module's other tests
stay green through the swap, which is the reason the contract needs a
test of its own rather than an assumption.

**Rejected: amending `strings.md` to the code's order.** Either order
works in isolation, but the document is authoritative here and its text
leans on the specific offsets; changing the code is two lines and one
comment, changing the design is a paragraph other documents cite.

## 2026-08-04 — the runtime reaches compiled PHP code as bitcode, not across the C ABI

`Cargo.toml` declared `rlib` + `staticlib` "for the C++/LLVM layer" and
`dev/INDEX.md` repeated it, so the crate read as though its surface to
generated code were the `ll_*` C ABI. It is not: the crate is also
emitted as LLVM bitcode and merged with compiler-generated IR
(`llvm::Linker::linkModules`), and the optimizer inlines across the
boundary — `README.md`'s "LLVM IR export" records `ll_retain` fully
inlining into its caller after `opt -O2`. The design behind it is
`rfc/runtime/implementation-language.md`; the C ABI proper survives only
between this crate and the thin C++ layer that holds LLVM.

**Decided:** both places now say so and point at those two documents.
The practical consequence for new work: a new entity kind does not need
an `ll_*` entry point to be reachable from generated code, so one is
added when a caller exists, not by convention. Strings are the first
kind to be built under that reading (`PLAN.md`, task 3).

**Cost:** a hot path is only fast in the builds that do the merge; a
consumer linking the `staticlib` and calling across the real ABI gets
the call, not the inlined body.

## 2026-08-03 — thread exit owns the order its per-thread state dies in

A6 made thread exit run user code for the first time: the static-block
teardown runs `__destruct` bodies, and those cascade into the candidate
buffer, the parked-free list and the weak table. All three were
`thread_local!`s holding a `Vec` or a `HashMap` — drop glue, therefore
registered for TLS destruction. `ll_thread_exit` is itself reached from
a TLS destructor (the guard `ll_thread_init` installs), and glibc runs
destructors in reverse registration order, which puts that guard
**last** exactly because it registers first. So the structures it needs
are reliably already destroyed, `LocalKey::with` panics with
`AccessError`, and a panic inside a TLS destructor cannot unwind: the
process aborts.

**Decided:** every per-thread structure the exit path can reach is a
`Cell<*mut T>` — no drop glue, never registered for destruction,
readable for the whole life of the thread — freed by an explicit
`dispose` that `ll_thread_exit` calls in an order it chooses:
static blocks (the only user code) → candidate buffer → parked list →
weak table → the heaps. `weak.rs` had already written down that its
table must go last once static-block teardown existed; this is that day.

**Rejected: `try_with`.** For the weak table it is not a smaller fix but
an unsound one — a swallowed death notification leaves the cell's target
dangling, and the next `__destruct` calling `get()` receives a retained
pointer into freed memory. `try_with` is right only where failure means
"fall back to the global tier", which is why `block_pool` and `reserve`
already use it and keep it.

**Rejected: making the exit pass run before any TLS destructor.** There
is no such hook in Rust for a thread that simply ends, and the ordering
cannot be arranged: registration order is first-access order, the guard
is registered at `ll_thread_init`, and "last first access" is
data-dependent. Windows order is unspecified too.

**Rejected: one consolidated per-thread state block** behind a single
TLS pointer. It would put a gc candidate buffer (L4) and a weak table
(L3) inside a structure owned by `heap` (L2) — an upward-knowledge
violation and a design event by this file's own rule — for no
correctness gain over per-module cells. It stays available later,
behind the same accessors, if a measurement ever justifies collapsing
the TLS reads; on Windows it would replace three dependent loads with
the one-instruction `gs` read.

**Cost:** none on `ll_release`, which is untouched instruction for
instruction — the buffered-bit test reads flags already in a register
and `buffer_candidate` is `#[inline(never)]`. Inside the moved bodies a
`RefCell` borrow-flag load/store pair becomes one dependent load plus a
null check that is cold after first use: not a new cost class, and
**not** claimed as a speedup, because this box's noise floor is 1.5–3%
and nobody measured it.

**Found by the same review, closed here:** `ll_static_block_register`
could abort on a refused growth, where this crate refuses instead
(`try_reserve`, and a hand-rolled allocation as in `ll_thread_init`).
`buffer_arena`'s key has the same drop-glue shape and no caller on the
exit path yet; it carries a note rather than a change.

**One suggestion from that review was tried and reverted, by Miri.**
Candidates still buffered at thread death keep their buffered bit while
their blocks go to the abandoned list, so an adopted object released
later can never be re-buffered — a silent cycle leak. Clearing those
bits during disposal looked free, since the drain walks them anyway.
It is not: a candidate can already be **gone**. An arena entity dies
with its arena without individual teardown, so nothing calls
`forget_candidate` for it and its buffer entry outlives its memory;
Miri reported a write through exactly such an entry (`gc.rs`, in-bounds
pointer arithmetic on a dangling address). The disposal now frees the
buffer and touches nothing in it, and the stale-bit leak is recorded as
a known limit on `gc::dispose` — closing it needs a liveness test the
buffer cannot provide, which is a design question rather than a
cleanup. Cross-thread entity survival is reserved today, so nothing
reaches the case.

## 2026-08-03 — a static block registers its layout, not a teardown function

A6 needs each thread to release its static blocks' roots at exit, and
`rfc/model/classes.md` describes the pass as a **compiler-emitted**
per-block teardown striding the block's reference slots. No compiler
emits one, so the registration ABI had to be chosen now and survive the
generated version later.

**Decided:** `ll_static_block_register(block, layout)` — a bare pointer
to the headerless block and the descriptor whose `ptr_runs`/`box_runs`
give its reference slots. The runtime strides them generically, exactly
as `ll_default_dispose` stands in for a generated dispose (A3).

**Why not a function pointer now.** `register(block, teardown_fn)` reads
like the eventual shape, but the only function it could carry today is a
generic one that still needs the descriptor — so the descriptor would
travel anyway, as a bound argument, and the ABI would carry two things
where one suffices. When the compiler does emit straight-line teardowns,
`ll_static_block_register_fn(block, fn)` is **additive**: a second entry
point, no change to this one, and blocks registered either way tear down
in one LIFO order because the registry holds the two forms in one list.

**Order is LIFO**, as C++ tears down function-local statics: a block
initialized later may have been initialized against an earlier one, so
the earlier one has to outlive it.

**Drops go through the barrier's `drop_ref` with `owner_cat =
LongLived`**, not a bespoke release. That is what makes the three cases
come out right without a branch here: a request-arena escapee loses its
escape hold-count (thread exit is the only decrement point besides an
overwrite mid-request), a heap reference releases and cascades into
teardown, an immortal one is a no-op. The block being headerless does
not matter, because `drop_ref` acts on the displaced entity and
`owner_cat` is a parameter rather than a load from owner flags — which
is why the barrier was built that way (A4).

**Cost:** a thread that never registers a static block pays one null
check at exit. The slot stride now has its third occurrence, so it is
abstracted rather than copied: `sever_counted_slots` takes a base and a
descriptor, and object teardown, the drain's sever and this pass all
call it.

## 2026-08-03 — retained blocks are walked through a per-block object index

Retained former-arena blocks were unwalkable because an arena's bump
allocator leaves mixed sizes and no stride to divide by, so their
occupants were root sources and a ring living entirely among promoted
survivors was never collected (`rfc/model/gc/retained-block-walk.md`).
The reset already builds the inventory — `promote`'s fixpoint collects
every survivor into a vector and then drops it. It is now kept, split
per block and sorted by address, as that block's object index. Three
properties make it cheap: the set is frozen because nothing allocates
into a dead arena, a dead entry validates itself by reading refcount 0
(the walk's own occupancy test), and a retained slot is never reissued
because the block leaves the pool only when all its survivors are gone.

**Granularity: one index per retained block, not one per retained set.**
Both enumerators reach a block first — one by the 64 KiB alignment mask,
the other by scanning the region registry — and need the index from a
block address. A per-set index would need a second block→set map on the
lookup path that the per-block one does not.

**Totality: the census keeps one lookup.** rc-walk.md requires row
omission and edge omission to be one decision taken at one test, and a
census with two sources could have become two lookups with two answers.
Instead the single sorted payload list carries both kinds of block, and
only the slot derivation branches after the match: a strided block
divides, an indexed block binary-searches its own index. One search,
one answer, and an address in neither kind still finds nothing.

**Lifetime: the index is owned by a registry keyed by block address**
(`memory/retained.rs`), released by one call. Nothing calls it yet —
retained blocks never return to the pool today — so the index lives
exactly as long as the retention it describes, which is the correct
lifetime and not a leak. The call exists so the hook is one line when
"return a fully emptied retained block" lands.

**Rejected: copying survivors into entity blocks at reset** so the
existing strided walk covers them. That is the evacuation the RFC
defers behind the escapee-reference fixup, it costs a copy per survivor
where the index costs one pointer, and it would have made a placement
choice out of a collectability requirement.

**Cost:** one pointer per survivor for the life of the retention, and
`EntityBlockSnapshot` stops being plain numbers — it now carries the
index for a retained block. The collector still computes slot addresses
itself and still touches slots only through the relaxed-atomic helpers;
what it gained is data it reads, not slots it reaches into.

## 2026-07-28 — the epoch walk's child test is a dense census, not a map

The collector's walked-row lookup is a per-slot `u32` array laid out
from the block snapshot, indexed by (block via 64 KiB alignment mask +
binary search over sorted payloads, slot via division with a remainder
validity test). **Why:** the per-epoch `HashMap<address, row>` hashed
every child and cost a build pass over the walked set — measured 2–3×
of the whole walk step (`BENCHMARKS.md` 2026-07-28). Rejected: hashing
with a cheaper function (still a probe per child, still a build pass);
a region-indexed table (more machinery, only pays if the binary search
ever shows up in profiles). Cost: the snapshot allocates and fills
4 bytes per snapshotted slot, virgin tails included — collector-side,
transient, two orders below the win.

## 2026-07-28 — the batched checkpoint splits: ack before the run, pickup after it

The lowering contract for a release run changes (rfc `3faf110`,
`model/gc/rc-walk.md` "Batched releases"): one `ll_gc_checkpoint_ack`
(new ABI) fronts the run — the activity bit is observed before any
free, as on the death branch — and the full `ll_gc_checkpoint` trails
it. `ll_release_vector` carries the same split. **Why:** a pre-run
pickup judges posted components while the run's transients are still
counted — a scope-exit loop then presents every pickup with the same
held borrow, the phase-lock that would defeat the forced verdict.
Rejected: keeping the single pre-run checkpoint (the phase-lock),
pickup on both sides (a second full test buys nothing after eager
death — any death in the run already picks up at its dispose's exit).
The synchronous collection also joins the pickup gate (`walk_active`
beside mid-drain and teardown depth): it is drain-class and holds
guards a message may name. Cost: none measured (`BENCHMARKS.md`
2026-07-28); four regressions pin the shape — the ack-only front, the
ack-before-first-death position, the vector phase-lock, the walk-active
gate — each verified failing.

Edmond's redesign (rfc `c2f91b1`, `model/gc/rc-walk.md`). A release
reaching zero mid-epoch now runs full teardown at the natural point —
`__destruct` on the owner thread, weak notify, sever, free — with only
the memory parked. Deleted wholesale: the F5 deferral branch, the
deferred-death marker, the shared condemned byte (bits 24–31 return to
the free pool), `collector_condemn`, `acquit_condemned` and the
acquittal message kind, `Epoch::drop`'s owed acquittals. Condemnation
is collector-private (the candidate list); acquittals are dropped in
private; only confirmations post. `drain_confirmed` opens with the
**corpse rule**: any member reading `rc 0` drops the message whole
before a field is traced or a guard written — DC0 closed by refusing
the message instead of preventing the corpse.

Two pre-existing BLOCKERs surfaced by the adversarial review of the
amendment, fixed in the same change (causally dependent — the eager
path makes both universal):

- **The death-branch checkpoint acks only** (`epoch::checkpoint_ack`);
  message pickup and the parked flush ride the outermost dispose's
  exit (`teardown_enter/exit`, bracketed in `ll_object_die` /
  `ll_entity_die`) and are refused mid-teardown by the full
  checkpoint. Between the committing zero store and dispose the dying
  entity's weak cell is live, and a drain destructor's
  `WeakRef::get()` returned a strong reference to the corpse
  (regression `the_drain_never_sees_an_entity_between_commit_and_
  dispose`, verified failing on the old order).
- **Parking is out-of-band** (`deferred_free` keeps a thread-local
  vector; a parked slot is never written until the flush). The old
  in-slot park link overwrote the class word at bytes 8–15 under the
  walker, which dereferences `+8` one pass after reading the header
  (regression `parking_leaves_the_corpse_bytes_intact`, verified
  failing on the in-slot write). Cost: the park path may allocate —
  cold, epoch-only. Flush frees in reverse park order to keep the
  LIFO free-list behaviour.
- `Epoch::drop` now **waits for posted confirmations** before
  releasing the deferral window (two epochs' verdicts must never be
  in flight).

**Why:** the deferral traded destructor timeliness — the one
userland-visible semantic, and design principle 1 — for drain
simplicity; the parked slot already guarantees corpse identity, so
refusing the message is as safe as preventing the corpse, and the
mutator's death path drops its last collector test. **Cost:** a
component that partially dies between posting and drain waits an
epoch for its survivors; the rfc's TLA+ battery models the
pre-amendment protocol until re-derived (banner notes in the rfc).

## 2026-07-27 — the drain goes through the relaxed header helpers; the exclusivity window is proven separately

**Decided:** every header access in the verdict drains
(`drain_confirmed` / `acquit_condemned` / `exact_test`) uses the
relaxed helpers (`update_header_flags`, `mutator_guard_retain`, the new
`header_refcount` twin), although the drain window is provably free of
collector interference. The proof — three links: post follows the last
read (queue mutex), no return after post, ack follows the drain with
Release/Acquire on the close gate — lives in `rfc/model/gc/`
`drain-window.md` with a dedicated TLC spec (`DrainWindow.tla`: sound
run exhausts in 23 states; three kill variants, one per link, each
violates the invariant). **Why:** the plain accesses were flagged by an
independent review as violating the crate's own rule; the soundness
argument was real but written nowhere, which is a trap for every future
reader. **Rejected:** keeping plain accesses and documenting the
exception — the relaxed forms are the same x86 instructions, so an
absolute rule costs nothing. **Cost:** none (cold path, identical
codegen).

## 2026-07-27 — undef stamping is the factory's, from descriptor `undef_runs` (A5)

**Decided:** the class descriptor carries `undef_runs` — the Box slots
declared without a default, as `(offset, count)` runs, and the generic
factory stamps `VALUE_UNDEF` over them after the zero-fill (a
compiler-known `new` site emits the same stores straight-line). The
layout regroups a class's defaultless Boxes behind its defaulted ones,
so the undef run is one contiguous tail of the box trace run.
**Why:** an all-zero Box is `null`, not undefined, so somebody must
stamp; the out-of-line factory is the dynamic-class path and can only
read the descriptor. **Rejected:** a per-`PropSlot` flag — the factory
would scan every property to find the few defaultless ones; runs make
the stamp a stride, the crate's existing trace-map idiom.
**Cost:** physical order diverges further from declaration order
(already true of the run grouping; `declaration_index` carries the
observable order). Construction is the field's only consumer.

## 2026-07-27 — bulk object operations: vector release and cell reservation (rfc bulk-operations.md)

**Decided:** two compiler-facing ABI groups. `ll_release_vector` — one
call per release batch, checkpoint served once at entry, destructors
in vector order (physical recycling order stays the manager's).
`ll_entity_reserve` / `ll_entity_cells_return` + `ll_object_new_in` —
best-effort-contiguous runs of GcHeap entity cells, keyed by size
class, consumed by a construct-into-cell factory. The parameters are a
request; the manager decides — it may refuse or serve short, and v1
policy is tame: blocks on hand first (virgin bump tail, then free
list), at most one pool draw, never region growth. **Why (Edmond):**
no per-object ABI call on statically-known batches, and deliberate
co-location of object graphs. **Rejected:** an arena reservation ABI —
one bump allocation of `count x size` already is the contiguous run
there. **Cost:** reservation flips the tail-first order (ordinary
alloc pops the free list first) — reserved runs prefer virgin memory;
contiguity is best effort in every category and never spans blocks.

## 2026-07-27 — the narrow mutator: hot paths store only the counter half; the condemned byte is three-valued

**Decided:** rc-walk's `ll_retain` and non-final `ll_release` store
only the 4-byte refcount half — no flags store, no condemned-byte
clear (rfc, "The narrow mutator"). The F5 branch mints a
deferred-death marker (byte value 2); the acquittal duties and the
exact test discriminate by it. `Epoch::drop` posts acquittals for
condemned-but-unposted components. **Why:** measurement showed the
clear-on-touch filter forced a whole-word RMW chain onto every counted
operation (+0.5–0.6 ns per retain/release pair); the filter was never
the safety gate — the Phase 4 exact test is. Adversarial review
(Fable, fresh context): SOUND WITH CONDITIONS; both conditions are in
this change, one of them fixing a pre-existing defect — the acquittal
duties keyed on `rc == 0` alone and could tear a corpse that died
ordinarily before its condemnation landed (parked slot, class word
already a park link; regression test seen failing at `object.rs:486`).
**Rejected:** collector-side byte clears (the zombie-minting race,
already rejected in the rfc); keeping the hot-path clear for its one
epoch of earlier acquittal on transient borrows. **Cost:** stale
verdicts survive to Phase 4 and are dropped there (cold);
transiently-borrowed garbage rings die one epoch earlier (observable
in destructor timing); mixed-size atomics remain Miri-invisible, as
before.

## 2026-07-27 — the epoch checkpoint rides the death branch of ll_release, not the entity factory; batched releases pay it once

**Decided:** `epoch::checkpoint()` moves from the end of
`heap::entity_alloc` to the `1 → 0` branch of `ll_release`, entered
after the release's own header store and before any teardown. New ABI
pair for lowering: `ll_gc_checkpoint` (serve the protocol now) +
`ll_release_batch` (`ll_release` minus the test), so a compiler-emitted
run of releases pays one checkpoint test instead of one per death.
**Why (Edmond):** the factory taxed every object creation with a test
that has nothing to do with creation; death is the one event the
protocol cares about — only deaths recycle memory — and the death
branch is already cold and expensive, so the test drowns there. Every
fast path (alloc, free, factory, non-final release) now carries
nothing. **Rejected:** ack riding `ll_free`'s existing parking branch —
drains and flushes cannot live inside free (free-in-free reentrancy),
and splitting the three duties across two sites bought nothing over
one cold home. **Cost:** epoch liveness is now tied to deaths — a
workload with no entity deaths starves the epoch until the poll
(finding F2 reshaped, recorded in the rfc); `ll_release`'s death
branch grew the test, unmeasured like its predecessor.

## 2026-07-27 — rc-walk is the default build; rc-trace moves behind --no-default-features

**Decided:** `default = ["rc-walk"]` in `Cargo.toml`. rc-walk is the
primary GC strategy (Edmond, 2026-07-27); the default build must be
the one that ships. **Why:** the opt-in feature made rc-trace look
primary and every plain `cargo` command exercised the secondary
collector. **Rejected:** keeping rc-walk opt-in until its collector-side
economics are measured — measurement gates step 5, not the default.
**Cost:** plain `cargo bench` now measures the rc-walk configuration —
existing baselines in `benches/RESULTS.md` were taken on rc-trace-default
builds and comparisons must name the configuration; Cargo feature
unification now switches any workspace consumer to rc-walk unless it
opts out.

## 2026-07-27 — weak references: the cell is the canonical WeakRef entity; one table row, not a subscriber list yet (rc-walk step 4)

**Decided:** `src/weak.rs` implements `rfc/model/weak-references.md`
as designed — no separate side entry, per-thread table, notification
at every death site — with one narrowing: the table row is a single
canonical-cell pointer, not the design's tagged subscriber list. The
list exists for `WeakMap`, which the crate cannot represent yet;
building the enum for one live variant is abstraction ahead of its
third case. The row widens when maps land; nothing else changes.

**Also decided here:** the weak walk hangs off arena reset in *both*
reset paths — `Arena::reset_with` (bare mechanics) and
`promote::arena_reset_full` — because a skipped walk is not a missing
feature but a dangling cell; the memory layer calling `weak` is the
same peer-service shape as its existing `ll_release` call. And
`promote::die` now dispatches through `ll_entity_die` (it read the
class pointer of any entity — wrong for kinds 3/5, which have none;
a released reference box in the release log leaked its Value).

**Rejected:** a `Subscriber` enum now (one variant, no consumer);
notification from the collector thread (the table would need locks —
every site already runs on the owning thread).

**Cost:** none measured; the hot paths gain one masked test on an
already-loaded flags word, teardown only.

**Decided:** under `rc-walk`, every mutator store a free-running
collector can race compiles as a relaxed atomic: object-field stores
(`barrier::write_ptr_slot` / `write_value_slot`, used by the store
micro-ops, sever and the reference box), header flag writes and reads
on GcHeap teardown paths (`mutator_load_header` /
`mutator_update_flags` / the guard pair in `refcount.rs`), and block
`kind` stores (`block_pool::store_block_kind`, release — pairing with
the snapshot's acquire, so a block reading "entity" has its class and
zeroed slots visible; kind is now published last at commissioning and
in the large-allocation headers). Field *reads* stay plain: the
collector writes nothing but the two header bytes.

**The one exception is the bump cursor — resolved by not reading it.**
An atomic `bump += 1` measured **+14% on larson** (isolated by
reverting exactly that line; `dev/BENCHMARKS.md`). Instead the
snapshot takes no cursor: commissioning zeroes every entity-slot
header, so the walker scans whole blocks and virgin slots skip on the
occupancy test. Collector-side work for mutator-side zero — the
design's own trade, and the rfc carries the amendment.

**A soundness hole found and closed while sweeping:** the dispose
guard's transient `rc 0 → 1 → 0` bypassed the F5 rule with plain
arithmetic — an entity condemned *while its own destructor ran* would
finish teardown under the verdict, and the drain would later tear a
freed slot. The un-guard is now condemned-aware
(`mutator_unguard_release`): reaching zero under the byte defers the
rest of teardown to the drain (fields intact, `DESTRUCTOR_RAN` set —
torn exactly once there). With that, the deferred-death store keeps
the byte SET as the drain's marker, so the acquittal duties can tell
a deferred death (tear it) from a slot that died ordinarily after a
touch and was freed (leave it) — `acquit_condemned` snapshots the
deferred set before clearing bytes.

**Known limits, accepted and on record:** collector byte stores
against mutator word stores are mixed-size atomics — sound on x86-64
and AArch64, unrepresentable to Miri, so the free-running stress test
is Miri-ignored while every stepped test keeps Miri coverage; a
mutator thread must not exit while an epoch is in flight (entity-block
retirement is between-epochs only — actors revisit).

---

## 2026-07-26 — the collector is a steppable state machine; edge validation is one map lookup (rc-walk step 3, commit 4)

**Decided:** the collector side (`src/collector.rs`) exposes each epoch
phase as a separately callable step (open → snapshot → walk → judge →
condemn → recheck_and_post → close), with `run_epoch` chaining them
behind spin-yield waits for the threaded shape. Stepping is what the
danger-case forcing harness needs (`rc-walk-review.md` layer 3: "a
collector single-stepped between walk, condemn and check") — and it
makes the deterministic tests single-threaded, so no data race executes
and Miri's verdict is meaningful. Phase 2 (`garbage_components`) was
extracted from `collect_cycles` and is shared array math.

**Child validation collapsed to a row lookup:** the design's occupancy /
slot-boundary / epoch-byte checks exist so the walker neither
dereferences a racy child pointer nor records an edge into a skipped
row. Recording edges as *indices into the walked-row map* achieves both
in one lookup — a child that maps to no row (immature, reused,
non-GcHeap, or garbage bytes) is dropped with a counter, and no child
is ever dereferenced at all. The A8 clause holds by construction.

**Mature-only class chase:** the walker reads a class pointer at `+8`
only for entities stamped in an *older* epoch. The factory's
header-last publish is relaxed, so a fresh entity's class store has no
ordering guarantee — but a mature entity's publish is separated from
this epoch's reads by at least one handshake, which is the fence. The
new/current classification is therefore also the memory-ordering guard.

**On record as owed (commit 5):** a free-running mutator still issues
*plain* stores — object fields (barrier micro-ops, sever), header flag
bits (`DESTRUCTOR_RAN`, drain guards), block cursors — that formally
race the collector's atomic reads. The stepped tests never execute
those races; the concurrent stress tests of commit 5 require the
relaxed-atomic sweep of those mutator sites first, with the hot-path
codegen re-measured (`bump += 1` and the barrier stores are
benchmarked paths).

---

## 2026-07-26 — checkpoints ride the factory allocation and the poll; the drain is one thread-local bit (rc-walk step 3, commit 3)

**Decided:** the rc-walk checkpoint (handshake ack + verdict drain,
`src/epoch.rs`) is tested at the **end of `entity_alloc`** and in
**`ll_gc_maybe_collect`** — not in raw `ll_malloc`. Two relaxed loads
and a predicted branch, taken only when the collector wants attention.
The verdict queue is a mutex (cold, per-epoch trickle — the 2026-07-20
cold-lock rule). Drain non-reentrancy is one thread-local bit: a
nested allocator entry from a draining destructor serves memory and
acks a handshake, but never picks up a message (finding F8).

**Why the placement:** the design puts checkpoints inside the memory
manager, not compiler polls — but inside `Heap::alloc` a `&mut Heap`
is live, and a draining destructor that allocates would re-enter the
heap through a second `&mut`: undefined behaviour, not just a design
hazard. At the end of the free function `entity_alloc` the borrow is
dead and reentrancy is plain recursion. Raw `ll_malloc` stays
checkpoint-free: it is the benchmarked C-ABI hot path, and a
buffer-only workload delays the epoch no worse than the accepted
no-allocation limit (F2).

**Per-message drain vs the batch:** `walk::drain_confirmed` processes
one component alone; that is sound only because a destructor's release
into a *different* condemned component stops at that component's
condemned byte. The synchronous `collect_cycles` keeps its batch-guard
umbrella — there no bytes are set, and a sibling component would be
unprotected. Two flows, each documented with its own invariant.

**Acquittal duties order:** clear every member's condemned byte
*first*, then tear the deferred deaths from a pre-snapshotted set —
a teardown can release another member to zero (byte must already be
clear, or the death defers to a drain that never comes), and it can
free a live member (so counts are never re-read mid-loop). The
confirmed path clears bytes before its guards for the same reason:
its own un-guards must reach real deaths.

---

## 2026-07-26 — deferred free parks through the dead memory itself (rc-walk step 3, commit 2)

**Decided:** the GC activity bit is one global `AtomicBool` tested with
a relaxed load in `ll_free` after the kind dispatch; a parked
allocation threads a **thread-local intrusive list through its own
bytes 8–15**, and the owning thread flushes through the real free
between epochs (`src/memory/deferred_free.rs`). All four freeable
kinds park — entity slots, raw heap buffers, pooled large, OS-direct
runs — not just the two the walker chases today.

**Why bytes 8–15:** parking must not allocate (a free path with a
growth failure mode is a new defect class, and a `Vec` push is exactly
that), the memory is dead and ≥ 16 bytes in every freeable kind, and
bytes 0–7 of an entity slot must keep the final refcount-0 header —
the walker's occupancy test. Same choice the in-slot free list already
made, same cache line.

**Why all four kinds:** the walker chases array storage once Phase C
lands, and storage can be any size; parking large/huge now costs
nothing and removes a future correctness edge. Not parked: no-op kinds
(arena, retained — nothing recycles, identity holds) and cross-thread
`free_foreign` (single-mutator crate; actors reopen it — on record).

**Owed to commit 4:** the collector must publish the bit through a
handshake **before** snapshotting (a mutator that has not observed the
bit can still recycle a snapshotted slot), and the flush runs on the
owning thread after the epoch closes.

---

## 2026-07-26 — GC strategy is a build-time cargo feature; rc-walk claims the flags top half (rc-walk step 3, commit 1)

**Decided:** the `rc-walk` cargo feature selects the rc-walk collector
in place of rc-trace. Under it: flags bits 16–23 are the **epoch byte**
(header byte 6) and 24–31 the **condemned byte** (header byte 7);
every `ll_retain`/`ll_release` clears the condemned byte with one mask
on the header word it already loads and stores; a release reaching zero
on a condemned entity **skips teardown** and leaves the death to the
Phase 4 drain (the F5 rule); `ll_release` loses the candidate-buffering
tail entirely. All header accesses on this path compile as relaxed
atomics on the whole 8-byte word, and `RcHeader` is now `align(8)` —
the factory always published it as one 8-byte store, the attribute
makes the requirement explicit (the pinned layout test moved 4 → 8).
Header publication is centralized in `refcount::publish_header`.

**Why:** the two strategies claim the same bits — rc-trace's candidate
index is bits 15–31 — and rc-walk's mask on every retain/release would
corrupt a live index, so coexistence in one binary is impossible; the
rfc makes strategy selection build-time for exactly this kind of reason
(`rfc/model/gc/strategies.md`). Runtime switching was considered and
rejected: a per-operation branch in retain/release is the cost the
rc-walk design exists to avoid, and a shared layout would cost rc-trace
its O(1) candidate forget.

**Evidence (release asm, x86-64):** rc-walk `ll_retain` is one 8-byte
load + mask/inc + one 8-byte store, no lock prefix, no RMW; `ll_release`
likewise, with the condemned test two `sete` on the already-loaded word
and **no call tail** (rc-trace's buffering call is gone — the design's
advertised net reduction). The default configuration's code is
unchanged. Verification now runs both configurations
(`dev/WORKFLOW.md`); rc-trace's tests are gated to the default
configuration, where they keep running.

---

## 2026-07-26 — entity blocks are a second heap population (rc-walk step 1)

**Decided:** GC entities (`GcHeap`/`LongLived` factory allocations) come
from their own block population — `BLOCK_KIND_ENTITY`, served by a second
`Heap` instance per thread — while raw C-ABI allocations keep
`BLOCK_KIND_HEAP`. Four rules make the population walkable
(`rfc/model/gc/rc-walk.md`): the in-slot free-list link moves to slot
bytes 8–15 (bytes 0–7 keep the dead entity's final refcount-0 header —
the occupancy test), commissioning zeroes every slot's first 8 bytes,
the factory publishes the header last as one 8-byte store, and the block
pool records every region base in an append-only registry so blocks can
be enumerated. Abandoned-block lists are segregated per population.

**Why:** a walker cannot tell a live 40-byte object from a live 40-byte
C buffer in a shared block, and reading the buffer's first 8 bytes as a
header is a wild read. Design and proofs live in the rfc repo (rc-walk
spec, model, 22-run TLC battery).

**Considered and rejected:** an in-header FREE stamp (dies with the
first free once the link lived in bytes 0–7 — retired in the spec);
moving the link only for entity blocks (two code paths for no gain: the
new offset is the same cache line, and every class is ≥ 16 bytes).

**Cost:** one 8-byte zeroing store per slot per entity-block
commissioning (cold, once per block); a second `Heap` per thread
(~1.2 KB); huge entities (> 8 KB) stay outside the walk, conservatively.

---

## 2026-07-25 — a generated lifecycle body unrolls small, loops large

**Decided:** the counted-field strides of a compiler-generated lifecycle
operation — `dispose`'s releases, the retain strides of `clone` /
`deep_clone` / `thread_*`, and `factory`'s non-zero-default stamps — are
emitted unrolled for a class with few counted fields, and as a loop over
`traced_runs` once the field count crosses a threshold. The RFC's
"straight-line, release slot 1, release slot 2, …" is the small-class
shape, not an absolute (`rfc/model/classes.md`, "Generated body shape";
`lowering.md`, Allocation).

**Why:** these bodies run once per object, so unrolled path length wins
for the common handful-of-fields class — no loop counter, no map load, and
the releases cancel against matching retains under the ARC optimizer. But
code size is paid per class *per operation*; a class with dozens of
counted fields would emit those dozens in `dispose`, again in `clone`,
again in each `thread_*`, bloating the icache for no gain. Past the
threshold the loop reads the same `traced_runs` the GC already strides, so
a large class needs no second teardown map — the trace map serves both the
inline data consumer (GC) and the looped code consumer (`dispose`).

**Not fixed by the model:** the crossover field count is a codegen tuning
parameter, calibrated against real workloads like every other size
threshold here (order of a few tens of fields). The model fixes the two
shapes and the rule that picks between them, not the number.

**Bearing on this crate:** `dispose` is generated code, so the threshold
lives in the compiler, not in `ll-model`. Here it only means A1's generic
runtime `ll_object_die` is a temporary stand-in: A3 replaces it with the
descriptor's `dispose` pointer, and the crate's tests supply hand-written
`dispose` functions that model the generated small-class (unrolled) form.
The GC trace keeps striding `traced_runs` as data regardless.

---

## 2026-07-21 — the store barrier is funded, not checked

**Decided:** two blocks per thread are held back for arena log growth
(`memory::reserve`). `grow_log` draws on them when the pool refuses; the
draw sets a flag that `ll_gc_maybe_collect` — the compiler's safepoint
poll — refills on. A reserve block is linked into the arena's block list
so reset returns it, but it **never becomes the arena's bump block**.

**Why:** `ll_ref_store` has no channel and must not grow one — a check
after every reference store is the Zend shape this runtime rejects. But
it can fail, because recording an escape grows a log. The reserve does
not make failure impossible, it *moves* it: the barrier keeps working
from the reserve, and the next poll, which runs in a frame that can
raise, turns the shortage into an ordinary memory-exhausted exception —
thousands of records before the reserve would run dry.

**Considered and rejected:** intrusive log links in the entities
themselves, which would need no reserve at all. It fails on
release-at-reset (one heap entity gets one record per store, in several
arenas' logs) and would cost 8 bytes in every arena object's header —
paying permanently, per entity, to remove a reserve that costs two
blocks per thread.

**Cost:** the arithmetic depends on a compiler contract that does not
exist yet — a bounded number of barrier operations between two polls.
Until a compiler emits polls, the refill only happens where something
calls `ll_gc_maybe_collect`, so in this crate the reserve is a
mechanism with its trigger stubbed by tests.

---

## 2026-07-21 — the barrier owns the whole slot, and publishes it first

**Decided:** `ref_store` takes `slot: *mut Value` and `new: Value`, and
writes the whole 16-byte value **before** releasing the displaced one.
`ll_ref_store` changes shape with it.

**Why:** two defects that turn out to be one. Releasing first lets a
`__destruct` collect while the slot still points at the value being torn
down (audit C1); publishing first removes that edge. But publishing only
the payload word — which is all the barrier used to write, leaving every
call site to stamp the tag — makes the slot readable while torn, and
"tag says object, pointer is null" is a crash rather than a
miscount. One slot has one writer.

**Considered and rejected:** keeping the split write and having the
collector tolerate a torn slot. That spreads the invariant to every
future reader instead of removing it, and the reader that forgets is a
crash under memory pressure.

**Cost:** an ABI change, taken now because no generated code exists yet.
The `Value` travels by value into an `extern "C"` function, which on
Windows x64 means a pointer to a caller copy; unmeasured, and recorded
here rather than in `BENCHMARKS.md` for that reason.

---

## 2026-07-21 — a destructor is owed by the constructor, not by the factory

**Decided:** creation is two steps. `ll_object_new` is the factory: it
allocates and stamps the header, nothing more. `ll_object_constructed`
runs after the user constructor returns successfully — it sets
`DESTRUCTOR_PENDING` on the header (named `HAS_DESTRUCTOR` at the time;
renamed in the 2026-07-22 flags compaction) and registers the arena log
record.
Teardown dispatches on that header flag, never on the class.

**Why:** a constructor that throws must not get its `__destruct`
(`rfc/runtime/object-lifecycle.md`). Registering in the factory leaves a
record demanding exactly the forbidden call, for exactly the objects
forbidden to have it. Dispatching on the class does the same on the
refcount path.

**Considered and rejected:** a separate "constructed" bit. The header
flag already meant "this object owes a destructor" everywhere it was
read; making it mean that literally costs nothing and removes a bit.

**Cost:** generated code must emit the second call. A class with no
destructor needs no call at all, so the cost lands only where the
guarantee exists.

---

## 2026-07-21 — a refused destructor record fails the creation

**Decided:** `Arena::track_destructor` returns false instead of
aborting; the creation that asked for it raises memory-exhausted. The
other three arena logs keep the abort, moved from `grow_log` to each
caller so the reason can be stated where it applies.

**Why:** a lost escapee or release record dangles or leaks — there is
nothing safe to continue into. A lost destructor record only skips a
side effect, and there is a better answer available: fail the creation,
which lands on the already-specified path for a constructor that threw.
Nothing is silently skipped, because an object whose registration failed
does not survive its own creation.

**Cost:** the aborts are still there, now three instead of one. They are
placeholders for the reserve (`rfc/runtime/exceptions.md`, "The log
reserve protocol"), which is not built.

---

## 2026-07-20 — the arena handle is a raw pointer, not `&mut Arena`

**Decided:** `resolve`, `resolve_arena`, `ref_store`, `escape_gain` and
`arena_reset_full` all take `*mut Arena`. A `&mut` is materialized for
one leaf operation at a time, and the rule is: **no borrow may be live
across a call that can run user code.**

**Why:** destructors are documented to run reentrantly on the reset
path. `arena_reset_full` held a `&mut Arena` across its settling loop
while a `__destruct` it invoked resolved the same arena — two live
`&mut` to one object. That is UB regardless of the machine code being
correct, and the reset path is exactly where the optimizer is most free
to assume otherwise.

**Considered and rejected:** keeping `&mut Arena` and relying on
discipline about when it is held. It had already been violated in the
one place it mattered, and nothing would catch the next violation.

**Cost:** the borrow checker no longer helps on the arena API. Misuse
is now caught by Miri, or not at all. The reset's release drain also
had to become collect-then-release, because it used to run `die` inside
the drain closure while the drain held the arena.

---

## 2026-07-20 — trailing inline data is reached through raw pointers

**Decided:** `Object::prop_at`, `Class::vtbl` and `LLString::bytes`
take a raw pointer spanning the whole allocation instead of `&self`.
Entity pointers keep whole-allocation provenance rather than being
narrowed through `&mut RcHeader` (`ll_release` now buffers its original
argument, not the reborrow).

**Why:** all three types put inline data past a fixed header. A
reference carries provenance over the header only, so every access to
the trailing data was outside the reference it was derived from.

**Considered and rejected:** leaving it, on the grounds that it works.
It is also what stops Miri before it reaches anything else, so leaving
it meant giving up the only tool that can see this class of defect.

**Cost:** these accessors are no longer methods, so call sites are
slightly noisier.

---

## 2026-07-20 — the block header is split by access rule, not by topic

**Decided:** `HeapBlockHeader` is four `repr(C)` structs laid back to
back. Line 0 holds `BlockPrivate` (counters, free list, `available`
links) together with `BlockShared { owner }`; line 1 holds
`BlockRemote { remote_free }` alone; line 2 holds the cold
`BlockLinks`. The owner borrows `&mut (*block).private` and cannot name
the shared half.

**Why:** `&mut HeapBlockHeader` claimed exclusivity over two atomics
that every thread reaches by design, so the owner's borrow raced every
non-owner's legitimate read. The audit filed this as a non-owner
problem; Miri showed the owner is equally implicated, so no amount of
care on the non-owner path could fix it. Making it a type rule was the
only option that cannot be violated again — it had already been
violated twice, in `adopt` and in `free`.

**Considered and rejected:** grouping both atomics into one isolated
"shared" struct. `owner` and `remote_free` have opposite access
profiles — `owner` is read on every `free` and written twice in a
block's life, `remote_free` is CASed by other threads — so isolating
them together evicted a hot field and measured slower. Two further
layouts were measured and rejected; see `BENCHMARKS.md`.

**Cost:** none measured — faster on both benchmarks (larson −3.03%,
rptest −1.54%), and the header still fits the block's reserved 256-byte
line, so slots-per-block is unchanged. The price is conceptual: the
layout is now load-bearing, and the pinning test exists to say so.

---

## 2026-07-20 — cold concurrent structures take a lock, not a CAS loop

**Decided:** the block pool's free chain moved from a lock-free Treiber
stack to a `Mutex`, matching the abandoned-block list. The standing
rule: a structure whose users are all cold does not get a lock-free
implementation here.

**Why:** `pop_global` read a node's `next` non-atomically while another
thread could already own that block and be writing the same bytes
through a different view of the tagged-union header. Making `next`
atomic would not have fixed it — the racing write is the owner's, on
the allocation hot path, and making *that* atomic to serve a cold path
is the wrong trade. The race had to become impossible rather than
atomic.

**Considered and rejected:** giving the pool a dedicated atomic link
field at an offset no owner view touches, keeping the stack lock-free.
It works, but adds another layout invariant to a union five modules
share, and keeps the ABA tag.

**Cost:** none measured (see `BENCHMARKS.md`). A per-thread cache
already fronts the chain and refills in batches, so the lock is taken
rarely. The ABA tag is gone, which also retires the audit's concern
about its width.

---

## 2026-07-20 — Miri runs against a UNIX target

**Decided:** the Miri suite runs with
`--target x86_64-unknown-linux-gnu`, and the bench-only `mimalloc`
dev-dependency is gated behind `cfg(not(miri))`.

**Why:** the Windows TLS fast path is inline `asm!`, which Miri cannot
execute at all. The crate already has a portable `thread_local!` path
for non-Windows targets, so pointing Miri at a UNIX target selects it
with no source change.

**Considered and rejected:** adding a `cfg(miri)` branch to the TLS
module. It would have put a Miri-shaped concern into the hot path for
no gain over choosing the target.

**Cost:** Miri never exercises the Windows TLS path, which is the one
that actually ships here. `-Zmiri-ignore-leaks` is also required, so
Miri is blind to the leak-shaped findings.

## 2026-08-18 — uncounted objects under a compiler-cleared stack-presence bit: refused

Edmond's proposal, ruled by Sage at full depth the same day, with the
literature sweep of `dev/RESEARCH.md` (same date) beside it.

One header bit meaning "a stack slot may hold me" cannot be kept sound
by compiler set and clear alone: clearing requires knowing whether
another live frame still holds the object, and two independent heap
loads of one object in sibling frames with adversarial exit order
defeat every static duty assignment — "some frame holds me" is a
disjunction, and a disjunction is not cancellative, so removal needs
the other operands: a per-object holder count, holder enumeration
(stack maps, a shadow stack, or re-assertion at checkpoints — against
the central bet that roots are derived, never enumerated), or a static
at-most-one proof, which is unique ownership and is already designed.
The counter floor costs exactly today's narrow retain and release (a
header load plus a 4-byte relaxed store, no RMW — `refcount.rs`), so
the stack side can never undercut the current scheme.

The heap side falls to a second fact: in rc-walk the refcount **is**
the write barrier — Phase 3 sees a moved reference only through the
count, and the exact test balances counts — so an uncounted partition
must fund a replacement per-publish trace, capping its saving below
the counted publish while adding a second judgement mode, a hand-back
channel, weak-nulling round-trips and a promotion rule.

The literature agrees from the other side: every published system that
lets a mutator announce stack possession uses per-slot, per-frame or
per-thread state, a scan, or a sticky one-way bit; no clearable
per-object stack bit is published, and the closest industrial hybrid
(free-threaded CPython's deferred-refcount flag) pays with whole-frame
scans at every collection.

Re-open only if the Phase D publish census — the instrument that also
prices the birth count and unique ownership — shows post-construction
publishes into multiply-referenced, transitively pure object classes
dominating the barrier bill.

## 2026-08-18 — proof-horizon granularity: the class bit is policy, always-provable elision is lawful in both regimes, and nothing introduces a write barrier

Ruled by Edmond, closing open question 4 of
`dev/design/proof-horizon.md` after three Critic rounds attacked both
readings.

**Decided:** whether a class's locals are counted or enter the borrow
lattice stays a class property — the emitter's default. On top of it,
a closed set of **always-provable elision rules** applies at any site
in either regime, the way Swift ARC's guaranteed optimizations do:
rules whose soundness follows from the language semantics alone, with
no summary, no heuristic and no cross-unit assumption. A counted
class's local may lose its pair under such a rule; a horizon class
works as designed. Summary-driven or heuristic per-site deviation
stays barred until the certificate-plus-shadow-lowering audit exists
(the document's hybrid section).

**The set's bound** (Critic round 4, same day — the bound follows
from the criterion above rather than weakening it): a rule qualifies
only when it is decidable from IR shape alone — the enclosed region
contains no call, no store, no release and no checkpoint — because a
"horizon-free" proof that consults the may-alias oracle is fallible
by the document's own certificate analysis; the lattice's owned base
cases are preconditions (non-COW-eligible, transitively
destructor-free, non-unique target); every rule preserves the
Zend-observable destructor timing, a constraint Swift's optimizations
are not under, so the Swift precedent covers the mechanism and not
the contract; each admitted rule gets its own entry here — statement,
proof sketch, reviewer, date — and its elisions enter the shadow
lowering's journal.

**The standing constraint:** no elision rule of either kind may
introduce a write barrier or any other mutator work beyond the
program's own code — the GC philosophy applied to the compiler's
output. The constraint's scope is the per-site elision rules: the
family's checkpoint-progress compensation
(`dev/design/owned-slots-and-the-walk.md`, open question 3) and the
unique-move rule keep their own open questions outside it, and the
economics' measurement counter is exempt by name as instrument work.

**Why:** the Critic rounds showed unaudited per-site deviation is
indistinguishable from a miscompile at runtime; Edmond's cut is that
the danger lives in *fallible* proofs, so the lawful per-site class
is exactly the infallible ones.

## 2026-08-18 — child-release order is language surface, and the hand-off's external-child delay stands

Ruled by Edmond ("да, это сохраняется"), closing the two questions
`dev/design/pure-destructors.md` left with him.

**Decided:** the order in which a teardown releases an object's
children is **specified** — today's order is part of the language
surface and no tier of the purity ladder may reorder it. By the
ladder's own table that settles P2's shape: a P2 destructor keeps its
call and sheds only the resurrection machinery; erasing P2 to P0
would hand the release order to the raw sever, which the ruling
forbids. The ladder stays three-tier plus NR.

**And:** the hand-off drain's external-child delay — the release
batch of a drained component's external children running at a later
checkpoint than the prologue — is accepted as the design's cost, not
a defect to engineer away.

**Why:** PHP code observes teardown through `__destruct` bodies, and
an order that changes under an optimization tier is the timing class
this family has consistently refused to trade (the drop-point pin,
the destructor-bearing exclusion in `proof-horizon.md`).

## 2026-08-26 — `walk.rs` is split, not deleted: its upper half is the crate's only entity tracer

Sage ruling, after a Critic round over `PLAN.md` S30–S40 that Edmond asked
for.

**Decided:** the unconditional half of `src/walk.rs` — `Cell`, `CellShape`,
`OutsideCells`, `OutsideCarry`, `CellReader`, `PlainCells`,
`counted_box_cell`, `trace_entity`, `trace_cells`, `empty_cell`,
`sever_cells` — moves whole to a new crate-visible `src/cells.rs` in the
deletion commit itself. The collector below the file's build-step-2 marker
dies. Inside the moved half the epoch's re-check apparatus dies with the
feature: `RelaxedCells`, `WalkOutsideFn`, `OutsideRead`, the `walk_relaxed`
and `recheck` members, `Cell.raw` and the storage-version answers. `Census`
and `heap_census` move as `#[cfg(test)]`. `rc-cycle` builds no enumerator of
its own: S35's mark traces through `cells::trace_cells::<PlainCells>` and
S32.0's block-kind dispatch runs above that call, in the collector's
per-child visit, so `cells.rs` keeps no knowledge of shadow rows.

**Why:** the file is two modules. `class.rs` stores `OutsideCells` in the
class descriptor, `promote.rs`'s arena reset calls `trace_entity` four times,
`object.rs`'s dispose path is generic over `CellReader`, and `template.rs`,
`array/entity.rs`, `array/entry.rs` and `refcount.rs` compile against the
rest. Deleting the file breaks five modules for reasons unconnected to either
collector, which makes S30's own criterion — the crate builds green with no
cycle collector — unreachable by its own step. Renaming in place was refused
because the module doc and half the comments present the tracer as "rc-walk
build step 1" and cite a deleted document: the text is rewritten either way,
and the move is the rewrite's vehicle. The name is `cells` and not `trace`
because `rc-cycle.md` calls the S35 mark "the trace", and a substrate module
of that name would read as a collector again.

## 2026-08-26 — `gc.rs` survives the deletion of `rc-trace`: it is the ABI and the safepoint

Sage ruling, same round.

**Decided:** S30.3 deletes the `rc-trace` strategy *from* `src/gc.rs` — the
candidate buffer, the colours, trial deletion, the thresholds and
`COLLECT_PENDING` — and keeps the module. `ll_gc_collect_cycles`,
`ll_gc_maybe_collect`, `ll_gc_checkpoint` and `ll_gc_checkpoint_ack` keep
their names, the barrier's log reserve is still refilled inside
`ll_gc_maybe_collect`, and the interim bodies collect nothing and say so.
The trigger of this plan is the in-line collection at a failed allocation
plus one runtime arm — a failed enrolment, fired at the poll; thresholds are
the compiler's policy and stay in the backlog with the collector-thread
accelerator.

**Why:** three of the four things the file carries are not the strategy. The
checkpoint pair is the configuration-independent lowering surface, and
`object.rs:238,246` and `benches/lifecycle.rs:23` already depend on it.
`gc.rs:714` is the only steady-state refill of the log reserve —
`heap.rs:1734` fills once at thread init — so deleting the file reverts
`rfc/runtime/exceptions.md`'s "the next safepoint raises memory-exhausted"
to "the barrier eventually fails", with no test that can see the change.
And S30's verification links nothing and builds no bench target, so four
undefined C symbols would surface at integration rather than at the stage.

## 2026-08-26 — `rc-cycle`'s teardown is built, and its order is written down before the old collector goes

Sage ruling, same round.

**Decided:** S36 gains three steps for the teardown — the guard and the weak
window, destructors and the resurrection re-verify, sever and free and the
deferred drops — and all eight operations of `drain_confirmed` survive in
today's order. The resurrection re-verify survives. The order is written
into `rfc/model/gc/rc-cycle.md` as a "Cycle teardown" section by a new step
S30.6, executed **before** S30.2, so the section is transcribed against
running code; `rfc/model/weak-references.md` repoints its binding obligation
there. A block's `used` falls at the slot's return and never at the parking,
and S34.3's criterion names it.

**Why:** S36's goal claimed the frees while its two steps built the exact
test and the epoch parking and nothing else; with every box ticked the crate
would enrol, trace, judge, and return nothing to the allocator. The weak-cell
ordering is the part that cannot be re-derived later: null per member as each
is torn down, and the second member's destructor calls `get()` on the first,
receives a strong reference, and the slot is freed under it — the window PEP
442 exists to close. The re-verify survives the shortlist framing rather than
contradicting it: garbage is monotone only while no reference to the
component exists outside it, and the destructor runs holding `$this`, a
reference the teardown itself handed to user code.

## 2026-08-26 — maturation is Y9's edge-side prune, and the trace writes nothing

Sage ruling, same round.

**Decided:** S37 builds the prune and only it. A member whose stamp epoch is
current and whose age has reached `k` is read as an opaque live external and
is not descended into; the same test at depth zero skips a mature popped
root, which is all the root-side delay ever meant. The stamp has one writer,
the owning thread's commit, after judgement (new step S36.6); the trace loads
it and never writes it, so S35.1's "writes nothing into any entity" stands
verbatim and "the two-bit epoch's wrap is retired on contact" is withdrawn.
Ageing is component-wise in value and per-entity in residence:
`min(age) + 1` saturated at 3, stored identically in every member. A stale
stamp reads as age 0 and is left in place. **Acquittal never clears the
enrolment bit** (new step S37.4): a proven-live root parks in the suspects
buffer with its bit set and is re-offered at epoch turnover. `k = 3` and a
turnover every 64 completed collections are provisional, after YRC's only
known values, with the measurement owed at S40.1.

**Why:** the root-side reading filters which roots start a trace and does
nothing to the closure, and the closure is the problem — the subgraph
reachable from a median candidate root on a booted Laravel corpus is 381 of
381 objects. Commit is the only writer that the law of S34.2 allows, since a
mature stamp suppresses future suspicion, and it is what keeps S33.1's abort
free and S35.1's byte-identical hash true. The withdrawn clause was both
contradictory and useless: eager clearing fires only when a trace touches the
entity, and the stamp that wraps is exactly the one no trace touched for four
epochs. The epoch-scoped re-offer is the real backstop, and it turns every
stamp pathology — ring-mates matured apart, a wrapped stamp, a wrong dirty
proposal — from a permanent miss into bounded floating garbage.

## 2026-08-26 — Y14's non-wait clause retires with the handshake that was its reason

Sage ruling, same round, and a change to the specification rather than to the
plan alone.

**Decided:** a mutator whose GC-heap allocation is refused waits on a held
claim by any holder but itself, then takes the claim and collects. The clause
"a thread that finds the token taken does not wait" is retired. The claim
carries a thread-local held flag, so a destructor allocating inside its own
thread's collection collects nothing instead of waiting on itself.

**Why:** the clause was argued from the handshake's deadlock, and the
amendment of 2026-08-26 deleted the handshake. A holder's in-line collection
is synchronous and needs nothing from the waiter, so the waiter's only cost
is latency. The one hazard the deletion leaves is self-re-entry, and the held
flag is what distinguishes it from contention.

## 2026-08-26 — the boundary corrections the same round forced on S30

Bookkeeping, recorded because each was a claim in the plan that the tree
contradicted.

**Decided and fixed in `PLAN.md`:** the 13 files carrying
`not(feature = "rc-walk")` are a **subset** of the 24 gated on it, not a
second group, and both counts are of files that mostly survive — the guards
go, not the files. `Cargo.toml`'s `default = ["rc-walk"]` goes with the
feature. S30's `rg` gate widens from `src/` and `dev/` to `docs/`,
`benches/`, `bench-external/`, `Cargo.toml` and `PLAN.md`: `model/docs/` held
37 references in four files, and `memory/mod.rs` declares
`docs/memory-manager.md` normative for that module. `cargo bench --no-run`
joins the gate, because `cargo test --lib` builds no bench target. S30.4's
`--list` diff is taken in **both** GC configurations before against the
single configuration after — 526 tests under `rc-walk`, 493 under
`rc-trace`, union 553 on 2026-08-26 — and gains a third heading for a test
whose contract outlives its instrument, which a rewrite hides from the diff
because the name does not change. The 85 backticked citations of deleted
documents across 31 files of `model/src` are checked by grep, since
`linkcheck.php` reads only `rfc` and only bracketed links and reports zero
while all 85 are broken. `rfc/model/gc/strategies.md` leaves the deletion
list: Edmond's ruling named `rc-walk`, `rc-trace` and the horizon, and Y14
cites that file's arm/fire rule as a correctness requirement.

**Why:** every one of these was a sentence that read as checked and was not.
The counts were right and the sentence built on them was not; the gate grepped
two directories of six; the link checker cannot see the citation form the
Rust source actually uses.

## 2026-08-26 — where the two deleted collectors live: `archive/pre-rc-cycle`

**Decided:** the pre-deletion state of both repositories is the branch
`archive/pre-rc-cycle`, on `origin` as well as locally, at `model` `3d5d853`
and `rfc` `af10eae` — the commits that carry the amended plan and precede the
first deletion. Nothing is copied back from it without an entry here.

**Why:** the deletion ruling of 2026-08-26 keeps the old state reachable as a
branch rather than as files, and a branch that exists on one disk is not an
archive. The branch point sits after the plan amendment rather than before it,
so a reader who finds the branch also finds the plan that explains why the
code on it was deleted.

## 2026-08-26 — the test ledger of the deletion, in numbers

**Measured, not estimated.** Before the deletion the crate listed **526**
tests in the `rc-walk` configuration and **493** in `rc-trace`, **553** in
their union — 60 belonged to `rc-walk` alone and 27 to `rc-trace` alone, which
is why the before/after diff had to be taken in both configurations. After it
the single configuration lists **467**: 464 run, 3 are Miri-ignored. **87**
tests are gone and **1** is new.

**Why the diff cannot be the whole ledger.** A test whose contract outlives
the collector but whose *instrument* dies keeps its name when it is rewritten,
so the `--list` diff shows nothing for it, and a test deleted with the promise
that its contract re-lands at a later stage looks exactly like a test swept to
go green. Both classes are named in the commit body of `de18686` under their
own headings, with the stage each contract returns at.

## 2026-08-26 — one arm survives every `rc-walk` gate: the atomic one

**Decided:** where the deleted feature had two arms, the `rc-walk` arm stays
and the `not(rc-walk)` arm goes. The surviving arm reaches the header through
narrow relaxed atomics — `flags_load`, `refcount_load`, `refcount_store` — and
the other reached it through `&mut RcHeader` and plain field access.

**Why:** `rc-cycle` writes a maturation stamp into byte 2 of the flags word
from commit and reads it from the trace, and its collector-thread accelerator
reads the whole header from a thread that did not write it. A plain field
access cannot express either without undefined behaviour, and S31.4 is built
on the narrow-store discipline the surviving arm already has. The choice is
not free: the `not(rc-walk)` arm carried the candidate enrolment on a non-zero
decrement, which is exactly what `rc-cycle` needs, so `ll_release` now enrols
nothing at all until S34 builds the root queue. A garbage ring is retained
until then, and the crate has no cycle collection between S30 and S36.
