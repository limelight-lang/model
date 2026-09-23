# Critic on the lane amendment (`CYCLE-SPLIT-PACKAGE-3-LANE.md`)

Date: 2026-09-23. Read-only review; no test was run. Saved verbatim from the
Critic's hand-back. The model's disposition of the findings is recorded in
`PLAN.md`, S65.3.

**Summary**
1. The diagnosis is right. After `splice_after_tail` the front block is no longer the tail block, so `holds_at_least` answers yes at any threshold (ring.rs 401–417, 785). Under version 3 an unpacked merged lane is served next round as a threshold batch and doubles K.
2. The same happens in shipped code today, as a low-severity bug. If the poll that splices also consents to a standing request, it returns before the AllRoots fire (gc.rs 265–275, 292–295). The grant then batches an unpacked spliced R and doubles K.
3. F1 (highest): `lane_expected` is set only on `ReadLive`. Roots the mutator's exact collection over P reads live (`Unwalked` and `Proposed`) also go to the lane, through `VERDICT_DEFER_MARK` (compaction.rs 183–184). So at commit 1a no marker is written for the over-budget take, and the packed lane waits 4 s. The byte is also cleared by the very advance that comes before its own lane is filled.
4. F2: the rejected "hint in ring state" can be built cheaply as a merge counter on R's writer line. The splice already stores `r_tail_block` on that line, and the round already loads it. That replaces the marker, the byte and the store outside the hold, and closes the slot-free hole.
5. F3: the exact count stops K doubling only for lanes under 64. The ratchet itself is `size_the_next_batch` doubling K after a batch that took fewer roots than K, which is shipped behaviour. "Double only when the batch filled its clamp" fixes every shape and makes the walk unnecessary for K.
6. F4: the ordering promise also fails at polls with a closed gate: they consent but do not merge (gc.rs 253, 265, 275). Both holes recur on every wake of a sleeper, but neither is worse than version 3.
7. F5: red test 4 is already green on commit 1. Tests 1 and 4 exercise the test helper, not the production advance. Test 3 depends on no collector being alive. The existing `>` restamp assertions pass vacuously with `u64::MAX`.
8. F6: `signal_due` on the merge feeds the lost-wake loop. "On the poll — nothing" is false for up to one elder round after a sibling ends.
9. F7: a take over a three-block ring reads a false backlog (peek covers two blocks; the backlog reading is still the front block's).
10. Survived: the `OVERDUE` arithmetic, the single writer of `lane_expected`, `Standing` and the handover, 8,135 entries per block, and the S60.6 and 15,365 / 658,832 figures.

## Shipped-code verdict on point 1

The effect is real. The shipped poll normally hides it: the same poll arms
AllRoots and fires, and the collection's close compaction packs R. The
exception is when `read_and_act_on_this_thread` consents to a `REQUESTED` byte
in that poll: it answers `Reading::Collector`, and the poll returns at gc.rs
292–295 without firing. `answer_a_turnover_request` skips only on `COLLECTOR`
(queue.rs 1243), so a byte at `REQUESTED` lets the jump and the splice run
first.

Scenario: a mutator sleeps with 5 entries of drip in R; the round's take
request stands (`Unanswered`); after X, `ask_for_a_turnover_if_quiet`
(worker.rs 1034) requests a turnover. The mutator wakes, and its first
contact is a poll, which splices a 3-root lane, arms, and consents. The
checkpoint's `batch` reads `has_at_least(64)` as true (front ≠ tail) and
clamps to K; the trace completes, and `size_the_next_batch` sets K from 64 to
128 on an 8-root batch (worker.rs 1881–1943). A shipped-code bug, low
severity: one K step per such wake. Confidence high on the mechanism; the
frequency is not established.

## F1. `lane_expected` does not fire for the lanes the marker exists for

Claim: §4, "Ставится в `post_the_verdicts`, когда хотя бы один вердикт
batch'а — `ReadLive`". Confidence: high.

At commit 1a (before commits 3, 7 and 8): a drip thread's take of 63 disjoint
live roots meets the 8-block budget, so all 63 are posted `Unwalked` and
`lane_expected` stays 0; the mutator's collection over P traces them exactly,
finds them live, marks and defers them into the lane (compaction.rs 183–184,
`is_batch_root && VERDICT_DEFER_MARK`); the next advance writes no marker;
the merge poll finds `POSTED` from the next take and packs R; the 63 roots
wait 4 s — exactly the case the marker was built for. After commit 8,
`Proposed` roots the exact validation reads live take the same path; the
amendment's "what the byte doesn't see" names only the mutator's own
collections.

Second hole, a timing mismatch: the advance runs right after `serve` in
`read_one_record`, so a `ReadLive` posted in the round of the advance is
posted before it, and the byte is cleared by that same advance. The close
defers those roots afterwards, with the new cell as mirror, so they merge one
advance later, when the byte is 0 unless another `ReadLive` came in between.
On the `N_b` path this is always the case (the 64th batch and the advance
share a round). Better design: F2, which predicts nothing.

## F2. The rejected ring hint is the better mechanism

Claim: §7, "слово на линии R писал бы мутатор".

The splice already writes R's writer line — `self.0.tail_block.store(last,
Release)` (ring.rs 416), which is `writer.r_tail_block` — and the round
already loads that line on every visit (`front_block_reading`, ring.rs 721).
The amendment itself accepts a mutator store at the merge (`signal_due`); the
mutator-performance rule is met as well by one relaxed store to a line the
splice has just dirtied, once per X. Version 3 frees the 8-byte `commits` word
on that line.

Design: a `merges` counter on the writer line, bumped in
`reoffer_deferred_candidates` beside the splice; `merges_seen` on the hold
line; the round reads `merges` beside the tail block, and if it moved and the
ring stands below the threshold the answer is `Takes`, before any subtraction;
`ReleaseOnDrop` sets `merges_seen` to the value read under the token before
the peek. It removes `OVERDUE`, `lane_expected` and the change to
`post_the_verdicts`, the store outside the hold and its re-issue race (§3),
F1 entirely (lanes filled under pressure included), and the slot-free and
closed-gate holes (F4): a drip take records the old count, the later merge
moves it, and the next round takes the lane. Confidence: high that it is
sound under the same exclusion argument; not built.

## F3. The exact count fixes one shape of the K ratchet; the ratchet itself is shipped

Claim: §4, "форма batch'а по счёту"; §8, "(г) `batch_size()` … — 64".
Confidence: high.

`size_the_next_batch(mutator, clamp, …)` doubles the clamp, not the roots
taken (worker.rs 1942, 1994). A thread's live lane of 200 roots is still a
threshold batch after the exact count: first turnover, batches of 64 and 128,
K becomes 256; next, clamp 256, 200 taken, K 512; then 1,024. A thread steady
at about 64 entries reaches 1,024 in four batches the same way. Better: double
only when `roots.len() == clamp` — one comparison; the spliced 3-root lane
then leaves K alone in either form, and the bounded walk (with its non-cfg
`unread` variant) is not needed for K. A policy change on the threshold path:
it does not reopen "a take feeds K neither way", but it needs the owner's
word.

## F4. "Every later grant gets consent at a poll that merges first" is not what the code does

Claim: §4, "Порядок событий". Confidence: high on the mechanism; frequency
unmeasured.

gc.rs 265 merges only under `open`, while 275 reads and consents "whatever the
gate". A poll inside a destructor (`teardown_depth != 0`) or an open reset
window consents without merging, as a slot free does. The hole costs the
version-3 latency (drip take, `POSTED`, the merge poll packs, 4 s) and recurs
on every sleep and wake that spans an advance if the thread's first contact is
a free or a closed-gate poll; it reaches running threads too, since rounds
start on any mutator's block-fill wake. The proposed refinement
("`ReleaseOnDrop` leaves `OVERDUE` when the tail block did not move") brings
back the refused form ("an instant left standing across a take"), reads a
tail that also moves on an ordinary block fill (`Pushed::IntoNextBlock`), and
is exposed to ABA on a recycled block. F2 closes the hole.

## F5. Commit 1a's red tests

Confidence: high.
- Test 4 is green on the commit-1 tree (no marker exists, so
  `standing_since() == 0` and `Idle` hold trivially): a guard, not a red test,
  and it passes on a 1a tree that never writes a marker.
- Tests 1 and 4 advance through `turn_the_cell_of` "extended by the marker":
  if the helper writes `OVERDUE`, no test covers the production write in
  `read_one_record`, and the criterion can be met without running that code.
  Drive one test through a real round (`read_one_record` with a short quiet
  interval).
- Test 3's `signal_is_due()` is true only if `wake` answers false (queue.rs
  457–458): a live collector on the record's slot clears the flag and turns
  the test red after 1a; with no elder, the case's poll births one
  (`ensure_thread`, 463).
- Existing assertions `record.standing_since() > before_the_take`
  (the_take_after_an_interval.rs 125, 389) pass when `OVERDUE = u64::MAX` is
  left in place; add `!= OVERDUE`.
- Test 1's "today: the word equals now" is imprecise: below the threshold
  with a non-zero instant the branch leaves the grant-end stamp.
- Commit 3 replaces `post_the_verdicts` with per-part posts and
  `FinishThePosts`, and its invariants do not carry the setter; test 1 would
  catch the loss.

## F6. `signal_due` on the merge feeds the known lost-wake loop

Claim: §6, "Мутатору: … на poll'е и на free — ничего". Confidence: high on
the mechanism, low on the frequency.

A sibling advances a cell and then ends after 8 idle rounds; the mutator's
merge poll wakes the dead slot, `wake` answers false, and the flag stands
because the slot is not `ELDER`. Every poll then pays the mutex, `notify_all`
and a load until the elder's round renames the record (at most
`FALLBACK_INTERVAL_MAX`, 1 s); with the elder's slot empty, each poll also
pays `ensure_thread` (115–240 ns). Bench (в) cannot see this, not exercising
the merge. The loop already exists for block fills; the amendment adds a
trigger. The timer alternative has no mutator cost: it bounds merge-to-round
at 1 s, today's maximum, but pins a many-thread collector near 10 ms rounds
when advances are staggered.

## F7. A take can report a backlog

Confidence: medium; the shape is rare. The front block F is read out, T
holds the drip, the lane L is spliced after it; the exact count says take;
`peek` reads at most two blocks, F and T, and commit moves the front to T,
which is not the tail block, so `has_at_least(64)` answers true and a
sub-threshold take votes for a sibling (worker.rs 1937, 1039–1041). The
design says a take reads no backlog: read the backlog by the same exact count.

## Figures

- 8,135 = (65,280 − 192) / 8 − 1: correct.
- S60.6's 8.13–8.46 ns and the 15,365 / 658,832 instructions match
  BENCHMARKS.md (168–169, 512–513). "39 bytes of 64" is plausible.
- "K = 1,024 after four turnovers" is arithmetically right, but under version
  3 needs every merge unpacked (a `POSTED` merge packs) and a batch finding
  that many entries.
- "At X = 8 s and interval 4 s, no extra takes (8 / 4 is whole)" is not
  established: takes and advances fall on round boundaries, and the round
  runs `serve` before the advance, so a coincident interval take is followed
  by a marker take a round later. A run decides.
- The 200-root case (64, then 128, then the last 8 wait the interval) is right
  for d = 0.

## What I tried to break and could not

- `OVERDUE` is read before `saturating_sub`, and `serve_clock_now` cannot
  reach `u64::MAX`; `room == 0` and `Takes && is_standing()` keep the marker.
- `Standing` never reads the word; the handover and the elder's reclaim carry
  it.
- `lane_expected` has a single writer apart from the named re-issue race,
  avoidable with a second `take_for_reading` around the advance (a CAS once
  per X).
- A free record and an exit leave nothing that means something else after
  `take_record`.
- The walk under the token is safe although version 3 lets the poll splice
  under `COLLECTOR`: the ring only grows under a grant. §4's "under the token
  the chain stands" is not literal, but the walk holds.
- R always has a block while the lane is occupied (`dismantle` runs only at
  exit; `Packing::drop` never nulls the tail block).
