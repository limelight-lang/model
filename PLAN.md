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

Updated: 2026-09-22 · Active: S63; S62, a design stage, is done and waits on nothing. S61 went on 2026-09-22, the day it opened:
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
`dev/ARCHITECTURE.md` for the map. Deleted so far: S4 through S61, every
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
## S62 — A standing R is taken after N rounds  [done]

Goal: the second half of "A quiet thread's garbage is taken after X" has an
algorithm Edmond has accepted or a list of questions only he can answer —
the collector's own take of a candidate ring that stands non-empty below the
threshold for N of its rounds, the mutator's side unchanged.
Done when: `dev/design/a-standing-r-is-taken-after-n-rounds.md` has been
through two Critic rounds, every finding answered by a repair or a refusal
with its reason, the findings neither could answer ruled by the Sage, and
the document ends either as the final algorithm or as the questions to
Edmond; the chain is Edmond's of 2026-09-22 (Critic → repair → Sage if no
answer → Critic → repair → Sage if questions remain → Edmond).
Notes: a design stage — no code, no bench, no rfc edit; the rfc moves on
adoption (`dev/DECISIONS.md`, "analysis of a candidate that may be refused
stays in `dev/`, and the rfc moves only on adoption"). The rule in one line,
Edmond's: below the threshold the collector does not collect too often, but
after three or four of its intervals with the ring standing it collects
anyway, and the collector decides.

- [x] S62.1 The algorithm written
      done: the document states the rule, the words on the record, the
        round's reading, the serve at a threshold of one, the silent
        mutator's cost, the interactions with the turnover request, the
        siblings, the timer and the pressure path, the refused forms and the
        open points
      tier: T2 · role: —
      handoff: `dev/design/a-standing-r-is-taken-after-n-rounds.md`, first
        form; two open points, the silent mutator and the count's unit.
- [x] S62.2 Critic, round one, and the repairs
      done: every finding of the first Critic pass is repaired in the
        document or refused with its reason in this step's role line; a
        finding with neither goes to the Sage and its ruling is `Final`
      tier: T2 · role: Critic → Sage
      Critic 2026-09-22 round 1: a count of rounds is not "intervals" —
        rounds are wake-driven and 10 ms apart after a batch, so four rounds
        take the dripping mutator the threshold spares, and the S60 ruling
        rejected an X tied to the adaptive interval by name (accepted: a
        floor of 4 s on the serve clock from the instant the ring was first
        seen unchanged; the reading of "interval" is question 1 to Edmond);
        a standing request for a take lets blocked sub-threshold threads
        fill the 16-entry array and starve a producing silent mutator
        (accepted: a silent mutator is not taken; question 3); the identity
        test needed a third word and the tail-index equality is fooled by a
        pack (accepted: the block copy dropped with the argument why none
        is needed, and the clock's stamp joins the change test; the
        no-commit retirement named as a one-round residual); the cost
        omitted the recurring in-line trace per X of the roots read live
        (accepted, in Cost and in the measurement plan); an `AtomicU8` count
        with no saturation (moot, the count is gone); the threshold
        parameter's reach into `batch` and the checkpoints turns a leftover
        into a backlog (accepted: the parameter goes, `batch` reads the
        threshold itself, the take is a local flag); P without room and the
        count (accepted: the standing stands, the take waits a round); the
        ring needs a reader returning the front block's words (accepted,
        `Reader::front_block_reading`); "same non-empty R" is a reading to
        confirm (question 2). No finding went to the Sage: each had a repair
        or a question for Edmond.
      handoff: the document's second form; three questions in "Open".
- [x] S62.3 Critic, round two, and the repairs
      done: as S62.2 over the repaired document; a finding with no answer
        goes to the Sage
      tier: T2 · role: Critic → Sage
      Critic 2026-09-22 round 2: a take whose consent missed the wait marked
        the mutator silent, and the mark clears only on a grant, so one late
        answer excluded the running thread the rule is for, for the record's
        life (accepted: the take's withdrawal marks nothing and restarts the
        standing; question 3 re-stated); branches 1 and 2 zeroed the instant
        and kept the index, so a fresh ring at the old index met a zero
        instant and was taken at once (accepted: a zero instant is a change,
        and the index is cleared to `usize::MAX`); the take fed K's sizing —
        four takes of three doubled K to 1024, a K of one took a ring of
        three a root per round (accepted: the take's clamp is R's count and
        the sizing is skipped); "a take's close counts a commit" is false
        for verdicts all read live (accepted: the sentences rest on the
        restamp and the tail move); the drip is spared only while the lane
        is empty, S60's X collection tracing R whole otherwise (accepted, in
        the standing's argument, the interactions and the cost); dropping
        the threshold parameter re-aims `the_batch`'s clamp cases for no
        runtime change (accepted: the parameter stays, the take passes the
        round's threshold); the first measured term was the collector's
        time, not the mutator's (accepted: the withheld frees and the
        collection over P); "the tail index is the count" invites storing
        the span (accepted, struck). The narrow window between branch 3's
        clock read and the round's restamp joins the named residuals. No
        finding went to the Sage.
      handoff: the document's third form; three questions in "Open",
        the third re-stated.
      Edmond 2026-09-22, on the three questions: the interval is the
        collector's own time, a parameter of the ABI; once the interval
        has passed the ring's length no longer matters, so "non-empty" and
        not "unchanged"; a sleeping thread is left alone, and no count of
        requests may be capped — the 16-entry standing array is wrong as a
        mechanism and is filed as its own stage. The fourth form is the
        rule in those terms (`dev/DECISIONS.md`, "a standing R is taken
        after an interval of the collector's own, and no request count is
        capped").
      Critic 2026-09-22 round 3 (over the fourth form, at Edmond's word):
        a take restamps the turnover ask by fiat as any batch does, and an
        all-live take moves no clock, so a drip taken every interval is
        never asked for a turnover and the two halves cancel (accepted: the
        take's restamp is decided by the clock alone; the same hole in the
        built S60 rule is the backlog line "A batch that moved no clock
        restamps the turnover ask"); a hit left the instant standing, so a
        write-back ten milliseconds later was re-taken at the round's
        cadence (accepted: the instant restarts when the request lands,
        hit or miss); a sleeping mutator costs its collector a 2 ms wait per
        interval, serially inside the round, which the silent mark spares
        the threshold path and the take refuses (accepted as a cost: named,
        measured with a round-length term and an off value, and the record
        stage named as what removes it); four wordings an implementer would
        have to guess — the checkpoints under a take's wait, the miss's
        `Served`, the grant clearing the mark, the flag's reach (accepted,
        each stated); the rule's sentence claimed the ring's history where
        the word knows the collector's visits (accepted, amended). No
        finding went to the Sage.
- [x] S62.4 The final algorithm, or the questions to Edmond
      done: the document's "Open" section is empty and the rest is the
        algorithm as ruled, or "Open" lists the questions only Edmond can
        answer with a proposed answer to each, and Edmond has been shown
        which of the two it is
      tier: T1 · role: —
      handoff: the fifth form is the final algorithm, "Open" empty; the
        one price Edmond is told with it is the sleeper's 2 ms per interval
        inside the round until the standing request has a home on the
        record. Building it is a stage of its own, not opened.

## S63 — The standing request lives on the record  [in progress]

Goal: no count of standing requests is capped, a consented mutator's window
is bounded by one stranger's batch whatever the number of threads, and the
mutator's side is untouched — the algorithm of
`dev/design/the-standing-request-lives-on-the-record.md`, accepted
2026-09-22 (`dev/DECISIONS.md`, "the standing request lives on the record,
the checkpoint serves one grant, and no count is capped").
Done when: `worker::Standing` is a head over a list threaded through the
records with no capacity, the silent mark's "missed a wait" meaning is
gone, a checkpoint walks only on a byte event and serves one grant, the
registry refuses a linked record, the six tests the design owes are green
with the gate, and the `rfc`'s handshake document states the new form.
Notes: the mutator's `token.rs` state machine, `read_and_act_on_this_thread`,
the poll and the free path change in nothing; the only `token.rs` edit is
the wake entry the consent and the refusal call. Every step is a cycle-GC
step: the baseline recorded, a red test seen, the Critic over the repair.

- [x] S63.1 The words: the link pair and the released byte on the reader
      line, the sequence number on the collector's slot
      done: `ReaderLine` carries `standing_next`, `standing_prev`,
        `released_unserved` with the layout asserts standing; `reset`
        clears the byte, leaves the links and debug-asserts them null;
        `first_free_record` skips a linked record and a case shows the take
        falling through to a carve; `Collector` carries `byte_wakes`, and
        `wake_for_the_byte` is what `consent` and `take_unless`'s refusal
        call, a case reading the number move on each and on nothing else
      tier: T2 · role: Critic
      baseline: the reader line 48 of 64 bytes; the registry's gate one
        acquire load of the hold word; a consent one release swap and one
        wake; a refusal one acquire swap and one wake.
      Critic 2026-09-22: "`next != null` is linked" gated a two-word state
        with no store order, so the registry could hand out a record between
        an unlink's two stores or under a push in flight (accepted: `next`
        is the first word a link writes and the last an unlink clears, in
        the field's contract and in `link_for_test`); the head sentinel on
        the frame was a `*mut MutatorRecord` to two stack words (accepted:
        self-terminated ends, the design amended); the token case named
        slot 5, which its neighbour refuses a request on under the parallel
        harness (accepted: slot 7, the reason at the constant); the number's
        stated reason missed the collector's own withdrawal (accepted); the
        gate was duplicated across the cfg arms so production's arm ran
        under no test (accepted, lifted); plan-step numbers in the three
        `expect` reasons (refused: `dev/WORKFLOW.md`, "How a debt is
        written", names that form as the self-reporting one); the reset's
        abort from `ll_thread_init` holds as the crate's form once the
        order stands. Asides left as they are: `a_thread_asking_for` now
        exists in two test modules with differing bodies; "a spare" for the
        hold line in the record tests' module doc is older than this step.
      handoff: `ReaderLine::standing_next`/`standing_prev`/`released_unserved`
        and their accessors, `link_for_test`; `first_free_record` reads
        `stands_in_no_list`; `Collector::byte_wakes`, `wake_for_the_byte`,
        `testing::byte_wakes_of`. Both cases seen red (the reset's assert
        aborting with the gate cut; "the consent moved it" with the plain
        wake).
- [ ] S63.2 The list and the checkpoint
      done: `Standing` is the head pair with `push` (idempotent, at the
        tail), `forget` (O(1), no-op unlinked) and the gated pass — no walk
        without a byte event, the whole list read, every grant but the first
        released to `FREE` and marked, the first unlinked and served; the
        expired wait pushes instead of withdrawing; `serve` pushes a marked
        record with no wait; every entry to `serve_the_grant` is unlinked;
        the drop withdraws and unlinks; `STANDING_CAPACITY`, `silent`,
        `is_silent`, `note_silent` and the "past the capacity" arm are gone;
        the six owed cases green; `what_the_byte_arms`' moved assertions
        re-stated as the design says and nothing else of the suite changed
      tier: T2 · role: Critic
- [ ] S63.3 The handshake document and the journals
      done: `rfc/dev/design/trace-token-handshake.md`'s collector paragraph,
        "Cost", the third round's bound, timing (a) and E7 state the new
        form; `dev/ARCHITECTURE.md` and `dev/INDEX.md` name the list; the
        S51.5 sleeper probe re-run and its figure recorded beside the old
      tier: T1 · role: —

---

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
- [ ] **A batch that moved no clock restamps the turnover ask.**
  `worker::ask_for_a_turnover_if_quiet` restamps `served_at` for every
  `Served::Batch` by fiat; a batch whose verdicts all read live leaves the
  mutator a collection over P that closes `EmptyLane` with no commit, so the
  clock did not move and the stamp says it did. A thread at the threshold
  batched more often than X with all-live batches is never asked for a
  turnover, and its deferred lane waits for pressure or exit. Found by the
  Critic over S62's fourth form, 2026-09-22, as the same hole a take would
  open; the design decides the take's restamp by
  `clock_stood_since_the_stamp` alone, and the batch's is this line. done:
  the restamp after a batch is decided by the clock, a case shows a thread
  batched all-live every X/2 asked after X, and the S60 entry's "a serve
  that made a batch restamps" is amended with the date.
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
  (`MutatorRecord::served_at`). Its tail is
  unpriced: a thread with one entry in R that polls nothing within
  `REQUEST_WAIT` is marked silent, its request stands on the collector's
  frame up to `STANDING_CAPACITY`, and a standing request opens a
  foreign-holder window on the thread the moment it wakes.
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
