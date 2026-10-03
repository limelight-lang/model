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

Updated: 2026-09-30 · Active: S65, S67.

Review 2026-09-29: one day late. Pass 1, code `49b544a..25620f6` against the
thresholds (a function over 50 code lines, depth 3, a one-implementation
abstraction, a one-caller forward): ten functions over 50 lines, five of them
already over before the range, `worker::batch` at 160 the worst; four new
depths of 3; three one-use abstractions; three one-caller forwards; six
duplicates, the lanes' walk the widest; most of it in the refused arms'
code. The cuts are S65.46, after Edmond's ruling on those arms. The
independent pass over S67.2's commit found the `POSTED` key with two writers
and the byte-state table's lock on every test build's handshake, both
repaired in S67.2's follow-up. Pass 2: every arm of the range read equal to or
worse than the build without it, and the default build carries none of them;
whether the refused arms leave the tree is put to Edmond. Pass 3, the Critic
over the plan: eleven findings. Cut from the fog: the count of live readings
(built as S65.42's) and the long-iteration gate (S67.1's gates replace it).
S67's Done-when restated to what S67.2 found, S67.3's done-line to the
specification, S67.7 added for the protocol's instruments. Ruled by Edmond
the same day: S67's run gives a second verdict, best against plain of the
surviving scheme, so the two name the default build; the four refused arms
leave the tree (S65.47); S65.37, S65.38 and S65.39 cut, S65.41 moved to the
backlog line "If S67 keeps HG"; S65.28 and S65.31 closed; the rfc's lag a
backlog debt. Earlier reviews and the closed stages' summaries are in
`git log -- PLAN.md`.

**Closed stages are deleted whole** (rule 23.1.3), and what outlived each of
them is in the journals: `dev/DECISIONS.md` for a decision and its reason,
`dev/POSTMORTEM.md` for a trap, `dev/BENCHMARKS.md` for a measurement,
`dev/INDEX.md` and `dev/ARCHITECTURE.md` for the map. Deleted so far: S4
through S64, and S66. A number is never reissued, and the prose sections below the
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

- **D and HG, one or both; the four features of S65.36.** The Sage of
  2026-09-28 keeps D and would delete the loser after one bounded experiment,
  not keep both; Edmond is inclined to keep both. On the repaired rig
  (S65.42) the two best builds tie in every cell but one, the chain carrying
  little once a root waits by its readings, and the model recommends D, the
  scheme with less machinery at the same price. Edmond, 2026-09-28: keep
  both until S65.43 and S67's web loads are measured on them, then decide. Whether G1, G3, M1 and S1b
  stay or go is Edmond's (`dev/BENCHMARKS.md`, "S65.36: G1, G3, M1 and S1b").

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

- **Whether a root read live should leave every queue until it is
  decremented again.** Variant S (2026-09-26): the owner clears the candidate
  bit of a root read live and writes the epoch into byte 6; a later
  registration whose stamp is the current epoch goes to the deferred lane
  without a trace, an older one is traced. A live root nobody touches then
  costs neither thread anything, where the lane and the chain re-trace it
  every epoch (70,000 roots a mutator on `deferred-live-large`); a ring that
  dies in the stamped epoch is found at the next turn, as today. It amends
  `rc-cycle.md`'s rule that the maturation prune never spares a queue root,
  and the claim that a ring whose external reference goes registers a root is
  unproved: a case of that claim is its first check, and S65.28's run does
  not measure it.

- **How long a completed death behind a live entry holds its slot.** In R
  it waits for the retirement pass below 64 entries, or for the collector's
  batches to reach it above; behind a chain entry, under the proposal, for
  the next grant that posts anything, 32 such deaths, or its block's expiry.
  The rig's tally (`queue::withheld_by_an_entry`, `cfg(test)`) counts deaths
  withheld by any entry, not only those behind a live one; S65.28's load
  `live-churn-dies-by-count` makes it non-zero, and splitting it by the kind
  of entry is what would give the question a figure. Edmond,
  2026-09-26: "the memory of the object itself is held until the collector
  comes again — this needs thought".

- **The names of 2026-09-26.** R, P, the deferred lane and the chain, the
  arms' letters and the constants N and D each read two ways in this plan;
  Edmond chose role names that day (CandidateQueue, TraceResultQueue,
  EpochQueue by owner), which wait for `rfc/dev/GLOSSARY.md` and a rename
  stage of their own.

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
the free within S65.7's accepted figure (`dev/DECISIONS.md`, "a withheld
death pays for its count, and the marks bound what a grant withholds"), and
`ll_gc_maybe_collect` with its callees instruction for instruction the base's
or equal to it in one session, as S65.12 read it.
Miri owed at the stage's close, targeted (`dev/WORKFLOW.md`, "Miri"):
`epoch::turn_the_cell_of` and the record's accessors; `Reader::unread_up_to`,
the reading's reordered loads and `the_merged_lane`; the retirement pass under
the token and the reordered state line; `the_batch`'s cases; `the_recall`'s
stride and growth cases, its stops inside the trace, its two-mutator case's
raw list pointer and its two grant-behind cases; `the_held_stack`'s cases;
`when_a_withheld_stack_recalls_the_token`; `the_collectors_stamps`'s cases
(the collector's byte-6 writes, `refcount::stamp_as_read_live`,
`shadow::for_each_live_met_row`, the touched list's run reversed and put
back);
`the_cap_at_zero`'s cases, the ask's reading of a record under its
hold; `the_cap_set_under_work`'s, the list's withdrawal;
`what_the_poll_owes_the_queue::a_front_block_moved_under_a_reading_stays_in_the_circle`,
the hold's read-modify-write against a take (S65.22);
`queue::arm_to_retire_if_the_count_stands` and the test ring's
`fill_tail_block`, which writes the writer's copy of the front. The
collector's chain and its `ring::record_chain::tests` left the tree with HG
(S67.11).
Notes: dev/plans/S65.md

- [x] S65.1 Correct `queue.rs`'s claim that an entry's low four bits are clear
      handoff: `5014615`, three comments in `queue.rs`.
- [x] S65.2 The epoch cell is the collector's (package commit 1)
      handoff: `HoldLine::turnovers`; the poll 6.84–6.99 against 8.26–8.36 ns,
        `dev/BENCHMARKS.md`, "S65.2 the poll with the collector's epoch cell".
- [x] S65.3 A merged lane is taken at the next round, and K doubles only on a
      filled clamp (package commit 1a, in the Critic's form)
      handoff: `WriterLine::merges`, `HoldLine::merges_seen` its reading; no
        check is named; the record is `dev/plans/S65.md`, S65.3.
- [x] S65.4 Completed deaths of R are retired by a count (package commit 2)
      handoff: `MutatorCycleState::candidate_deaths`, `queue::retire_at_the_poll`;
        no check is named; the record is `dev/plans/S65.md`, S65.4.
- [x] S65.5 The machinery of the parts, the batch still traced once (package
      commit 3, first half)
      handoff: `worker::FinishThePosts`, `HAS_A_VERDICT`; no check is named;
        the record is `dev/plans/S65.md`, S65.5.
- [x] S65.6 The token's recall (package commit 4)
      handoff: `TraceToken::waiting`, read by `TraceScratchArena::inspect_position`;
        `dev/BENCHMARKS.md`, "S65.6 the token's recall".
- [x] S65.14 A grant held behind another mutator's batch is recalled
      handoff: `Collector::grants_recalled`; `dev/BENCHMARKS.md`, "S65.14 a grant
        held behind another mutator's batch".
- [x] S65.7 The marks by stack length (package commit 5)
      handoff: the price accepted, `dev/DECISIONS.md`, "a withheld death pays for its
        count, and the marks bound what a grant withholds"; `ForeignStack::count`.
- [x] S65.13 The batch runs in parts (package commit 3, second half)
      handoff: `worker::trace_in_parts`; `dev/DECISIONS.md`, "the batch runs in
        parts"; `dev/BENCHMARKS.md`, "S65.13 the batch in parts at a grown K".
        The parts went 2026-10-01 with S67.9 step (c).
- [x] S65.8 The live core stamped from a list (package commit 6)
      handoff: `cycle::live_list`; the `dev/DECISIONS.md` entry it cites is quoted in
        `dev/plans/S65.md`, S65.8; no check is named. The list went 2026-10-02
        with S67.9 step (f): the collector stamps itself
        (`cycle::collector_stamps`).
- [x] S65.9 The retry at the ceiling (package commit 7)
      handoff: `RETRY_BLOCK_BUDGET` in `worker::trace_in_parts`; the `dev/DECISIONS.md`
        entry it cites is quoted in `dev/plans/S65.md`, S65.9. The retry went
        2026-10-01 with S67.9 step (c).
- [x] S65.10 `Unwalked` is no root of the collection over P (package commit 8)
      handoff: `abd07f1`, rfc `ed68c6d`; `queue::BatchForm`, `Verdict::is_root_in`;
        `collect::tests::who_traces_an_unwalked_root`.
- [x] S65.11 The rfc read whole against the stage
      handoff: the rfc's amended documents and the ruling in `rfc/dev/DECISIONS.md`,
        the crate's `gc.rs` docs; citations 831/0, linkcheck 0.
- [x] S65.12 `cap 0` set before any work (package commit 9, first half)
      handoff: `worker/tests/the_cap_at_zero.rs`, nine cases, each red on its
        mutation; `dev/BENCHMARKS.md`, "S65.12 the poll under a cap of zero".
- [x] S65.15 A cap changed while collectors work (package commit 9, second
        half; `dev/S65-PLAN-CRITIC.md` F5)
      handoff: `Standing::withdraw_every_request`, the cap read in `serve_the_grant`;
        `worker/tests/the_cap_set_under_work.rs`, seven cases.
- [x] S65.16 The rig's placement and driver (structure agreed with Edmond
        2026-09-25)
      handoff: `worker/tests/the_rig.rs`, `dev/tools/rig.sh`; the smoke of 2026-09-25
        completed all 30 cells, every collector born pinned.
- [x] S65.19 The rig's figures, calibrated
      handoff: `worker::testing`'s figures; the known answers, `dev/BENCHMARKS.md`,
        "S65.19 the rig's figures, each read once on an input whose answer is known".
- [x] S65.20 The close over P reads no R (Edmond, 2026-09-25: "просто
        удалить вызов", on S65.17's run)
      handoff: `05f09a8` (rfc `741e39c`) and the Critic's fix `e6765ff`; gate green, each
        new case seen red on the old tree and on a mutation of its line.
- [x] S65.21 Decide how the slots of registered members a collection over
        P tears down come back — the close frees the run of completed deaths
        at R's front, and the poll's unlink waits on a reading's hold
        (`dev/DECISIONS.md`, 2026-09-26; measured in `dev/BENCHMARKS.md`,
        "S65.21 the front run against leaving the deaths in R"); on the way,
        the ring's stale tail copy (`02b25cf`, `dev/POSTMORTEM.md`).
      handoff: `dev/DECISIONS.md`, 2026-09-26; `dev/BENCHMARKS.md`, "S65.21 the front
        run against leaving the deaths in R"; the tail copy `02b25cf`.
- [x] S65.22 No block leaves R while a collector's reading holds it
      handoff: the hold asked in `ring::Writer::unlink_after_tail`, the interleaving
        case red on the old place; the record is `dev/plans/S65.md`, S65.22.
- [x] S65.23 The close of a collection over P frees the run of completed
        deaths at R's front (after S65.22)
      handoff: `compaction::free_the_front_run` and `rfc/model/gc/rc-cycle.md`; each
        case seen red on the mutation it guards; Critic 2026-09-26: no soundness hole.
- [x] S65.24 Build and measure A, B and C (Edmond, 2026-09-26: "one or two
        variants beside today's, no more than three; build them all, then
        prove by tests which is better"; and "implement, measure, show the
        result")
      handoff: "A stays" by the protocol, `dev/BENCHMARKS.md`, "S65.24 A, B and C on
        the rig" (`4fb9c05`) and "S65.24 A, B and C on a box with a PMU" (`dfeeb30`).
- [x] S65.25 Form D: a batch that proposed no set owes P's disposition and
        no trace window
      handoff: `collect::dispose_of_p`, `Arming::Disposal`, the release to
        `NOTHING_PROPOSED`; the Critic of 2026-09-26 found no soundness hole.
- [x] S65.26 The collector's chain (C, on top of S65.25)
      handoff: behind the feature `collector-chain`, its Critic on 61f286b; the
        blocking K sizing fixed with a case red on the old sizing.
- [x] S65.27 Find why C's drain frees nothing on `live-churn` (Edmond,
        2026-09-26: "найди")
      handoff: `VerdictWriter::room` re-reads the reader's front; the drain then freed
        91,488 of 91,488, and two cases are red with the re-read taken out.
- [x] S65.28 The collector's chain worked outside the token (Edmond,
        2026-09-26: "the collector can enter it without taking the token"; after
        S65.27)
      handoff: closed 2026-09-29, D stays by the rule and `CHAIN|s` unbuilt;
        `dev/BENCHMARKS.md`, "S65.28 D against H"; the record `dev/plans/S65.md`.
- [x] S65.31 A root read live waits in the deferred lane only once it has
        outlived an epoch (Edmond, 2026-09-27: "корни которые быстро умирают
        не ложатся в отложенные")
      handoff: closed 2026-09-29, not adopted; the arm left the tree with
        S65.47; the record `dev/plans/S65.md`, S65.31.
- [x] S65.32 H by generation: the collector keeps in its chain only a root of
        the second generation (Edmond, 2026-09-27: "моя схема — это H, которая
        не ложит в EpochQueue(GC) первое поколение, а только второе"; S65.31
        built the rule on D, which was not his scheme)
      handoff: `hold-by-generation`, `FinishThePosts::is_young` (`4b1bcff`);
        `dev/BENCHMARKS.md`, "S65.32 H by generation against H and D"; put to
        Edmond with the figures 2026-09-27, no ruling on adoption yet.
- [x] S65.33 Compare the schemes EpochQueue(M) and EpochQueue(GC), and
        tighten each (Edmond, 2026-09-27: "дай имена D и HG версиям
        алгоритма. попроси критика сравнить их между собой и попроси
        оптимизировать каждый ещё сильнее если это можно. но это уже не
        сейчас")
      names (Edmond, 2026-09-27): the scheme **EpochQueue(M)** is D, the
        default build (the mutator disposes of the collector's results and
        defers the roots read live into EpochQueue(M)); the scheme
        **EpochQueue(GC)** is HG, `hold-by-generation` (the collector holds in
        EpochQueue(GC) only a root that outlived an epoch, a younger one going
        the EpochQueue(M) way). H without the generation rule is superseded
        by it.
      handoff: three Critics and the Sage, 2026-09-27 (`dev/plans/S65.md`,
        S65.33): S1 and G2 built as S65.34 and S65.35, G1, G3, M1 and S1b
        measured first under S65.36; S2, M3, G4 and a sound dirty bit put to
        Edmond, each ruled to be tried (S65.37–S65.40); the S65.32 picture
        unchanged.
- [x] S65.34 The part's own root is not listed (S65.33, the Sage's S1; both
        schemes)
      handoff: built in every build, `deferral-by-generation` included, the
        Critic having found that build's root listed already by
        `list_a_root` (`dev/plans/S65.md`, S65.34); the rfc's "The live list
        of a batch" ("every row it left live") owed its clause, put to
        Edmond with the branch's landing.
- [x] S65.35 EpochQueue(GC) carries the generation in the entry (S65.33, the
        Sage's G2)
      handoff: built as ruled, `keep_unwalked` marking every entry as
        `keep_read_live` does; the mark's contract restated as "a reading
        before this entry's found the root live", the pressure re-offer and
        `ll_gc_reoffer_deferred` bringing the lane back without a turn
        (`dev/plans/S65.md`, S65.35)
- [x] S65.36 The rig's counters S65.33 owes, and G1, G3, M1 and S1b measured
      handoff: each arm loses or ties against its scheme by S65.28's rule
        (`dev/BENCHMARKS.md`, "S65.36: G1, G3, M1 and S1b"; run at
        `1146323`); Critic's two rounds in `dev/plans/S65.md`. Whether to keep
        or delete the four features is Edmond's.
- [x] S65.42 The best EpochQueue(M) and the best EpochQueue(GC): a root read
        live waits longer the more readings it has survived (Edmond,
        2026-09-28: "возьми лучшие оптимизированные версии HG D такие, чтобы
        все оптимизации были применены корректно. особенно очередь
        отложенных корней чтобы не читалась каждый раз по 10000 раз!!!!! 2
        круга критик и мудрец")
      handoff: `wait-by-readings` (`cb0c4bb`), the rig's repair (`74937ef`),
        `dev/BENCHMARKS.md` "S65.42 on the repaired rig"; Edmond 2026-09-28:
        both schemes stay until S65.43 and S67 are measured, then one goes.
- [x] S65.40 A third generation in EpochQueue(GC) (S65.33, G4; Edmond,
        2026-09-27: "попробуй")
      handoff: superseded by S65.42 (the Sage, 2026-09-28, Final): the ladder
        of waits carries it in both schemes; not built.
- [x] S65.43 The memory manager's draw of a fresh block hands the older
        lanes back early (Edmond, 2026-09-28: "запиши это как следующую
        оптимизацию, которая должна сократить объём памяти, что висит")
      handoff: closed 2026-09-29, not adopted, no diagnostic run; the arm left
        the tree with S65.47; `dev/BENCHMARKS.md`, "S65.43".
- [x] S65.44 A silent death on an old candidate releases the lanes (the
        Critic of S65.43, 2026-09-28; put to Edmond, not built)
      done: Edmond rules whether a per-thread count of non-final decrements
        on an entity already a candidate with two or more live readings,
        read at the poll, releases the lanes as S65.43's crossing does —
        the signal that fires on `deferred-then-dead`, where no block is
        drawn; if yes, built and measured as S65.43 is
      tier: T2 · role: Critic
      closed 2026-09-29, not built: Edmond, "нет не строим - это ухудшит
        release код" — the count stands on the release build's decrement
        path (`dev/DECISIONS.md`, 2026-09-29).
- [x] S65.17 The rig's run: three placements, three arms
      done: the placements of F6 and the S64 analysis (C−1 mutators and the
        collector on its own core, C mutators and the collector competing,
        C+1 mutators under `cap 0`) measured under each arm and recorded in
        `dev/BENCHMARKS.md`, the box's sharing named beside the figures; the
        constants L, M, M_b, M_c, D, N, X and N_b read off them; whether a
        grant needs G, the package's bound in touched blocks (section 7),
        put to Edmond with the figures against a grant of 1,024 parts that
        holds 32–37 ms on `disjoint-wide-live` while the other mutators named
        to its collector wait (`dev/BENCHMARKS.md`, "S65.13 the batch in
        parts at a grown K")
      tier: T2 · role: Critic
      amended by the plan review of 2026-09-26 (its Critic, F7): the three
        arms are the modes cap 1, cap 4 and cap 0, run on the build S65.28
        leaves as the default; the mutator is read by its instructions and
        the tail by the count of iterations over 200 µs, which replaced
        p99.9; of the constants only those a clause of Done-when or a
        backlog ruling needs are read, each against a stated criterion —
        the recall stride N for the recall clause, N_b and X for "The
        turnover period `N` is unruled"; the grant of 1,024 parts G is
        compared against is re-measured on that build, S65.27 having changed
        the batch's sizing.
      handoff: 2026-10-03 on the four-core cloud box, three usable cores, 63
        cells (`dev/BENCHMARKS.md`, "S65.17"): no take waited in any cell, so
        the stride N stands; X turns the arena loads' epochs and every web
        cell frees inside the drain; M is never reached; a grant on
        `web-arena-40k` lasts up to 185 ms with up to 582 blocks withheld at
        its release against `BLOCKS_MARK` 16. G is put to Edmond beside Q2.
        The grant of 1,024 parts is gone with the parts (S67.9 step (c)).
- [x] S65.18 The price of a batch that goes on past a part at B (Edmond,
        2026-09-24, on S65.9's second Critic round, finding 1)
      dropped 2026-10-01: the parts, B and `B_max` went with S67.9 step (c)
        on Edmond's ruling that the budget goes, so no batch goes on past a
        part at B
      done: with a live closure past `B_max` and a garbage ring between B
        and `B_max` both in R, measured and recorded: the garbage's bytes and
        the epochs they are held before a collection frees them, against the
        mutator's in-line time on the `Unwalked` it would have traced, and
        the collector's time per grant that the parts after a failed retry
        add, with the roots spread along a closure past `B_max` so that each
        opens a part at B (S65.9's third Critic round, finding 2)
      decides (the plan review's Critic, F10): whether a batch goes on past
        a part at B or ends at the first part past the ceiling; measured on
        the default build S65.28 leaves, since under H a deferred part past
        B goes into the chain; the load is S65.17's that meets `B_max`
      tier: T2 · role: Critic
- [x] S65.29 Decide whether a grant bounds the cross-thread frees it
        withholds (the plan review's Critic, F5; raised by S65.7's Critic)
      done: a slot another thread frees waits on its block's remote list
        while the token is held (`Heap::collect_remote`, through
        `deferred_slot_reuse::returns_are_withheld`) and no mark of S65.7
        counts it; either the remote frees count toward the blocks' mark at
        `collect_remote` and recall the grant, with a case, or a ruling in
        `dev/DECISIONS.md` exempts them from F3 with a figure of what a
        producer holds under the longest grant the rig reads
      tier: T2 · role: Critic
      handoff: 2026-10-03, counted: under a foreign holder the reclaim moves
        the remote stack onto the deaths' stack, each slot toward
        `DEATHS_MARK` (`deferred_slot_reuse::withhold_remote_frees`); the
        Critic broke a first form counted in pool blocks (five findings,
        `dev/plans/S65.md`, S65.29). One case, red without the move; one
        rewritten.
- [x] S65.47 The refused arms leave the tree (Edmond, 2026-09-29: "удаляй!")
      done: `deferral-by-generation`, `lane-back-by-blocks`,
        `r-first-under-a-small-chain` and `release-on-heap-growth` deleted
        with their cases, their gate builds and their `compile_error!` pairs;
        the commit to revert named in `dev/DECISIONS.md`; the gate green
      tier: T2
      handoff: deleted 2026-09-29, `bc642d0` the last commit holding them
        (`dev/DECISIONS.md`, "the refused arms leave the tree"); the gate
        keeps ten arm builds and one refused pair.
- [x] S65.46 The review's cuts of 2026-09-29 (the plan review, pass 1)
      done: each cut of
        `dev/plans/S65.md`, S65.46, that stands in the code left is made or
        recorded as kept with its reason; the thresholds read again over the
        cut functions
      tier: T1
      handoff: 2026-10-03, ten cuts made, eight gone with earlier steps, the
        cuts inside `collector-chain` and `wait-by-readings` kept until
        S67.6's verdicts decide those builds, the rig kept as S67's
        instrument (`dev/plans/S65.md`, S65.46, "The cuts, read 2026-10-03").
- [ ] S65.30 The stage's close: Done-when read clause by clause
      done: each clause of Done-when read on the build the stage leaves, by
        the test or figure that shows it; the Miri list above run and
        recorded, with S65.27's additions (`cycle::queue::verdicts::tests`,
        `worker::tests::the_batch` less its 16,000-member case; the chain's case
        left with HG, S67.11); the closed steps' debts ruled or
        given a backlog line (the plan review's Critic, F11): S65.15's
        unpinned sibling and a take waiting on `COLLECTOR` across the cap's
        flip, S65.16's `partly-overlapping` remnant freed only across a
        turnover, S65.23's k-th free past the first never fault-injected;
        then the Code Reviewer over the stage
      tier: T2 · role: Code Reviewer

## S67 — A web-server load on the rig  [in progress]

Goal: the schemes D and HG are compared on the memory load a request
server puts on the runtime, instead of on the rig's synthetic rings alone
(Edmond, 2026-09-27: "я хочу кейс web-сервер - типичная нагрузка на память
что наблюдается на сервере").
Done when: `web-arena` and `web-heap` run on the rig with their parameters
cited to sources or marked as assumptions, the rig's idle poll no longer
hides what a sleeping mutator costs (S67.2: it pays no token wait; the
requests' and `POSTED`'s standing times and the returns withheld through the
sleep are read instead), and D and HG are measured on them by a rule fixed
before the run, whose two verdicts name the default build — the scheme,
and whether it takes its best build's features; the figures put to Edmond,
who then drops one
(Edmond, 2026-09-28: both stay until S65.43 and S67 are measured; H is not
an arm).
Notes: dev/plans/S67.md

- [x] S67.1 Decide the load's parameters and the protocol
      handoff: `dev/plans/S67.md`, "The loads" and "The protocol (S67.1,
        before any run)", both through two Critic rounds.
- [x] S67.2 A mutator that blocks without polling
      handoff: `LL_RIG_WAIT_WITHOUT_POLL=1`, `sleep_without_poll` and the
        standing columns (`dev/plans/S67.md`, S67.2).
- [x] S67.3 Request graphs: lifetimes, payload and sizes
      handoff: `worker/tests/the_web_loads.rs`, `Plan`, `Request` and `Garbage`
        (`dev/plans/S67.md`, S67.3, with what S67.5 and S67.7 owe).
- [x] S67.4 Long-lived state: a shared core, a cache and sessions
      handoff: `LongLived` in `worker/tests/the_web_loads.rs`, built and
        registered to the steady state the simulation of the Critic's finding
        2 reads; fifteen cases, eighteen mutations red. S67.5 owes the list
        at the end of `dev/plans/S67.md`, S67.4, among it an epoch turned in
        the drain: a state let go is freed only after a turnover.
- [x] S67.5 The loads `web-heap` and `web-arena`
      done: both loads in the rig's table; `web-arena`'s request objects in
        the mutator's `Arena`, reset at the request's end with the release
        log; a smoke cell of each completes under D and HG and their best
        builds; what S67.3 and S67.4 left it, listed at the end of their
        sections in `dev/plans/S67.md`, built or read — among it the
        columns, the no-remote-free assertion, the ceiling in bytes, the
        long-lived state in the mutator's setup and the parts at N = 150k
        that meet `RETRY_BLOCK_BUDGET`; the let-go state freed after the
        drain's readings by a turnover by hand, the drain itself left to the
        scheme's own turnovers (S67.5's Critic, 2026-09-29)
      tier: T2 · role: Critic
      Critic 2026-09-29: ten findings on the proposal, among them
        `web-arena`'s setup registering `web-heap`'s steady state, a
        retained-blocks gate the live session writes swamp (the protocol's
        gate amended to the blocks less theirs) and `web-heap` moved to
        N = 150k; each answered in `dev/plans/S67.md`, S67.5.
      review 2026-09-29 (independent, of the code): the setup's and the
        teardown's collections in the cell's figures, and six more; each
        repaired or answered there.
      handoff: `Variant::Arena` and `a_web_mutator` (`the_web_loads.rs`,
        `the_rig.rs`), 22 cases, the web columns; the smoke cells of the
        three loads under the four arms complete, and no arm frees either
        load's garbage inside the drain (`dev/plans/S67.md`, S67.5, the smoke
        cells), put to Edmond before S67.6.
- [x] S67.8 The collector's operations in the debug journal
      done: under `debug-journal`, beside its births and deaths, a kind of
        record each for a root's registration in R (first, or a candidate
        already), a batch's start (its roots and form), a batch's end with its
        reason (a recall, B met, a refused allocation, run to its end, and the
        rest of `trace_in_parts`' exits), a root's verdict (proposed, read
        live, count zero, unwalked), a root deferred, written back to R and
        offered again at a turnover, a turnover, and a free by the mutator's
        disposition; a count by kind beside the ring, which the rig's line
        prints, since a cell's millions of events outrun a ring of records;
        each kind read once on an input whose answer is known (Edmond,
        2026-09-29: "логгер операций GC ... жить в условной компиляции с
        опцией"; the kinds agreed the same day)
      tier: T2 · role: Critic
      handoff: kinds 12–23 (`COLLECTOR_KINDS`), `journal::counts`, the rig's
        `j_loop_*`/`j_drain_*` columns; the diagnostic cells of 2026-09-30 in
        `dev/plans/S67.md`, S67.8: `web-heap` batches 18k roots against 1.6M
        registrations and proposes none, `web-arena`'s loop batches all end
        recalled inside their first part.
- [x] S67.7 The protocol's instruments
      done: each instrument of `dev/plans/S67.md`, "Instruments owed", that
        S67.2–S67.5 do not build is in the rig — among them the arrivals'
        queue and the latency from arrival, the counted spin and its
        calibration, the void reading, the collector threads' counters, the
        warm-up's snapshot and the X turns against the batch turns — each
        read once on an input whose answer is known; a smoke cell of each
        web load reads a collector's birth before any deciding run (S67.2:
        at a 10 ms pace a load registering little births none)
      tier: T2 · role: Critic
      handoff: `LL_RIG_ARRIVALS`, `SpinCost`, `WindowEdge`, `arms.sh` phase
        `web`, `paired_excess.py` (`dev/plans/S67.md`, S67.7); pilots set the
        interarrivals 29.68, 29.46 and 108.6 ms; smoke cells: `web-heap` hits
        the garbage ceiling, best D's `instructions_a_request` spreads 48M to
        27M between two cells and its A/A p99.9 is 657 ms, every cell void on
        a shared box — put to Edmond before S67.6.
- [x] S67.9 The collector's progress guarantee — first, before S67.6
      done: garbage behind a live component larger than B, and garbage whose
        trace outlasts the mutator's poll interval, are each freed within a
        bounded number of turnovers on the web loads and on a case of each;
        the step first finds what earlier stages decided for progress (the
        B_max retry, the recall bounding the wait, the live list stamping the
        core for the next trace to prune at, the deferred lane's turnover) and
        why none holds here — a part past B or recalled lists no core, so no
        later trace prunes, and the same trace repeats every turnover
        (`dev/plans/S67.md`, S67.5 "why nothing is freed", S67.8's diagnostic
        cells) (Edmond, 2026-09-30: "это баг алгоритма. Его надо решать
        первым", and he held it decided earlier)
      tier: T2 · role: Critic
      handoff: closed 2026-10-03 on `fb08f9e` (`dev/plans/S67.md`, S67.9,
        "done-line read"): one trace a batch, the posted set, two recall
        levels, the collector's stamps, the epoch turned by proofs; garbage
        behind the state and behind cut traces freed within one X on
        web-heap-150k and web-arena-40k. Open: Q2 and Q1 to Edmond; the
        chain of rings to idea (ii).
- [x] S67.6 Measure D and HG on the web loads
      done: the run by S67.1's rule, the arms fresh and interleaved, the
        figures in `dev/BENCHMARKS.md`, the verdict put to Edmond
      tier: T2 · role: Critic
      handoff: 2026-10-03 on the four-core cloud box, cap 1 only and CPU in
        place of instructions (`dev/BENCHMARKS.md`, "S67.6"): best HG wins no
        cell and is dropped by the rule; best D against D wins no cell on
        CPU, while D fails the drain's absolute gate in six of fifteen cells
        and holds 6.5 times bestD's mean garbage on `web-heap`; the latency
        gate is below the box's own A/A excess. Both verdicts put to Edmond;
        his answers decide HG's removal and the default build.
- [ ] S67.10 The proof-epoch collector becomes the default build
      done: `wait-by-readings` folded into the code unconditionally, the
        plain build's one lane and its cases gone (Q3: a case pinning the
        replaced mechanism is rewritten or deleted), the feature removed from
        `Cargo.toml` and the gate, the rfc amended where it states one lane;
        the gate green (Edmond, 2026-10-03: the second verdict, "думаю да")
      tier: T2 · role: Critic
- [x] S67.11 HG leaves the tree (Edmond, 2026-10-03: "да, удаляй HG")
      done: `collector-chain`, `hold-by-generation` and
        `death-check-back-off` deleted with the chain, `ring::record_chain`,
        their cases, the rig's chain columns and journal codes, and the lane's
        mark `REOFFERED_MARK` that only HG read; the commit to revert named in
        `dev/DECISIONS.md`; the gate green
      tier: T2

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
against the code on 2026-09-24.

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
- [ ] **The critical reserve's third customer.** Who arms a collection is
  settled (`rfc/model/gc/strategies.md`, "Collection requests and triggers").
  The reserve's third customer, a mutator whose gate is closed, is answered
  null until the ABI
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

- [ ] **The `rfc` lags the default build** (Edmond, 2026-09-29: "в долг
      запиши"). `rfc/model/gc` has no `NOTHING_PROPOSED`: form D's batch that
      proposed nothing releases to it (`cycle::collect`'s `dispose_of_p`),
      where the `rfc` describes the release to `POSTED` alone; and
      `rc-cycle.md`'s section on the live list listed every row a part left
      live, where the code left the part's own root out since S65.34. The
      live list went with S67.9 step (f), and its section with it
      (2026-10-02); form D's release is amended once S67's verdict keeps D.

- [ ] **The review's cuts of 2026-09-26** (pass 1 over `89bb0dc..49b544a`,
  with the comment reviewers' findings before the push): production —
  `worker::batch` 148 lines (the chain's preamble, the shares, the sort,
  the trace's timing and the tail as functions), `cells::trace_cells_until`
  87 at depth 4 (the array's arm out), `compaction::compact` 81,
  `worker::trace_in_parts` 80 at depth 4 (retry-or-defer out),
  `serve_the_grant` 60 (its release guard at module level, one
  `released_byte(posted, proposed)` shared with `verdicts::testing`),
  `mutator_record::take_record` 59, `token::take_recalling` 57 at depth 4,
  `ll_gc_maybe_collect` 56, `collect_under_pressure` 56,
  `make_returns_withheld_under_a_foreign_trace` 54, `Standing::checkpoint`
  54, `block_pool::put` 56; `RecordChain::check` at depth 5 and its
  `(front, tail)` pair read in five places (a `span` helper);
  `for_each_met_root` and `shadow::for_each_met_row_where` at depth 4;
  one-implementation `GrantsBehind`, `PartsOutcome`, `ChainPosts`; forwards
  `chain::peek_the_ready_part`, `commit_the_ready_part`,
  `shadow::for_each_live_met_row`, `take_under_pressure`, `dispose_of_p`,
  `Reader::unread_at_most`; `chain::DEATHS_TO_POST`, read only by its own
  assertion. Tests — `the_rig`'s `fields` 321 lines, the calibration 216,
  `a_mutator` 165 at depth 4, its 19 loads spelled field by field (a base
  and `..BASE`); the traced-batch bracket written 14 times and the token-wait
  stamp 4 times (two helpers); `what_a_grown_k_costs` at depth 5; the
  fixtures `what_a_batch_without_a_proposal_owes` copies from
  `what_the_byte_arms` (to `collect/tests.rs`); the chain's `ignore` reason
  written 25 times; `arms_table.py` and `two_arms_table.py` in parallel
  copies; whether `what_a_take_costs`'s short-take retries still meet a
  short take now that P's room is read off the reader's front, unverified.

- [ ] **Garbage behind few roots signals no collector.** A collector is born
  by the poll's signal on a filled block of R, 8,135 entries, so a load that
  registers one root an iteration holds its garbage until memory runs short
  or the thread exits: `one-large-root` stood at 392 MB with none freed in
  the rig's calibration (`dev/BENCHMARKS.md`, S65.19), and S65.16 read
  790 MB/s standing over three mutators. Done when a bound on the bytes
  standing before a collector is signalled is built with a case, or a ruling
  in `dev/DECISIONS.md` names the pressure path as the bound with the figure
  (the plan review's Critic of 2026-09-26, F6).

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
- [ ] **The turn's ratio is unread by Edmond.** The batch count `N_b` went
  with S67.9: the epoch turns when the collector's work reaches
  `epoch::SPENT_PER_PROOF` (2) times what its proofs cost, or at X
  (`worker::EPOCH_INTERVAL`), X waiting for twice the proving walk
  (`dev/DECISIONS.md`, 2026-10-03). The ratio 2 is the Sage's and goes to
  Edmond with the figures of `dev/plans/S67.md`, S67.9 (ratios 1, 2 and 4
  read on `web-arena-40k`, moving nothing within the spread). Y9's
  minimum-over-stamped-members question changes no prune at `k = 1` and
  reopens with any `k` above 1
  (`mark::tests::the_pruned_share_against_a_survival_rate` under
  `pin_threshold`). `template.rs` builds its entity without reading the
  class flags, so a template class marked acyclic would miss `ACYCLIC_GATE`
  — conservative, left until one is marked.
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
- [ ] **A root the collector cannot walk circles P and R.** A root sent back
  `Unwalked` — every grant recalled by a mutator freeing past a mark, or a
  pool that refuses every arena of the collector's while the mutator's own
  allocations succeed — is written back into R untraced by the collection
  over P and taken by a later batch, so it and the garbage behind it wait for
  a shortage of memory or the exit; the mutator pays a collection over P per
  turn (`rfc/model/gc/rc-cycle.md`, "The collector's batch"). How often a
  workload reaches it is unmeasured. Whether a collector thread may spend its
  critical reserve and the unbounded spare-cell surplus are recorded in
  `dev/DECISIONS.md` 2026-09-16.
  done: a case that recalls every grant, the turns it takes and what the
  mutator pays per turn, read and recorded, and a ruling on a breaker or on
  keeping the wait.
- [ ] **The collection over P opens a window with no proposal to trace.** A
  P holding only `ReadLive`, `ZeroCount` and `Unwalked` entries still opens
  `ActiveTrace`, traces zero roots and commits an empty membership before the
  disposition, which `retire_candidates_and_dispose_of_verdicts` makes
  without a window (Critic of S65.10, finding 6).
  done: `what_the_poll_costs`-style probe of that collection with and without
  the window, both arms in one session; the window skipped if it pays, or
  the figure recorded as kept.
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
