# Critic 1 on the Sage's report (`CYCLE-SPLIT-SAGE-REPORT.md`)

Date: 2026-09-23. Read-only review; no test was run. The subject is
`dev/CYCLE-SPLIT-SAGE-REPORT.md` (line numbers below are that file's). Saved
verbatim from the Critic's hand-back; not yet answered — the Sage's answers
go to the synthesis.

**Twelve-line summary**
1. Most severe: D2's "one line" (`is_root` false for `Unwalked`) produces false out-of-memory — the same predicate builds pressure, exit and explicit batches, so a pressure collection that begins while P holds `Unwalked` entries traces none of them and the one allocation retry fails; D1a's hint makes that a meeting every time.
2. F1 + D2 in-order write-back blocks everything behind an oversize root: it heads every batch it is in, so every prefix is empty; the Sage's own lemma licenses the better redo (the batch minus that root).
3. The hold-line bit is mutator-wide and none of its clearing events can happen on its own path, so the collector stops serving the thread permanently; the design then leans on R2, which breaches line (3) more broadly than the `Unwalked` walk D2 removes.
4. False premise: every collection, empty or `NothingProposed` included, closes a commit — stamps turn over every 64 collections, not per X.
5. Shipped bug from the same premise: the quiet-thread ask never fires on a batched thread, and the test guarding it simulates the batches.
6. The Sage and §2.4 miss that the turnover already fires an in-line collection over R whole, so "R grows until pressure" and "standstill until D3" are wrong for any thread with deferred roots, and shipped code already breaches line (3) every turnover.
7. F1's redo can fail with the graph unchanged — the first attempt never ran the prefix's scan, and a budget met in the scan phase has no root j.
8. D1a's "the waiter waits one block of rows" is false (the budget bounds memory, not time); the shipped comment makes the same claim; the hint misses the exit's and the refused-in-teardown claims.
9. R2 counts polls, not blocks filled, whenever a wake is lost (sibling ending/starting); its 64-byte line is full and statically asserted.
10. F1's mutator saving was priced on the all-live arm only; on dead rings it saves nothing; several smaller figures are off (finding 9).
11. Survived: the lemma PU_S ⊆ PU_all (no counterexample graph), F1's re-peek, the mid-grant reset, the candidate-bit claims, the age-1 stamp, the "shift" refutation for live roots.
12. Survived also: D1d is sound but gains almost nothing; the D1a hint is not a race and does not reopen E10.

Findings are ranked by severity; each names the claim, a failing scenario, the
code line and a confidence.

## 1. D2's "one-line" change strips `Unwalked` from the pressure path and makes it throw a false out-of-memory

**Claim attacked.** sage-1.md line 65: "the change is one line of
`Verdict::is_root`"; line 83 (R1): "Under D2 the mutator's pressure collection
over R whole then reaches the written-back roots".

**Mechanism.** `Batch::walk_roots` selects P's roots through
`verdicts::is_batch_root` for every batch kind (queue.rs:919), not only the
collection over P: the pressure path, the exit's rounds and the explicit fire
all `read_batch`. The pressure path never hands P back to R during its loop:
`close_and_take_batch` disposes of nothing and `restore_batch` drops the batch
in place. P is disposed of only in `CollectingThread`'s drop, through
`retire_candidates_and_dispose_of_verdicts` (collect.rs:303), after every trace
round. `entity_alloc_under_pressure` runs one collection and one retry; the
retry's null becomes memory-exhausted (heap.rs:2599-2601).

**Failing scenario.** Thread T holds 63 dead disjoint rings standing, and the
collector's take of them meets the budget (with D1a, the collector's next
`grow` sees T raise its collecting word and abandons; without D1a, whenever the
batch in flight was going to meet its budget). T's allocation is refused;
`collect_under_pressure` raises the collecting word; `HeldToken::take` waits
out COLLECTOR; the collector releases to POSTED; the take proceeds from POSTED
(token.rs:373). The batch is 63 verdicts and nothing else in R; `walk_roots`
yields no root; the harvest is empty; zero is freed; the drop writes the 63
back into R; the retry returns null. The program gets memory-exhausted with 315
garbage entities standing registered. Today `Unwalked` is a root there and the
same pressure trace reaches all 315.

**Better design.** "Root" parameterised by batch kind: `Unwalked` stays a root
in whole-R batches (pressure, exit, explicit fire, turnover) and stops being
one only in the collection over P; the same parameter must reach
`mark_for_deferral` (queue.rs:993) and `dispose_verdicts`'s "deferrable" test
(compaction.rs:184). Not one line.

**Confidence:** high.

## 2. F1's prefix cut plus D2's in-order write-back parks every batch behind one oversize root

**Claim attacked.** Line 70: "with F1 that is the right order (the ones that
fit are judged, the leftovers are re-written)"; line 63 (F1).

**Mechanism.** F1 judges only r₁..r_{j−1}; D2 writes `Unwalked` back in P's
order through `append_entry` (compaction.rs:196), so the offending root goes
back ahead of its batch-mates. Nothing about the order reflects "which fit".

**Failing scenario.** Root A's closure alone exceeds the budget, and A is R's
oldest. Threshold path: batch [A, B₁..B₆₃] → prefix 0, nothing judged, all 64
written back as A, B₁..B₆₃, F1 sets K = max(0,1) = 1. Each K=1 batch then
judges only what was registered in the last window until A is at the front
again; K=1 on A fails and sets the hold-line bit (finding 3). The B roots fit,
and are judged only while A is not at the head.

**Better design.** The Sage's lemma holds for any subset S (line 59), so it
licenses the redo S = batch ∖ {r_j}: post `Unwalked` for r_j alone, allow at
most one or two such exclusions per grant, and write the excluded root back
last.

**Confidence:** high.

## 3. The hold-line bit is mutator-wide and its clearing events cannot occur on the path it creates

**Claim attacked.** Line 71: set on the smallest span meeting the budget, read
by `serve` "where it turns the take and the K=1 batch into `Served::Idle`",
"cleared by any batch that completes or when the mutator's note of a freeing
disposition arrives"; lines 114 and 116.

**Mechanism.** While the bit stands, K=1 and the take are both Idle, so no
batch runs and none completes; K never doubles either, since
`size_the_next_batch` runs only inside `batch` (worker.rs:1941-1943). The
freeing-disposition note is written only in `ll_gc_maybe_collect`
(gc.rs:317-318); `collect_under_pressure` never writes it, so even a pressure
collection that frees everything leaves the bit standing. No batch means no
POSTED, so nothing arms the collection over P. Under D2 the mutator never reads
the roots live, so the deferred lane stays empty, and with an empty lane the
poll ignores the turnover request (gc.rs:265).

**Failing scenario.** R drains down to A (live, oversize closure); the K=1
batch on A sets the bit. T keeps registering small garbage rings that would
each fit. The collector now serves T nothing, R grows without bound, and only
pressure or exit collects; after pressure the bit still stands. One root
disables the collector for the whole thread, for roots it never tried. The
Sage's only exit is R2, which collects R whole in line with no shortage
(line 116) — a wider breach of line (3) than the `Unwalked` walk D2 was
introduced to remove.

**Better design.** Per-root failure rather than per-mutator: exclude the
offending root (finding 2) and give it an attempt count in the spare low bits
of its P entry or R; K and service continue for the others.

**Confidence:** high (follows from the Sage's text and the note's write sites).

## 4. Every collection closes a commit, `NothingProposed` and empty ones included — with a shipped bug

**Claim attacked.** Line 108: "A `NothingProposed` collection commits nothing,
so the epoch does not turn and the stamps stand until the S60 ask turns it
after X" (and the owner's document §2.4, "one full trace per side per X").

**Mechanism.** `collection()` calls `commit` unconditionally (collect.rs:453);
the commit's `Revalidation::close` calls `epoch::commit_closed`
(finalization.rs:610-622, "A trace that proposed nothing still opens and closes
a finalization and counts"; epoch.rs:83-85). This holds for a collection over P
with no roots too: `Membership::rows` over an empty touched list gives a
zero-member membership. It is measured as well: BENCHMARKS.md:179 has the
`overlapping-live` take arm end `NothingProposed` with 0 roots, an ending
assigned only after `commit` ran.

**Consequences for the design.** Stamps turn over every 64 collections
(`COMMITS_PER_EPOCH`, epoch.rs:44), not per X: a thread batched at the 10 ms
minimum re-traces its core un-pruned on both sides about every 0.64 s, not
every 8 s. D2's retake loop turns the epoch by itself: each all-`Unwalked`
collection over P is a commit, so the loop strips maturity from the heap and
makes the next budget miss likelier.

**Shipped bug.** The amended 2026-09-22 ruling (DECISIONS.md:423-431) and
worker.rs:72-74 rest on "the collection over them closes with no commit", so
`ask_for_a_turnover_if_quiet` restamps whenever `clock_stood_since_the_stamp`
is false (worker.rs:491; mutator_record.rs:610-612). Any batch served between
two visits, all-live ones included, moves the clock, so such a thread is never
asked, and its lane waits 64 commits — about 4.3 minutes at one take per 4 s,
against X = 8 s, exactly what the amendment set out to prevent. The test that
guards the amendment, `a_thread_batched_all_live_oftener_than_x_is_asked_after_x`
(the_quiet_thread.rs:91-115), never runs a collection over P: it simulates the
batches by calling `ask_the_quiet_thread` alone, so it agrees with the premise
by construction.

**Better design.** Either leave empty commits uncounted, or have the restamp
read the number of freeing commits rather than the raw clock.

**Confidence:** high on the mechanism; the 64-commit cadence is arithmetic, not
a reading.

## 5. The turnover already fires an in-line collection over R whole, and the worst case omits it

**Claims attacked.** Line 112: "R grows without bound … right; the end is a
refused allocation … right"; line 114, "a standstill … until D3"; line 116,
"R2 … is what ends the standstill on a producing thread"; lines 53 and 55,
which use line (3) as D2's reason.

**Mechanism.** At the poll, gate open and deferred lane occupied,
`reoffer_deferred_if_epoch_moved` is followed by `arm()` (gc.rs:265-269);
`arm()` is `Arming::AllRoots` (gc.rs:85-87), which the same poll fires as
`ll_gc_collect_cycles` (gc.rs:310): the mutator collects R whole, in line, with
no shortage.

**Failing scenario (for the claim).** The candidate-core spiral at K=1 in
shipped code: every root the collection over P reads live is deferred, so the
lane is occupied. After 64 commits — at most 64 batches, by finding 4 — the
turnover re-offers the lane and collects R whole in line, one trace of the
core with sharing. R is drained once per epoch, not left to grow until
pressure. Under D2 with an occupied lane, the S60 ask plus this re-offer end
the standstill every X without R2.

**Two readings of line (3).** (a) The mutator never traces roots nobody
judged — D2's reading. (b) The mutator never collects of its own accord outside
shortage. The shipped turnover collection breaches both, and it is the
accepted floor the Sage's accounting leans on ("the X collection over R whole",
line 55). D2 cannot be justified by line (3) while this collection stands; the
owner has to say which reading he means.

**Confidence:** high on the code; the premise question is the owner's.

## 6. F1's redo can fail with the graph unchanged

**Claim attacked.** Line 63: "the redo can meet the budget again only if the
graph grew under the grant".

**Mechanism.** The worker's `trace` marks every root before it scans any
(worker.rs:2007-2023), so the first attempt that failed at root j never ran the
prefix's scan. The scan draws worklist segments through `push_onto` → `alloc` →
`grow` (arena.rs:999-1014, 1108-1112), and it re-pushes an entity whose colour
changes from potentially-unreachable to Live (scan.rs:179-194), so its stack
can grow deeper than anything the mark kept.

**Failing scenario.** A hub H of count 0 points to 600 members; one member m
points to L, which is held from outside; L points back to the other 599
members. Mark: the deepest worklist is about 601 entries, 3 segments of 256.
Scan: 600 members are pushed as potentially unreachable, L is found live, and
599 are pushed again as Live — about 1,198 entries, 5 segments. If the
prefix's rows leave less than two segments (about 8 KiB) under the budget, the
redo fails with the heap unchanged. A second case needs no special graph: when
the budget is met inside the scan phase, every mark has already fitted and
there is no "root j" to cut at; read literally, the prefix is the whole batch,
and the redo repeats the same trace and fails again.

**Moved ambiguity.** Whether a failed redo counts as "prefix empty" for the
hold-line bit (line 71): one reading sets the bit and stops service to the
mutator, the other only halves K.

**Confidence:** medium-high on the mechanism; the segment counts are my
arithmetic.

## 7. D1a's time bound is false, and the shipped comment says the same

**Claims attacked.** Line 41: "The bound the waiter gets is one block's rows"
and "a late-seen waiter costs one more block of trace"; line 127.

**Mechanism.** `grow` is the only reading site, and it runs only when the bump
runs out (arena.rs:545, 1108). Work between two `grow` calls is bounded by
edges, not rows: a met array of 10⁷ cells naming a few already-met targets
costs 10⁷ subtractions and no draw; the scan draws only worklist segments, so a
waiter who arrives during the scan is almost never seen.

**Shipped.** worker.rs:50-51, "What a mutator waits for when it needs its token
is one batch's trace, bounded by the blocks rather than by the roots" — the
same failing scenario: one huge array root holds the token, with every death
withheld, for the whole walk, drawing no block.

**Coverage gaps.** The exit's claim (collect.rs:652) and the refused-in-teardown
retirement, a memory-shortage path (`take_or_hold_posted`, collect.rs:745),
take the token without raising the collecting word; the hint cannot see them.

**What survived.** The word is an `AtomicBool` read `Relaxed`
(mutator_record.rs:241, 638-639): no data race. It is a hint and not an
exclusion, so it does not reopen E10.

**Confidence:** high on the absence of a bound; 10⁷ is a construction, not a
reading.

## 8. R2 counts polls on a lost wake, and its control line is full

**Claim attacked.** Line 85: a counter "incremented on the poll's signal branch
only … once per 8,152 registrations".

**Mechanism.** `signal_the_collector_if_due` clears `signal_due` only when
`wake` returns true (queue.rs:456-464); `wake` returns `is_alive`
(worker.rs:595-603), false for a slot STARTING, ENDING or UNBORN. For a sibling
slot the flag simply stands, so the branch runs at every poll until the elder's
next round renames the record — up to `FALLBACK_INTERVAL_MAX`, one second.

**Failing scenario.** The elder ends sibling 2 while T is still named to it;
T's next N polls take the branch, and within microseconds R2 collects R whole
in line, although a collector serves T within one round.

**Fix.** Count on the false-to-true transition of `signal_due`, on the growth
path, not on the poll's branch.

**Layout.** `MutatorCycleState` is 64 of 64 bytes (queue.rs:225-276), asserted
exactly 64 (queue.rs:284); growing it moves `OVERFLOW_CAPACITY` and
`POLL_STRIDE`. The counter has to be packed — for example `signal_due` as a
byte with a flag bit and a 7-bit count. The block figure is also wrong: a ring
block holds 8,135 entries (`BLOCK_ENTRIES`, ring.rs:70-73); 8,152 is the
overflow buffer.

**Confidence:** high.

## 9. Figures and smaller false claims

**F1 pricing uses the live arm (line 63).** "8 × 45,000 = 360,000 against
2,872,618" is the disjoint-live baseline (BENCHMARKS.md:171-172), while W1 is
stated "live or dead". On the dead disjoint shape (8,908,260 instructions,
BENCHMARKS.md:166) the prefix's roots come back `Proposed` and are traced
again (W2), so F1 without D2 saves the mutator nothing on garbage; the saving
is proportional to the prefix's live share.

**Smaller errors.**
- W3 (line 17) quotes "+3.4 % instructions" from BENCHMARKS.md:352, which
  lines 346-350 attribute to "the arm's own residue, not the take", and pairs
  it with L1D misses from a different table (lines 50-51).
- Per-row cost (line 41): "60–95 ns per row" should be 20.6/381 to 36.1/381,
  54–95 ns, and it is a whole take's wall divided by rows, fixed costs
  included.
- Arena block capacity (line 41): "a block of 16,000 dense rows" — a 64 KiB
  arena block holds three 16,408-byte arrays, 12,240 reserved rows, and
  reserved rows are not met rows.
- Rows under the budget (line 110): "about 128,000 dense rows" — at class 16
  the 579,200-byte budget holds about 35 arrays, about 142,800 rows.
- Recall bound (line 59): "when {B, C}'s teardown releases B → A and A's ring
  is re-registered by that decrement" is false: `CANDIDATE_GATE_MASK` includes
  `CANDIDATE_BIT` (refcount.rs:489, 494-498), so A, standing in the lane, is
  not registered again. The only bound is the turnover.
- The letter X means a count in the owner's line (2) and a time in the Sage's
  text ("per X") and in worker.rs:63. Read literally, line (2) forbids the
  sub-threshold take D2's retake loop is built on; that take was ruled
  separately, so this is a tension to name, not a proven defect.

**Confidence:** high on each.

## Claims I tried to break and could not

**The lemma PU_S ⊆ PU_all, for a single snapshot.** The prune at a mature
non-candidate target and an edge out of the GC heap are the same predicate
under S and under the full batch; a zero-count root is expanded by nothing in
both (mark.rs:333); saturation is sticky (shadow.rs:188-190), and clamping at
zero keeps row_S ≥ row_all; the reachability step holds because z → w adds to
rc(w) and is not subtracted under S. Under concurrent mutation the lemma
compares against a trace that never happened, so it says nothing there; safety
does not rest on it but on the owner's exact validation. No counterexample
graph.

**F1's mechanics.** The re-peek returns the same entries: nothing moves R's
front under the grant, and `unlink_after_tail` refuses the front block and any
block holding an entry. The mid-grant reset is safe for the free path: under a
foreign holder every death is withheld without reading a stamp
(deferred_slot_reuse.rs:976-997), and the arena's blocks are the collector's
own. Posting the prefix's verdicts plus `Unwalked` and advancing the whole
batch fits P's clamp.

**The worst-case correction.** A candidate is never pruned
(mark.rs:453-457), and its bit is cleared only in `compaction::free` at death.
The first stamp reaches age 1 even on `NothingProposed` (collect.rs:1302;
maturation.rs:394, 402).

**The rest.** `dispose_verdicts` retires completed deaths before writing
anything back (compaction.rs:170-176). "`FREE` promises an empty P" survives
D2. The "shift" refutation survives for live roots (gc.rs:265 needs an
occupied lane); for dead roots on a thread with a lane, the ruling's "shift"
does hold. D1d is sound but gains almost nothing: a zero-count root is already
expanded by nothing, and completed deaths are retired whatever their verdict.
Quoted benchmark lines 186-187, 319, 171-172, 203, 62-63, 106, 84, 168, 207,
211-212, 412-413 and 426-427 all match. The constant arithmetic 2,080, 16,408
and 579,200 is correct.
