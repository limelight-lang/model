# The general algorithm: Edmond's ten principles over the recycler

Wave 1 of the rework Edmond set on 2026-10-06 (`dev/DECISIONS.md`, the
entry of that day): the general algorithm on one page, with no code and no
thresholds; then the Critic, Claude's answers, the Sage, and Edmond. Polish,
code, and per-function analysis come in later waves, each on his word. The
detail it rests on is `recycler-over-counts.md` (§2–§5f); where this page and
that one differ, this page is the proposal and that one the accepted design
until Edmond rules.

## The principles (Edmond, 2026-10-06)

1. The mutator alone decides when to open a window.
2. The batch is far larger than 1,024 roots (PHP's buffer holds 10,000;
   perhaps 16k); the size is measured, not chosen.
3. The collector does not start at the window's opening, but once a clear
   delta has formed.
4. The mutator executes several times fewer instructions than in the
   default build.
5. The collector executes more, but does not enter freshly tagged entities,
   unless their count is 0.
6. The mutator's +1 and −1 are optimised and reviewed with care.
7. Neither side waits for the other, except at edge cases.
8. Putting entities into the roots is cheap, and a long-lived entity is not
   put there again and again.
9. The trace is analysed and judged on its own. With window tags the trace
   need not enter fresh tags, no epoch is needed for the judgement, and one
   stage suffices: the set is judged at once.
10. Destructors run on the mutator only: it accepts the collector's proved
    cycles and runs them without judging them again.

## The algorithm

**The mutator.**
- A −1 that leaves a count above zero puts the entity into its root queue R
  once in its life (the buffered bit); an entity read live several times
  waits out longer lanes before it is offered again (P8).
- Every count store and every slot store the trace reads first writes the
  current window's number F into the entity's (or holder's) tag byte, then
  the data with a release (P6). Open: whether a −1 that does not reach zero
  needs the tag (Edmond's hypothesis, below).
- At its own poll, when R holds a batch, the mutator turns its window to F
  and offers the batch's end on its token. Nothing else turns the window
  (P1). The offer is the delta: R's roots, frozen at the frame (P3).
- It does not wait for a take: it goes on, and withholds returns of memory
  until the collector releases the token or a mark withdraws the offer (P7).
- At a later poll it takes the cycles the collector proved and holding
  destructors, and runs their destructors without judging them (P10).

**The collector.**
- It takes an offered batch when it comes round, never before the offer
  (P3), and traces the batch's roots over its own shadow rows.
- It does not expand an entity whose tag equals F unless its count is 0:
  such an entity was touched after the frame, so it and what it reaches are
  live for this batch (P5, P9).
- After the trace, one acquire fence and one tag read a member: a set none
  of whose members carries F is garbage and judged at once, in the same
  batch (P9). No epoch enters the judgement.
- It frees the garbage that holds no destructors and no weak references
  itself; the rest goes to the mutator as proved (P10). It releases the
  token. Neither side waits except where the table names an edge case (P7).

## Where the accepted design and the build stand

Read on 2026-10-06 from main at `4c96740` by three read-only audits, from the
code, not run; the counts are from `dev/BENCHMARKS.md` with their dates.

| P | Accepted design (§5f) | As built | Gap |
|---|---|---|---|
| 1 | mutator offers at its poll; §2.1, §4.7 and §1 still describe the consent and the checkpoint | only the offer turns the window (`offer.rs`, from the poll) | the old sections to rewrite; the collector still gates the offer: none without a standing collector, the standing interval on its round clock, a withdrawal waits its next round, cap 0 turns an offer into an ask |
| 2 | batch ≤ `BATCH_BOUND` 1,024; offer at 64 | as designed; no threshold above 1,024 ever run | the workspace copy caps a batch at 1,780 and a threshold at about 7,100; a 10k–16k batch needs the workspace and the read-ahead reworked |
| 3 | offer at 64 roots, a standing R after 4 s, or merged lanes due; the trace starts at the take | as designed: the take starts at once, at the start of window F | "a clear delta" is defined nowhere (design, PLAN, DECISIONS); the only delta read is the Δ-test after the trace; to rule |
| 4 | — | B's mutator CPU 1.2–1.7× under A's on `web-heap` and the ring loads (2026-10-05); no instruction counts (no PMU) | "several times" not met; per operation B executes more, the gain is the owner's collections leaving the mutator |
| 5 | tag read only at the judgement; the prune at F is an Open item (no room on the row) | the trace never reads the tag | not built; needs a side bitmap or another place for the colour |
| 6 | every count store tagged and released | `ll_retain` 13 → 24 instructions, `ll_release` 46 → 59 (2026-10-04); linked (`benches/lifecycle`, 2026-10-06) the window's read is `mov %fs:0`, `lea`, a byte load, with no call, but the function keeps the frame (three pushes, two pops) the unlinked call needed and splits the `inc`; the window is a plain `thread_local!`, not the heap's fast TLS (`memory/heap.rs`, `mod tls`), so on Windows it pays the module chain the heap's fix removed | candidates: the window beside the heap's fast TLS slot or in `ctx`; read the window before the count; no tag on a −1 to 0; candidate registration out of line; AArch64 unmeasured |
| 7 | the consent and the checkpoint wait gone; returns withheld under `OFFERED`, no blocking | slow paths block on a take (pressure, exit, fire, teardown pass, compaction, reset), up to a whole commit (36 ms read at the 1 ms pace); one batch a mutator at a time, the next waits on the owner's drops; a thread that never polls is never collected | the commit wait and one batch at a time are not edge cases; to rule |
| 8 | buffered bit, lanes 1/3/7 | active in B, but an X turn hands back every lane on a caught-up thread, and B's epoch turns only by X | the lanes do little in B; a refused set's roots also return once |
| 9 | trace → scan → Δ-test → split → post; epoch in use | as designed; the owner re-meets every member of a proved set and confirms by the counts' sum | two stages and the epoch remain |
| 10 | S (destructors, weak) goes to the owner; S if cyclic goes the exact way (§6.15) | the collector tears down C (no destructors) itself; the owner confirms a proved set by the counts' sum and walks it in full when the sum differs (always in debug), and re-traces every unproved set | the confirm and the re-traces are the judging P10 rules out |

## Open for the waves

- **A −1 that does not reach zero, untagged.** Dropped on 2026-10-05 on a
  Critic's two counterexamples without Edmond's word; a Critic and the Sage
  are judging it again with the release kept and a compiler obligation for
  borrowed references.
- **The prune at F during the trace.** A tag read during the trace can only
  send entities to live; the fence and the tag pass over the set after the
  trace remain for soundness, unless an asymmetric barrier replaces them.
- **The batch size.** Measured at 64 and at a 1,024 ceiling only; 10k and
  16k are to be measured once the workspace allows them.
- **The second stage.** What of the owner's recheck and the epoch can go
  once the judgement is immediate and the prune at F is built.

## Wave 1: the Critic's findings and Claude's answers (2026-10-06)

The Critic (opus) attacked "The principles" and "The algorithm", reading the
code, running nothing. Each finding with Claude's answer; the Sage judges
both next, and nothing here is accepted before Edmond rules.

1. **The prune at F keeps a garbage structure forever (liveness).** Stale tags
   are cleared on members of W only; a pruned member never enters W, so a
   stale tag equal to F prunes it again at every attempt. *Accept.* A root
   whose trace pruned anything is posted unwalked with a no-prune bit (the
   second-chance bit's mechanism); its next trace enters everything, W forms
   and its tags are cleared.
2. **"Live for this batch" in the live colour proves liveness.** The live
   colour feeds the read-live posts, the lanes and the stamps, so garbage
   dropped after the frame waits a lane. *Accept.* The prune is a fourth
   outcome, neither live nor member (the side bitmap §5f's Open item asks
   for); a root met only through it is posted unwalked and stamps nothing.
3. **The epoch cannot go.** The judgement reads no epoch already; the epoch
   is the clock of the maturation stamps and the lanes, and a stamp cannot be
   retired by window tags. *Accept.* P9's "no epoch" holds for the
   judgement; the epoch stays as the stamps' clock. Whether the lanes keep
   it is finding 9.
4. **P10 has two readings.** Dropping the validation after destructors is a
   use-after-free (a `__destruct` stores `$this` into a global). Dropping the
   counts'-sum confirm before them buys nothing measurable. *Accept,* with
   the Critic's alternative to rule on: the owner's confirm reads the tags of
   S for F in place of the sum (the window cannot turn while P stands), which
   also lets a weakly held S be proved instead of retraced. The validation
   after destructors stays: it is PHP's semantics, not a second judgement.
5. **P2: a 10k–16k batch is clamped and defeats itself.** P is one block
   (about 8,159 verdicts); the withholding marks (8,192 deaths, 16 blocks)
   recall a long trace, a cut batch goes to the owner and K halves.
   *Accept.* Before P2 is measured, P grows and the marks scale with the
   batch, or a cut batch's work is kept; both are prerequisites, not tuning.
6. **P3: "a clear delta" has two readings.** If it is the offer, it is built.
   If the collector waits after the offer, the withheld returns reach a mark,
   the offer is withdrawn, and no batch is ever taken. *Accept the risk;*
   the meaning is Edmond's to state. Claude's proposal: the delta is defined
   on R (roots never offered, the oldest's wait, lanes due), the take follows
   at once, and no delay after the offer is built before it is measured.
7. **P5: "unless their count is 0" has three readings.** *Accept reading
   (a):* an F-tagged entity at count 0 gets no row and no expansion, as the
   corpse rule does today; expanding a corpse subtracts its released
   children twice.
8. **P9: "a set none of whose members carries F" drops the U split.**
   *Accept.* U, the closure of the touched members, is refused; W − U is
   judged.
9. **P8: the lanes against turns by X.** B turns only by X, so the lanes
   wait 8, 24, 56 s, and a larger offer threshold would release every lane
   at every X turn. *Accept.* Proposal: the lanes count the mutator's own
   offers, with a time floor, independent of the epoch and of the batch
   size.
10. **P7: inherent and removable waits.** Inherent: withheld returns from
    offer to release, a recall's stride, exit and teardown waiting out the
    heap's reads. Removable: the commit-length wait (unproved), one batch at
    a time (needs an "in a posted set" mark first, or a destructor runs
    twice), the withdrawal's wait for the next round (policy), a thread that
    never polls. *Accept.* Proposal for the last: the park
    (`ll_gc_blocking_call`) offers when R is not empty, still the mutator's
    decision, at a point where every reference is counted.
11. **P4 cannot hold per operation.** B's +1/−1 carry A's work plus a tag.
    *Accept the reading, not the restatement:* P4's goal is Edmond's; the
    Critic's "GC-attributable mutator instructions several times fewer" is a
    measurable form for him to accept or reject.

Sound as stated, by the Critic: P1 (but the thread that never polls), P6 as
a goal, P8's first half, P10's first clause, the judgement's fence and tag
read, and the prune's soundness for frees.

## Wave 1: the Sage's judgement (2026-10-06)

The Sage (Fable) read the page, the Critic's findings and Claude's answers
against the design and the code, running nothing.

**Conclusion.** The amended algorithm is sound for what the collector frees,
but not yet live in the sense P8 asks; nine of the eleven findings are
right and Claude's answers hold; two the Sage answers differently.

- **Finding 1, answered differently.** The window advances at every offer
  (`the_next_window`), so a member pruned in F carries a stale F in F+1, is
  expanded and cleared; the repeat is the 1/255 coincidence, not forever.
  The no-prune bit buys nothing; the existing second-chance bit is the
  liveness rule: pruned once, posted unwalked; pruned twice, read live.
- **Finding 2, answered differently.** A member pruned at F leaves its
  out-edges unsubtracted and unrecorded, so a ring's root reads live with
  nothing in the record tying it to the prune; telling it apart needs the
  taint over the raises the design rejected (§5a item 5). Today the same
  ring is refused as U and requeued once; with the prune it waits a lane.
  The prune at F is worse for held garbage than the built U split; its only
  gain, trace work, is unmeasured.
- **Finding 4.** The owner's tag read in place of the sum is sound and
  stronger: a weak upgrade is a count store and tags; the window cannot turn
  while P stands. For an S that is not weakly held the confirm is redundant,
  so P10's first clause can hold literally there.
- **Finding 5.** Scaling the marks with the batch is a trade of P2 against
  P7 to be priced in bytes; a longer trace is a longer window, so the
  refusals grow with the batch as well.
- **Finding 6.** A third reading of "a clear delta": the Recycler's
  deferral, roots old enough that the transient ones died by counting. The
  record scan's read-live count is 2.5 times the heap scan's, unexplained.
- **Finding 9, answered differently.** Not a third clock: the epoch turns on
  this thread's taken batches, with X as the floor; the caught-up rule is
  decoupled from the offer threshold first.
- **Finding 10.** `ll_gc_blocking_call` exists in no source file; an offer
  before a blocking call is a new export. The commit-length wait is one act
  by ruling; its only lever is a second collector.
- **Finding 11.** The gain is the owner's collections leaving the mutator;
  callgrind gives instruction counts where the rig has no PMU.

**Ranked open problems.** P5's prune regresses liveness against the second
chance; `write_through` is the one untagged slot store, closed only by its
tagged count stores; eight-bit windows turn with the offer rate; the offer
is gated by the collector and the drops backlog, so P1's letter needs its
edge cases listed; a lost wake under `OFFERED` can withdraw at a mark round
after round; the owner's second stage stays for S.

**Questions for Edmond** (the Sage's recommendation in brackets).
1. P3's "clear delta": the offer, a wait after it, or roots older than an
   age? (the offer now; measure the age)
2. P5's prune at F: measure its share of the trace's expansions first,
   build it with a side bitmap, or drop it? (measure first)
3. P9: refuse W whole or U only? (U)
4. P10: the owner reads one tag a member before destructors, in place of
   the sum? (yes)
5. P2: the mutator's memory withheld during a 16k batch: marks times 16, or
   a byte bound? (a byte bound, measured)
6. P8's clock: the epoch re-based on taken batches, or a clock on offers?
   (the epoch)
7. P7: a second collector to take while one commits, or cap 1 stands?
   (measure first; cap 1 stands)
8. P4's form: "GC-attributable mutator instructions several times under
   A's", by callgrind? (yes)
9. The untagged −1 not reaching zero: an arm after P6's cheaper wins, or
   not? (after: it saves two instructions where the TLS read and the frame
   cost nine)

**Order of the waves.** Polish: §1, §2.1, §4.7 rewritten to the offer, an
edge-case table for P1 and P7. Measure before code: the share of
expansions at F, corpses and read-live per batch against the oldest root's
age, windows a second and coincidence refusals, owner time by posting kind,
withdrawals at a mark, callgrind counts of `ll_retain` and `ll_release`.
Then code: P6 (the window beside the heap's fast TLS, the frame removed);
P2's prerequisites (workspace and P for 16k, marks in bytes, then 1k, 4k,
16k); P8–P10 (epoch re-based, the sum replaced by the tag read); P5 only if
its measured share is material. Per-function analysis last.
