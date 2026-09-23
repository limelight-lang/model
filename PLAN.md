# Plan

Destination: `ll-model` is the runtime the compiler links — the memory
manager, the object model and the cycle collector built to the `rfc`'s
design and calibrated on parameterized test heaps, each reading naming its
parameters.

Implementation plan, re-sorted last on 2026-07-24 against the object-layout
redesign of 2026-07-22.

Design lives in `rfc` and is authoritative — read before coding, do not
re-derive: `model/classes.md`, `model/values.md`, `model/lowering.md`,
`model/gc/rc-cycle.md`, `model/gc/cycle/questions.md`, `model/memory/ffi.md`,
`runtime/object-lifecycle.md`.

The `rfc` repository carries its own plan at `dev/PLAN.md` for work that lands
in the specification rather than in this crate.

Updated: 2026-09-23 · Active: S65.
S65 opened on 2026-09-23 from Edmond's rulings of the day: the collector
finds and the mutator judges, the epoch clock is the collector's, and a
recall of the token replaces the budget as the bound on the mutator's wait
(`dev/DECISIONS.md`, "the collector finds and the mutator judges, and a
recall of the token bounds the mutator's wait instead of the budget"). The
two fog lines it answers — the take that meets the block budget, and the
mutator collecting from R while the collector goes on — left the fog with it,
and the two shipped defects filed that morning are its steps S65.2 and S65.6.
S64 went on 2026-09-22, the day its design was accepted: a mutator whose
candidate ring stands non-empty below the serve threshold has it taken as an
ordinary batch once `STANDING_INTERVAL` of the collector's own clock has
passed, an unanswered take leaves its request standing on the record, the
batch's form is read off the ring under the grant, and a round's spending on
consent waits that expire is capped (`dev/DECISIONS.md`, the four entries of
2026-09-22; `dev/design/a-standing-r-is-taken-after-n-rounds.md`; the `rfc`
states the take at `rfc/model/gc/rc-cycle.md`, "Signals"). Its six steps closed with their
Critics, three Sage rulings and the Code Reviewer over the stage, whose one
defect — the handover renaming a record a standing request was linked on —
is `dev/POSTMORTEM.md`, "a safety property derived from one mechanism, and a
second mechanism that reached the same state"; the figures are
`dev/BENCHMARKS.md`, "what sleeping sub-threshold threads cost a round",
"what a take costs by the shape of its roots" and "a second producer behind
the sleeping threads". S63 went on 2026-09-22, the day after
its design: the collector's standing requests live on the records as a list
with no capacity, read at a checkpoint only after a byte event and serving
one grant per pass, the silent mark gone (`dev/DECISIONS.md`, "the standing
request lives on the record, the checkpoint serves one grant, and no count is
capped"; `dev/design/the-standing-request-lives-on-the-record.md`, the
`rfc`'s handshake document amended); its three steps closed with their
Critics and the Code Reviewer paid, the two sleeper probes re-run
(`dev/BENCHMARKS.md`, "the sleeper probes on the standing list"). S62 went
with it, a design stage: the take of a standing R after an interval of the
collector's own is the accepted algorithm, and S64 below builds it
(`dev/DECISIONS.md`, "a standing R is taken after an interval of the
collector's own, and no request count is capped";
`dev/design/a-standing-r-is-taken-after-n-rounds.md`). S61 went on 2026-09-22, the day it opened:
the sibling-birth red of the journal gate was the harness's stand-in for a
disposition racing the round it stands in for, found by stretching the other
mutator's tick and repaired by clearing every mutator's `POSTED` by hand
before each backlog wake (`dev/POSTMORTEM.md`, "a wake inside the other
mutator's tick meets a backlog of one"); the Critic over the diagnosis and
the Code Reviewer over the stage are paid, and what each found is in that
entry. S60 went the same morning with its six steps closed and both gates
paid; its ruling stands in `dev/DECISIONS.md`, "a quiet thread's turnover is
the collector's to ask for", the figures in `dev/BENCHMARKS.md`, "S60.6 what
the poll costs with a deferred record standing". The destination's last mile
— the compiler that links this crate — is outside this plan: `rfc/BACKLOG.md`,
"The big one", and the front end in `limelight`.

Review 2026-09-21: overdue by a day, the hook reading `dev/PLAN.md` and never
this file. Pass 1, code `c58d49f..128cefc` against the thresholds, 203
functions touched: `heap::ll_thread_init` at 59 code lines,
`collect::collect_under_pressure` at 51, depth 3 in `mark::mark`'s tracer
closure and `static_block::run_thread_exit_teardown`, two test helpers and six
`#[test]` bodies at depth 3; the cuts are the backlog line "The review's cuts
of 2026-09-21". Pass 2: the period's algorithms are Edmond's rulings, so a
simpler form is a premise change for him. Pass 3, the plan against its
destination: "calibrated on a Phase-D corpus" was refuted by the ruling of
2026-09-19 (second) and is amended to that ruling's terms; the five backlog
lines that waited on the corpus or on a workload are re-stated on the test
heap; the bounded sweep's line, the futex paragraph of the birth's line, the
accelerator sentence and the header's copy of the S36 residue repeated a
journal entry or each other and are cut; the language-runtime, Phase-D,
proxy, interceptor and provenance lines named work no stage of this crate
can be drawn from and go, the per-structure line folds into the telemetry
line; the fog line on a cross-arena store, overtaken by the rulings of
2026-09-13, becomes a test debt below, and the three fog lines the `rfc`
owes moved to `rfc/dev/PLAN.md`'s fog. S37's own gate ran the same day and
found the ignored turnover load red at the new threshold
(`dev/POSTMORTEM.md`, 2026-09-21).

Review 2026-09-18, second: pass 3 by the Critic over the plan as rewritten
by S57.7 — its findings and their disposition were in that step, deleted with
S57 the same day. Pass 1 over
the period's code is void, the period's only source changes being comments;
pass 2 likewise.

Review 2026-09-18: the first one recorded here, and overdue — the hook reads
`dev/PLAN.md` while this plan sits at the repo root, so nothing named the
period. Code of 2026-09-15 to 2026-09-17 (`abfba48..c58d49f`) against the
thresholds: eleven places, the largest `worker::serve` at 80 code lines where
the same function was 42 before the slice, `collect::collect_under_pressure` at
79 and control depth 4, `worker::thread_body` at 77; they became S37.8, closed 2026-09-18
(`dev/BENCHMARKS.md`, "S37.8 the review's cuts leave the poll where it was"). Two
proposed deletions were refused — `make_withheld_returns_before_the_retry`
states a precondition its callee's `# Safety` does not and carries two callers,
and deleting `collect_off_the_poll` moves a contract paragraph into `gc.rs` and
re-points eight `As [...]` citations. The algorithms of the period are Edmond's
own rulings, so a simpler form is a premise change for him rather than a review
finding. The plan lost S40, whose goal was discharged on 2026-09-12, ten closed
steps, the obituary of nine deleted stages, two fog lines and a cross-cutting
section duplicated 640 lines from its twin; the nine orphan debts that cut would
have dropped were filed first, in `dev/DECISIONS.md` (four entries),
`dev/POSTMORTEM.md`, `dev/WORKFLOW.md`, `dev/RESEARCH.md` and, since S37 went,
the backlog line "What S37 named and left".

**Closed stages are deleted whole** (rule 23.1.3), and what outlived each
of them is in the journals rather than here: `dev/DECISIONS.md` for a
decision and its reason, `dev/POSTMORTEM.md` for a trap,
`dev/BENCHMARKS.md` for a measurement, `dev/INDEX.md` and
`dev/ARCHITECTURE.md` for the map. Deleted so far: S4 through S64, every
number this plan has spent. A number is never reissued, so a
stage added later sits where it is to be done rather than where its
number falls, and the prose sections below are the backlog stages are
drawn from.

**Every cycle-GC step has one review gate, and the Sage is the escalation**
(Edmond, 2026-09-10 and 2026-09-12: the Critic first, the Sage only for what
the model cannot answer itself). The pre-change baseline the step would erase
— operation count, manager-allocation budget, cache working set, lifetime and
refusal model — is recorded in the step before the first edit; a red test is
seen failing; the Critic reviews the repair and its mutations before the step
is checked, and its findings are recorded in the step. This applies to every
step of a cycle-GC stage; one broad review does not waive a later step's gate. The rest of
the routine — the gate, Miri in slices, the citation checks — is
`dev/WORKFLOW.md`.

## Fog

A line here is an unresolved question rather than a step: it carries no
criterion, and it leaves when it gets one or when it is ruled on. The three
lines that ended in a sentence the `rfc` owes — which destructors "ran
anywhere" counts, the object handed to a survivor, the exit's safepoint word
— moved to `rfc/dev/PLAN.md`'s fog on 2026-09-21.

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
the review chain behind it is `dev/CYCLE-SPLIT-*.md` and
`dev/S64-GC-IMPROVEMENT-ANALYSIS.md`. Each step is one commit of the package's
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
Done when: the mutator's collection over P traces no `Unwalked` root, no poll
arms a collection over R whole outside `cap 0`, a recalled grant returns the
token within N inspected positions, one bounded post and one arena reset,
and the poll and the free are where `dev/BENCHMARKS.md` S60.6 and S38.3 put
them.

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
        after; `what_the_poll_costs` within S60.6; `rfc/model/gc/rc-cycle.md`
        says the epoch clock is the collector's cell, advanced by its batches
        or its interval
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
        tracing nothing; the pass is `compact(None, false, None)`: it reads R,
        its overflow and P, and leaves the deferred lane to the turnover as
        today, so that its work is bounded by R below the threshold and not
        by the lane (`dev/S65-PLAN-CRITIC.md` F2); its red test red today; beside it a lane of 10⁵ live entries
        with D deaths in R, the pass's queue-work count no higher than R's
        length; the poll unmoved
      tier: T2 · role: Critic
- [ ] S65.5 The trace runs in parts (package commit 3)
      done: a reset to the watermark above the root copy, a `judged` bit, posts
        per part and `FinishThePosts`; the block budget stays one per grant
        in this step — the reset to the watermark returns the blocks and
        leaves `drawn` standing, so a grant of K parts reads no more than
        today's one trace (`dev/S65-PLAN-CRITIC.md` F3; S65.6 lifts it); the
        batch [r1, r3, r2] case posts exactly three verdicts; `disjoint-live`
        in 63 parts with the mutator's census at 0 roots and 0 rows; a token
        requested during the first of 63 parts waits no longer than on
        today's tree, measured; `rfc/model/gc/rc-cycle.md` says P is posted
        in the parts' order and R advanced once after the last
      tier: T2 · role: Critic
- [ ] S65.6 The token's recall (package commit 4)
      done: `waiting` set in `take_unless`, checked every N inspected
        positions in both phases — a position is a Vector element, a Hash
        entry, an object field or a hooked cell, counted whether or not it
        yields a reference, since `counted_box_cell` returns `None` for a
        scalar and a million-cell scalar array would otherwise be read
        between two checks (`dev/S65-PLAN-CRITIC.md` F1); `trace_cells` and
        `walk_concurrent` stop on `ControlFlow` (the customers' concurrent
        walk only, named as their change); the release after a recall is
        `FinishThePosts` (at most K verdicts and one advance) and one reset,
        and the contract names both; the per-part budget of the package's
        section 7 is switched on here, S65.5's per-grant bound gone; the
        budget's two comments say it bounds the arena; a pressure collection
        under a grant raised after the mark waits at most N positions, the
        posts and a reset — over a scalar Vector of 10⁶ cells, a sparse Hash,
        an object of null fields and a hooked storage, the interval measured
        from the request to the release; `rfc/model/gc/rc-cycle.md` states
        the recall and the handshake document records `waiting` as a hint
        beside E10
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
        return under it, dropped under pressure; the collector reads
        `waiting` every N rows of the touched-list walk that writes the list
        and leaves the part unstamped when it is set, so that the list adds
        no unbounded work before the release (`dev/S65-PLAN-CRITIC.md` F1,
        second instance), with a recall raised inside that walk measured;
        after a take over `overlapping-live` every member carries the stamp
        and the next take prunes; the handshake document records the list
        and the stamp beside E12
      tier: T2 · role: Critic
- [ ] S65.9 The retry at the ceiling and the parking (package commit 7)
      done: a part failing at B retried at `B_max` in the same grant, once per
        grant; the roots that attempt met parked in the chain's second section
        with a mark in byte 6 bits 20–22; the second section carries two
        kinds of entry, park and unpark, and the collector writes an unpark
        entry for every root it read marked whose part then finished, so
        that the mark's clearing rides the section the mutator reads whole
        (at most K entries, one block) and never the optional live list
        (`dev/S65-PLAN-CRITIC.md` F4; Edmond's yes, 2026-09-23); a section
        block the pool refuses leaves the mark until a grant with a block,
        named as the limit; a 300-block ring judged in one grant; x parked,
        the epoch advanced, the retry finished with the list's allocation
        refused and separately with L consumed, then four epoch tags on —
        the old mark gone after the finished retry and no attempt skipped;
        `rfc/model/classes.md` names bits 20–22 of byte 6 as the parking
        mark, the owner its writer
      tier: T2 · role: Critic
- [ ] S65.10 `Unwalked` is no root of the collection over P (package commit 8)
      done: `Verdict::is_root_in(BatchForm)`; P = [`Unwalked` x] writes x back
        into R untraced; under pressure 63 `Unwalked` are still traced; the
        turnover's `arm()` gone outside `cap 0`; `rfc/model/gc/rc-cycle.md`
        states who finds and who judges, and when the mutator searches
      tier: T2 · role: Critic
- [ ] S65.11 The rfc read whole against the stage
      done: every amendment S65.2–S65.10 made is read in its place with its
        neighbours — no sentence of `rfc/model/gc/rc-cycle.md`, the handshake
        document or `rfc/model/classes.md` still describes the budget as the
        wait's bound, P in R's order, the turnover's collection over R or
        bits 20–23 as reserve; the citation checker over both repositories
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
        N_b read off it
      tier: T2 · role: Critic

## Then: arrays as a performance problem

Opened 2026-08-07 at Edmond's request; the representation work is built and
its reasoning is in `dev/DECISIONS.md` (2026-08-07 the entry, 2026-08-11 the
head). What is left is measurement. Three constants — the compaction
threshold taken from Zend at about 3 %, and the flood ladder's
`EQUAL_HASH_LIMIT` and `CHAIN_LIMIT` — stand on borrowed numbers and wait for
a machine that can resolve an effect under this box's 1.5–3 % noise floor
(`dev/BENCHMARKS.md`); the fourth is resolvable here:

- [ ] **The string-key cancellation threshold, measured here.**
  done: the control-byte index and the plain check are timed against each
  other at N of 56, 512, 4,096 and 28,672 on string keys of realistic length,
  with the deletion margin read beside them, and the default either changes
  or is recorded as kept with the readings; the criterion is
  `rfc/model/arrays-hashtable.md`'s own — a 1.5× win on both lookups without
  the deletion margin worsening — and the figures go to `dev/BENCHMARKS.md`.
- [ ] **Whether an escalation raises an operations-visible signal.**
  done: `rfc/model/arrays-hashtable.md`'s open bullet carries this half and
  this plan never picked it up; the entry is either a decision recorded with
  its reason or a line saying the rfc answers it, and it names which.

## The vocabulary

The rename closed on 2026-09-02 against `rfc/dev/GLOSSARY.md`; what stands is
the net — three guards in `src/cycle/tests/` fail on a retired identifier, a
retired word in a comment, or a metaphor outside a citation, and their
messages name `dev/CYCLE-TERMINOLOGY-AUDIT.md` and
`dev/PROJECT-TERMINOLOGY-AUDIT.md` (`dev/DECISIONS.md`, "the vocabulary is
held by three guards, one per surface"). `rfc`'s own S9.1 carries the
cross-repository remainder. Five residues have no owner:

- [ ] **`promote` keeps `corpse` for the reset's torn-down entity**, which the
  glossary names a *torn-down entity* (`dev/CYCLE-TERMINOLOGY-AUDIT.md`,
  "Glossary check"); the exemptions in the two metaphor guards say so.
- [ ] **The row-initialization bitmap's accessors have no ratified name.**
  `groups`, `group_bit` and `group_bytes` were never put to the glossary, so
  the crate is naming them for itself (`dev/DECISIONS.md`, "an uncovered term
  is a gap rather than a local ruling").
- [ ] **The comment guard reads fourteen words and one term of a
  ninety-one-row mapping.** `refused` is not among them, which is how five
  files carried its retired sense until a Critic read them; 65 comment
  occurrences of `colour` stand against the audit's US-spelling rule.
- [ ] **Neither metaphor guard refuses a stale exemption.** The identifier
  guard has a test that fails on a file which has stopped offending; the name
  and comment guards have none.
- [ ] **`cycle::deferred_slot_reuse` outgrew its name.** `ActiveTrace` lives
  there and owns the scratch arena, takes the candidate batch and hands out
  rows, while the module header still describes the slot-return window alone.

## Beside the hashtable: the memory categories

Opened 2026-08-06; the routing item of that round is closed
(`memory/routing.rs`). Two are left, and the second gates the first.

- [ ] **Rename the memory categories**, in the RFC where they are defined,
  through the documents that refer to them, and in the crate — deferred
  2026-08-06, reasoning in `dev/DECISIONS.md`: `LongLived` is named after a
  duration rather than an owner, which is why its reclamation was never
  decided; `Region` and `Arena` each make a document's sentence false before
  the mechanism justifying it exists. Meanwhile the category is marked out of
  use on the enum itself.
- [ ] **The region reset, and the refusal that waits on it.** What a region
  owns, when it resets, how the owner's O(1) death reaches its entities, and
  what promotion across a region boundary is (`rfc/model/memory/regions.md`).
  It gates `ll_string_new_dynamic`'s refusal of that category — today nothing
  would reclaim such a string. Blocked on design, not scheduled.

## What is left of the old phase lists

Each line verified against the code on 2026-08-13; the lists that framed
them were deleted with the 2026-07-24 snapshot. The horizon's borrow elision
of 2026-08-18 is no task here: the proof logic left `rfc`'s scope on
2026-08-23, its six documents went on 2026-08-26 and are on
`archive/pre-rc-cycle` (`dev/DECISIONS.md`, "where the two deleted collectors
live: `archive/pre-rc-cycle`").

- [ ] **A3's factory half.** The descriptor carries `dispose`, and
  `ll_default_dispose` stands in until the compiler generates one. `factory`
  cannot be stood in for the same way — its signature `factory(ctx, category)`
  has no class parameter, so it needs per-class generation, and the generic
  path stays `ll_object_new(ctx, class, category)`. `clone`, `deep_clone`,
  `thread_clone` and `thread_move` are reserved for the multi-threading future.
  `rfc/runtime/object-lifecycle.md`'s "Only the GC reads layout as data,
  through `traced_runs`" holds once generated disposes replace the stand-in.
- [ ] **A7, no zeroing by default.** `ll_object_new` zero-fills the whole body
  unconditionally; which slots need a defined initial state is the factory's
  to decide (`rfc/BACKLOG.md`, deferred optimizations).
- [ ] **`Lazy` (code 1) and `Box` (code 10) have no producer.**
  `ll_entity_die`'s switch serves five; Box waits on the FFI surface and Lazy
  on the compiler, and only Box reaches the `debug_assert!` meanwhile, the
  switch routing `OBJECT | LAZY` to `ll_object_die`. `StringDynamic` (code 9)
  has a producer, `string::publish_uninit`. `Lazy` answers yes to `EntityKind::closes_a_ring` on the
  argument recorded in `dev/DECISIONS.md`, "a kind's ring classification is
  written at its declaration, before a factory stamps it".
- [ ] **The threshold arming policy.** The arming policy is the compiler's
  (`rfc/model/gc/strategies.md`, arm/fire); the critical reserve's third
  customer, the mutator whose gate is closed, is answered null today and draws
  nothing, because which runtime progress operations a reserve would fund is
  what the ABI does not yet name (`rfc/model/memory/critical-reserve.md`,
  "Mutator progress while collection is unavailable").
- [ ] **The birth count and the unique-owner policy.** The text went with the
  file on 2026-08-26 and is on `archive/pre-rc-cycle`; two composition stubs
  in `rfc/model/gc/pure-destructors.md` still cite it. The move rule is owed a
  home outside `rfc` by the ruling of 2026-08-23. Gated on the compiler: the
  share of dynamic publications with compiler-provable targets is its figure
  to give.
- [ ] **Pure destructors, and the hand-off drain.** Proposed by Edmond
  2026-08-18; the analysis is `rfc/model/gc/pure-destructors.md` with the
  2026-08-23 amendment that withdraws the collector-side free. The
  runtime-only step (the specialized P0 dispose and the raw-sever drain arm)
  needs no ruling and no compiler; the hand-off drain waits on the
  residual-duties and tail-bound questions the analysis names; the compiler
  tiers wait on the compiler. The composition with the ownership pair is
  `dev/design/owned-slots-and-the-walk.md` on `archive/pre-rc-cycle`, a
  source to re-read rather than a conclusion to carry.
- [ ] **The three `promote` tests that claim Miri as their whole regression.**
  `the_reset_reads_no_zero_count_member`'s three cases guard the reset window
  against reading a large run after it was unmapped, and whether any of them
  still exhibits its defect is unverified: one was run under Miri on
  2026-08-29 with `reset_window::park_large` returning false and passed. Each
  test is either seen failing under Miri against the mutation it names, or its
  comment is corrected to say what it does prove (`dev/WORKFLOW.md`, Miri).
- [ ] **Strategy 1, the typed vector.** No producer, so the 1 → 2 transition
  waits on one (`dev/DECISIONS.md`, 2026-08-13).

## Residual / carried-over items

- [ ] **The review's cuts of 2026-09-21.** Pass 1 of the review over
  `c58d49f..128cefc`, measured by script over the 203 functions the hunks
  touch: `memory::heap::ll_thread_init` at 59 code lines (the unstarted life's
  unwind and the heap-funding tail each become a function);
  `cycle::collect::collect_under_pressure` at 51 (the round's tail after
  `freed += taken` becomes `after_a_round`); depth 3 in `cycle::mark::mark`'s
  tracer closure (`refused = refused || !visit_child(..)`, a hot path, so with
  the poll bench) and in `static_block::run_thread_exit_teardown`
  (`pop_the_newest` lifted out of the loop); the helpers
  `cycle::testing::traced_from_a_collector_thread` and
  `the_metaphors_the_comments_still_carry::retired_in` at depth 3; and six
  `#[test]` bodies at depth 3, one of which
  (`critical::tests::where_the_first_touch_happens`) carries the fifth copy of
  a directory walker four other source-reading tests carry.
  done: each place under its threshold or recorded as kept with the reason,
  the poll unmoved, in the form of `dev/BENCHMARKS.md`, "S37.8 the review's
  cuts leave the poll where it was".
- [ ] **What S37 named and left, 2026-09-21.** The turnover period `N` is
  YRC's 64 and was not replaced: it is Y9's dial
  (`rfc/model/gc/cycle/questions.md`, Y9), both costs are linear in it and
  pull opposite ways (a live root is re-traced once per `N` collections, a
  component that dies behind a prune waits up to `N − d + 1` for its
  re-offer), and both are measured on the test heap (`dev/BENCHMARKS.md`,
  "S37.5 what a turnover re-offers, and what a deferral costs"). Which side
  pays is Edmond's to state; the ruling goes to `dev/DECISIONS.md` and
  `epoch::COMMITS_PER_EPOCH` moves with it. Y9's minimum-over-stamped-members
  question (whether a component's age is the minimum over its stamped
  members alone or over all of them, refused for the stamp's step by the Sage
  of 2026-09-10) changes no prune at `k = 1`, where every stamped component is
  at the threshold; it reopens with any `k` above 1 and reads off
  `mark::tests::the_pruned_share_against_a_survival_rate` under
  `pin_threshold` when it does. And `template.rs` builds its Object-kind
  entity without reading the class flags, so a template class the compiler
  marks acyclic does not carry `ACYCLIC_GATE` — conservative, and left until
  a template class is marked.
- [ ] **The citation check does not read the journals.** `dev/tools/citations.py`
  walks `src/`, `benches/`, `docs/` and the three maps, so a journal entry
  citing a plan line by its title is checked by nobody, and two are dead
  already: `dev/DECISIONS.md` 2026-09-15 cites the backlog line "A root the
  worker read live is never deferred" and 2026-09-18 the stage "The collector
  thread's spawn allocates through the global allocator", neither in this
  file (found by the Critic of 2026-09-21). done: the journals join the
  checker's population, its total moving with them, and the two citations
  are re-pointed at what replaced their targets or cut.
- [ ] **A store of one request arena's entity into another's object.**
  `store_category_barrier` keys on the memory category alone, so the slot
  holds the pointer raw, neither the escape arm nor the COW copy taken; a
  destructor run inside arena A's reset can resolve and reset arena B, whose
  `count_children` then meets A's entity as an ordinary child — a non-COW
  child is promoted out of A early, which is conservative, and a COW child
  has its count raised by B's pass while the edge behind that `+1` is
  recorded in B's log and freed with it, so A's reconciliation subtracts a
  reference a promoted holder still holds. Raised by the Critic of 2026-09-13
  over the COW reconciliation's rows; the rulings of the same day take a
  reset nested in another arena's as ordinary (`dev/DECISIONS.md`, "one
  arena is reset once at a time on a thread, and a second entry is refused").
  done: one red test builds the store and resets B inside a destructor of
  A's reset, or a demonstration that the barrier or the reconciliation
  refuses the shape is recorded in `dev/DECISIONS.md`.
- [ ] **A quiet thread's garbage is taken after X.** Edmond, 2026-09-18: the
  GC takes a thread's garbage of its own accord once some time X has passed.
  The half that turns a quiet thread's deferred lane over is built: the
  collector's round asks after X (`worker::quiet_interval`, 8 s by default,
  `ll_gc_set_quiet_interval` the embedder's dial) and the poll answers
  (`dev/DECISIONS.md`, "a quiet thread's turnover is the collector's to ask
  for"). What
  is left is R: a collector serves a mutator whose R holds
  `worker::SOFT_THRESHOLD` (64) records or more, the round's threshold is
  constant in the working build (`worker::threshold_for_rounds`), and a thread
  below the threshold, under no pressure and making no explicit fire keeps
  its cycle garbage until it crosses the threshold or exits
  (`cycle::collect::tests::what_the_byte_arms::`
  `an_unarmed_poll_leaves_a_completed_death_registered`). The cheapest form
  leaves the poll alone: a round serves such a mutator at a threshold of one
  once X has passed since the instant the round stamps on its record
  (`MutatorRecord::served_at`). The algorithm is accepted and S64 builds it:
  `dev/design/a-standing-r-is-taken-after-n-rounds.md`
  (`dev/DECISIONS.md`, "a standing R is taken after an interval of the
  collector's own, and no request count is capped") — the take after an
  interval of the collector's own, an ABI dial, of a ring standing non-empty,
  a sleeping thread left alone. A thread with one entry in R that polls
  nothing within `REQUEST_WAIT` leaves its request standing on its record in
  the collector's list, and a standing request opens a foreign-holder window
  on the thread the moment it wakes, bounded by one stranger's batch
  (`dev/design/the-standing-request-lives-on-the-record.md`).
- [ ] **An occupant of a retained block freed from another thread inside the
  reset that retained it.** `retained::occupant_freed` subtracts from the low
  half of a count word whose high half holds the pins, so a free arriving
  before the reset has established occupant counts borrows out of the pins —
  a `debug_assert` in a debug build, a silently eaten pin in release, and with
  no pin standing the whole word wraps and `has_held_occupants` answers true
  for the life of the process; the guard that would refuse it reads
  `reset_window::is_open`, a thread-local, so it answers for the freeing
  thread. Raised by the Critic of 2026-09-13 as a probe: whether a promoted
  survivor is reachable from another thread mid-reset is unestablished.
  done: one red test reaches the free from a second thread inside the reset,
  or a demonstration that the shape is unreachable is recorded in
  `dev/DECISIONS.md`.
- [ ] **The twenty-sleeper case has no Miri run, 2026-09-22.**
  `the_standing_list::twenty_sleepers_stand_and_are_all_served_on_waking` was
  killed at 50 minutes in a process of its own, twice on the day it was
  written, and now carries `cfg_attr(miri, ignore)` with that reason. What it
  covers over the other thirteen cases of its file is the list at twenty
  entries — the order of service across passes — and that is the part no Miri
  run has read. Either the case takes a width under `cfg(miri)`, four
  sleepers standing in for twenty, or the sleepers stop being threads: a
  stand-in that moves the byte without a thread would let Miri run the whole
  order. Neither is priced.
- [ ] **What S64 named and left, 2026-09-22.** A round whose walk reads no
  record carries its opening checkpoint's readings — `batches_served`,
  `saw_work` and the backlog — to the next round, since the fold is
  `read_one_record`'s and nothing drains them after `for_each_record`. The
  round reports no batch and its timer lengthens where the checkpoint's batch
  would have shortened it; `batches_served` has had the shape since S63 and
  S64 put two more fields in it. A drain after the walk closes all three and
  needs a case over a collector whose records are all named elsewhere. Found
  by the stage's Code Reviewer, 2026-09-22.
- [ ] **What S64.5 named and left, 2026-09-22.** Two arms the take's
  measurement still does not build. A pool that sleeps and wakes, where every
  round clears and re-makes the standing requests, is the population
  `EXPIRED_WAITS_PER_ROUND` was ruled for, and both sleeper probes hold their
  sleepers asleep instead, so each pays its wait once and stands
  (`dev/BENCHMARKS.md`, "what sleeping sub-threshold threads cost a round").
  And the mutator's collection is timed with the collector retired, so what a
  round in flight beside it costs is unmeasured. Neither moves a constant on
  its own: what each would price is the tail of a round and not the take. The
  third arm, a take of live roots, was built the same evening
  (`dev/BENCHMARKS.md`, "the live-roots arm"), and the mix between the two
  ends after it ("the mix", the same journal): what the mutator pays falls
  with the live share of its ring, and the sweep holds every reading of the
  case. What no arm fixes is the point a workload sits at, there being no
  corpus driver.
- [ ] **What S59 named and left, 2026-09-19.** Two branches of the birth have
  no arm: the guard's `mprotect` failure, which no test can order, and a join
  that fails, for which glibc documents `EDEADLK` on a self-join alone and no
  caller here can be one. The stack is 2 MiB because that is std's default,
  not because anything measured the collector's frames; the probe that would
  size it is lowering `COLLECTOR_STACK_BYTES` under the worker suite until a
  run dies on the guard. The guard's own case asserts the slot's mapping is
  exactly 2 MiB plus its guard, which a later anonymous mapping of at most
  64 KiB landing at its end and merging with it would break — no source of
  one was found in the test binary. And a wake that lands in the last instant
  of a consent wait is neither spent nor remembered, which predates the stage
  and is bounded by `FALLBACK_INTERVAL_MAX`.
- [ ] **What S51 named and left, 2026-09-17.** Every non-ignored case runs the
  serves at a request wait of 2 s (`worker::testing::HARNESS_REQUEST_WAIT`),
  a thousand times the crate's 2 ms, so the shipped bound is exercised by the
  ignored probes alone; and two fixtures — `worker::tests::Mutator::start`
  and `the_siblings`'s round wait — clear `POSTED` by hand, which suspends
  "`FREE` promises an empty P" for those cases. A harness at the crate's wait
  needs the consent to come from a real poll on a loaded box; a fixture that
  disposes instead of clearing pays a collection per batch. Neither is priced.
- [ ] **The second heapless arm has no case off Windows, 2026-09-18.** A
  thread whose heap is built and whose `tls::set` then refuses it is left
  heapless by the same two lines as a refused allocation
  (`memory::heap::ll_thread_init`), and `memory::heap::refuse_thread_heap`
  reaches the allocation alone; on Linux `tls::set` answers `true` always,
  so the arm's one case is Windows-only
  (`heap::tests::blocks_going_home_with_nobody_asking::`
  `a_thread_without_a_tls_slot_reports_instead_of_dying`) and reads three of
  the six promises `a_life_the_allocator_left_heapless` states. The other
  three need a Windows run.
- [ ] **What S49 named and left, 2026-09-16.** A component past the
  collector's block budget whose owner-side trace the pool refuses circles P
  and R under pressure, arming a collection each round; the collector's arena
  draws its own thread's critical reserve on a pool refusal, and no ruling
  says whether a collector thread may spend a reserve; a ring a burst grew
  while both spare cells were full keeps its blocks until a cell is spent,
  with no bound (`dev/DECISIONS.md`, "the ring's surplus goes into a short
  spare cell").
- [ ] **A weak map, and the second kind of death subscriber.**
  `rfc/model/weak-references.md` names two subscriber kinds and the crate
  builds one, the canonical `WeakReference` cell; the map keyed by object
  identity is the other (Edmond, 2026-09-07: it can wait). Its shape: an
  ordinary GC-heap entity over `array::Table`, key uncounted and value
  counted; a target's weak-table row becomes a list, which the row's reserved
  tag bits were left for; the death notification's displaced value goes to
  `cycle::reclamation`'s queue inside a teardown — between the sever and the
  last free no user code may run (`rfc/model/gc/rc-cycle.md`, "Cycle
  finalization and reclamation", step 6) — and to an inline release
  everywhere else, told apart by a thread-local sink the teardown arms
  (`weak::notify_death` cannot take a parameter, `dispose` being a C ABI
  pointer). Two obligations with it: an arena-resident key belongs on the
  arena's weak log, and the last subscriber leaving clears
  `HAS_WEAK_REFERENCES`.
- [ ] **The ladder's refusal has nowhere to go.** `InsertOutcome::AdmissionDenied`
  is answered inside the crate — a null from `ll_cow_separate`, a `false` from
  `element::set` — because the crate has no error channel; `rfc/model/maps.md`,
  "Rung three, refusal" says the runtime raises it as a catchable error, which
  waits on the exceptions work. Until then a refused insert is
  indistinguishable from memory pressure.
- [ ] **The equal-identity trigger's tag test has no test of its own.** In an
  array "the tag equals the incoming string's" names the same set as "not an
  integer key", so the change was verified by reading; `Map` is where the two
  differ, and the test is owed there.
- [ ] **The long-key slot itself.** `strong_hash`'s doc stands in for
  HighwayHash-64 behind a length threshold `rfc/model/strings.md` says is
  unmeasured; blocked on that measurement, with the strings work.
- [ ] **The publication fence's ARM64 price.** `refcount::publish_header`
  emits one `fence(Release)` per entity built — a compiler barrier on x86-64
  (instruction multiset identical with and without it, 2026-09-15), a
  `dmb ish` on ARM64 whose cost is unmeasured; no ARM64 machine exists in the
  project, and Edmond relaxed the rfc's "measured before it is emitted" to
  "before the first ARM64 build" (`rfc/dev/DECISIONS.md`, "the publication
  fence lands before its ARM64 price"). The measurement is
  `benches/lifecycle.rs`'s create/release pair on an ARM64 box, both arms in
  one session, before that build ships.
- [ ] **The per-process key's Windows randomness source.** `hash/process_key`
  is unix-only, `#[cfg(not(unix))]` a `compile_error!` naming this gap, until
  a session on the Windows box adds the source (`BCryptGenRandom` or an
  equivalent) and runs the gate there. Deferred by Edmond, 2026-08-17.
- [ ] **The collector's birth has no run off Linux.** `cycle::worker::birth`
  has a windows arm (`CreateThread`, `WaitForSingleObject`, `CloseHandle`)
  that type-checks against `x86_64-pc-windows-gnu` and has run nowhere, and
  a unix arm whose `pthread_setname_np` is declared for linux and android
  alone; the aarch64 target checks clean and has run nowhere. The Windows
  run waits on the same session as the key above; what a build for the
  non-futex unixes owes the locks first is in `dev/DECISIONS.md`, "the
  collector thread is born by the OS entry, on a stack its slot keeps, and
  is woken by a word".
- [ ] **No ABI entry creates or mounts an arena.** An external caller can
  build an `LLContext` and reach the store barrier, but every `*mut Arena` in
  the crate is made by Rust code inside tests; an embedder needs that entry
  before anything outside this crate exercises the arena paths.

What S36 left without an owner, 2026-09-14. Its sites under the ruling that
no runtime path may end the process on an allocation the manager could have
refused (`dev/DECISIONS.md`, "the reset window's memory comes from the
manager") are closed: the three on the exit path (`dev/DECISIONS.md`, "the
exit path holds no container: the buffer arena is its thread-local, and the
static registry is a chunk closed per life") and the collector thread's
spawn (`dev/DECISIONS.md`, "the collector thread is born by the OS entry, on
a stack its slot keeps, and is woken by a word"); the rest:

- [ ] **The collection's journal kinds.** `journal/kinds.rs` carries no record
  for a collection's begin or end (`dev/design/debug-modes.md`, §9.5);
  `cycle::collect` records `KIND_EXIT_RESIDUE` alone. A window over a
  collection needs its two ends and which path it took.
- [ ] **A member of a kind other than an object or an array is untested
  through the commit.** A reference box, a template and a class with cells
  outside itself each have an arm in `cells::trace_cells` and
  `cells::sever_cells`, and no case drives one through validation,
  finalization and reclamation as a member; `Lazy` waits on a producer.
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

- [ ] **Batch the cross-thread free, once a workload exists.** `Heap::free_remote`
  posts each foreign slot with its own CAS onto the owning block's
  `remote_free` stack, and `buffer_arena::post_remote` the same for a chunk,
  so the cost is linear in items freed; snmalloc pays one atomic per batch.
  The shape if wanted: a bounded thread-local staging buffer, grouped by
  block on flush, each group chained through the dead slots and pushed by
  one CAS; the price is return latency, so peak RSS rises by the batch, and a
  thread exiting with a staged batch must flush it. Removing the atomic
  entirely means a per-thread-pair SPSC ring (`ck_ring`), memory per pair,
  the trade snmalloc declines; the thread-exit flush's shape is
  `deferred_free.rs` on `archive/pre-rc-cycle`. Nothing drives the path
  today — the crate is single-mutator, and the callers are
  `heap::tests::frees_arriving_from_another_thread` and whatever reaches the
  raw C ABI from another thread. Order: a test heap that frees another
  thread's objects in bulk, then a measurement, then this.
- [ ] **Reallocate in place when the class does not change.** `stdapi::ll_realloc`
  allocates, copies and frees on every call, so 40 bytes to 48 costs a block,
  a `memcpy` and a free to move inside one 48-byte slot;
  `stdapi::ll_usable_size` already reads the class size. What comes first is
  a harness: `rptest` in `benches/standard.rs` never reallocates, and nothing
  calls the path while the `#[global_allocator]` install is owed.
- [ ] **Size classes for the band between 8 KiB and one block.** Classes stop
  at `heap::MAX_SMALL` and everything above takes a whole 64 KiB block. Five
  classes — 10880, 13056, 16320, 21760 and 32640 — divide the payload without
  a tail and hold the worst case to 1.33–1.5×, chosen in the cold function
  past `MAX_SMALL` so `CLASS_LUT` stays 514 entries; the cost is five classes
  in three per-heap arrays and the abandoned table, about 120 bytes per
  thread, and a high block switch rate on a two-slot class — against today's
  pool get and put per object. Free simplifies: these become ordinary heap
  blocks the `BLOCK_KIND_HEAP` arm serves. What comes first is a footprint
  measurement in `blocks_out` and RSS — `benches/alloc.rs` stops at 8192.
  The routing list at the head of `stdapi.rs` and `docs/memory-manager.md`
  move with the change; entities past 8 KiB stay reached by their own block
  header's row (`cycle::row::resolve_edge_target`).
- [ ] **The commissioning zero pass: delete it or name its production
  reader.** `Heap::refill` writes eight bytes into every slot of an entity
  block unconditionally, up to 4080 stores at the 16-byte class; the walker
  that needed the invariant, `for_each_entity_slot`, has no production caller
  since `rc-walk` went, while `heap.rs`'s module doc still says the pass
  exists for a trace that strides the block. One of the two is wrong: either
  the pass goes with the doc corrected, or its reader is named and the
  zeroed-block flag of `dev/RESEARCH.md`, "rpmalloc" (a flag that names the
  stride it holds for, carried across recycling) is what would retire the
  cost. Refill runs about 0.00003 times per allocation on the steady-state
  benchmarks — a reading, not a measurement.
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

Object model, deferred by design: the interception proxy and the vtable-slot
interceptors are `rfc/model/classes.md`'s deferred questions, and no stage of
this crate is drawn from them before the rfc answers.

- [ ] Allocation telemetry layer 2 / debug mode — full design in
  `dev/design/debug-modes.md`, build order its section 10; item 1, the event
  journal, is built, the rest unscheduled, the per-structure breakdown of
  collection's logical bytes (§8, an axis A feature; Edmond, 2026-09-01: not
  in a production build) among it.
