# Critic on the package, version 2 (`CYCLE-SPLIT-PACKAGE-2.md`)

Date: 2026-09-23. Read-only review; no test was run. Line numbers are version
2's unless another file is named. Saved verbatim from the Critic's hand-back;
not yet answered.

**Summary**
1. Most severe: `OVERSIZE` in P's bit 3 is not a free bit. Promoted survivors in retained blocks are eight-aligned candidates in production, and fixtures are eight-aligned in tests; widening P's mask to bit 3 points every verdict for such an entity 8 bytes too low.
2. The recall bound "N edges plus one reset" holds only for the mark. The scan's per-edge path never reaches `visit_child` and seldom reaches `grow`, and hooked `OutsideCells` walks cannot break. Finding 11(b) stays open.
3. B and B_max failures are paid once per registered root, not once per component: a failed part judges nothing, so each of 63 roots of one oversize component fails at B and again at B_max, every X.
4. The gate keyed on the list word has two costs: the teardown's pressure refusal can no longer return any withheld block (a regression against shipped code), and because the gate sits in `put` it also withholds arena resets and raw-heap and buffer blocks no list can name.
5. Commit 1's red test cannot turn green: `arm()` on the merge stays until commit 7 and re-defers the live roots inside the same poll, so `deferred_count()` reads 63 on both trees. Commit 1 also removes the instrument of every turnover test.
6. The list has no bound: one chain per grant holds 8 bytes per live row, outside B, and the mutator's stamp at the take is a pause that grows with it, on the path where a refused allocation waits.
7. Two readings of the gate: editing `under_a_foreign_holder`, as §5 literally says, also withholds chunks under POSTED and brings finding 9 back in the chunk drain.
8. The parking mark is never cleared, so for the rest of the root's life it matches the cell once every four epochs, not once.
9. Figure: 128 `put`s take about 120 pool locks, not 30, because `push_global` locks once per block; the previous Critic's correction was itself wrong, and v1's figure was close.
10. Previous findings: closed 2, 3, 5, 6, 8, 10; closed on correctness with a new cost 1 and 9; not closed 11(a) and 11(b); 4 sound except commit 1's test; 7 closed per root, cost understated.
11. Held up: the stamp precedes every return on every path, exit included; the run arm; the watermark reset; the `judged` bit; byte 6's read-modify-write; the single-writer turnover byte; the parts lemma; the cap-0 arm.
12. Shipped code: `queue.rs` 31–35 and 1010–1012 say an entry's low four bits are clear. That is false for promoted survivors (eight-aligned), but nothing shipped writes bit 3.

## 1. `OVERSIZE` in bit 3 names the wrong entity for every eight-aligned candidate

**Claim attacked.** §2 item 7, line 25: "бит 3 записи P, свободный: биты 0–1
вердикт, бит 2 — метка мутатора, `verdicts.rs`"; also line 130, commit 5 at
line 161, §10 at line 176.

**Mechanism.** The file v2 cites says the opposite. `verdicts.rs` 103–111
calls bit 2 "the third of the three bits a fixture's eight-byte header
alignment frees"; `LOW_BITS` is `0b111`, and `verdict_entity` (135–137) masks
exactly those bits. `queue.rs` 1018–1023 keeps `ENTRY_MARK_BITS` at bit 0
alone because "a fixture's header stands on any eight-byte boundary, and a
mask over bits nothing writes would fold two such headers into one".
Production has an eight-aligned population too: an arena entity is
eight-aligned (`memory/arena.rs` 20–25, 59–65); promotion rewrites its
category to GcHeap in place inside a retained block (`promote.rs` 32);
`CANDIDATE_GATE_MASK` (`refcount.rs` 494–498) then admits it as a candidate;
`stdapi::points_to_gc_entity` (89–92) treats a retained survivor as an
entity. `OVERSIZE` on an ordinary sixteen-aligned slot forces `LOW_BITS` to
widen to `0b1111`, or `verdict_entity` returns the address plus 8.

**Failing scenario.** A promoted survivor at `…7f0048` is registered and live;
its part completes and the collector posts ReadLive, entry `…7f0049`. With
the mask widened, every P entry for it reads `…7f0040`: the disposition
retires, defers or parks the wrong entity, and a park writes byte `…7f0046`,
inside the neighbouring survivor. With the mask narrow, the entity is read
correctly, but bit 3 of its own address reads as `OVERSIZE`: it is parked
although it fitted, and a real `OVERSIZE` on it cannot be told from its
address. Eight-aligned test fixtures meet the first reading in every verdict
case.

**Better design.** Keep P at three low bits and carry the parked roots as a
second section of the list chain, which the mutator already reads at the
take-stamp. Or make every candidate population sixteen-aligned, arena bump and
fixtures included, and state it in `queue.rs`.

**Confidence.** High on the bits (read); medium-high that a promoted survivor
reaches R in production — the gate and the in-place rewrite are read, no test
registers one.

## 2. The recall bound does not cover the scan or hooked cells

**Claim attacked.** §2 item 11(b), line 33, and §6, line 102: the flag is read
"в `visit_child` раз в N рёбер…, в `grow`, между частями и перед записью
списка", latency "N рёбер плюс сброс арены"; commit 3, line 157.

**Mechanism.** `visit_child` is the mark's (`mark.rs` 396). The scan's
per-edge callee is `classify_and_schedule_entity` (`scan.rs` 131–141), and it
pushes onto the worklist whose emptied segments the mark kept (`records.rs`
163–197; `arena.rs` 999–1014), so it rarely calls `grow`. Read as written, a
scan over a whole part's closure passes no check. The stoppable stride covers
only the Vector and Hash loops; objects walk their outside groups through
`OutsideCells::walk_concurrent: unsafe fn(*mut u8, *const Class, &mut dyn
FnMut(Cell))` (`cells.rs` 167, 186), implemented by customers in other
repositories (`cells.rs` 147–149), and they return nothing.

**Failing scenario.** A B_max retry of a dense live part, about 2,071,040 rows
(v2's figure), finishes its mark; the scan starts; the mutator's allocation is
refused, it sets `waiting` and blocks. Nothing reads the flag until the scan
ends — on the order of 50–100 ms at v2's per-row rate (estimate), against the
11–19 µs claimed. An object whose hooked storage holds 10⁶ cells is read to
the end. Commit 3's red test raises `waiting` from a hook without saying in
which phase, so an implementation that checks only in the mark passes it.

**Better design.** Put the counter and the `Break` in the visitor wrapper both
phases pass to `trace_cells`, all under `R::CONCURRENT`; give `OutsideCells`
walks a `ControlFlow` result, named as a change in the other repositories;
red-test with the flag raised after the mark has finished. The shipped half
is the comment defect PLAN already names; this finding is about the repair
v2 claims.

**Confidence.** High.

## 3. Failure cost is paid once per registered root, not once per component

**Claim attacked.** §2 item 7, line 25: "один провальный trace на потолке раз
в X на живой негабаритный корень — 112–197 мс на 8 с ≈ 1,4–2,5 %"; §6
"Части", line 104; §7, line 130.

**Mechanism.** A part that meets B "откладывается до конца batch'а". A failed
part posts nothing and sets no `judged` bit, so the next root of the same
component is unjudged, opens its own part and fails at B the same way. At the
batch's end each deferred root is retried at B_max in turn: if the component
fits B_max, the first retry judges the rest; if not, every retry fails. At the
next X every mark is stale (the epoch moved), so every root tries again.

**Failing scenario.** v2's `overlapping-live` shape (63 roots, one component)
scaled dense past B_max: per take 63 × 142,336 rows at B plus 63 × 2,071,040
at B_max ≈ 139 M rows; at 54–95 ns per row (estimate, v2's rate) 7.5–13.2 s
per X of 8 s — more than one collector core. The grant holds COLLECTOR
throughout, so the mutator withholds every return until the mark recalls;
after the recall `FinishThePosts` posts Unwalked to the unjudged roots, which
return to R under commit 7, and the next take 4 s later starts over, so the
set is never parked whole. The sparse fill costs 63 × (278 + 4,045) ≈ 272,000
rows, 15–26 ms per X; v2 prices it at "доли миллисекунды". At the threshold,
K reaches 1,024 roots.

**Better design.** Retry at B_max immediately after the B failure, so a
success judges the component's other roots before they open parts; after a
B_max failure park every root that attempt met (their closures lie inside the
failed one, so parking one that would have fitted costs only recall); at most
one B_max attempt per grant.

**Confidence.** High on the mechanism (in the text); the figures are
estimates.

## 4. The word-keyed gate withholds what the teardown's pressure needs, and withholds every kind of block

**Claim attacked.** §2 item 1, line 13; §5, lines 75, 85; commit 6, line 163.

**(a) The retirement pass.** `refused_under_pressure(Teardown)` calls
`take_or_hold_posted` (`collect.rs` 744–749), which returns at POSTED without
taking (`token.rs` 372), so no stamp is written; `retire_candidates` then
frees and `make_withheld_returns_before_the_retry` drains, and the new block
drain exits in O(1) because POSTED ∧ word ≠ 0 still holds. Shipped code
returns these blocks: `under_a_foreign_holder` reads COLLECTOR only
(`deferred_slot_reuse.rs` 918–927), so under POSTED the drain hands every
block to the pool.

**Scenario (a).** A grant reads a live part, writes a list and releases
POSTED; during the grant the mutator emptied many blocks, all withheld;
before the next poll a refcount cascade runs destructors (teardown depth
above zero); a destructor's allocation is refused by both the pool and the
OS. Today the pass returns the grant's blocks and the retry succeeds; under v2
every block withheld during the grant and since stays withheld, and the
destructor gets memory-exhausted.

**(b) The gate is blind to block kind.** It sits in `BlockPool::put`, which
every population uses, so under POSTED ∧ word it also withholds request-arena
resets (`memory/arena.rs` 1001–1012), raw-heap blocks from `retire_empty`
(`heap.rs` 1285–1308, which zeroes the kind before `put`, so `put` cannot
tell a raw block from an entity block), and buffer-arena blocks
(`buffer_arena.rs` 397, 606). None can hold a listed member; line 85 prices
only "блоки, опустевшие … у потока с ненулевым словом списка".

**Better design.** Do not withhold under POSTED; stamp instead. The stamp
needs only that no collector writes the list, which POSTED already
guarantees — not MUTATOR. Stamp and zero the word at the first return the
mutator makes under POSTED ∧ word: inside the gate predicate `put` and the
run arm read, and in the retirement pass. The gate stays COLLECTOR-only as
today, the O(1) drain exit becomes unnecessary, "one place" becomes "the
first return after the release", and the stamp moves onto the free path once
per grant, where it runs no user code and allocates nothing.

**Confidence.** High on (a) and (b) (read); scenario (a) needs an OS refusal
inside a destructor.

## 5. Commit 1's red test cannot distinguish the two trees

**Claim attacked.** §2 item 11(a), line 33; §8, line 134; commit 1, lines 144,
153: the test asserts `deferred_count() = 0` after X.

**Mechanism.** `gc.rs` 265–270 arms AllRoots on the merge and 303–311 fires
it in the same call; commit 1 keeps that arm, which goes only in commit 7.
The in-line collection reads the merged roots live and defers them again
(`queue.rs` 1150–1157, 1297–1322); today, after 64 commits, the same
re-deferral happens.

**Scenario.** v2's test: 63 live roots in the lane, X = 40 ms, takes oftener
than X, a poll between them. Red today; after commit 1 the merging poll
re-defers all 63 before it returns, so `deferred_count() == 63` and it is
still red.

**Better design.** Assert the change itself — before commit 7,
`deferred_turnover_mirror()` (`queue.rs` 1647) moving to the post-X reading,
or a count of merges. Commit 1 also kills the instrument of the tests that
drive turnovers by commits in processes with no collector thread:
`collect/tests/when_the_turnover_reoffers.rs` (24 uses), `epoch/tests.rs`,
`queue/tests/the_tokens_every_lane_holds.rs`,
`mark/tests/the_volume_a_turnover_reoffers.rs`,
`finalization/tests/what_the_commit_stamps.rs`. They need a test shorthand
that advances a record's cell; v2 names only `ask_the_quiet_thread`.

**Confidence.** High.

## 6. The list and the stamp at the take have no bound

**Claim attacked.** §2 item 5, line 21; §5, lines 73, 75; §7, line 120
("бюджет ограничивает память арены").

**Mechanism.** The chain takes 8 bytes per live row of every completed live
part in the grant, through `gc_metadata::acquire` — outside the arena and
outside B — and the take stamps one byte per address.

**Scenario.** One dense B_max part of 2,071,040 rows needs 254 list blocks,
16.6 MB, twice the arena's 8.4 MB, held until the mutator takes the token.
The take then stamps 2 M headers: about 2 ms warm or about 165 ms cold at the
1 ns and 80 ns estimates v2 accepts, and under pressure that pause sits on the
refused allocation's path. At the threshold, 1,024 parts near B could append
up to 1,024 × 142,336 addresses.

**Better design.** Cap the chain at a fixed number of blocks per grant —
stamping a subset is safe by v2's own argument — and state the bound on the
stamp's pause next to the recall bound.

**Confidence.** High on the arithmetic; the pause figures are estimates.

## 7. One reading of the gate brings finding 9 back for chunks

**Claim attacked.** §5, line 85: "`under_a_foreign_holder` отвечает «чужой» …
при `state == POSTED`, когда слово списка … ненулевое"; line 29.

**Mechanism.** The function has two callers: the block gate
(`deferred_slot_reuse.rs` 964–974) and the chunk gate (939–949). Reading A
edits the function itself: chunks are then withheld under POSTED ∧ word, and
the chunk drain tests COLLECTOR on each pop and nothing else (1039–1052, via
1094), so every free under POSTED takes the whole chunk stack and withholds
each chunk again — O(chunks) per free, finding 9 moved to chunks. Reading B
adds a predicate for blocks only. The text names reading A while saying
chunks return "как сейчас".

**Better design.** Give the block gate a new function; finding 4's fix makes
this moot.

**Confidence.** Medium; the defect is an ambiguity in the text.

## 8. The parking mark recurs every fourth epoch

**Claim attacked.** Line 25, "обёртка двух бит стоит один лишний X"; line 130,
"Метку никто не чистит: она стареет эпохой, как штамп".

**Mechanism.** Every commit that reads a component rewrites its stamp; the
mark is written only on a B_max failure and cleared only by
`publish_header`.

**Scenario.** A root parked at epoch 5 whose closure shrinks enough to fit at
epoch 6: at epochs 9, 13, 17 and on, the take still skips it untraced and
posts ReadLive — one X in every four for the root's life. Recall only.

**Better design.** The mutator clears bits 20–23 on any verdict for the root
other than ReadLive with `OVERSIZE`.

**Confidence.** High; severity low.

## 9. Figures

- Line 35 and line 102, "порядка 30 захватов". A `put` overflows at 9 cached
  blocks and flushes 9 − 4 = 5 of them (`block_pool.rs` 39, 46, 807–819), and
  `push_global` locks once per block (838–854). From an empty cache, 128 puts
  flush 24 times at 5 locks each — 120 acquisitions; by the same count v1's
  figure at B = 32 was 25 locks. The previous Critic's correction left out
  the per-block lock, and v2 adopted it.
- Commit 2's "инструкции ≈ 15 000" and commit 6's "стена ниже 20,6 мкс" are
  predictions carried over from `overlapping-live`, written as gate readings.
- Checked and correct: 4,045 × 512 = 2,071,040; 278 × 512 = 142,336; 1,032;
  35; 512; 112–197 ms and 1.4–2.5 % against 2.8–4.9 %; the ladder at 248
  against 136; 11–19 µs; 8,412,800; 8,158 addresses per block and 16 blocks;
  2 MiB and 32 blocks; 34 min for the 8-bit alias; 54–95 ns = 20.6 µs / 381
  to 36.1 µs / 381; 11 of 64 bytes on the hold line, 38 after the additions
  with `batches_since` a byte, 21 of 64 on the writer line.

## Missing, needed on first use

- `discard_standing_verdicts` → `clear_posted_for_test` writes FREE over
  POSTED and leaves the word set; the next grant overwrites it and the chain's
  blocks leak.
- `gc_metadata::thread_stats` (316–331) panics on a mutator thread that has
  released list blocks the collector acquired; 19 test files read it.
- `rfc/model/classes.md` lines 43 and 45 ("Byte 6 has one writer", "20–23
  Collector reserve") need the parking amendment, and §10 omits it.
- Commit 7's cap-0 arm cannot be reached until commit 8 lifts the clamp
  (`worker.rs` 422–424), so it goes untested for a commit.

## Previous findings

| # | Status | Evidence or where it moved |
|---|---|---|
| 1 | Closed for correctness; new cost | Every mutator path that frees under MUTATOR takes from POSTED first — poll/P, explicit fire, pressure with the gate open, exit (`collect.rs` 209, 652); nested takes hold nothing. Cost moved to finding 4. |
| 2 | Closed | `LargeEntityHeader` and `LargeHeader` share `run_bytes` at offset 16; the stack link at `BlockHeader::next` (offset 8) overlays `size`, unread after unlink. |
| 3 | Closed | All three chains are `LazyChain`, whose `rewind` forgets its segments; the copy lies below the watermark; bit 1 is unused in R (`queue.rs` 1016–1023), and even eight-aligned entries leave bits 0–2 free. |
| 4 | Closed except commit 1's test | Commits 2–6 each keep the arm-on-merge path; after commit 7 only garbage larger than B_max depends on pressure, which v2 names. Commit 1's test: finding 5. |
| 5 | Closed | — |
| 6 | Closed | — |
| 7 | Closed per root; cost understated | Finding 3. |
| 8 | Closed | — |
| 9 | Closed for blocks | One reading moves it to chunks: finding 7. |
| 10 | Closed | — |
| 11(a) | Not closed | Finding 5. |
| 11(b) | Not closed | Finding 2. |
| 12 | One figure wrong | Finding 9. |

## What I tried to break and could not

- Exit always takes from POSTED, and its drain empties the block stack before
  `dispose_thread_state` asserts it is empty.
- An entity that died before a grant cannot enter that grant's list: under a
  concurrent reader `visit_child` returns at count zero (`mark.rs` 410–420).
- `write_maturation_stamp` preserves the reserve bits (`refcount.rs`
  964–971), and every writer of byte 6, parking included, is the owner.
- Stamping a free slot is harmless: the free-list link sits at +8 (`heap.rs`
  124–141).
- The consent clears `waiting` before its release swap, and every grant begins
  with a consent, so no stale 1 survives into a new grant.
- A stop mid-entity only discards the part: `sweep_rows` rewinds the worklist
  and the component stack before it unstamps (`arena.rs` 693–719).
- The `PlainCells` branch is constant, provided the counter and the `Break`
  both sit under `R::CONCURRENT`.
- The turnover byte has one writer; the 8-bit alias costs one X; a merge right
  after a shift between the arena's open and the lane's fill is correct.
- The parts lemma holds.
- Under cap 0, arming AllRoots on the merge is within ruling 1.
