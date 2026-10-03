# The proof-epoch collector: an algorithm description for review

The proof-epoch collector (`rfc/dev/GLOSSARY.md`) is the cycle collector
measured as `bestD` on 2026-10-03 (`dev/BENCHMARKS.md`, "S67.6"): the
default build of that day plus the feature `wait-by-readings`, built at
`a72693d` (`c52ff90` is S65.17's build, plain D). Its name is for its epoch,
which turns when the collector's work reaches twice what its proofs cost.
Edmond took it as the default build on 2026-10-03, and S67.10 folded the
feature into the code, so the default build is this collector.
Written for readers outside the project: it states the algorithm as the code
runs it and names the source of each part. The normative text is
`rfc/model/gc/rc-cycle.md`; where the two differ, the code is what was
measured. The run's data are in `dev/data/s67.6/`.

## 1. The setting

- **Per-thread reference counting.** Every heap entity (object, array,
  string, box) has an 8-byte header with a non-atomic reference count. Each
  mutator thread owns its heap blocks (64 KiB); an entity is freed by its
  owner when its count reaches zero. A slot another thread frees is pushed
  on its block's remote stack and reclaimed by the owner.
- **Cycles.** Reference counting misses garbage cycles. A decrement that
  leaves a count above zero registers the entity as a *candidate root*: a
  bit in the header and an entry in the mutator's candidate ring **R**, a
  ring of 64 KiB blocks the mutator writes and a collector reads behind it.
- **Byte 6 of the header** holds the maturation stamp: an epoch (four bits
  under this build, sixteen epochs) and an age, plus two bits that count the
  live readings of a candidate (section 7). It is written only by the holder
  of the mutator's trace token.
- **One owner validates, one collector finds.** A collector thread proposes
  what is garbage; the owning mutator validates the proposal exactly, under
  its own token, and frees. No entity is freed on the collector's reading
  alone.

## 2. The trace token

Each mutator has a one-byte token in its record. Its states:

| state | written by | meaning |
| --- | --- | --- |
| `FREE` | either | nobody traces this thread; returns of memory are made at once |
| `MUTATOR` | mutator | the mutator's own collection is running |
| `REQUESTED` | collector | a collector asks to trace; no consent yet |
| `COLLECTOR` | collector or consenting mutator | the collector traces; the mutator withholds every return of memory |
| `POSTED` | collector | a batch's verdicts and its proven set wait in P; the mutator owes a collection over P |
| `NOTHING_PROPOSED` | collector | as `POSTED`, but no verdict proposes; the mutator owes P's disposal only |
| `ASKED` | collector | cap zero: the mutator owes a collection over R whole |

The mutator consents at its next slot free or safepoint poll. While a
collector holds the token, the mutator runs on, but **every return of
memory is withheld** — slot frees, buffer chunks, whole blocks — on three
stacks threaded through the dead memory itself, because the collector may
hold an address into any of it (`cycle::deferred_slot_reuse`). The stacks
are given back when the mutator next reads the token free.

**The recall.** A mutator that needs its token (its own collection) marks
it before waiting; a withheld stack at its mark recalls without waiting.
Two levels:

- **wind-down**, at a stack's mark — deaths `M` = 8,192, chunks `M_c` = 256,
  blocks `M_b` = 16 (a large entity counted by the blocks it spans): the
  collector ends its mark, runs the scan and posts;
- **stop**, at twice a mark (`STOP_MARKS` = 2) or at a take: the collector
  stops where it stands, posts a snapshot, resets its arena and releases.

The collector reads the recall every `RECALL_STRIDE` = 1,024 positions of
storage it inspects, at every block its arena draws, and at every root.

A slot another thread freed into the mutator's block, reclaimed while a
foreign holder has the token, moves from the block's remote stack onto the
deaths' stack and counts toward `M` like the mutator's own deaths
(`dev/DECISIONS.md`, 2026-10-03).

## 3. The collector's round

Collector threads are born lazily (the first full block of R, or a
pressure collection). Mutators are divided among up to `cap` collectors.
A round visits every record and serves a mutator whose R holds at least
`SOFT_THRESHOLD` = 64 entries, or whose R has stood non-empty for
`STANDING_INTERVAL` = 4 s, or whose deferred lanes were merged back into R
since the last grant. Between rounds a collector sleeps 10 ms to 1 s
(adaptive) and is woken by a mutator's poll that filled a block of R, by a
consent, or by a pressure collection.

Before any request the collector reads, without the token, whether the
mutator has work (R's length, P's room), so an idle mutator never pays a
withholding window. Then it requests the token, waits for the consent, and
serves one batch.

## 4. The batch

The collector peeks up to K entries from R's front without consuming them
(K from 64, doubling after a batch that completed or stopped past its first
regions, halving after a stop inside them, between 1 and 1,024; clamped to
the room in the verdict ring P), and copies them into its workspace.

The trace is **trial deletion over shadow rows**, never over the heap: each
entity the trace meets gets a row in the collector's arena initialised to
its reference count, and each edge followed subtracts one from the target's
row. The collector writes no entity during the mark or the scan.

**The mark** (`cycle::mark`):

1. **All roots are met first**, before any is expanded, so an edge into a
   batch root is never a first visit.
2. **First regions.** The roots are expanded depth first with an explicit
   worklist. A registered target (one carrying the candidate bit) that is
   not a batch root, met for the first time, has its edge subtracted and is
   put on a **held stack** instead of being expanded.
3. **Passes.** When the worklist is empty, a pass over the held stack
   expands every held entry whose row reads zero — every referrer already
   met and subtracted — and repeats while a pass expands anything.
4. **Final drain.** What is still held is expanded depth first with nothing
   held.

A complete mark leaves every row as plain depth-first trial deletion would
(each met entity is expanded once, and subtraction is order-free). What the
order changes is what a stop leaves: a dead request's registered interior,
one in-edge each, reaches zero in the passes before the long-lived state
behind it is walked in the final drain.

Two edges are not followed and not subtracted: one out of the GC heap, and
one into a **stamped live target** — a target whose byte-6 stamp carries the
arena's epoch at age ≥ 1 (`TRAVERSAL_AGE_THRESHOLD`) and which this trace
has not met. Such a target is read as an opaque live external. A child that
yields no counted cell (a string, a box, an object with no counted
property) is met and subtracted but pushed nowhere.

**The scan** colours the closure from each root: a row above zero is held
from outside the traced component, and live colour spreads from it; what
remains white is potentially unreachable. Each root is posted *proposed*
(white) or *read live*. Roots no trace can place are posted first: a count
read zero is *zero-count*, a root with no row is *read live*.

**Stops.** A wind-down ends the mark and still scans and posts. A stop
posts a snapshot: a root whose met row reads zero is *proposed*; one above
zero is *read live* where the first regions had ended or the batch is one
root; every other root is *unwalked* and goes back into R untraced.

**The proven set.** A batch that proposes posts, beside its verdicts, the
set it proved unreachable (the white members' addresses and the blocks
they stand in), on GC metadata blocks handed to the mutator with the
release (`cycle::posted_set`).

**The collector's stamps.** A batch whose trace completed writes, under its
grant, the stamp `{epoch, age 1}` on the live rows of the row arrays its
final drain touched first, oldest first (`cycle::collector_stamps`). On a
web load those are mostly the long-lived state; a live request cut by a
batch boundary is walked earlier and stays unstamped, so once it dies the
next batch walks it rather than pruning at it. The walk stops at the stop
level only.

Then the collector advances R past the batch (every root has a verdict,
also on unwind), resets its arena, and releases the token to `POSTED` or
`NOTHING_PROPOSED`.

## 5. The owner's collection over P

The mutator's next free or poll reads `POSTED` and arms the **collection
over P**; the poll runs it, under the mutator's own token (`cycle::collect`,
`cycle::queue::verdicts`):

- it meets every member of the posted set, follows only the edges between
  members, scans, and frees what its scan leaves unreachable. Trial
  deletion restricted to a set is sound for any set of live entities: an
  edge not followed leaves its target's row higher, so a wrong member costs
  a refusal, never a wrong free. A listed slot whose occupant died and was
  reused is read as its new occupant, by its current count and fields: a
  live occupant is refused, and one that is garbage with the rest of the
  set is freed with it, either way soundly; a block a member stands in that
  goes back to the pool under `POSTED` drops the set whole;
- it disposes of P whole: a root whose death completed is retired; a root
  read live goes to a deferred lane (section 7); an unwalked root, and
  anything the close cannot dispose of, is written back into R.

The pressure path, the exit and an explicit collection read P first, then R,
and collect in line over R whole.

## 6. The epoch

Each mutator's epoch is kept by its collector (`cycle::epoch`). The
collector advances it at a visit when either holds:

- **by proofs**: the positions its batches inspected since the last turn
  reach `SPENT_PER_PROOF` = 2 times what the stamps they wrote cost to prove
  (2 in the measured build; 4 by default since Edmond's ruling after S67.15,
  the same day, and the embedder's to set through `ll_gc_set_epoch_ratio`).
  A completed batch prices its proof as
  `final-drain positions × stamps / final-drain rows`; work counts toward
  the turn only once a price stands;
- **by time**: X = `EPOCH_INTERVAL` = 8 s of the collector's clock, but not
  before `SPENT_PER_PROOF` times the wall of the batch that proved the epoch.

The epoch is the validity window of the collector's proofs: a stamp of an
older epoch reads as no stamp, so every turn makes the next batches walk
the state again. The collector publishes the epoch's low byte beside the
token, where the mutator's poll reads it.

## 7. Deferral with waits by readings

A root read live is not re-traced at once; it waits in a deferred lane
until enough epoch turns pass for its reading to be worth repeating.

- The mutator counts, in byte 6 bits 22–23, how many times a collection read
  this candidate live and deferred it (saturating at 3), at the deferral.
- There are three lanes, waiting **1, 3 and 7 epoch turns**. A root goes into
  the lane of its count: first live reading → 1 turn, second → 3, third and
  later → 7.
- At its poll the mutator compares the collector's epoch byte with each
  lane's mirror and splices every lane whose wait has passed back into R
  behind its tail; a turn made by X releases every lane at once.
- The stamp's epoch is sixteen wide, so the longest wait
  plus a turn of lag stays below a full cycle of the epoch, and a member's
  stamp of a root's last reading never reads current at its next.

Plain D, the build measured beside it, had one lane, re-offered at every
epoch turn.

## 8. Constants

| name | value | role |
| --- | --- | --- |
| `SOFT_THRESHOLD` | 64 | entries in R that make a mutator worth a grant |
| `INITIAL_BATCH`, `BATCH_BOUND` | 64, 1,024 | K's start and ceiling |
| `STANDING_INTERVAL` | 4 s | R non-empty below the threshold this long is served |
| fallback sleep | 10 ms – 1 s | between rounds, adaptive |
| `RECALL_STRIDE` | 1,024 positions | how often a trace reads the recall |
| `M`, `M_c`, `M_b` | 8,192 deaths, 256 chunks, 16 blocks | withheld-stack marks (wind-down) |
| `STOP_MARKS` | 2 | the stop level, as a multiple of a mark |
| `TRAVERSAL_AGE_THRESHOLD` | 1 | a stamp of the current epoch prunes |
| `SPENT_PER_PROOF` | 2 | the epoch turns when spent ≥ 2 × proving |
| X (`EPOCH_INTERVAL`) | 8 s | the epoch's turn by time |
| `LANE_WAITS` | 1, 3, 7 epoch turns | the deferred lanes' waits |
| `WARM_BLOCKS` | 16 | arena blocks kept warm; pages past them discarded at a reset |

## 9. What is claimed, and what is open

- **Soundness** rests on the owner's exact validation of the posted set
  under its own token, and on the withholding of every memory return while
  a collector holds the token. A collector's verdict alone frees nothing.
- **Progress** (`dev/plans/S67.md`, S67.9): with no budget on the trace, a
  completed batch stamps the live rows its final drain touched first, so the
  next batches prune at them until the epoch turns; garbage behind a live
  component is freed within a bounded number of turns **where a batch over
  it completes**. A garbage component whose own walk outlasts every grant,
  each one recalled inside its mark, is not freed by the collector: a stop
  posts its root read live or unwalked and keeps no continuation, and only
  the owner's collections over R whole — under pressure and at the exit —
  collect it (`dev/plans/S67.md`, Q1, open). On the web loads every best-D
  cell freed all its garbage inside a 12 s drain.
- **Open, measured** (`dev/BENCHMARKS.md`, "S65.17", plain D): a grant on
  `web-arena-40k` lasts up to 185 ms in all, and the blocks a mutator
  withholds at its release reach 129–582 against a mark of 16; under best D
  in S67.6, 438–945. The recall is read; the token is held past it by the
  collector's work after the stop — the stamps' walk after a completed
  trace, which reads the recall at the stop level alone, is the longest
  part, 105–123 ms on `web-heap` and 23–32 ms on `web-arena-40k` — and by
  a grant standing behind another
  mutator's batch, which a stopped trace does not release before its tail.
- **Open, measured** (`dev/data/s67.6/cells.csv`,
  `verdict_collection_longest_us`): the owner's collection over one posted
  set is one pause at one poll, 124–215 ms on `web-heap` over sets of up to
  410k members. Neither K nor the recall bounds it.
- **Open, latent**: the passes over held entries read N(N+1)/2 entries on a
  chain of N registered targets met in an order unrelated to the chain; on
  the web loads 0.9–1.6 passes a batch.
- **Open, measured**: on the arena loads the epoch turns mostly by X, which
  releases every lane, so there the waits by readings act little.

## 10. How it measured (S67.6, summary)

The collector is the arm *best D*. Four arms — D, HG, best D, best HG — on two mutators and one collector of a
four-core box without hardware counters, three web loads, five repeats,
116 s with a 20 s warm-up and a 12 s drain. Best D against plain D: no CPU
difference resolved (+0.4 %, −0.1 %, +2.9 % against tolerances of 9–17.5 %);
garbage left at the
drain's end in no cell against six of fifteen for D; on `web-heap` 71 MB of
garbage on the mean against 461 MB, and 345 against 750 MB at the peak, for
+6 to +11 % of collector CPU (to the loop's stop, the warm-up included).
Best HG ties best D on CPU and is dearer on the
collector in every `web-heap` repeat. Full figures, the protocol's
deviations and the gates: `dev/BENCHMARKS.md`, "S67.6"; data:
`dev/data/s67.6/README.md`.

## 11. Where each part lives

| part | source |
| --- | --- |
| token, consent, recall | `src/cycle/token.rs` |
| withheld returns and their marks | `src/cycle/deferred_slot_reuse.rs` |
| collector thread, round, batch, epoch clock | `src/cycle/worker.rs` |
| mark: held stack, passes, final drain, prune | `src/cycle/mark.rs` |
| scan and colours | `src/cycle/scan.rs` |
| shadow rows and the arena | `src/cycle/arena.rs` |
| collector's stamps | `src/cycle/collector_stamps.rs` |
| posted set | `src/cycle/posted_set.rs` |
| owner's collection over P | `src/cycle/collect.rs`, `src/cycle/queue/verdicts.rs` |
| R, deferred lanes, re-offer | `src/cycle/queue.rs` |
| epoch turn | `src/cycle/epoch.rs` |
| the model | `rfc/model/gc/rc-cycle.md` |
