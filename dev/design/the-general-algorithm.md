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
| 6 | every count store tagged and released | `ll_retain` 13 → 24 instructions, `ll_release` 46 → 59 (2026-10-04); a `__tls_get_addr` call on every counted store | candidates: initial-exec TLS or the window in `ctx`; read the window before the count; no tag on a −1 to 0; candidate registration out of line; AArch64 unmeasured |
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
