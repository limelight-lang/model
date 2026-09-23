# S65 plan, for review

A snapshot of `PLAN.md`'s stage S65 at `b7245ce` (2026-09-23), put in a file of
its own for a Critic's review. `PLAN.md` stays the authoritative copy: a
finding is applied there, and this file is not kept in step with it.

**What to read with it.**
- The ruling the stage builds: `dev/DECISIONS.md`, the entry of 2026-09-23,
  "the collector finds and the mutator judges, and a recall of the token
  bounds the mutator's wait instead of the budget".
- The design, in Russian: `dev/CYCLE-SPLIT-PACKAGE-3.md` — section 11 is the
  build order the steps follow, section 12 the premise changes Edmond
  accepted.
- The lane amendment, `dev/CYCLE-SPLIT-PACKAGE-3-LANE.md`, taken in the form
  of its Critic's F2 and F3 (`dev/CYCLE-SPLIT-PACKAGE-3-LANE-CRITIC.md`).
- The review chain behind it: `dev/CYCLE-SPLIT-*.md`, and Astra's
  `dev/S64-GC-IMPROVEMENT-ANALYSIS.md`.
- The routine every step obeys: `PLAN.md`'s header (the Critic gate per step,
  the baseline recorded before the first edit) and `dev/WORKFLOW.md`.

**The owner's rule for every open choice:** the mutator's performance comes
first — a cost on the collector alone is acceptable, one on the mutator's
poll, free, allocation, wait or withheld memory is not.

## S65 — The collector finds, the mutator judges  [in progress]

Goal: the mutator searches for cycle garbage only when the memory manager
refuses it or the embedder turned the collector off; in every other case the
collector finds and the mutator judges what it proposes, and what bounds the
mutator's wait for its token is a recall rather than the trace's block budget
(`dev/DECISIONS.md`, "the collector finds and the mutator judges, and a recall
of the token bounds the mutator's wait instead of the budget"). The design is
`dev/CYCLE-SPLIT-PACKAGE-3.md` with `dev/CYCLE-SPLIT-PACKAGE-3-LANE.md` as
amended by its Critic (`dev/CYCLE-SPLIT-PACKAGE-3-LANE-CRITIC.md`, F2 and F3);
the review chain behind it is `dev/CYCLE-SPLIT-*.md` and
`dev/S64-GC-IMPROVEMENT-ANALYSIS.md`. Each step is one commit of the package's
build order (its section 11), with that section's mechanism, red test and
measurement; the rule of the stage is the owner's: the mutator's performance
comes first.
Done when: the mutator's collection over P traces no `Unwalked` root, no poll
arms a collection over R whole outside `cap 0`, a recalled grant returns the
token within N edges and one arena reset, and the poll and the free are where
`dev/BENCHMARKS.md` S60.6 and S38.3 put them.

- [ ] S65.1 Correct `queue.rs`'s claim that an entry's low four bits are clear
      done: the two comments (the module head and `ENTRY_MARK_BITS`) say three
        bits, eight-aligned promoted survivors being candidates; text in the
        package's section 10
      tier: T0 · role: —
- [ ] S65.2 The epoch cell is the collector's (package commit 1)
      done: the cell on the record's hold line, advanced at the first of 64
        batches or X of the collector's clock; the turnover byte has one
        writer; one epoch reading per collection; `turn_the_cell_of` replaces
        the commit-driven instrument in the seven test files; the red test of
        the quiet thread's defect — real all-live takes oftener than X, the
        lane's mirror moved after X — seen red on today's tree and green
        after; `what_the_poll_costs` within S60.6
      tier: T2 · role: Critic
- [ ] S65.3 A merged lane is taken at the next round, and K doubles only on a
      filled clamp (package commit 1a, in the Critic's form)
      done: a `merges` counter on R's writer line bumped beside the splice,
        `merges_seen` on the hold line, the round answering `Takes` when it
        moved; `size_the_next_batch` doubles only when the batch took its
        clamp; a take reads its backlog by the exact count. Red first: the
        shipped K ratchet — a poll that splices and consents, then a batch of
        8 roots moves K from 64 to 128 — seen red today; a packed lane taken
        at the next round; a take over three blocks reports no backlog
      tier: T2 · role: Critic
- [ ] S65.4 Completed deaths of R are retired by a count (package commit 2)
      done: a count on the candidate arm of `ll_free`, `Arming::Retire` at the
        threshold, the pass on a poll with the gate open and the token free,
        tracing nothing; its red test red today; the poll unmoved
      tier: T2 · role: Critic
- [ ] S65.5 The trace runs in parts (package commit 3)
      done: a reset to the watermark above the root copy, a `judged` bit, posts
        per part and `FinishThePosts`; the batch [r1, r3, r2] case posts
        exactly three verdicts; `disjoint-live` in 63 parts with the mutator's
        census at 0 roots and 0 rows
      tier: T2 · role: Critic
- [ ] S65.6 The token's recall (package commit 4)
      done: `waiting` set in `take_unless`, checked every N edges in both
        phases through the visitor wrapper; `trace_cells` and
        `walk_concurrent` stop on `ControlFlow` (the customers' concurrent
        walk only, named as their change); the budget's two comments say it
        bounds the arena; a pressure collection under a grant raised after the
        mark waits at most N edges and a reset
      tier: T2 · role: Critic
- [ ] S65.7 The marks by stack length (package commit 5)
      done: a length beside each of the three withheld stacks' heads, a limit
        each, `waiting` stored at the limit with no block; a mutator freeing
        under a grant recalls it at M and never waits; the withheld-free arm
        of `what_a_foreign_holder_costs` within its spread
      tier: T2 · role: Critic
- [ ] S65.8 The live core stamped from a list (package commit 6)
      done: one chain per grant of at most L blocks, its head on the hold
        line, stamped at the take from `POSTED` or at the first block or run
        return under it, dropped under pressure; after a take over
        `overlapping-live` every member carries the stamp and the next take
        prunes
      tier: T2 · role: Critic
- [ ] S65.9 The retry at the ceiling and the parking (package commit 7)
      done: a part failing at B retried at `B_max` in the same grant, once per
        grant; the roots that attempt met parked in the chain's second section
        with a mark in byte 6 bits 20–22; a 300-block ring judged in one grant
      tier: T2 · role: Critic
- [ ] S65.10 `Unwalked` is no root of the collection over P (package commit 8)
      done: `Verdict::is_root_in(BatchForm)`; P = [`Unwalked` x] writes x back
        into R untraced; under pressure 63 `Unwalked` are still traced; the
        turnover's `arm()` gone outside `cap 0`
      tier: T2 · role: Critic
- [ ] S65.11 The rfc follows
      done: `rfc/model/gc/rc-cycle.md` states who finds and who judges, the
        recall, the parts and P in parts' order; the handshake document
        records `waiting` as a hint beside E10 and the stamp beside E12;
        `rfc/model/classes.md` names bits 20–22 of byte 6 as the parking mark
      tier: T2 · role: Critic
- [ ] S65.12 The N-mutators-on-N-cores rig, then `cap 0` (package commit 9)
      done: the rig's three placements measured and recorded; only then the
        clamp lifted, the elder kept as the clock without takes, and the
        mutator collecting at its threshold and at the merge under `cap 0`
      tier: T2 · role: Critic
