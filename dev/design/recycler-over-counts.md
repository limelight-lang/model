# Recycler over counts: the collector judges and frees a garbage set alone

The design ruled on 2026-10-04 (`dev/DECISIONS.md`, "the collector judges and
frees a garbage set by window tags, built as an arm beside the default
build"), after two Critic rounds and the Sage (Fable). It is built as a
feature beside the default build (`dev/design/the-proof-epoch-collector.md`),
which stays as the arm it is measured against. The idea is Bacon–Rajan's
Recycler (PLDI'01, ECOOP'01): a garbage set cannot be touched, so a touch
during the judgement proves it live. Here the counts stay immediate and
owner-written; a tag stands in for the Recycler's mutation log.

## 1. What does not change

Immediate, owner-written, non-atomic reference counting; destructors run as
today for everything not collected as a cycle; copy-on-write reads exact
counts. Candidates and the ring R, batches of K roots, the token and its
recall, the withheld returns, the collector's stamps, the epoch and the
deferred lanes are all as in the default build.

## 2. The window and the tags (the mutator)

1. A collector requests the token and the mutator consents. From the consent
   the mutator's window is open, numbered 1..255; the number advances at every
   consent, and only once the consent's swap succeeds. It stays open after the
   grant ends: the collector releases the token on its own thread and cannot
   close the mutator's window, and a tag written after the grant names a
   number no later grant carries until it comes round. A close at a recall
   would be unsound — a recall at the wind-down level does not stop a
   completed mark's scan, whose set is still Δ-tested, and zeros stored
   before T would erase the tags that test reads. 0 is the number before the
   first consent.
2. While the window is open the mutator works as today and also writes the
   window number into header byte 7:
   - of the entity whose count it writes — every count write, through the one
     primitive `refcount_store` (retain, release, `set_header_refcount`,
     `severed_edge_release`, the weak upgrade, promotion's `count_children`,
     the destructor guard, `ll_owned_child_die`);
   - of the holder whose slot it writes a pointer into — object slots,
     `Table::insert`/`remove`/key stores, `ref_store`, and the in-array
     permutations (`begin_move`/`set_storage`: the tag precedes
     `begin_move`'s fence, since arrays are strided outside the version
     bracket).
   One relaxed one-byte store, no branch. Holders
   in an arena need no tag (`owner_cat` is a compile-time constant).
3. Byte 7 has one other writer, the collector's one-byte CAS at the Δ-test
   on members of W only (§4.8). Nothing else writes it: the reset's COW
   reconciliation, which held
   bit 24 there, marks the survivors it has in hand by a bias on the count
   word instead (`dev/DECISIONS.md`, "the COW reconciliation marks its
   survivors by a bias on the count word…"), and a heap block a reset empties
   stays with its size class until the reset ends (`Heap::retire_empty`).

## 3. The collector's batch

4. The batch traces over shadow rows as today, and records every edge it
   subtracts: one 8-byte entry an edge (the target's row) after one a run
   (the expanded entity's row, heading the edges its expansion subtracted),
   in segments of the trace's arena (`src/cycle/recorded_edges.rs`).
5. The scan spreads the live colour over the recorded edges only, never over
   the current heap: writes made after the mark to entities outside the set
   (a live array growing, a move out of a live holder) cannot whiten a live
   entity. White entities form the set W. Two passes: the record in order,
   each row coloured by its count at its first naming and a run's header row
   taking its run's index in place of the count; then the live rows' runs,
   raising their targets. It opens on a reading of the recall that stands for
   a stride, since the record is shorter than the storage a heap scan reads.
6. A batch stopped by a recall before its scan completes posts as today and
   its set goes the default build's exact way — and so does a batch whose
   mark a wind-down cut: its scan reads the heap, as today, and its set is
   never Δ-tested. Only the set of a completed mark's completed scan over
   the record is.

   *What the record costs in held garbage.* An entity a live holder dropped
   during the trace reads live through the holder's run, where the heap scan
   saw it at zero and proposed it: its root posts read live and waits for the
   epoch's turn, and in a final-drain array it can be stamped mature and
   pruned at for the rest of the epoch. S68.6 builds no answer to it (the
   Sage, 2026-10-04, on S68.6c): a taint over the raises costs the stamps'
   pruning on a hot holder's state or a header read a run, and S68.8
   measures the held garbage against the default build first, with a load
   whose hot holder drops a cyclic subtree during the trace (§5a, S68.6c,
   item 5).

## 4. The judgement

7. The cut-off T is the mutator's next safepoint checkpoint (not a slot-free
   reading, inside which a frame may hold ARC-elided temporaries): a
   handshake — a release by the mutator that the collector acquires (and a
   release on T's request, an acquire on the poll's read) — after which the
   collector sees every tag stored before T. Built as a byte beside the
   token (`TraceToken::ask_for_the_checkpoint`), answered by
   `ll_gc_maybe_collect` under an open gate alone — inside a teardown, a
   reset or a collection the runtime holds references it has not counted —
   and waited for under the grant up to a bound (`delta_test::CHECKPOINT_WAIT`,
   2 ms, a placeholder S68.8 reads) or the mutator's recall at the stop level
   — a wind-down does not end the wait, as it does not end the scan — the
   wait releasing the grants held behind this one at each reading, as the
   trace's stride does; a missed checkpoint sends the set the exact way.
   *Open:* a mutator that blocks with no poll to come — between requests, in
   native I/O — misses every checkpoint; a "parked" byte it stores with a
   release before blocking, at a point where every reference is counted,
   could stand for T (the Critic of S68.5, finding 7). S68.8's count of
   missed checkpoints decides whether it is built.
8. The collector reads byte 7 of every member of W. Any member carrying the
   window's number, or one whose address cannot be recovered to read it:
   the set is not judged. A member with weak references keeps the set
   unproved as well — an upgrade after T makes it live with no tag — and
   S68.6 takes it and what it reaches the exact way. A set marked proved
   lists every member the test read: one the pool closed short, or whose
   walk left a row out, is a part of the garbage that may not be freed
   while the rest names it (the Critic of S68.5, findings 1–2). None: W is garbage at T and stays
   garbage — judged by the collector alone. While it reads, it clears every
   tag that is neither 0 nor the window's by a one-byte
   `compare_exchange(stale, 0)`: a value other than the window's predates the
   consent and says nothing of [consent, T], and the CAS — never a plain
   store — cannot turn a fresh tag into 0. Garbage is never touched, so
   without the clear a set whose members carry k stale numbers would be
   refused in k/255 of its attempts, and one grown across 255 consents for
   good. A refused set goes back to the queue once and the exact way on a
   second refusal: the first refusal may be a stale tag equal to the window,
   which the next attempt, in the next window, clears. The clear writes
   garbage lines only; a clear during the trace would dirty the header line of
   every live entity traced, the line every retain and release hits (the
   Sage, 2026-10-04).

   *Why (the Sage's proof).* An entity untagged at T had no count write and
   no slot write in [consent, T], so its count is the value read and every
   recorded edge out of it stands at T. A recorded in-edge of a white entity
   comes from W, since a live source colours its target live over that edge.
   So at T every reference into W is a recorded edge inside W; at a
   checkpoint every reference is counted (locals included), so nothing
   outside W refers into it. Garbage cannot be resurrected after T (weak
   cells aside, which §5 routes to the owner).

   The holder's tag is what makes "every recorded edge out of it stands at T"
   true under count-free moves — an ARC-cancelled pair, or the runtime's own
   `Table::remove` or a pop — which move a reference into or out of a slot
   with no count write on the target: the slot's holder is tagged, so a W
   whose rows changed is refused, and W's slots are frozen between the trace
   and T. The fallback, if the gate fails on the holder's tag, is to tag the
   moved entity instead, with three coupled obligations across the compiler
   and the runtime and drops read from live slots at the free (the Sage,
   2026-10-04).

## 5. Splitting and freeing

9. S = every member of W reachable from a member with a destructor or weak
   references; C = W − S. S is closed under successors, so no destructor in
   S can resurrect C.
10. The collector frees C itself, member by member, in this order: build the
    member's drops (what its `dispose` would release outside C, edges into S
    included), write count 0 and `DEAD_IN_PLACE`, hand the slot to its
    block's chain. A stop may fall between members, never inside one.
    (S68.6b's commit is one act instead, a stop falling before or after it:
    a stop between members would leave the rest naming freed slots — the
    Sage, 2026-10-04, on S68.9.)
    Freeing follows `ll_free`'s routing (retained blocks, entities in another
    thread's block, large runs) but not `free_remote`, whose withholding would
    recall the collector itself; registered members stay dead in place for
    the owner's retirement pass, as today.
11. The collector releases the token (a set dropped unread — pressure, exit,
    a block return — releases it too); the window stays open until the next
    consent opens the next one (§2.1).

### 5a. How S68.6 is built (the plan, before the code)

Measured first (`dev/BENCHMARKS.md`, "S68.6a: the owner's pause over a 400k
garbage ring is not its trace"): of the owner's 50 ms over a 400k ring, its
trace was 3–4 ms; the meeting of the members, the guards and weak
notifications, the teardown and the close are the rest. So the steps:

- **S68.6a (built).** A set proved by its tags reaches the owner marked, with
  the count of edges the collector recorded between its members; the owner
  meets the members with no trace of their cells and the commit confirms the
  set by the counts' sum against that count, walking it where they differ.
  The owner still tears the set down.
- **S68.6b, the collector frees a set it can free whole** (the plan as the
  Critic of 2026-10-04 on it left it). The first form takes W whole or not
  at all, and only where every member is one the collector can free off the
  owner's thread:
  - an `Object` of a class whose dispose releases exactly its counted cells
    (today: the default dispose) and that has no destructor and no outside
    cells; a `Reference`; a `String` with its bytes inline; an `Array` whose
    storage, if any, is a body in a block of kind `BLOCK_KIND_BUFFER`;
  - no weak references (the Δ-test's `WeaklyHeld`), not an arena escapee,
    in a slot of an entity-heap block the granting mutator's heap owns — not
    a retained block, not a large entity, not another thread's block.
  `Lazy` waits for a factory to test it with. Otherwise W goes the S68.6a way.
  The split W = C ∪ S of items 9–10 is the form after this one.

  **Two phases, so that no part of W is ever freed alone.** A part freed and
  the rest left is unsafe both ways: the rest still holds counts from the
  freed part and leaks, or, given those as drops, releases cells naming slots
  already handed out again.
  1. *Preparation, read-only, stoppable anywhere.* Check every member's
     eligibility; build every drop — one record a counted cell naming an
     entity outside W, in the order the member's own dispose would release
     them; build the per-block chains — head, tail and length a block, the
     links not yet written; draw every metadata block these need, charged to
     the ledger and handed over with the release as the posted set's are
     (`gc_metadata::hand_over`). A stop, a pool refusal or an ineligible
     member here sends W the S68.6a way, nothing written. A debug build
     validates W exactly here, read-only on the collector's thread under the
     grant (trial deletion over its cells), and goes on only where it agrees.
  2. *The commit, one act the recall waits for.* For each member: take the
     slot (`take_slot_for_free`: count zero, `DEAD_IN_PLACE`); a registered
     member (`CANDIDATE_BIT`) stays so for the owner's retirement pass; every
     other slot's free-list word at byte 8 written to the next of its block's
     chain; an array's body posted to its chunk's remote stack
     (`BufferArena::post_remote`). A cap on W's size bounds the act — a W
     above it goes the S68.6a way — first set at 64k members and read by
     S68.8.

  **What the owner receives, on a record word of its own.** P's verdicts name
  W's roots as completed deaths; the drops and the chains stand on a word of
  the record beside the posted set, never in place of it, and **every path
  that gives P back unread applies them first**: the take under pressure, the
  exit (before `abandon_all`), the teardown's refusal, `dispose_of_p`, a held
  token's drop, a block return under `POSTED` (`posted_set::drop_before_a_return`).
  Dropped, the drops would leave each child outside W a count no one holds,
  and the chains would leave `used` counting dead slots past the thread's
  exit. The application, at the owner's poll or on those paths:
  - each drop through `drop_ref(GcHeap, child)` itself — the dead holder's
    category, the child's read by `drop_ref` at application, which is what
    covers a child a reset promoted since — never a copy of its body;
  - each chain spliced into its block's free list with `used` lowered by the
    chain's length, the block's owner checked again first (an exit and an
    adoption can move it), and then `collect_remote`'s tail: an emptied block
    retired, an unlinked one linked;
  - the registered members counted as candidate deaths.
  Its pause is then the drops' cascade and one splice a block. The entity
  deaths the journal records for an owner's free are written at the
  application, one a chain, not a member.
- **S68.6c, the split, the second chance and the taint** (the plan, before
  the code; revised after the Critic of 2026-10-04 on it).
  1. *The split.* A seed is a member the first form cannot free: ineligible
     by S68.6b's list, weakly held, holding a child with an ownership mark,
     or standing in a block that is not a slotted block of the granting
     mutator's heap. S is the seeds' closure over the recorded runs inside
     W, and C = W − S; S is closed under successors, so no destructor and no
     weak upgrade in S reaches C. The closure is complete: a completed mark
     expands every met entity that is not a leaf once, and an edge out of a
     member never expanded was never subtracted, so its target is not in W.
     It is a walk on the collector's thread before the preparation, over the
     run indices the scan's first pass left in the header rows, marking S by
     the top bit of a potentially unreachable row's payload (the record's
     bound, `recorded_edges::MAX_ENTRIES`, halves to leave it). The
     preparation and the commit then read C alone; a member of C's drops are
     the counted cells naming anything outside C, S included; the cap counts
     C. An empty C is the S68.6a path as it stands. **A preparation that
     fails after the split** — past the cap, a pool refusal, a recall, frees
     standing — drops it: the S bits are ignored and W goes the S68.6a way
     whole, with W's edge count.
  2. *What the owner receives of S.* S is listed in the posted set and C is
     not (the append reads S's rows alone when a split is carried); the
     roots in C read their completed deaths, the roots in S `Proposed`.
     **No set with a weakly-held member is ever marked proved** — not S, not
     a W whose C is empty, not a W a failed preparation sent back whole: an
     upgrade after T paired with a cut keeps the sum and frees an entity a
     global holds (the Critic, round 2, finding 1). S, with no weakly-held
     member, is marked proved with the edges recorded between two members of
     S plus C's edges into S, and **C's drops into S are held back** on the
     record beside the rest, apart: the poll applies C's other drops, reads
     P, and the commit confirms S by the sum before any drop into S has run,
     so no member of S can have died or had its slot reused at the reading
     (the Critic, round 1, finding 1: applied first, a drop can free a member
     of S and a destructor reuse its slot for an entity held from outside,
     which the sum alone would confirm). S confirmed and freed, its held
     drops are discarded with it; S walked instead, or P given back unread
     on any path, the held drops are applied at the next point where
     destructors may run — the poll, the pressure path, the exit — and
     lower S's counts to a release that registers it again. The teardown's
     refusal splices and applies nothing that runs user code. Every reader
     of P applies C's other drops first, the explicit fire
     (`ll_gc_collect_cycles`) under an open gate among them; one that did
     not would read S live and stamp it. An S no root of the batch lands in
     is not posted: its held drops are applied at the poll, and it waits to
     be registered again.
  3. *The Δ-test.* `WeaklyHeld` no longer refuses the set; a weakly-held
     member is a seed. `Touched` and `NoCheckpoint` still refuse it whole.
  4. *The second chance.* A set refused as `Touched` posts its roots whose
     rows read potentially unreachable `Unwalked`, so the disposition writes
     them back into R for the next batch; roots read live stay `ReadLive`.
     The exception is a refused set one of whose potentially unreachable
     roots' entries carries the second-chance bit — bit 2 of an R entry, set
     by the disposition on each `Unwalked` entry it writes back and on no
     other write: that set goes the exact way whole, as today. *Amended by
     the Sage, 2026-10-05 (§5d):* at a second refusal U leaves W as at the
     first and its roots carrying the bit read `ReadLive`, S staying proved;
     only a set one of whose members' addresses could not be read keeps U
     in W and goes unmarked (`kind::UNREADABLE`).
     `ENTRY_MARK_BITS` becomes bits 0 and 2 and the queue's ledger says so,
     so that no owner's walk hands out an address carrying it, nor a P entry
     built from one reads it as `VERDICT_DEFER_MARK`. A path that strips the
     bit — `pass_over_r`'s keep, `defer_entry`, the overflow buffer — gives
     a root one more chance; all but the overflow follow an owner's trace.
     The refusal is of the batch's whole W, so a ring untouched since the
     consent goes back with a touched one, and at its next refusal — by any
     touched member of the next batch's W — the exact way: under steady
     touch traffic the second chance saves only batches with no touched
     member (the Critic, round 2, finding 4).
  5. *The taint: none in S68.6, measured first* (the plan's proposal, for
     the Sage). Transitive — a run tainted by a tagged header or a tainted
     raise, its targets tainted, tainted rows never stamped, a tainted root
     posted `Unwalked` once — leaves a hot holder's whole state unstamped
     and walked again every batch, the pruning the stamps buy lost (the
     Critic, round 1, finding 3). One hop — only the targets a tagged header
     raises — holds less than it seems: a dropped entity carries the window
     itself, from its own decrement, so its children are tainted too and a
     subtree deeper than two levels is still stamped and held for the
     epoch; and it costs 8 bytes a run in every record (the record holds
     rows, and a row of a slotted block maps back to no entity), a random
     header read a live run in a scan built to read the record alone, and
     the second-chance bit spent on live children of hot holders (round 2).
     The held garbage the taint targets has not been measured: S68.8 reads
     it against the default build, the gate's "held garbage no worse".
     Should it be material, the first variant measured is the near-free
     one — the stamps' walk, which loads every header it stamps, passes
     over a row whose own byte 7 carries the window — and one hop is judged
     against that.
  6. *Counted.* Members put in S by reason (ineligible, weakly held,
     ownership mark, foreign block, reached), sets split, sets whose split
     a failed preparation dropped, S's held drops discarded and applied,
     sets re-queued and sets taken the exact way on a second refusal; in
     `frees_counts()` (`NOT_FREED` resized), the
     Δ-test's counts and the rig's columns; a journal kind for a split and
     one for a re-queue.
  7. *Tests.* A ring with one destructor-bearing member and a garbage tail
     hanging off a clean ring: the collector frees C, the owner frees S;
     the Critic's round-1 finding 1 as a case — a destructor in S that
     would free a member of S and reuse its slot, were the drop into S
     applied before the reading — run under the release profile too, where
     no debug assertion stands between it and a free; a weakly-held member
     splits the set rather than refusing it, and S then goes unmarked; a
     preparation failing past the cap after a split sends W the S68.6a way
     whole, unmarked where a member is weakly held; an S that C holds is
     confirmed by the sum with C's edges and freed, its held drops
     discarded; a walked S gets its held drops at the next poll; the
     explicit fire applies the frees before it reads P; a touched set is re-queued once,
     its live roots read live, and taken the exact way at its second
     refusal.

  8. *The Sage's ruling* (2026-10-04), which overrides items 1–5 where they
     differ.
     - Item 2's held drops are built, as ruled sound: at T nothing outside
       W refers into S, S has no edge into C and no weakly-held member, and
       C's other drops run destructors that reach only what they reference.
       Four conditions. (a) The held drops travel with P — on the posted
       set's first block — and not on the frees word: a destructor run by
       an application may call `ll_gc_collect_cycles`, which reads P while
       the outer application holds the frees in a local, so drops held
       there would be applied after the inner commit freed S. (b) A path
       that gives P back unread and may not run user code — a block return
       under `POSTED`, the teardown's refusal — moves the held drops to the
       frees word for the next open poll. (c) A sum that does not match on
       an S with held drops is no walk: the walk would read S held from
       outside by C's counts and stamp it for the epoch; the held drops are
       applied instead, and counting takes S. (d) The expected sum is one
       pass over the record counting the entries whose target row carries
       the S bit.
     - The second chance refuses U, the closure over the recorded edges of
       the members the Δ-test read touched, and not W: a touched member is
       made live only by a count write, which tags it, or a count-free move
       out of a tagged holder, so only its recorded successors are suspect,
       and its predecessors' edges into U are drops like any edge out of
       C. U is closed under successors, so S has no edge into it. U's rows
       are recoloured `Unclassified` — listed by no append, stamped by no
       walk, read by `is_a_member` as outside W — and `verdict_for` answers
       `Unwalked` for such a root, so the second-chance bit applies to U's
       roots alone; a root of U carrying it makes U a seed of S, which then
       goes unmarked. `NoCheckpoint` still refuses W whole.
     - No set with a weakly-held member is marked proved, confirmed; and
       where U is not empty, neither is W sent back whole by a failed
       preparation. That W, U taken out, goes the exact way unmarked.
     - `prepare`'s refusal while frees stand covers a P with held drops
       standing too; a thread holds one P, which a debug build asserts.
     - The taint: none in S68.6, confirmed (§3).
     - Tests besides item 7's: an explicit fire inside a destructor that one
       of C's other drops runs, while S stands posted, under the release
       profile; a sum that does not match applying the held drops with no
       stamp; a touched member whose closure is refused while the rest of
       W is freed and confirmed; a block return moving the held drops to the
       frees word.

  9. *Built* (S68.6c, `crate::cycle::split`), and where the code departs
     from item 8.
     - The held drops are not simply discarded at the confirmation: the
       commit's validations — the debug build's exact one beside the sum,
       and the guarded revalidation after S's destructors — read S's counts,
       which carry C's edges while the drops stand held. So the commit
       allows for them: the validation's sum is the internal edges, the
       guards and the held count (`validation::validate_component_holding`);
       no member reads zero while they stand, each naming a member. Freed
       whole, S gives them back with it; any other ending leaves them to the
       posted set's drop, which stands them on the record as drops.
     - Held drops ride only on a set marked proved; an S that goes unmarked,
       or that no root of the batch lands in, gives them to the owner as
       drops like the rest, applied before P is read.
     - A child of C with an ownership mark refuses the preparation, the
       split dropped, rather than seeding S: the seeds are read off headers
       alone, and the mark is a child's.
     - Whether S holds a weakly-held member is read off the Δ-test's reading
       of all of W, so a weakly-held member of U unmarks S too.
     - A collection over R whole — the explicit fire's among them — gives P
       back unread and then applies what stands on the record, the held
       drops with it, before its trace, as the pressure path does (the
       Critic of the S68.6c code, finding 2).
     - A child of S with an ownership mark refuses the preparation too: the
       dispose a held drop stands in for destroys an owned child, which a
       release does not (finding 4).
     - Where U is kept in W, the debug build's exact check is not run: it
       asserts every count of W internal, which a live member of U breaks
       (finding 1).
     - *Open* (finding 3): the disposition sets the second-chance bit on
       every unwalked root it writes back, and a root left unwalked by a
       recall or a refused allocation is one; such a root, met by a touch
       at its next batch, goes the exact way at its first refusal. P's
       entries have no bit free to tell the two apart (two for the verdict,
       one for the deferral's mark), so the cost is counted rather than
       removed: the runs read `split_second_refusals` against
       `split_requeued`.

### 5b. What S68.8's first reading asks: S68.9 (the plan, before the code)

S68.8's first reading (`dev/BENCHMARKS.md`, "S68.8, first reading") failed
the gate on the owner's pause in both arms and on held garbage in this one.
The pause is the exact way of the sets whose Δ-test found no checkpoint —
a third of them on `web-heap`, whose mutators sleep between requests with no
poll, as a worker blocked in `accept` does — and of the sets past the cap.

1. *The parked state* (revised after the Critic of the plan). The
   checkpoint byte (`TraceToken`, beside the token) takes two more values,
   `PARKED` and `PARKED_ASKED`, and the runtime two exports, `ll_gc_park()`
   and `ll_gc_unpark()`, which bracket a stretch in which the thread blocks
   with no poll to come.
   - *The contract is the poll's.* A park stands where a poll could: every
     reference counted, the gate open. The gate is enforced in code — a
     park under a closed gate (a teardown, a reset, a collection) stores
     nothing — and the counting is the compiler's, as the poll's is: it
     emits the pair around a call it treats as a safepoint, holding no
     ARC-elided temporary across it (§7, item 16 extended).
   - *Every transition is a read-modify-write of the one byte*, so its
     modification order orders them all:
     - park: `swap(PARKED, AcqRel)`; an `ASKED` it finds is answered by the
       park, which stands as `PARKED` and is read as reached;
     - the ask: `swap(ASKED, AcqRel)`, its previous value read: `PARKED` is a
       checkpoint reached at once, T being the park, and the ask writes
       `PARKED_ASKED` instead (a second `compare_exchange`), so that the
       byte keeps the park; a previous `REACHED` is stale — an answer to an
       earlier grant's ask — and is not a checkpoint;
     - the poll's answer: `compare_exchange(ASKED, REACHED, Release,
       Acquire)`, on the branch where its load read `ASKED` alone, so that
       an unasked poll pays nothing more and a withdrawn ask is never
       answered late;
     - the wait reads `REACHED`, `PARKED` or `PARKED_ASKED` as reached;
     - the withdrawal: `ASKED` or `REACHED` to `NONE`, `PARKED_ASKED` to
       `PARKED`, by `compare_exchange`, never a store, so that it cannot
       bury a park;
     - unpark: `swap(NONE, AcqRel)`.
   - *A park that outlives its stretch answers nothing later.* Every poll,
     and every consent, reads the byte already; one that reads `PARKED` or
     `PARKED_ASKED` — a skipped unpark — clears it to `NONE` (or `ASKED`)
     first. A record reset for a new life stores `NONE`, and the exit
     unparks before its own collection. A debug build keeps a thread-local
     parked flag and asserts it clear at every count write, poll and
     consent.
   - *Why it is sound.* The park's release puts every tag stored before it
     ahead of the collector's acquire of `PARKED`; between park and unpark
     the thread writes no count and no slot (the debug flag checks it; a
     free into its heap from another thread writes only dead slots'
     metadata); and the unpark's read-modify-write, later in the byte's
     order than any ask that read the park, acquires the ask's release, so
     that no write after the unpark is one the trace before the ask could
     read (the load-buffering half the poll's acquire covers, §4.7).
   - *Both builds export the pair*: no-ops in the default build and on a
     thread with no record. The rig's web loads park around their wait for
     the next arrival in both arms.
2. *The cap is measured, not set.* S68.9's reading runs `web-heap` at 64k and
   at 1M: at 64k the act already read 5.0–6.2 ms, so the cap alone does not
   bound what a recall waits for; a commit that a stop could break between
   members would (§5, item 10, against S68.6b's one act), which is a
   question for after the reading.
3. *The loom model* (`checkpoint_model`) takes: a park against an ask, an
   unpark against an ask that read the park, a withdrawal against a park,
   a late poll answer against a withdrawal, and a stale `REACHED` read by
   the next ask.
4. *Counted*: parks; asks a standing park answered; parks that answered a
   standing ask; unparks that found the collector's write; stale parks a
   poll or a consent cleared — in the Δ-test's counts, the rig's `tag_*`
   columns and a journal kind. *Tests*: a park under a closed gate stores
   nothing; a park, an ask and a withdrawal leave the park standing; a
   skipped unpark is cleared by the next poll; a thread that exits parked
   leaves `NONE`; a set proved at a park is freed. *Docs*: §7's obligation,
   a DECISIONS entry.
5. *After the Critic's second round* (overriding items 1–4 where they
   differ).
   - *The ask and the unpark are one CAS each from the value read.* The ask
     loads the byte and moves it by one `compare_exchange`, retried on
     failure from the value the failure read: `NONE` or a stale `REACHED`
     to `ASKED`, `PARKED` to `PARKED_ASKED` (reached at once); a byte already
     `PARKED_ASKED` or `ASKED` is a defect, one ask standing a grant. The
     unpark is `compare_exchange(PARKED or PARKED_ASKED, NONE, AcqRel,
     Acquire)`, so that a park the gate refused never erases a live ask. The
     withdrawal loops likewise, from each value read back, until the byte
     reads `NONE` or `PARKED`.
   - *The runtime owns the bracket.* Not a pair the compiler places, but
     `ll_gc_blocking_call(f, arg)`, which parks, calls, and unparks on every
     way out, an unwind's included, so that no exit can leave `PARKED` up;
     and every entry from native code into compiled code — a callback, a
     comparator — unparks on entry and parks again on its return, so that
     no count is written under a park. §7, item 16, gains the obligation
     before the code: the compiler emits no park of its own and holds no
     ARC-elided temporary across a blocking call.
   - *The clear of a stale park at a poll or a consent* stays, as the
     release build's guard; the debug build's flag asserts first, so a
     stale park is a defect in a test, never an outcome.
   - *The exit* unparks before its thread-local destructors run.
   - *The order of the work.* First the byte, its loom model over every
     transition (an unpark or a re-park between an ask's read and its CAS,
     the withdrawal's loop, a gated park against a live ask), and the rig's
     web loads parking around their wait — where the contract holds by
     construction — then S68.8's reading again: whether the sets that
     missed a checkpoint come proved and whether the owner's pause meets
     5 ms, at the 64k cap and at 1M. The exported bracket is built only if
     both hold.
7. *The Sage's ruling* (2026-10-04), which overrides items 1–6 where they
   differ.
   - The parked state as item 5 shapes it is sound — between park and
     unpark the owner writes no count and no slot, so counts the trace reads
     after T equal those at T — and is built now, in the rig first, on four
     conditions: no cross-thread path writes a live header of a parked
     thread's heap (remote frees, a body's remote post, the stale clear,
     which touches W alone), and the debug flag asserts it; the park is a
     checkpoint, not a poll — no frees applied, no arming, no user code;
     `ll_gc_blocking_call`'s argument and anything native code holds across
     the park is held by a counted reference, written into §7.16 now; and
     every transition names its case in the loom model, a hand-kept copy.
   - The exported bracket is built once the rig's park drives the
     no-checkpoint count near zero and the longest pause, attributed by the
     posting's kind, is no longer a no-checkpoint set — not on the whole
     gate, which can fail for causes the bracket cannot touch.
   - *The commit act is not an owner pause.* A recall blocks nothing: the
     mutator marks its token and goes on, its withheld returns standing a
     little longer. The gate's owner pause is every wall a mutator spends
     stopped by the collector: the collection over P in all its phases, the
     application of the collector's frees, and a take's wait under
     `COLLECTOR` (`token_wait_longest_us`). The commit stays one act: a stop
     between members would leave the rest of C naming freed slots (§5, item
     10, qualified accordingly).
   - *The gate's 5 ms* holds for the sets the design routes to the
     collector; a set it routes to the owner for its destructors is bounded
     by those destructors, which no collector moves (§9).
   - *Held garbage is diagnosed before the park's code*: first from the
     cells in hand — whether this arm stamps more and prunes more, and how
     many record scans end cut; then one differential arm that runs the heap
     scan after the record's over the same roots and, for each root the
     record reads live and the heap does not, records the run that raised
     it — its holder's tag, and whether the holder's current cells still
     name the target. The taint's shape, or §4.8's holder-tag fallback, is
     decided on that.
   - The rig attributes the longest collection over P by the posting's
     kind: proved S, unmarked whole, no checkpoint, past the cap, second
     refusal, weakly held.
8. *Built* (S68.9, against the Critic of the code).
   - The Δ-test takes an ask a stretch answered as reached whatever the
     byte reads after, a stretch that ends before the wait's first reading
     erasing the ask.
   - *The Sage's first condition, read path by path*: no thread but the
     owner writes a live header of its heap. A remote free writes a dead
     slot's free-list link and its block's remote stack; a body's remote
     post a dead chunk; the collector's stale clear and its commit touch
     members of W, garbage the owner cannot reach; the collector's stamps
     write byte 6 of live entities, which is no count, no slot and no tag.
     No debug assertion checks it: a header names no owner cheaply on the
     hot path; the argument stands here.
   - The clear of a stale stretch at a poll or a consent is a mitigation,
     not a guard: a skipped leave followed by count writes and an ask under
     the grant already standing, before the next poll, takes the stale
     stretch as T. The rig cannot skip a leave; the exported bracket, which
     leaves on every way out, is what closes it.
   - The posting's kinds separate a cut batch, a whole set marked from one
     unmarked, and a mark lost at the publication.
9. *Not in S68.9*: the record scan's `ReadLive` count, two and a half times
   the heap scan's from S68.4 on, whose cause neither the root's tag nor the
   epoch's measure of work explains (`dev/BENCHMARKS.md`, the same entry);
   S68.9's re-reading measures what remains of the garbage first.

### 5c. The deferred lanes under turns by X alone: S68.10 (the plan, before the code)

**What was read** (`dev/BENCHMARKS.md`, the S68.9 second reading's entry,
"The X turn's release of every lane"). A root read live waits one, three or
seven turns in its lane; a lane filled before an X turn it has not seen goes
back into R at the next poll whatever its wait (the Sage, 2026-09-28;
`reoffer_the_lanes_due`). Under the record scan the epoch turns by X alone —
no batch's positions reach twice the proving price — so every turn releases
every lane and no root waits at all: 882k roots offered again in a 60 s cell
against 162k with the waits kept, and the garbage 159–181 MB against
62–72 MB. The default build pays the same rule on its X turns, about half of
its turns on `web-heap`: 72–106 MB against 63–71 MB.

**What the release buys.** The bound the crate's X is documented as
(`EPOCH_INTERVAL`, 8 s): how long a component that died behind a deferred
root waits on a thread whose batches prove too little to turn the epoch. A
wait counted in X turns alone would hold such a ring seven X, 56 s at the
crate's X. Read at the drain: with the waits kept the last garbage went at
32.7 s of a 90 s drain in B and 24.4 s of a 30 s drain in A; with the
release, within 7.8 s.

**The choice.**
1. *The waits kept across X* (the release dropped). The bound becomes
   `7 × X`; the garbage the lowest read in either build.
2. *A lane goes back once two X turns passed since it filled*. The bound
   `2 × X`; B's garbage 99–105 MB, between the two.
3. *As 2 with the count `K` the embedder's* (`ll_gc_set_lane_x_turns`, a
   knob beside the epoch's ratio), the default read on `web-heap` and the
   ring loads.

Proposed: 3 with `K` = 7 by default, which is choice 1, since the longest
lane's own wait is seven turns; the bound written into `EPOCH_INTERVAL`'s
contract as `K × X`. To be read before the build:
the ring loads (`partly-overlapping` and the churn loads) at `K` = 1, 2 and
7, whose remnant this rule moves (`dev/DECISIONS.md`, 2026-10-03,
"`partly-overlapping`'s remnant is the scheme's behaviour").

**Tests.** `an_x_turn_releases_every_lane` pins the release at one X turn;
it is rewritten onto `K`: the longest lane goes back at the `K`-th X turn
and not before. A case for `K` = 1 keeps today's behaviour reachable.

**The Sage's ruling (2026-10-05)**, which overrides the proposal above.
The one-X bound stands as `EPOCH_INTERVAL`'s contract writes it. At an X
turn the lanes go to a re-offer tail, and each batch takes a fixed share of
its roots from it — not only when R is empty, which starves the tail under
steady load and loses the bound. Kept on two readings: a column that proves
the cause (the age of a zero-count entry at its reading, or R's length
ahead of it), and A and B with the tail against their kept waits (63–71 and
62–72 MB). `K` may be an embedder's knob, default 1. The protocol's drain
gate and its 12 s stay.

### 5d. The sets that still fall to the owner: S68.11 (the plan, before the code; revised after the Critic of its first draft)

**What was read** (`dev/BENCHMARKS.md`, "S68.11: where the checkpoints are
missed", and the S68.9 second reading). The probe's 400k-member ring, taken
by the collector, pauses its owner 0.07 ms (objects) and 0.33 ms (arrays),
what §8 estimated; the owner pauses over 5 ms on `web-heap` are sets the
design sends the exact way whole — past the cap (144–167 ms at 64k, none at
1M), without a checkpoint (33–195 ms), a second refusal (3–14 ms) — and the
application of the collector's frees (3.1–8.6 ms). Of 105–118 missed
checkpoints a cell, 70–84 fell while the mutator ran the rig's draw of the
next request's plan and 32–34 in a request's build between two of its
polls: the rig's code, not the runtime's.

**The first draft's items and what became of them** (the Critic, 2026-10-05):
- *A checkpoint answered inside the runtime's teardown* is dropped. On
  `web-heap` no teardown runs where the misses fall, and §4.7 rules a
  checkpoint under a closed gate out: inside a teardown the runtime holds
  references it has not counted. Retracting that premise is the Sage's to
  rule, and nothing measured asks for it now.
- *The application in slices* and *a set without a checkpoint taking the
  second chance* wait for the reading below. As drafted, the slice lost the
  held drops a destructor's refused allocation stands on the record
  (`stand_the_held` under a slice's put-back), its time had no bound, and
  the second-chance bit would have meant a touch, a recall's unwalked root
  and a miss at once.
- *No cap at all* is not built: the commit reads no recall, so a take under
  pressure, the explicit fire and a second mutator's grant held by the same
  collector wait the whole act, which the gate counts.

**What is built** (revised after the Critic's second round).
1. *The rig's own work leaves the mutator's exposure, and its build polls.*
   (a) The draw of the next plan is the harness's bookkeeping, which the
   rig already subtracts from the window as its own work (`draw_a_plan`):
   it touches no entity of the runtime's heap (the test binary's allocator
   is `System`), runs at teardown depth 0 under an open gate, and writes no
   count, slot or tag. It runs inside a blocking stretch, entered and left
   outside the draw's instruction bracket so that B's runtime instructions
   are not subtracted as the rig's. This is not a compiled program's form —
   the compiler emits no stretch of its own (§7.16) — but the harness taken
   out of the program; what it removes from the no-checkpoint count does not
   count toward the Sage's condition for `ll_gc_blocking_call`, which is
   about blocking calls. (b) Within one slice the build advances to the
   slice's point in steps of at most `BIRTHS_AN_ADVANCE` births, a poll after
   each step, the polls' walls added to the deadline as today's one poll's
   is: a build's loop polls on its back-edge, the timeline and the order of
   events unchanged, nothing carried into the next slice that today's
   advance would have built in this one. `Request::advance` keeps its
   meaning; a bounded `advance_at_most` is the slice's. `BIRTHS_AN_ADVANCE`
   is `POLL_STRIDE` and is a new parameter of the protocol
   (`dev/design/the-web-loads.md`); it bounds births, not time, so the
   reading reads the longest step.
2. *The default cap 1M, as a trade.* At 64k a set past the cap pauses its
   owner 144–167 ms, certainly; at 1M a take that lands on a commit waits up
   to its length, 8.7–12.0 ms read on `web-heap` (objects; an array-heavy
   commit, which posts bodies remote, not read), and with one collector and
   two mutators the second's withheld returns stand as long. No measured cap
   keeps the act under 5 ms; 1M trades a certain pause for a possible one.

**Then read**, by the gate (§9), five 96 s cells of A and of B at 1M: the
longest owner pause by the posting's kind, missed checkpoints by the rig's
section (charged at the wait's end), the longest build step, the
application's longest, the garbage; and one cell where a take can land on a
commit — two mutators on one collector with the explicit fire, or a heap
limit that refuses allocations. What remains over 5 ms decides what comes
next; none of the queued items touches the second refusals (3–14 ms), the
application (3.1–8.6 ms) or a take's wait on a commit, and the second chance
for a miss would add to the first.

**The Sage's ruling (2026-10-05)**, which overrides the above where they
differ (`dev/DECISIONS.md`, the same date).
- Items 1 and 2 are built. `MEMBER_CAP`'s doc reads "the bound of a take's
  wait"; the take-on-commit cell is judged against `token_wait_longest_us`,
  and if it fails there the knob for that configuration goes to Edmond.
- The commit and its publication stay under the grant.
- §4.7's premise is qualified: the reset and the collection keep it; in a
  teardown only a compiled destructor's frame remains, under §7.16. A
  checkpoint inside a teardown is sound on the Critic's confirmation that
  every decrement to zero on its entries goes through `refcount_store`; not
  built.
- *A second refusal reads live* (§5a, S68.6c, item 4 amended): U's roots
  read live, U recoloured out of W as at the first refusal, S stays proved;
  `SECOND_REFUSAL` removed; a member whose address cannot be recovered goes
  the exact way.
- *The application in slices* (§5a, S68.6b, "the application", and §6,
  item 12, amended): the chains spliced and the registered members counted
  in the first slice; the drops `POLL_STRIDE` a poll; the rest put back by a
  splice behind what stands, never a store; P read after the last slice.
  `FreesStand` refusals read before and after; if they rise, the frees word
  becomes a list (the collector pushes, the owner takes).
- *Then read* adds the owner's freed members by posting kind.

**Tests.** `advance_at_most` of a plan with more births than the bound
stops at the bound and the next call goes on from it, the events in the
order `advance` builds them; a slice whose plan places more births than the
bound polls between its steps and ends at the same point on the timeline;
the draw runs between an entered and a left stretch.

## 6. The owner's poll

12. The owner applies the drops, each through `drop_ref`, typed at
    application by the child's category then (a reset may have promoted it).
    Destructors that these deaths reach run here, on the owner's thread.
13. It splices the chains per block, correcting `used`.
14. Members dead in place return through the retirement pass, as today.
15. S, if cyclic, goes the exact way; an acyclic S dies by ordinary
    reference counting after step 12.

## 7. The compiler's obligations

16. No uncounted reference is live across a safepoint checkpoint (an
    ARC-cancelled retain/release pair never spans a poll) — and a runtime
    entry that polls inside itself is one: `ll_release_vector` calls the
    full poll every `POLL_STRIDE` elements, so no uncounted reference may be
    live across a call to it either. A blocking stretch is a checkpoint too
    (§5b): no uncounted reference is live across a call to
    `ll_gc_blocking_call`, its argument and everything native code holds
    across the stretch are held by counted references, and the compiler
    emits no stretch of its own; a callback from native code into compiled
    code leaves the stretch on entry and enters it again on return.
17. The store barrier receives the holder (`ll_store_*_in(ctx, owner_cat,
    holder, slot, new)`): an ABI change, enforced — under the feature the
    untagged `ll_store_ptr`/`_box`/`_owned` names are not exported, so an
    emitter still calling them fails to link; a slot of a headerless static
    block, a root, stores through `ll_store_ptr_root`/`ll_store_box_root`.

## 8. Cost, by who pays and how often (estimates, not measured)

| when | mutator | collector |
|---|---|---|
| always | every count write: + one byte store (≈ +2 instructions, measured ≈ +0.35 ns on a hot store); every GcHeap pointer store: + the holder's byte store and one argument (measured 6.26 → 7.40 ns on a hot heap-into-heap store) | — |
| a grant | the consent as today, the window number advanced | the request as today |
| the batch | runs on, withholds returns as today | every traced edge: the subtraction as today + one recorded entry (8 B), and one a run; the scan over the record, not the heap |
| T | one handshake | one byte read per member of W, + one CAS per stale member |
| splitting and freeing | — | per member: the S test, its drops, count 0 and dead in place or the chain; per edge out: one drop entry |
| the owner's poll | per drop: `drop_ref` (+ its cascade, an ordinary RC death); per block: a splice; dead in place: the retirement pass as today; S: the exact way | — |

Totals: ≈ +1–3 % mutator CPU on `web-heap`; the owner's pause ≈ 0.1–1 ms on
a 400k-member set (≈ 300 block splices and a few drops) against 48–51 ms
measured today; the collector ≈ 30–40 ms on that set against 26–28 ms today.

## 9. Gate and checks

The Sage's gate: retain/release and store microbenchmarks with and without
the tag; on `web-heap`, mutator CPU within +3 % of the default build, the
longest owner pause under 5 ms in every cell — the pause being every wall a
mutator spends stopped by the collector (the collection over P, the
application of the collector's frees, a take's wait), and the bound holding
for the sets the design routes to the collector, a set routed to the owner
for its destructors being bounded by them (the Sage, 2026-10-04, on S68.9) — held garbage no worse,
Δ-refusals under 10 % of batches. A loom model of the handshake at T; a debug
build asserting the tag at every primitive; a debug build running the exact
validation beside the collector's verdict and checking both agree, and
asserting the collector writes byte 7 of no entity outside W (as
Firefox's debug builds do, `dev/RESEARCH.md`, "immediate RC with a concurrent
or incremental cycle collector").

## 10. Prior art

No system with owner-written immediate counts and a concurrent collector
thread judging over shadow state was found. Closest: Firefox's incremental
cycle collector (the same "touched during the collection means live" rule,
in one thread), Samsara (Rust; a concurrent thread over immediate RC with a
DIRTY flag), Pony (proposal and confirmation). Lessons carried here: an
object that dies mid-scan must not whiten what it pointed to (Firefox bug
1023758 — here §4's recorded edges and the withheld returns); the final
decision and the free need no mutation between them (§4's handshake); what
deferral holds must be watched (CPython 3.14's reverted incremental
collector). `dev/RESEARCH.md`, the two entries of 2026-10-04.
