# Critic on the package (`CYCLE-SPLIT-PACKAGE.md`)

Date: 2026-09-23. Read-only review; no test was run. Line numbers are the
package report's. Saved verbatim from the Critic's hand-back; not yet answered.

**Summary**
1. Most severe: the live-core stamp is written after the mutator's own frees — on the pressure path `collect.rs:917` returns every withheld block (POSTED-held ones included) before the drop stamps at `collect.rs:303`; byte-6 writes can land in pooled or other threads' blocks.
2. The run gate misses `BLOCK_KIND_ENTITY_LARGE_RUN`: `large_entity::free` unmaps directly, so under POSTED the later stamp faults.
3. Parts as written destroy the batch's root copy (`reset` rewinds the bump over it), and nothing marks a root judged across a reset, so a root gets two verdicts — a panic at P's `.expect` or one slot freed twice.
4. Step 1 cannot land before steps 3 and 6: until then a sub-threshold take past B (63 dead `disjoint` rings) comes back all-`Unwalked` every 4 s forever, and a live core above B has no path to a stamp.
5. "One list per batch" contradicts "every live part writes a list": one reading leaks list blocks, the other leaves every live closure the collector reads unstamped for good.
6. Under `cap 0` the elder's clock merges the lane into an R nobody judges below the threshold, so the clock does not buy what it is kept for.
7. The retry at the ceiling loops every 4 s forever on a live oversize root, since no pressure collection stamps what its scan reads live; at the report's own per-row rate the cost is about 10× its figure.
8. The turnover byte keeps two plain-store writers and an unspecified clear: one reading loses the collector's field, the other merges a just-deferred lane at the next poll.
9. Under POSTED every slot free drains and re-withholds all withheld blocks — O(blocks) per free; "compares with two values" is not the price.
10. Low: the cell is read twice per collection, so a stamp can be one epoch ahead of its prune; the list can overwrite a newer stamp; §7's "cannot be shown red today" is false.
11. Figures: the §6 table, the list and descent sizes and the S38.3/S60.6 quotes are right; the ceiling cost and "~10 µs for 1,300 edges" are wrong.
12. Survived: a stamp on a live stranger costs recall, never a free; at k = 1 a flat list prunes what an SCC stamp prunes; the 2-bit wrap, the new-life window and handover are recall-only; the parts lemma; `waiting` as a hint; a flat poll cost.

None of findings 1–9 exists in shipped code; they come with the package.
Finding 11(b) is about a shipped comment.

## 1. The list stamp is written after the mutator has already freed, so the POSTED gate is undone before the stamp it protects

**Claim attacked.** §4, line 58: "в `dispose_verdicts` на каждом пути… путь
давления штампует и возвращает блоки до retry"; line 68: "придержанные блоки
возвращаются при первом `make_returns…` после закрытия сборки над P"; §9,
line 156.

**Mechanism.** The gate holds block returns only while the byte reads
COLLECTOR or POSTED; the stamp is written at disposal, under MUTATOR, after
the collection has already freed memory.
- Pressure path: it tears down, retires completed deaths (`collect.rs:893`),
  then runs `make_withheld_returns_before_the_retry` (`collect.rs:917`) under
  MUTATOR, which sends every block the POSTED gate held through
  `BlockPool::put` with the gate open. P, and so the list, is disposed of
  only later, in `CollectingThread::drop` (`collect.rs:303`).
- Ordinary path: `commit` (`collect.rs:453`) runs destructors before
  `dispose_batch_on_close` (464). A destructor that frees a member in a block
  this trace has no row for returns it physically, and an emptied block
  reaches `put` with the thread's own window open; `under_a_foreign_holder`
  (`deferred_slot_reuse.rs:918-927`) reads that as not foreign.

The shipped code states the rule this breaks, at `maturation.rs:135-138`:
"Called before the first guard and before any destructor… a stamp written
after it would land in whatever occupies the slot next."

**Failing scenario.**
1. The collector's part over root r reads r live. The closure is r plus
   m2..m300, one member per entity block (the report's own step-6 shape
   without the ring edges). The collector writes a list of 299 addresses and
   releases to POSTED.
2. The mutator nulls r's fields, so each m dies by refcount; each block
   empties, and the new gate holds it.
3. The next allocation is refused. `collect_under_pressure` takes MUTATOR
   and at line 917 gives all 299 blocks to `put`: 8 stay in the thread cache,
   the rest are flushed to the global stack, where other threads commission
   them.
4. The drop stamps 299 addresses: each a byte read-modify-write at slot
   offset +6 inside another thread's arena, buffer or entity block of another
   class — a data race and silent corruption. No destructor is needed.

**Better design.** On the pressure path, drop the list unstamped and return
its blocks: a pressure collection stamps only validation results anyway
(`rfc/model/gc/rc-cycle.md:75-77`). On the ordinary path, stamp right after
the take, before the trace; a member that died after POSTED then costs
recall. Alternatively key the gate on "the list word is non-null" rather than
on the byte, so it holds through the mutator's own collection and through the
drain at line 917 until the stamp is written.

**Confidence:** high on the ordering (read); the corruption itself needs
another thread to draw the block.

## 2. The run gate misses large entities on OS-direct runs

**Claim attacked.** §4, line 68 ("gate… и run-ветвь `ll_free`… начинают
придерживать и под POSTED") and the table row "large entity в run'е после
unmap".

**Mechanism.** The run arm the report means, `unmap_run_unless_withheld`
(`stdapi.rs:582-588`), serves only `BLOCK_KIND_LARGE_RUN`, a raw buffer.
Entity runs go through `stdapi.rs:603-606` to `large_entity::free`, whose run
arm (`large_entity.rs:177-184`) unlinks and calls `os::unmap` with no gate.
Today such a death is safe under COLLECTOR because it is withheld at the slot
level: `can_lose_trace_identity` covers large entities (`stdapi.rs:72-76`,
`deferred_slot_reuse.rs:805-816`). The package returns slots at once under
POSTED, which removes that cover.

**Failing scenario.** A live core contains an object big enough for an
OS-direct run; it dies by refcount between POSTED and the take. The slot arm
answers false, `ll_free_large` unmaps the run, and the stamp then faults with
SIGSEGV or writes into whatever mapping took the range.

**Better design.** Gate the `ENTITY_LARGE_RUN` arm of `large_entity::free`
with the same `withhold_block_under_a_foreign_trace`; `return_withheld_block`
must then send that kind back through `large_entity::free` (unlink first).

**Confidence:** high.

## 3. Parts destroy their own root list, and a root gets two verdicts

**Claims attacked.** §5, line 90: "корень, чья строка уже встречена
предыдущей частью, часть не открывает — его вердикт уже решён (`verdict_for`
читает его строку…)", and "`verdict_for` для него и для каждого корня
batch'а, чью строку эта часть встретила". Step 3, line 142:
`find_initialized_row` before each part, `reset` between parts, "каждый
peeked корень — ровно один вердикт".

**Mechanism.**
- (a) `batch` copies the peeked roots into the arena's bump
  (`worker.rs:1904-1913`); `reset` rewinds the bump to the start of the
  workspace (`arena.rs:779-782`), so the next part's row arrays are placed
  over the slice the loop is iterating.
- (b) After a reset, `find_initialized_row` finds no row for a root an
  earlier part met, and on a live root with no row `verdict_for` answers
  ReadLive (`worker.rs:2044-2050`). "Reads the row that part coloured" cannot
  happen.
- (c) Nothing remembers which roots already have a verdict.

**Failing scenario.** Batch [r1, r3, r2], r2 inside M(r1), r3 disjoint.
Part r1 posts r1 and r2; part r3 resets and posts r3; r2 has no row now, so
part r2 opens and posts r2 again. Then, by P's room:
- room equals the take: the fourth post fails
  `.expect("the batch was clamped to P's room")` (`worker.rs:1983`), a panic
  on the collector thread;
- room to spare, r2 dead at disposal: P holds two entries for r2. Retiring
  the first clears DEAD_IN_PLACE and CANDIDATE and frees the slot
  (`compaction.rs:170-175, 225-238`); `ll_free` sets DEAD_IN_PLACE again, so
  the second entry also reads as a completed death and frees the slot a
  second time — one slot on the free list twice;
- room to spare, r2 live: deferred twice, and the stale entry later frees
  whatever occupies the slot.
The reverse order, r2 before r1, posts twice as well.

**Better design.** A reset between parts that keeps the copy below a
watermark (sweep the rows, return the drawn blocks, rewind only down to the
copy), and a "judged" bit per copy index in the entry's spare low bits, set
when a verdict is posted; a part posts only for unjudged roots it met, and
`FinishThePosts` reads the same bits.

**Confidence:** high on (a) and (b) (read); either way an implementer reads
the text, the result is wrong.

## 4. The build order ships a permanent unjudged regime between step 1 and steps 3 and 6

**Claim attacked.** §8, line 126 ("(b) владельца и удаление `arm()` не
зависят ни от чего") and step 1, line 138.

**Mechanism.** After step 1, `Unwalked` is not walked in the collection over
P and goes back to R through `append_entry`. A sub-threshold take clamps to
threshold − 1 and never reads K (`worker.rs:1882-1894`), and
`size_the_next_batch` runs only at the threshold (1941-1943). Until step 3, a
batch that meets B posts `Unwalked` for every root (`worker.rs:1976-1980`).

**Failing scenario 1 (garbage).** The `disjoint` shape: 63 dead rings of 5,
one member per block, below the threshold. Their rows need 315 × 2,080 =
655,200 bytes, more than 579,200 at B = 8. Every take meets B and posts 63
`Unwalked`; step 1 writes all 63 back to R; 4 s later the same take runs
again. The 315 garbage entities stand until pressure or exit; today the
mutator walks and frees them.

**Failing scenario 2 (live).** A live core larger than B is stamped today by
the mutator's commit over `Unwalked`, and the next take prunes it. After
step 1 nothing stamps it: the collector never finishes the trace, the mutator
does not walk it, the arm on the turnover is gone, and a pressure collection
stamps only validation results (`rc-cycle.md:75-77`). The first way back is
step 6.

**Better design.** Land step 1 in the same commit as step 3 and step 6's
retry and list, or keep `Unwalked` a root of the collection over P until the
list exists.

**Confidence:** high.

## 5. How many lists a batch has is left open, and both readings fail

**Claim attacked.** §4, line 56 ("Одно слово — один список на batch; … на
batch и приходится один такой корень") against §9, line 156 ("После
законченной части, прочитавшей корень живым…") and the §4 table's claimed
gain, "один полный обход ядра за оборот".

**Reading A — every live part writes a list.** `disjoint-live` gives 63 live
parts and one hold-line word; each list written over the word loses the
previous list's GC-metadata blocks for good, unless parts append to one
chain.

**Reading B — only the retried root writes a list.** A closure that fits in
B and reads live is stamped by nobody: the mutator does not walk ReadLive,
and the arm on the turnover is gone. Every take and threshold batch then
re-traces it unpruned for the life of the process.

**Better design.** One chain per grant that every part appends to. The
arena's epoch is fixed for the grant, so one header epoch serves all parts;
the count goes in the first block's header. A refusal mid-append keeps what
was written — a subset, and stamping a subset is safe.

**Confidence:** high that the text supports both readings.

## 6. Under `cap 0` the elder's clock does not buy what it is kept for

**Claim attacked.** §2, line 31 and §9, line 160 ("иначе… компонент, умерший
зрелым, не переоткрывается ничем"); step 7, line 150.

**Mechanism.** The elder's tick sets the byte and the poll merges the lane
into R without arming (§3). Below the threshold the only judge is the
collector's take, which `cap 0` removes; step 7's mutator collects only when
R reaches the threshold.

**Failing scenario.** Under `cap 0` a thread defers 10 roots read live, then
registers nothing, and their component dies. At the next tick the 10 roots
land in R, below the threshold of 64. No take comes and no threshold is
reached, so the garbage stands until pressure or exit — the state the tick
exists to prevent.

**Better design.** Under `cap 0` the merge arms `AllRoots`, which ruling 1
permits since `cap 0` is one of its two cases; or step 7's threshold test
counts the merged lane.

**Confidence:** medium — step 7 is one line and may intend this, but the text
says the poll arms nothing.

## 7. The retry at the ceiling never ends for a live oversize root, and its cost is understated

**Claim attacked.** §6, line 116: "take через 4 с повторяет с потолка… 10–20
мс… 0,3–0,5 %… Выход из этого — отказ аллокатора".

**Mechanism.** A pressure collection reads the root live and defers it: it
splices the lane back at `collect.rs:842` and defers again, stamping nothing
its scan read live (`maturation.rs:88-94`, `rc-cycle.md:75-77`). The next
turnover merges the lane, the take fails at B_max, posts `Unwalked`, the root
returns to R, and the cycle repeats every 4 s. Refusal is no way out.

**Arithmetic.** The arena at B_max is 56,960 + 128 × 65,280 = 8,412,800
bytes, and a row is 4 bytes (`bytes_for(512)` = 2,080 = 512 × 4 + 32). A
dense fill is about 2.07 M rows (4,045 class-128 arrays × 512, or 512
class-16 arrays × 4,080); at the report's own 54–95 ns per row that is
112–197 ms per failure, 2.8–4.9 % of a collector core, not 10–20 ms and
0.3–0.5 %. A sparse fill, one row per touched block, is about 4,045 rows and
well under a millisecond; the report states neither case. Each retry also
draws and returns 8 MiB.

**Better design.** Park a root that failed at the ceiling: an attempt count,
and a retry only at the next turnover or when a new registration touches it.

**Confidence:** high on the mechanism; the per-row rate is a whole-take
figure (Critic 1), so the real cost needs a measurement.

## 8. The turnover byte has two writers and an unspecified clear

**Claim attacked.** §3, lines 35 and 37: the collector stores
`1 | (T mod 128) << 1`, and the poll "снимает байт и сливает lane".

**Mechanism.** Today both parties write with plain stores
(`mutator_record.rs:617-633`), and `defer_entry` clears the byte when it
fills an empty lane (`queue.rs:1320`).
- Reading A — the flag gates the comparison and the clear stores 0: a
  mutator's clear landing after the collector's store of T+1 erases it, and
  the lane waits one X more. Recall only, bounded.
- Reading B — the field alone decides, as "зеркало хранит семь бит"
  suggests: after any clear the field reads 0, so any mirror other than 0
  differs. The next poll merges the lane, roots deferred a moment earlier
  included; the take re-traces them within 4 s, they defer again, the clear
  runs, the lane merges — each live deferred root is re-traced about every
  4 s instead of once per turnover, giving back the recall the deferral buys
  (`queue.rs:1187-1193`).

**Better design.** One writer: the collector stores the field, the mutator
never writes the byte, the poll merges when the field differs from the
mirror. The flag bit and both clears go.

**Confidence:** medium — a gap in the text; both guesses fail concretely.

## 9. The free path under POSTED costs one pass over the withheld blocks per free

**Claim attacked.** Step 6, line 148: "`BlockPool::put` и run-ветвь `ll_free`
сравнивают с двумя значениями вместо одного".

**Mechanism.** Under POSTED every slot free calls
`make_returns_withheld_under_a_foreign_trace` (`deferred_slot_reuse.rs:819`).
Its drain tests COLLECTOR only (line 1094), so it takes the whole block list
and gives each block to `put`, whose gate now withholds it again: k pops, k
gate reads and k pushes per free with k blocks held. Widening the drain's
test to POSTED does not help: `splice_behind_the_head` (1121-1129) then walks
the whole chain on every free.

**Scenario (estimate).** A mutator holding 30 withheld blocks keeps freeing
64-byte objects through the POSTED window; each 4.3 ns free gains about 30
list operations.

**Better design.** Exit the block drain early on a POSTED reading, and drain
blocks only at the poll and at the collection's close.

**Confidence:** medium on the cost; the mechanism is read.

## 10. The cell is read twice per collection, and the list can overwrite a newer stamp (low)

**Claim attacked.** §2, lines 19 and 25.

**Mechanism.** The arena reads the cell when it opens, and
`Finalization::begin` reads it again (`finalization.rs:212-216`). Today both
read a counter only the owning thread moves, and never inside its own
collection. Under the collector's clock an advance between the two reads
makes a commit stamp e+1 on components read live because e-stamps were
pruned. Separately, the list stamp is applied after the mutator's own commit,
so it can overwrite an e+1 stamp that commit wrote with the collector's e.
Both cost recall only.

**Better design.** `Finalization` takes the arena's epoch; the list stamp
skips an entity whose stamp already carries the mutator's current reading.

**Confidence:** high on the mechanism; low severity.

## 11. The two shipped defects (§7)

**(a) Defect 1.** The package does make it moot once step 2 lands, but
"красным на нынешнем дереве это не покажешь" (line 120) is false. A case that
drives real all-live takes more often than X and checks that the lane is
re-offered after X is red today — every commit over P moves the clock
(`worker.rs:489-494`). That case is PLAN.md's own done line, and the
cross-cutting rule requires a test verified to fail on the bug.

**(b) Defect 2.** The package moves this one rather than closing it. The new
comment would say the wait is bounded by recall, but the tracer does not stop
inside one entity's cells (`mark.rs:300-311`), and neither the list write
after a part nor `reset` checks `waiting`. The comment has to say the wait is
N entities, plus the largest entity's cells, plus the list write, plus the
reset — still unbounded in one array's length. This one is about a shipped
comment.

## 12. Figures

- §6, line 116: see finding 7.
- §5, line 88: "~10 µs" for "1,300 edges". At the report's 54–95 ns rate, 256
  entities give 14–24 µs and 1,300 edges would give 70–124 µs; 10 µs matches
  neither.
- §4, line 58: "130,000 RMW ≈ 100 µs" is labelled a lower bound. A cold core
  touches about 130,000 distinct cache lines, so the realistic figure is
  around 10 ms at ~80 ns per miss (the Critic's estimate).
- §5, line 88: "32 `put` под мьютексом пула" is wrong: `put` locks only when
  the cache of 8 overflows (`block_pool.rs:800-822`).
- Checked and correct: 2,080; 579,200 / 2,080 = 278; 1,032; 4,045; 35; 512;
  15.9 blocks for the list; 2 MiB and 32 blocks for the descent; S38.3's
  25,000 slots and 1.6 MB; S60.6's 8.13–8.46 against 6.95–7.83 ns;
  128 × 8 s = 17 min.

## What I tried to break and could not

- **A stamp on a live stranger in a reissued slot costs recall only.** The
  prune (`mark.rs:453-458`) leaves the edge unsubtracted and the target
  unmet, so rows only rise; the scan skips targets with no row; exact
  validation counts from refcounts and never reads a stamp; the teardown's
  releases into a pruned live target are ordinary refcounting. A byte-6 write
  to a free slot of the same block is harmless: the free-list link sits at +8
  (`heap.rs:141`) and `publish_header` rewrites the whole word.
- **At k = 1 a flat list prunes what an SCC stamp prunes.** Every Live row
  gets age ≥ 1 either way; the minimum read through `AtomicCells` is stable,
  because stamps change only under MUTATOR.
- **The rest of the epoch attacks are recall-only:** the 2-bit wrap; the
  new-life window (about one round, at most `FALLBACK_INTERVAL_MAX` = 1 s,
  not X); the adopter's 1-in-4 alias; handover, where the cell is written
  under the reading hold's CAS or under the grant. DECISIONS 2026-09-19's
  "one turnover past" holds in that recall-only sense.
- **The parts lemma holds:** M({rᵢ} ∪ the roots it meets) = M(rᵢ); a verdict
  is only a proposal, and safety rests on exact validation.
- **`waiting` as a hint** does not reopen E10; the clear has to come before
  the consent's release swap — unspecified in the report, but trivial.
- **The poll's cost stays flat:** fewer loads than today's, which also reads
  `commits`; the 7-bit alias costs one X; `dispose_verdicts` works whatever
  P's order.
- **The gate covers the rest of the list's address space:** the list never
  names chunks; entity blocks change class only through the pool
  (`empty_reserve` is per class); arena entities are untracked; retained
  blocks go back through `put`.
- **The critical reserve** is per collector thread and returned at `reset`;
  the collector's thread cache can strand at most 8 blocks.
- **Doubling M** does guarantee progress. The "zero progress ends in
  allocator refusal" argument is false on an uncapped heap, but nothing rests
  on it.
