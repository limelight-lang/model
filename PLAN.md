# Plan

Destination: `ll-model` is the runtime the compiler links — the memory
manager, the object model and the cycle collector built to the `rfc`'s
design and calibrated on parameterized test heaps, each reading naming its
parameters.

Design lives in `rfc` and is authoritative — read before coding, do not
re-derive: `model/classes.md`, `model/values.md`, `model/lowering.md`,
`model/gc/rc-cycle.md`, `model/gc/cycle/questions.md`, `model/memory/ffi.md`,
`runtime/object-lifecycle.md`. The `rfc` repository carries its own plan at
`dev/PLAN.md` for work that lands in the specification rather than in this
crate. The destination's last mile, the compiler that links this crate, is
outside this plan: `rfc/BACKLOG.md`, "The big one", and the front end in
`limelight`.

Updated: 2026-09-24 · Active: S65.

Review 2026-09-23: on time, found by hand, the hook still blind to this
plan. Pass 1, code `128cefc..89bb0dc` against the thresholds (a function over
50 code lines, depth 3, a one-implementation abstraction, a one-caller
forward): five production places — `worker::batch` at 59 lines, cut to 42 by
S65.3; `Standing::checkpoint` at depth 4; `epoch::of_record` and
`Standing::take_backlogged`, one-caller forwards; `HandBackOnDrop`, kept as
the unwind guard `AdvanceOnDrop` already is — and 45 in tests, the cuts being
the backlog line "The review's cuts of 2026-09-23". Pass 2: the period's
algorithms are Edmond's rulings of 2026-09-23. Pass 3 is the Critic over the
S65 section of the same day (`dev/S65-PLAN-CRITIC.md`). Earlier reviews and
the closed stages' summaries are in `git log -- PLAN.md`.

**Closed stages are deleted whole** (rule 23.1.3), and what outlived each of
them is in the journals: `dev/DECISIONS.md` for a decision and its reason,
`dev/POSTMORTEM.md` for a trap, `dev/BENCHMARKS.md` for a measurement,
`dev/INDEX.md` and `dev/ARCHITECTURE.md` for the map. Deleted so far: S4
through S64. A number is never reissued, and the prose sections below the
active stage are the backlog stages are drawn from.

**Every cycle-GC step has one review gate, and the Sage is the escalation**
(Edmond, 2026-09-10 and 2026-09-12: the Critic first, the Sage only for what
the model cannot answer itself). The pre-change baseline the step would erase
— operation count, manager-allocation budget, cache working set, lifetime and
refusal model — is recorded in the step before the first edit; a red test is
seen failing; the Critic reviews the repair and its mutations before the step
is checked, and its findings are recorded in the step. One broad review does
not waive a later step's gate. The rest of the routine — the gate, Miri in
slices, the citation checks — is `dev/WORKFLOW.md`.

## Fog

A line here is an unresolved question rather than a step: it carries no
criterion, and it leaves when it gets one or when it is ruled on.

- **Whether a grant should bound the cross-thread frees it withholds.** A
  slot another thread frees waits on its block's remote list while this
  thread's token is held (`Heap::collect_remote`, through
  `deferred_slot_reuse::returns_are_withheld`), and none of the three marks
  of S65.7 counts it, so a producer freeing this mutator's entities holds
  memory for the grant's whole length. Raised by the Critic of S65.7,
  2026-09-24; how much a workload reaches it is unmeasured.

- **Whether a survivor list should prefer a block this reset has already
  retained.** `Arena::alloc_preferring` tries the described block's tail, the
  reset's current block, then a fresh pool block. A bump arena's survivor
  blocks are normally full, so the common answer is the second, and that
  block — which may hold no survivor of its own — is then retained and held
  until the last survivor of every block whose list it carries has died; the
  third answer holds 64 KiB drawn at reset time for the same span. Preferring
  the tail of a block the reset is keeping anyway would cost nothing extra in
  bookkeeping and would build pairwise dependencies between retained blocks,
  which `release_emptied` already recurses through. Raised by the Critic of
  2026-09-13 over the survivor-list grouping, and priced nowhere.

- **What a process-wide `pthread_key` would buy the reserve draw.** The
  thread-locals of this crate that carry drop glue — the census test
  `critical::tests::where_the_first_touch_happens` lists them — register a
  TLS destructor at their first touch whose failure ends the process rather
  than reporting (`dev/DECISIONS.md`, "what the first touch of a thread-local
  with drop glue may cost"). `ll_thread_init` touches every one, so the death is
  deterministic in place; the class itself is still there for a thread that
  never runs init. A guard on a key taken once at process start, where a
  failure is reportable, would remove it if `pthread_setspecific` allocates
  nothing per thread — which nobody has read, on any target. Named when the
  reserve's first touch was decided on 2026-08-29 and priced nowhere since.

## Cross-cutting (every stage)

- The old collectors are reachable at `archive/pre-rc-cycle` and nowhere else.
  Nothing is copied back without a decision entry.
- Every fix carries a regression test verified to fail on the bug
  (`dev/WORKFLOW.md`, Tests); the gate, Miri in slices and the citation
  checks are `dev/WORKFLOW.md`'s, the review chain is the header's.
- A claim about speed is a measurement or it is not made; a bench does not
  cross the C ABI — ABI-entry work is shown by IR or asm (`dev/BENCHMARKS.md`).
- Every byte owned for cycle collection comes from the memory manager and is
  identifiable there as GC memory; production collection paths use no
  allocator-owning Rust container. The deny run is
  `cycle::collect::tests::what_a_collection_asks_the_allocator`, the ledger
  `memory::gc_metadata` (`dev/DECISIONS.md`, "GC memory is counted once, and
  the block kind is the split").
- `dev/ARCHITECTURE.md` is the crate's knowledge map and moves with behaviour
  like any other document (`dev/WORKFLOW.md`).

## S65 — The collector finds, the mutator judges  [in progress]

Goal: the mutator searches for cycle garbage only when the memory manager
refuses it or the embedder turned the collector off; in every other case the
collector finds and the mutator judges what it proposes, and what bounds the
mutator's wait for its token is a recall rather than the trace's block budget
(`dev/DECISIONS.md`, "the collector finds and the mutator judges, and a recall
of the token bounds the mutator's wait instead of the budget"). The design is
`dev/CYCLE-SPLIT-PACKAGE-3.md` with `dev/CYCLE-SPLIT-PACKAGE-3-LANE.md` as
amended by its Critic (`dev/CYCLE-SPLIT-PACKAGE-3-LANE-CRITIC.md`, F2 and F3);
the analysis behind it is `dev/S64-GC-IMPROVEMENT-ANALYSIS.md`. Each step is one commit of the package's
build order (its section 11), with that section's mechanism, red test and
measurement; the rule of the stage is the owner's: the mutator's performance
comes first. The Critic over this section (`dev/S65-PLAN-CRITIC.md`,
2026-09-23) returned seven findings, F1–F7, all taken into the steps below
and into the package in place; the split itself it left standing. Two of
them are rules for every step: a step that changes a behaviour the `rfc`
states carries the `rfc`'s amendment in its done-line (F7, `dev/WORKFLOW.md`,
"Documentation follows the logic, in the same commit"; the two repositories
commit together), and no step may lengthen the wait or the withheld memory
of a mutator under a grant beyond today's until the recall bounds it (F3).
A wait under a shortage of memory is the exception and not a breach: the
pressure collection and the retirement after a refused allocation inside a
teardown wait for the token by necessity (Edmond, 2026-09-23;
`dev/DECISIONS.md`, "a mutator short of memory waits for its token, and the
wait is the rule's exception").
Done when: the mutator's collection over P traces no `Unwalked` root, no poll
arms a collection over R whole outside `cap 0`, a recalled grant returns the
token within N inspected positions, one bounded post and one arena reset,
and the poll and the free are where `dev/BENCHMARKS.md` S60.6 and S38.3 put
them.
Miri owed at the stage's close, targeted (`dev/WORKFLOW.md`, "Miri"):
`epoch::turn_the_cell_of` and the record's accessors; `Reader::unread_up_to`,
the reading's reordered loads and `the_merged_lane`; the retirement pass under
the token and the reordered state line; the arena's watermark cases and
`the_batch`'s cases; `the_recall`'s stride and growth cases, its two-mutator
case's raw list pointer and its two grant-behind cases;
`when_a_withheld_stack_recalls_the_token`; `the_live_list`'s cases but the
bound's (the chain's writes and reads, `refcount::stamp_as_read_live`,
`shadow::for_each_live_met_row`, the block drawn back and filled);
`the_ceiling`'s failed retry, whose met-roots walk counts a position per
block.

- [x] S65.1 Correct `queue.rs`'s claim that an entry's low four bits are clear
      handoff: `5014615`, three comments in `queue.rs`.
- [x] S65.2 The epoch cell is the collector's (package commit 1)
      handoff: the cell is `HoldLine::turnovers`, advanced by
        `worker::advance_the_epoch_if_due` before the serve; the poll
        6.84–6.99 against 8.26–8.36 ns, `dev/BENCHMARKS.md`, "S65.2 the
        poll with the collector's epoch cell".
- [x] S65.3 A merged lane is taken at the next round, and K doubles only on a
      filled clamp (package commit 1a, in the Critic's form)
      handoff: the count is `WriterLine::merges`,
        `HoldLine::merges_seen` its reading; the backlog is
        `Reader::has_at_least_by_count`.
- [x] S65.4 Completed deaths of R are retired by a count (package commit 2)
      handoff: `MutatorCycleState::candidate_deaths` against
        `retire_after`, the pass `queue::retire_at_the_poll`, the arming
        `gc::Arming::Retire`.
- [x] S65.5 The machinery of the parts, the batch still traced once (package
      commit 3, first half)
      handoff: `worker::FinishThePosts` and bit 1 of the copy,
        `HAS_A_VERDICT`; the reset to the watermark, which S65.13
        switched on.
- [x] S65.6 The token's recall (package commit 4)
      handoff: `TraceToken::waiting`, read by
        `TraceScratchArena::inspect_position` every `RECALL_STRIDE` and
        at a growth; `dev/BENCHMARKS.md`, "S65.6 the token's recall".
- [x] S65.14 A grant held behind another mutator's batch is recalled
      handoff: `Collector::grants_recalled`, released by
        `Standing::release_the_recalled`; `dev/BENCHMARKS.md`, "S65.14 a
        grant held behind another mutator's batch".
- [x] S65.7 The marks by stack length (package commit 5)
      handoff: `ForeignStack::count` and `uncount`,
        `token::recall_if_a_mark_stands`; the price accepted,
        `dev/DECISIONS.md`, "a withheld death pays for its count, and
        the marks bound what a grant withholds".
- [x] S65.13 The batch runs in parts (package commit 3, second half)
      handoff: `worker::trace_in_parts` and
        `post_the_roots_the_part_met`; `dev/DECISIONS.md`, "the batch
        runs in parts"; `dev/BENCHMARKS.md`, "S65.13 the batch in parts
        at a grown K".
- [x] S65.8 The live core stamped from a list (package commit 6)
      done: one chain per grant of at most L blocks, its head on the hold
        line, stamped at the take from `POSTED` or at the first block or run
        return under it, dropped under pressure; the collector reads
        `waiting` every N rows of the touched-list walk that writes the list
        and stops when it is set, keeping what it wrote, so that the list adds
        no unbounded work before the release (`dev/S65-PLAN-CRITIC.md` F1,
        second instance), with a recall raised inside that walk measured;
        after a take over `overlapping-live` every member carries the stamp
        and the next take prunes; the handshake document records the list
        and the stamp beside E12
      tier: T2 · role: Critic
      baseline 2026-09-24 (`ea9d092`), what the step erases: a grant draws
        from the pool only its arena's blocks, all back before the release,
        and no GC block crosses a thread; the take from `POSTED` is one swap
        and writes no header; byte 6 is written by the owner's commit alone;
        a block's return under `POSTED` reads the token once and the run arm
        of `large_entity::free` reads nothing; a take over `overlapping-live`
        meets all 381 rows and the next one in the same epoch prunes no edge
        (read at the step's red run); no refusal is new.
      Critic 2026-09-24 round 1: six findings. (1) The recall case could not
        fail for a check before the walk: the seam moved inside it, the bound
        asserted in positions, no wall probe. (2) A sleeping owner pinned the
        list's blocks: the collector gives a stale list back. (3) The rewind on
        a recall put block returns before the release and threw away live
        rows: the walk stops and keeps them, done-line amended. (4) The
        pressure case could not see its order: read inside a destructor. (5)
        The non-`POSTED` arm of the return hook became a debug assertion, E13
        corrected. (6) E11's closure must cover the list's writes: named in
        E13. All accepted.
      Critic 2026-09-24 round 2, over the corrections: the give-back walked a
        chain with no happens-before (publication now a release, the swap an
        acquire); its case called the function directly (a round-driven case
        added); it borrowed the standing interval (keyed on the epoch's advance
        instead, which costs no stamp). All accepted
        (`dev/DECISIONS.md`, "the live core a batch read is stamped by the
        owner from a list, at the take from `POSTED` or at the first return
        under it").
      handoff: `cycle::live_list` (writer, the owner's stamp, the return hook,
        the collector's give-back after an advance), `HeldToken::take` /
        `take_giving_back_the_live_list`; eleven cases in
        `worker::tests::the_live_list`, eleven mutations red; 381 rows then 63
        on `overlapping-live`. On the way the test block budget was made to
        cover adoption (`dev/POSTMORTEM.md`, "an adopted block served a case
        whose budget refused the pool").
- [x] S65.9 The retry at the ceiling (package commit 7)
      done: a part failing at B retried at `B_max` in the same grant, once per
        grant; a failed retry, and a part meeting B with the retry spent, post
        every live root their rows met `ReadLive`, which the deferred lane
        holds until the turnover, and the batch goes on with the next root; a
        retry the recall stops ends the batch; K never halves; a 300-block ring
        judged in one grant; a ring past `B_max` read live once in a grant and
        retried after the turnover; `rfc/model/gc/rc-cycle.md` states that
        `ReadLive` as a deferral rather than a trace's verdict. Amended
        2026-09-24 from a parking mark in byte 6 bits 20–22, written and
        cleared by the owner from a second section of the live list's chain
        (`dev/S65-PLAN-CRITIC.md` F4), and from a batch ended with the rest
        `Unwalked` and K halved: Edmond's rulings on the Critic's findings,
        below. That the owner pays nothing for a closure past B holds once
        S65.10 drops the turnover's arm, which until then traces every
        deferred root in a collection over R.
      tier: T2 · role: Critic
      baseline 2026-09-24 (`2610a33`), what the step erases: a part that meets
        B ends the batch, every root left without a verdict is `Unwalked` and
        K halves, so a root whose closure passes B comes back `Unwalked` at
        every take; bits 20–23 of byte 6 have no writer; the live list's chain
        holds one section; a grant draws at most B blocks above the workspace
        at any instant.
      Critic 2026-09-24 round 1, seven findings: (1) the mark's skip is
        unreachable in the ordinary flow — a failed retry's roots go read live
        to the deferred lane, which returns them only at a turnover, where the
        mark is already stale; checked by hand, holds; to Edmond. (2) A failed
        retry's `ReadLive` leaves garbage past `B_max` to pressure and the exit
        once S65.10 drops the turnover's arm, against the rfc's "no colour of
        an abandoned trace is a verdict"; to Edmond. (3) The collector's
        give-back drops the marks section's clearings; waits on (1). (4) The
        met-roots walk read no position per block: fixed. (5) Branches no case
        drives (a met root's clearing, the clearings on the give-back paths,
        the advance check on marks): cases owed. (6) rfc text against the code
        in three places: owed. (7) `grow` refused at equality while the retry
        put B back over drawn blocks: fixed (`>=`).
      Edmond 2026-09-24 on (1): "согласен" to the recommendation — the mark
        and its skip go, the retry stays; bits 20–22 back to reserve, the
        marks section and F4's clearing moot, (3) with them. Read as that
        recommendation; he asked what and where the mark is first (byte 6,
        bits 20–22 of `RcHeader`'s flags) and did not name another option.
      Edmond 2026-09-24 on (2): `ReadLive`, the package's and the
        recommendation — `Unwalked` would trace the closure on the mutator's
        thread, and after S65.10 retry it at every take; the rfc reworded as a
        deferral. On the old case
        `the_batch::a_second_part_that_meets_its_budget_leaves_the_first_parts_verdicts_and_halves_k`,
        red because a part at B is retried and finishes: "тест устарел
        переделать". Rewritten as
        `a_part_that_meets_its_budget_with_the_retry_spent_leaves_the_earlier_verdicts_and_halves_k`,
        two long rings registered in turn, so that the contract it kept —
        `Unwalked` past the retry — stays covered (renamed again after the
        ruling on round 2, below:
        `a_part_past_b_with_the_retry_spent_defers_the_roots_it_met_and_the_batch_goes_on`). Findings (3) and (5) moot
        with the mark; (6) re-read in round 2.
      Critic 2026-09-24 round 2, over the removal, six findings: the removal
        clean in code, rfc and `classes.md`. (1) The claim of one failed retry
        per epoch and nothing on the owner's thread held only for the roots a
        retry met: a root of the closure it did not meet comes back `Unwalked`,
        traced in line until S65.10 and retried at its next take after it.
        (2) Halving K on a failed retry spreads its fixed cost over fewer
        roots. Edmond 2026-09-24 on both: the batch goes on — a part past the
        ceiling defers the roots it met read live and the next root opens a
        part — and K no longer halves, having answered the roots lost to
        `Unwalked`; he asked its price and that it be measured (S65.12).
        (3) rfc against the code: "every live root", the retry meeting
        `B_max` in the `Unwalked` list, K left on a refused or recalled retry,
        B with `B_max` bounding the arena; `TRACE_BLOCK_BUDGET`'s and the
        record's K doc corrected. (4) A retry the recall stops had no case:
        `a_recalled_retry_leaves_its_roots_unwalked_and_k_where_it_stands`,
        two surviving mutations now red; a dead `forget_the_budget_met`
        before each part removed, then restored once the batch went on past a
        failed retry, whose flag it clears. (5) "Read live to every root left"
        survived: `a_root_the_failed_retry_did_not_meet_opens_a_part_of_its_own`,
        red. (6) The package's other mentions of the mark: a banner and
        amendments in section 7 and in the premise of section 12. Seven
        mutations of the going-on red.
      Critic 2026-09-24 round 3, over the going-on: no code defect. (1) That
        the owner pays nothing holds only once S65.10 drops the turnover's
        arm: qualified in DECISIONS and the done-line, the rfc's own rule
        already arming nothing while a collector lives. (2) The collector pays
        per root, a part at B for each root no attempt of the grant met: rfc
        and DECISIONS priced so, S65.12's measurement given the spread shape.
        (3) Which failure goes on was pinned by nothing: a hook before a part,
        `a_recall_in_the_part_after_a_deferral_leaves_its_root_unwalked` (the
        flag's clearing red when dropped) and
        `a_retry_the_pool_refuses_leaves_its_roots_unwalked` (the collector's
        own block budget at zero from the retry's start, red without it); the
        budget read before the pool named in `trace_in_parts`. (4) Stale text
        in `batch`, `Verdict::Unwalked` and
        `every_part_draws_under_the_budget_of_its_own`, which now asserts no
        part met B. (5) The rfc's garbage sentence split in two.
      handoff: `worker::trace_in_parts` (the retry, the deferral and the
        going-on), `RETRY_BLOCK_BUDGET`, `size_the_next_batch` without the
        halving; six cases in `worker::tests::the_ceiling` and the rewritten
        `the_batch` case; seventeen mutations red over the three rounds. The
        gate of 2026-09-24: plain, three runs at eight threads,
        `hash-folding`, three `debug-journal` runs, all green (1,192 and
        1,201 passed).
- [ ] S65.10 `Unwalked` is no root of the collection over P (package commit 8)
      done: `Verdict::is_root_in(BatchForm)`; P = [`Unwalked` x] writes x back
        into R untraced; under pressure 63 `Unwalked` are still traced; the
        turnover's `arm()` gone outside `cap 0`, which makes true the rfc's
        "the re-offer arms the poll's collection only while the record names
        no living collector", false of the code since 2026-09-15;
        `rfc/model/gc/rc-cycle.md` states who finds and who judges, and when
        the mutator searches
      tier: T2 · role: Critic
- [ ] S65.11 The rfc read whole against the stage
      done: every amendment S65.2–S65.10 made is read in its place with its
        neighbours — no sentence of `rfc/model/gc/rc-cycle.md`, the handshake
        document or `rfc/model/classes.md` still describes the budget as the
        wait's bound, P in R's order or the turnover's collection over R; the
        citation checker over both repositories
        with no miss
      tier: T2 · role: Critic
- [ ] S65.12 `cap 0`, then the N-mutators-on-N-cores rig (package commit 9)
      done: the clamp lifted, the elder kept as the clock without takes, and
        the mutator collecting at its threshold and at the merge under
        `cap 0`; a cap set to zero at run time is met at the elder's next
        round: the round withdraws its standing list (the walk of
        `Standing::drop`, factored), releases a grant it reads back unserved,
        issues no request and serves no checkpoint while the cap is zero, and
        a trace already running finishes; a cap set back resumes the takes
        (`dev/S65-PLAN-CRITIC.md` F5); switched with an unanswered request,
        a consented grant, a running trace and P posted, and once more with
        a sibling — no stranded token, no record left linked, P intact, the
        clock still turning, the takes back after the restore; only with
        this built is the rig run, its third arm being `cap 0` itself, the
        in-line collection reclaiming what the collector would have (F6,
        `dev/S64-GC-IMPROVEMENT-ANALYSIS.md`, "Какие опыты нужны"), each
        thread placed on a named physical core; the three placements
        measured and recorded, and the constants L, M, M_b, M_c, D, N, X,
        N_b read off it, and whether a grant needs G, the package's bound in
        touched blocks (section 7), against a grant of 1,024 parts that
        holds 32–37 ms on `disjoint-wide-live` while the other mutators
        named to its collector wait (`dev/BENCHMARKS.md`, "S65.13 the batch
        in parts at a grown K"); and the price of a batch that goes on past
        a part at B rather than ending with the rest `Unwalked` (Edmond,
        2026-09-24, on S65.9's second Critic round, finding 1, asked to be
        measured): with a live closure past `B_max` and a garbage ring between
        B and `B_max` both in R, the garbage's bytes and the epochs they are
        held before a collection frees them, against the mutator's in-line
        time the `Unwalked` it would have traced costs, and the collector's
        time per grant the parts after a failed retry add, with the roots
        spread along a closure past `B_max` so that each opens a part at B
        (S65.9's third Critic round, finding 2)
      tier: T2 · role: Critic

## Then: arrays as a performance problem

Opened 2026-08-07; the representation is built (`dev/DECISIONS.md`,
2026-08-07 and 2026-08-11). The compaction threshold, `EQUAL_HASH_LIMIT` and
`CHAIN_LIMIT` stand on borrowed numbers until a machine resolves an effect
under this box's 1.5–3 % noise floor (`rfc/model/arrays-hashtable.md`,
"Open").

- [ ] **The string-key cancellation threshold.**
  done: the control-byte index against the chain at N of 56, 512, 4,096 and
  28,672 on string keys of realistic length, the deletion margin beside
  them, judged by `rfc/model/arrays-hashtable.md`, "Open" (1.5× on both
  lookups, margin not worse); the default changes or is recorded as kept,
  figures in `dev/BENCHMARKS.md`.
- [ ] **Whether an escalation raises an operations-visible signal**
  (`rfc/model/arrays-hashtable.md`, "Open").
  done: a decision with its reason in `dev/DECISIONS.md`, or a line saying
  the rfc answers it.

## The vocabulary

The rename closed 2026-09-02; three guards in `src/cycle/tests/` hold it
(`dev/DECISIONS.md`, "the vocabulary is held by three guards, one per
surface"). The cross-repository remainder is the `rfc` plan's step "Rewrite the
documents and the crate to the glossary".
Residues with no owner:

- [ ] **`promote` keeps `corpse`** for the glossary's *torn-down entity*,
  under an exemption in both metaphor guards.
- [ ] **`shadow::groups`, `group_bit` and `group_bytes` were never put to the
  glossary** (`dev/DECISIONS.md`, "an uncovered term is a gap rather than a
  local ruling").
- [ ] **The comment guard reads 14 words and one two-word term of the
  audit's mapping.** `refused` in its retired sense is not among them;
  `colour` stands 100 times in comments against the US-spelling rule (count
  of 2026-09-24).
- [ ] **Neither metaphor guard refuses a stale exemption.** The identifier
  guard has a test that fails on a file which has stopped offending; the name
  and comment guards have none.
- [ ] **`cycle::deferred_slot_reuse` outgrew its name.** `ActiveTrace` lives
  there and owns the scratch arena, takes the candidate batch and hands out
  rows, while the module header still describes the slot-return window alone.

## Beside the hashtable: the memory categories

The region reset gates the rename.

- [ ] **Rename the memory categories** in the rfc, the documents citing them
  and the crate, once the region reset decides what a region's contents
  carry (`dev/DECISIONS.md`, "`LongLived` goes out of use, and its rename
  waits for a mechanism"). `LongLived` is marked out of use on the enum.
- [ ] **The region reset.** What a region owns, when it resets, how the
  owner's death reaches its entities, and what promotion across a region
  boundary is (`rfc/model/memory/regions.md`). Until it exists
  `ll_string_new_dynamic` refuses `LongLived`, since nothing would reclaim
  such a string. Blocked on design.

## What is left of the old phase lists

Carried from the phase lists deleted with the 2026-07-24 snapshot; checked
against the code on 2026-08-13.

- [ ] **A3's factory half.** `factory(ctx, category)` takes no class, so it
  needs per-class generation; until then the generic path is
  `ll_object_new(ctx, class, category)`, and `ll_default_dispose` stands in
  for a generated `dispose`, which `rfc/runtime/object-lifecycle.md`'s "Only
  the GC reads layout as data" waits on. `clone`, `deep_clone`,
  `thread_clone` and `thread_move` are reserved for multi-threading.
- [ ] **A7, no zeroing by default** (`rfc/BACKLOG.md`, deferred
  optimizations): `ll_object_new` zero-fills the whole body.
- [ ] **`Lazy` (1) and `Box` (10) have no producer**: Box waits on the FFI
  surface, Lazy on the compiler. `ll_entity_die` routes `LAZY` to
  `ll_object_die`; `Box` reaches its `debug_assert!`.
- [ ] **The threshold arming policy.** The policy is the compiler's
  (`rfc/model/gc/strategies.md`, arm/fire). The critical reserve's third
  customer, a mutator whose gate is closed, is answered null until the ABI
  names the runtime progress operations a reserve would fund
  (`rfc/model/memory/critical-reserve.md`, "Mutator progress while
  collection is unavailable").
- [ ] **The move rule of unique ownership has no home.** The rfc ruling of
  2026-08-23 (`rfc/dev/DECISIONS.md`, "compiler logic leaves this
  repository's scope; a uniquely owned entity is not collected") strikes the
  birth count and makes the move rule compiler business owed a home outside
  `rfc`; this line holds it until that home exists.
- [ ] **Pure destructors, the P0 runtime step.** A specialized dispose for a
  class with no `__destruct`, and a raw-sever arm in reclamation for a
  component with no pending destructor (`rfc/model/gc/pure-destructors.md`,
  "P0 gains available without compiler work"); no ruling and no compiler
  needed, but the analysis predates `rc-cycle` (that document's status), so
  the step is revalidated against it first. The compiler tiers wait on the
  compiler.
- [ ] **Three `promote` tests claim Miri as their whole regression**
  (`dev/WORKFLOW.md`, Miri, "Three tests claim Miri as their whole
  regression").
  done: each seen failing under Miri against the mutation it names, or its
  comment corrected.
- [ ] **Strategy 1, the typed vector.** No producer, so the 1 → 2 transition
  waits on one (`dev/DECISIONS.md`, 2026-08-13).

## Residual / carried-over items

- [ ] **The review's cuts of 2026-09-21** (pass 1 over `c58d49f..128cefc`):
  `memory::heap::ll_thread_init` at 59 code lines (the unstarted life's
  unwind and the heap-funding tail become functions);
  `cycle::collect::collect_under_pressure` at 51 (the tail after
  `freed += taken` becomes `after_a_round`); depth 3 in
  `static_block::run_thread_exit_teardown` (`pop_the_newest` lifted out of
  the loop), `cycle::testing::traced_from_a_collector_thread` and
  `the_metaphors_the_comments_still_carry::retired_in`; six `#[test]` bodies
  at depth 3, one of them `critical::tests::where_the_first_touch_happens`
  with the fifth copy of a directory walker.
  done: each under its threshold or recorded as kept with the reason.
- [ ] **The review's cuts of 2026-09-23** (pass 1 over `128cefc..89bb0dc`):
  `Standing::checkpoint` at depth 4 (one record's pass becomes a function
  with early returns, the kept grant's serve another); `epoch::of_record` and
  `Standing::take_backlogged` inlined into their one caller each; in tests,
  32 bodies over 50 lines (largest `the_siblings`' birth-and-handover case,
  130, and `when_the_turnover_reoffers`' mature-member case, 128) and six at
  depth 3–4, most cut by shared fixtures: a ring deferred behind a keeper
  (four copies), a keeper's release-and-die pair (eleven), `keeper_class`
  (five files), the sleepers' start and end in `under_stress.rs`, the three
  hooks forwarding to one `OneShot`, the two `EmbeddersInterval` guards.
  done: each under its threshold or recorded as kept.
- [ ] **The turnover period `N` is unruled.** `epoch::BATCHES_PER_EPOCH` is
  YRC's 64 (Y9's dial, `rfc/model/gc/cycle/questions.md`); both costs are
  linear in it and pull opposite ways, measured on the test heap
  (`dev/BENCHMARKS.md`, "S37.5 what a turnover re-offers, and what a
  deferral costs"). Which side pays is Edmond's; the ruling goes to
  `dev/DECISIONS.md` with the constant, X beside it. Y9's
  minimum-over-stamped-members question changes no prune at `k = 1` and
  reopens with any `k` above 1
  (`mark::tests::the_pruned_share_against_a_survival_rate` under
  `pin_threshold`). `template.rs` builds its entity without reading the
  class flags, so a template class marked acyclic would miss `ACYCLIC_GATE`
  — conservative, left until one is marked.
- [ ] **The citation check does not read the journals.**
  `dev/tools/citations.py` walks `src/`, `benches/`, `docs/` and four files,
  so a journal's citation of a plan line is checked by nobody; two are dead
  — `dev/DECISIONS.md` 2026-09-15 ("A root the worker read live is never
  deferred") and 2026-09-18 ("The collector thread's spawn allocates through
  the global allocator").
  done: the journals join the checker's population and the two are
  re-pointed or cut.
- [ ] **A store of one request arena's entity into another's object.**
  `store_category_barrier` keys on category alone, so the pointer is stored
  raw. A destructor inside arena A's reset can reset arena B, whose
  `count_children` meets A's entity as an ordinary child: a non-COW child is
  promoted early (conservative); a COW child's count is raised by B's pass
  while the edge's record is freed with B's log, so A's reconciliation
  subtracts a reference a promoted holder still holds (Critic, 2026-09-13;
  nesting itself is ordinary, `dev/DECISIONS.md`, "one arena is reset once
  at a time on a thread, and a second entry is refused").
  done: a red test resets B inside a destructor of A's reset, or a
  demonstration that the shape is refused, in `dev/DECISIONS.md`.
- [ ] **A retained block's occupant freed from another thread inside the
  reset.** `retained::occupant_freed` subtracts from the low half of a word
  whose high half holds the pins, so a free before the reset has set
  occupant counts borrows from the pins (a `debug_assert`; a lost pin in
  release; with no pin, a wrapped word that makes `has_held_occupants` true
  for good), and the guard, `reset_window::absorbs_retained_free`, reads the
  freeing thread's window. Whether a promoted survivor is reachable from
  another thread mid-reset is unestablished (Critic, 2026-09-13).
  done: a red test from a second thread, or a demonstration of
  unreachability in `dev/DECISIONS.md`.
- [ ] **The twenty-sleeper case has no Miri run.**
  `the_standing_list::twenty_sleepers_stand_and_are_all_served_on_waking` is
  `cfg_attr(miri, ignore)` (killed at 50 minutes, twice); the order of
  service across passes at twenty entries is read by no Miri run. Either
  four sleepers under `cfg(miri)`, or a stand-in that moves the byte without
  a thread. Unpriced.
- [ ] **A round whose walk reads no record carries its checkpoint's
  readings.** `batches_served`, `saw_work` and the backlog fold in
  `read_one_record` and nothing drains them after `for_each_record`, so such
  a round reports no batch and its timer lengthens
  (`Standing::take_saw_work`).
  done: a drain after the walk, and a case over a collector whose records
  are all named elsewhere.
- [ ] **Two arms of the take's measurement are unbuilt**: a pool that sleeps
  and wakes, and the mutator's collection beside a round in flight
  (`dev/BENCHMARKS.md`, "What these arms are not" under the two S64.5
  entries of 2026-09-22). Neither moves a constant alone.
- [ ] **Two residues of the collector's birth.** The guard's case asserts
  the slot's mapping is exactly 2 MiB plus its guard, which an adjacent
  anonymous mapping of up to 64 KiB merging at its end would break (no
  source found in the test binary). A wake landing in the last instant of a
  consent wait is neither spent nor remembered, bounded by
  `FALLBACK_INTERVAL_MAX`. The untested `mprotect` and join failures and the
  unmeasured stack are in `cycle::worker::birth`'s docs.
- [ ] **The harness serves at a 2 s request wait**
  (`worker::testing::HARNESS_REQUEST_WAIT`, a thousand times the crate's
  2 ms), so only the ignored probes exercise the shipped bound;
  `worker::tests::Mutator::start` and `the_siblings`' round wait clear
  `POSTED` by hand, suspending "`FREE` promises an empty P" for those cases.
  A harness at the crate's wait needs consent from a real poll on a loaded
  box; a fixture that disposes pays a collection per batch. Unpriced.
- [ ] **The second heapless arm has no case off Windows.** A heap built and
  then refused by `tls::set` is left heapless by the same two lines as a
  refused allocation (`memory::heap::ll_thread_init`); `refuse_thread_heap`
  reaches the allocation alone and `tls::set` never refuses on Linux, so the
  one case (`…::a_thread_without_a_tls_slot_reports_instead_of_dying`,
  Windows-only) reads three of `a_life_the_allocator_left_heapless`'s six
  promises. The other three need a Windows run.
- [ ] **A component past the collector's block budget whose owner-side
  trace the pool refuses** circles P and R under pressure, arming a
  collection each round (unverified since the batch runs in parts). Whether
  a collector thread may spend its critical reserve and the unbounded
  spare-cell surplus are recorded in `dev/DECISIONS.md` 2026-09-16.
- [ ] **A weak map, the second death-subscriber kind**
  (`rfc/model/weak-references.md`; Edmond, 2026-09-07: it can wait). Shape:
  a GC-heap entity over `array::Table`, key uncounted and value counted; a
  target's weak-table row becomes a list in its reserved tag bits; the
  displaced value goes to `cycle::reclamation`'s queue inside a teardown
  (`rfc/model/gc/rc-cycle.md`, "Cycle finalization and reclamation", step 6)
  and is released inline elsewhere, told apart by a thread-local sink the
  teardown arms. Owed with it: an arena-resident key on the arena's weak
  log, and `HAS_WEAK_REFERENCES` cleared when the last subscriber leaves.
- [ ] **The ladder's refusal has nowhere to go.**
  `InsertOutcome::AdmissionDenied` ends as a null or `false` inside the
  crate, indistinguishable from memory pressure; `rfc/model/maps.md`, "Rung
  three, refusal" raises it as a catchable error, which waits on the
  exceptions work.
- [ ] **The equal-identity trigger's tag test has no test of its own.** In an
  array "the tag equals the incoming string's" names the same set as "not an
  integer key", so the change was verified by reading; `Map` is where the two
  differ, and the test is owed there.
- [ ] **The long-key slot itself.** `strong_hash`'s doc stands in for
  HighwayHash-64 behind a length threshold `rfc/model/strings.md` says is
  unmeasured; blocked on that measurement, with the strings work.
- [ ] **The publication fence's ARM64 price.** `refcount::publish_header`'s
  `fence(Release)` is a `dmb ish` on ARM64, unmeasured, and is measured
  before the first ARM64 build ships (`rfc/dev/DECISIONS.md`, "the
  publication fence lands before its ARM64 price"): `benches/lifecycle.rs`'s
  create/release pair on an ARM64 box, both arms in one session.
- [ ] **The per-process key's Windows randomness source.** `hash/process_key`
  is unix-only, `#[cfg(not(unix))]` a `compile_error!` naming this gap, until
  a session on the Windows box adds the source (`BCryptGenRandom` or an
  equivalent) and runs the gate there. Deferred by Edmond, 2026-08-17.
- [ ] **The collector's birth has run only on Linux.**
  `cycle::worker::birth`'s windows arm (`CreateThread`, `WaitForSingleObject`,
  `CloseHandle`) has run nowhere and cannot build until the key above has its
  Windows source; `pthread_setname_np` is declared for linux and android
  alone; aarch64 checks clean and has run nowhere. What a non-futex unix owes
  the locks is in `dev/DECISIONS.md`, "the collector thread is born by the OS
  entry, on a stack its slot keeps, and is woken by a word".
- [ ] **No ABI entry creates or mounts an arena.** An external caller can
  build an `LLContext` and reach the store barrier, but every `*mut Arena` in
  the crate is made by Rust code inside tests; an embedder needs that entry
  before anything outside this crate exercises the arena paths.

Collection coverage, open since 2026-09-14:

- [ ] **The collection's journal kinds.** `journal/kinds.rs` carries no record
  for a collection's begin or end (`dev/design/debug-modes.md`, §9.5);
  `cycle::collect` records `KIND_EXIT_RESIDUE` alone. A window over a
  collection needs its two ends and which path it took.
- [ ] **A member of a kind other than an object or an array is untested
  through the commit.** A reference box, a template and a class with cells
  outside itself each have an arm in `cells::trace_cells` and
  `cells::sever_cells`, and no case drives one through validation,
  finalization and reclamation as a member.
- [ ] **The refusal path over a component whose member's guard is its last
  reference.** Such a member is freed inside `GuardedComponent::release`, and
  the case that exercises the refusal has no such member.
- [ ] **Two shapes the deny run over a reset inside a collection never
  enters**: an emptied chain — `register` answering true for a block whose
  listed survivors all died inside the reset — and a destructor round that
  escapes something new. Each is one case in
  `cycle::collect::tests::what_a_collection_asks_the_allocator`.

Memory manager, still open. The readings behind the first five are
`dev/RESEARCH.md`, "Memory managers" and "rpmalloc"; each names what would
have to be measured first, and none is measured here.

- [ ] **Batch the cross-thread free, once a workload exists.**
  `Heap::free_remote` and `buffer_arena::post_remote` pay one CAS per item;
  snmalloc pays one per batch. The shape: a bounded thread-local staging
  buffer grouped by block on flush, each group chained through the dead
  slots and pushed by one CAS, flushed at thread exit (`deferred_free.rs` on
  `archive/pre-rc-cycle`); the price is return latency and peak RSS by the
  batch. A per-pair SPSC ring removes the atomic at memory per pair. Order:
  a test heap that frees another thread's objects in bulk, a measurement,
  then this.
- [ ] **Reallocate in place when the class does not change.** `stdapi::ll_realloc`
  allocates, copies and frees on every call, so 40 bytes to 48 costs a block,
  a `memcpy` and a free to move inside one 48-byte slot;
  `stdapi::ll_usable_size` already reads the class size. What comes first is
  a harness: `rptest` in `benches/standard.rs` never reallocates, and nothing
  calls the path while the `#[global_allocator]` install is owed.
- [ ] **Size classes between 8 KiB and one block.** Above `heap::MAX_SMALL`
  everything takes a 64 KiB block. Five classes (10880, 13056, 16320, 21760,
  32640) divide the payload without a tail, hold the worst case to
  1.33–1.5×, and are chosen past `MAX_SMALL` so `CLASS_LUT` stays 514
  entries; cost about 120 bytes per thread and a high block switch rate on a
  two-slot class (`dev/RESEARCH.md`, "rpmalloc"). First a footprint
  measurement in `blocks_out` and RSS past 8192.
- [ ] **The commissioning zero pass: delete it or name its production
  reader.** `Heap::refill` writes eight bytes into every slot of an entity
  block unconditionally, up to 4080 stores at the 16-byte class; the walker
  that needed the invariant, `for_each_entity_slot`, has no production
  caller since `rc-walk` went, while `heap.rs`'s module doc says the pass
  was built for `rc-walk` and outlives it because `rc-cycle` reaches its
  shadow rows by the same arithmetic over the same blocks. Whether a
  production reader still needs a free slot to read as refcount 0 is
  unsettled: either the pass goes with the doc corrected, or its reader is
  named and the zeroed-block flag of `dev/RESEARCH.md`, "rpmalloc" (a flag
  that names the stride it holds for, carried across recycling) is what
  would retire the cost. Refill runs about 0.00003 times per allocation on
  the steady-state benchmarks — a reading, not a measurement.
- [ ] **Return memory to the OS, and cache huge mappings.** A region is an OS
  mapping since `8208815` and `os::unmap` is used by the large-run path, so
  the item needs a reading on a test heap, not a mechanism: rpmalloc decommits free pages
  past a per-type threshold and caches freed huge mappings in a 32-slot cache
  evicted by age; ours never come back and `LARGE_RUN` unmaps on every free.
- [ ] Buffer *K*, the memory-pressure mode thresholds
  (`rfc/model/memory/buffers.md`) and the per-block dense/sparse reset
  threshold (`rfc/model/memory/arena-reset.md`) stand on borrowed numbers; by
  the ruling of 2026-09-19 (second) they are read on a parameterized test
  heap when a stage takes them, the entry naming the parameters.
- [ ] Allocation telemetry layer 2 / debug mode — full design in
  `dev/design/debug-modes.md`, build order its section 10; item 1, the event
  journal, is built, the rest unscheduled, the per-structure breakdown of
  collection's logical bytes (§8, an axis A feature; Edmond, 2026-09-01: not
  in a production build) among it.
