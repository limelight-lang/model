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
   consent; 0 means closed.
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
   One relaxed one-byte store, no branch (a closed window writes 0). Holders
   in an arena need no tag (`owner_cat` is a compile-time constant).
3. `RECONCILING`, today the only bit in byte 7, moves into the reset window's
   capture log, so byte 7 has no other writer.

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
   window's number: the set is not judged (back to the queue, or the exact
   way). None: W is garbage at T and stays garbage — judged by the collector
   alone.

   *Why (the Sage's proof).* An entity untagged at T had no count write and
   no slot write in [consent, T], so its count is the value read and every
   recorded edge out of it stands at T. A recorded in-edge of a white entity
   comes from W, since a live source colours its target live over that edge.
   So at T every reference into W is a recorded edge inside W; at a
   checkpoint every reference is counted (locals included), so nothing
   outside W refers into it. Garbage cannot be resurrected after T (weak
   cells aside, which §5 routes to the owner).

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
11. The collector releases the token; the window closes (a set dropped unread
    — pressure, exit, a block return — closes it too).

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
17. The store barrier receives the holder (`store_ptr(holder, slot, …)`): an
    ABI change.

## 8. Cost, by who pays and how often (estimates, not measured)

| when | mutator | collector |
|---|---|---|
| always, while a window is open (nearly always on `web-heap`) | every count write: + one byte store (≈ +2 instructions); every GcHeap pointer store: + one byte store and one argument | — |
| a grant | the consent as today, the window number advanced | the request as today |
| the batch | runs on, withholds returns as today | every traced edge: the subtraction as today + one recorded edge (≈ 4 B); the scan over the recorded edges |
| T | one handshake | one byte read per member of W |
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
validation beside the collector's verdict and checking both agree (as
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
