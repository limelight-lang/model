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
   would be unsound — a wind-down still scans, and zeros stored before T would
   erase the tags the Δ-test reads. 0 is the number before the first consent.
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
   subtracts (source row → target row, about 4 bytes an edge).
5. The scan spreads the live colour over the recorded edges only, never over
   the current heap: writes made after the mark to entities outside the set
   (a live array growing, a move out of a live holder) cannot whiten a live
   entity. White entities form the set W.
6. A batch stopped by a recall before its scan completes posts as today and
   its set goes the default build's exact way.

## 4. The judgement

7. The cut-off T is the mutator's next safepoint checkpoint (not a slot-free
   reading, inside which a frame may hold ARC-elided temporaries): a
   handshake — a release by the mutator that the collector acquires (and a
   release on T's request, an acquire on the poll's read) — after which the
   collector sees every tag stored before T.
8. The collector reads byte 7 of every member of W. Any member carrying the
   window's number: the set is not judged. None: W is garbage at T and stays
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
    Freeing follows `ll_free`'s routing (retained blocks, entities in another
    thread's block, large runs) but not `free_remote`, whose withholding would
    recall the collector itself; registered members stay dead in place for
    the owner's retirement pass, as today.
11. The collector releases the token (a set dropped unread — pressure, exit,
    a block return — releases it too); the window stays open until the next
    consent opens the next one (§2.1).

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
    ARC-cancelled retain/release pair never spans a poll).
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
| the batch | runs on, withholds returns as today | every traced edge: the subtraction as today + one recorded edge (≈ 4 B); the scan over the recorded edges |
| T | one handshake | one byte read per member of W, + one CAS per stale member |
| splitting and freeing | — | per member: the S test, its drops, count 0 and dead in place or the chain; per edge out: one drop entry |
| the owner's poll | per drop: `drop_ref` (+ its cascade, an ordinary RC death); per block: a splice; dead in place: the retirement pass as today; S: the exact way | — |

Totals: ≈ +1–3 % mutator CPU on `web-heap`; the owner's pause ≈ 0.1–1 ms on
a 400k-member set (≈ 300 block splices and a few drops) against 48–51 ms
measured today; the collector ≈ 30–40 ms on that set against 26–28 ms today.

## 9. Gate and checks

The Sage's gate: retain/release and store microbenchmarks with and without
the tag; on `web-heap`, mutator CPU within +3 % of the default build, the
longest owner pause under 5 ms in every cell, held garbage no worse,
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
