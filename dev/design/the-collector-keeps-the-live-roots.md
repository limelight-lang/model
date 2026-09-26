# The collector keeps the live roots

Proposed 2026-09-26, not built, not ruled: the target of two design rounds
(a Fable Critic and the Sage each round) over Edmond's two requirements —
**the mutator does no unnecessary work, and the collector takes on the most
it can** — with only the mutator freeing memory. Three parts amend earlier
rulings and wait for Edmond's word; the rest is the Sage's to decide and is
marked so.

## What changes

**A root the collector read live costs the mutator nothing.** Today such a
root costs the mutator a whole collection over P each epoch: the token, a
trace window, the arena, three walks of P, a cold header read and a store
into the deferred lane (`queue::compaction::dispose_verdicts`,
`queue::defer_entry`), then the epoch byte compared at every poll and a
splice back into R. A batch of 63 live roots left the mutator 15,365
instructions against 658,832 in line (`dev/BENCHMARKS.md`, the live-roots
arm), and that remainder is the collection over P that frees nothing.

Under a grant the collector puts a root it read live, and a root its trace
did not reach, into **the collector's chain**: pool blocks the collector
draws, their head on the owner's record beside the live list, each block
stamped with the record's epoch, the bytes charged to the owner's GC ledger
(`gc_metadata::hand_over`). At a later grant whose epoch has passed a
block's stamp, the collector reads that block ahead of R and traces its
roots again. It posts no verdict for them into P, and a batch that posted
nothing releases the token to `FREE`: the mutator does nothing at all. The
candidate bit stays set throughout, so a parked root that dies keeps its
slot until the collector reads it as a completed death and posts it into P
as `ZeroCount`.

**A P of dead counts alone opens no trace window (form D; the Sage's,
final).** A batch that posted only `ZeroCount` releases to a third value of
the byte beside `ASKED`; the poll's arming for it runs a pass over P alone
that retires completed deaths, writes back resurrected ones and advances P.

**The mutator's deferred lane goes.** Its mirror, the poll's epoch compare
and the splice go with it. A pool refusal of a chain block posts the root
into P as `ReadLive`, and the mutator writes it back into R, R's one writer.
The mutator's own collections — the pressure path, the exit, cap 0 — read
the chain as a batch source under `MUTATOR`, as they read the deferred lane
today; the spare-cell parking stays for their live roots under a refusal,
or the regression is stated.

## What stays

- Registration, the overflow buffer, the mutator's frees.
- The collection over P traces the proposed roots itself (rows form, exact
  validation, guards, destructors, revalidation, sever, frees).
- The close's run of completed deaths at R's front (S65.23) and the count's
  pass below the threshold.
- Nothing new is withheld under `POSTED`. P names roots alone, each held by
  its own candidate bit, so today's argument for the mutator's freeing
  under `POSTED` stands.
- One registered entity, one entry: in R, in transit under a grant with its
  advance owed, in P, in the chain, or in the overflow buffer.

## What waits for a measurement

**The collector's live-core stamps.** The collector could write header
byte 6 itself under its grant instead of handing the mutator a live list.
Correct by the token's ordering, but each store lands on a line the mutator
retains and releases. Three arms on `what_a_take_costs`, `overlapping-live`,
the mutator's instructions, cycles and misses over the window after the
grant: stamped by the mutator from the list (today), stamped by the
collector, not stamped at all. Until then the collector writes no mutator
header. Amends Edmond's 2026-09-23 ruling if taken.

## What was dropped

**Validation by the collector's member list** instead of the mutator's
trace. Its gain is unmeasured and bounded by the mark and scan share, since
the validation still walks every member's fields and searches a sorted list
per edge where a row is address-computed. The list holds 64 KiB blocks from
the post to the collection over P, where a dense 381-member ring's rows
cost the ledger no block (2,112 bytes in the thread's workspace). A member
never registered carries no candidate bit, so its slot can be reissued
under `POSTED`; making the list safe needs either withholding at `POSTED`
(four gates, every death paying 0.4 ns now and 4.2 ns at the return) or
stale hooks with a fallback to the trace. Edmond doubted it; the Sage
dropped it. It returns only if a phase split shows the mark and scan at
50 % or more of an overlapping garbage collection **and** a hand-fed
listed validation beats the rows form by 25 % of the whole collection.

## Edmond decides

1. The collector's chain: amends `rfc/dev/DECISIONS.md` 2026-08-27 ("the
   deferred-candidate buffer is the owner's"), Y12 clause 8, and
   `rfc/model/gc/rc-cycle.md` "The live list of a batch", "The mutator's
   disposition" and "P does not grow".
2. Form D stands on the Sage's ruling; recorded here for the plan.
3. The live-core stamps, after the three arms.
