# Decisions

An architecture changelog: what was decided and why, not what changed
in the code. Routine fixes and renames belong to git, not here.

A superseded decision is replaced by a **new entry**; old entries are
never edited or deleted.

---

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
