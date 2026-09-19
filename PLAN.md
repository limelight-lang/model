# Plan

Destination: `ll-model` is the runtime the compiler links — the memory
manager, the object model and the cycle collector built to the `rfc`'s
design and calibrated on a Phase-D corpus.

Implementation plan, re-sorted last on 2026-07-24 against the object-layout
redesign of 2026-07-22.

Design lives in `rfc` and is authoritative — read before coding, do not
re-derive: `model/classes.md`, `model/values.md`, `model/lowering.md`,
`model/gc/rc-cycle.md`, `model/gc/cycle/questions.md`, `model/memory/ffi.md`,
`runtime/object-lifecycle.md`.

The `rfc` repository carries its own plan at `dev/PLAN.md` for work that lands
in the specification rather than in this crate.

Updated: 2026-09-19 · Active: S37, whose two open steps were unblocked the same
day: Edmond ruled that a corpus over `ll-model`'s own heap is not coming and
test data is what the calibration gets (`dev/DECISIONS.md`, "the calibration
runs on a parameterized test heap, and the entry names its parameters"). S37.5
measures the turnover and S37.7 the traced share, both now on a test heap whose
parameters the entry names. Both steps' readings were taken on 2026-09-19: S37.5 is closed, and
S37.7 waits on one ruling — the preference between rows spared and recall
latency, both of which are now measured. The prose sections below are the backlog a stage is
drawn from.
The S36 residue's four allocation sites are closed, the last of them on
2026-09-19: the three on the exit path (`dev/DECISIONS.md`, "the exit path
holds no container: the buffer arena is its thread-local, and the static
registry is a chunk closed per life") and the collector thread's spawn
(`dev/DECISIONS.md`, "the collector thread is born by the OS entry, on a
stack its slot keeps, and is woken by a word"). What the residue still
holds, none of it under that ruling, is the section below.

Review 2026-09-18, second: pass 3 by the Critic over the plan as rewritten
by S57.7 — its findings and their disposition are in that step. Pass 1 over
the period's code is void, the period's only source changes being comments;
pass 2 likewise.

Review 2026-09-18: the first one recorded here, and overdue — the hook reads
`dev/PLAN.md` while this plan sits at the repo root, so nothing named the
period. Code of 2026-09-15 to 2026-09-17 (`abfba48..c58d49f`) against the
thresholds: eleven places, the largest `worker::serve` at 80 code lines where
the same function was 42 before the slice, `collect::collect_under_pressure` at
79 and control depth 4, `worker::thread_body` at 77; they are S37.8 below. Two
proposed deletions were refused — `make_withheld_returns_before_the_retry`
states a precondition its callee's `# Safety` does not and carries two callers,
and deleting `collect_off_the_poll` moves a contract paragraph into `gc.rs` and
re-points eight `As [...]` citations. The algorithms of the period are Edmond's
own rulings, so a simpler form is a premise change for him rather than a review
finding. The plan lost S40, whose goal was discharged on 2026-09-12, ten closed
steps, the obituary of nine deleted stages, two fog lines and a cross-cutting
section duplicated 640 lines from its twin; the nine orphan debts that cut would
have dropped were filed first, in `dev/DECISIONS.md` (four entries),
`dev/POSTMORTEM.md`, `dev/WORKFLOW.md`, `dev/RESEARCH.md` and S37.5's
`handoff:`.

**Closed stages are deleted whole** (rule 23.1.3), and what outlived each
of them is in the journals rather than here: `dev/DECISIONS.md` for a
decision and its reason, `dev/POSTMORTEM.md` for a trap,
`dev/BENCHMARKS.md` for a measurement, `dev/INDEX.md` and
`dev/ARCHITECTURE.md` for the map. Deleted so far: S4 through S36 and S38
through S59 — every number this plan has spent but S37. A number is never
reissued, so a
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
step of S37; one broad review does not waive a later step's gate. The rest of
the routine — the gate, Miri in slices, the citation checks — is
`dev/WORKFLOW.md`.

## Fog

A line here is an unresolved question rather than a step: it carries no
criterion, and it leaves when it gets one or when it is ruled on. Three of
the lines end in a sentence the `rfc` owes — which destructors "ran
anywhere" counts, the object handed to a survivor, the exit's safepoint word
— and they stand here until that repository's plan takes them.

- **What a reset owes an arena entity that belongs to another arena.**
  `store_category_barrier` keys on the memory category alone, so a store of
  one request arena's entity into another's object takes neither the escape
  arm nor the COW copy: the slot holds the pointer raw. A destructor run
  inside arena A's reset can resolve and reset arena B, and B's
  `count_children` then meets A's entity as an ordinary `RequestArena`
  child — `is_arena_entity` reads the category too. A non-COW child is
  admitted to B's survivor chain and promoted out of A early, which is
  conservative; a COW child has its count raised by B's counting pass while
  the edge behind that `+1` is recorded in **B's** log and freed with it, so
  A's reconciliation subtracts a reference a promoted holder still holds.
  Raised by the Critic of 2026-09-13 over the COW reconciliation's rows,
  which neither create the shape nor widen it: the premise verified is the barrier's, and whether
  two request arenas can name each other's entities at all is the question
  under it.

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

- **What a process-wide `pthread_key` would buy the reserve draw.** Four
  thread-locals of this crate carry drop glue, and the first touch of one
  registers a TLS destructor whose failure ends the process rather than
  reporting (`dev/DECISIONS.md`, "what the first touch of a thread-local with
  drop glue may cost"). `ll_thread_init` touches all four, so the death is
  deterministic in place; the class itself is still there for a thread that
  never runs init. A guard on a key taken once at process start, where a
  failure is reportable, would remove it if `pthread_setspecific` allocates
  nothing per thread — which nobody has read, on any target. Named when the
  reserve's first touch was decided on 2026-08-29 and priced nowhere since.
- **Which destructors "a destructor ran anywhere" counts.**
  `rfc/model/gc/rc-cycle.md`, "Cycle finalization and reclamation", step 5 gates
  the second reading on a destructor having run anywhere in the commit, and
  `cycle::finalization` reads that as a member's pending `__destruct` at step
  4, which is what `DestructorPass` records. The other reading is any user destructor at all, the
  external children of a component's teardown included — and those run without
  touching the flag, which `cycle::finalization`'s
  `a_child_of_a_dying_member_runs_its_destructor_inside_the_release` shows. The
  skip is sound under the first reading by the induction written at
  `Revalidation::revalidate`; which reading the specification means is
  unresolved, and it is `rfc`'s sentence to sharpen.

- **An object handed to a survivor by a destructor of the same batch still
  runs its own `__destruct`.** The settle loop drains a round's destructor
  entries before the re-trace, so `$survivor->keep = $this->y` in one body
  does not spare `y`'s body later in the same batch: `y` is promoted
  already-destructed, its later `dispose` finding `DESTRUCTOR_RAN` and
  skipping. Zend would not destruct it, `y` never having become
  unreachable. Named by the Critic of 2026-09-12 over the re-trace; whether the
  rfc's "survives already-destructed" paragraph admits it is the rfc's
  sentence to write.
- A destructor's `ll_thread_exit` waits for the thread's top
  (`memory::heap::thread_exit_pending`), and nothing tells the code above
  the destructor that a request stands: whether the emitted safepoint reads
  that word and unwinds on it, and under what name the ABI carries it, is the
  rfc's to say (`dev/DECISIONS.md`, "an exit requested inside a collection
  runs at the thread's top").

---

## S37 — Maturation and the two class gates  [blocked: the Phase-D corpus]

Goal: the trace stops following the whole heap. On a booted Laravel corpus the
subgraph reachable from a median candidate root is 381 of 381 objects, so this
stage is what makes a trace affordable rather than what tunes it.

- [x] S37.0 The commit stamps the live components it read   *(before S37.1)*
      handoff: `cycle::maturation`, from `commit_before_drops` after
        `Finalization::begin`; the ruling and its refusals are `dev/DECISIONS.md`,
        "the live population is stamped by component".
- [x] S37.6 The close disposes of a batch per root   *(after S37.0, before S37.1)*
      handoff: `queue::DEFERRED_MARK`, written by
        `ActiveTrace::mark_roots_for_deferral` and masked off by
        `compaction::stage_entry`; the lane is a side exit of the compaction
        pass (`dev/DECISIONS.md`, "the deferred lane is a side exit of the
        compaction pass").
- [x] S37.1 The maturation stamp is an edge-side prune
      handoff: `cycle::mark::visit_child` stops at a target stamped with this
        collection's epoch at `TRAVERSAL_AGE_THRESHOLD` (3) that no queue
        names; `take_edges_pruned` counts the cut; `k = 3` and the
        64-collection turnover are S37.5's and S37.7's to measure
        (`dev/DECISIONS.md`, "the edge-side prune cannot tear down a live
        component").
- [x] S37.4 The deferred-candidate buffer and the turnover re-offer
      handoff: `queue::defer_candidates` and `reoffer_deferred_if_epoch_moved`,
        the cases `cycle/collect/tests/when_the_turnover_reoffers.rs`
        (`dev/DECISIONS.md`, "the deferred lane holds a registered root";
        YRC's 56 % is `dev/RESEARCH.md`, 2026-09-18).
- [x] S37.8 The review's simplifications in the collector's own modules
      handoff: eight functions cut under fifty code lines and nine helpers
        lifted, the poll unmoved at 7.46–7.61 ns (`dev/BENCHMARKS.md`, "S37.8
        the review's cuts leave the poll where it was").
- [x] S37.9 The owned store's refusal, and the plain store's
      done: a refused `store_ptr_owned` leaves the displaced entity's mark, the
        slot and both counts as they were, driven by a real refusal — the
        escape copy of a COW value the pool will not give a block for — and the
        same case proves which allocation was refused rather than asserting a
        `false` that any early return could produce; the plain `store_ptr`'s
        refusal takes a case of the same shape beside it
      tier: T1 · role: — (tests only: no production line changed, and the two
        cases are read by the mutations below rather than by a reviewer)
      note: the debt S37.3 named and left, moved here out of the residual list
        on 2026-09-18. What blocked it then was the instrument: `force_oom`
        refuses a **pool block**, and a copy that fits the thread's current
        block asks the pool for nothing. A copy larger than `BLOCK_PAYLOAD`
        does not help either — the large half draws on the OS and the flag does
        not reach it
        (`memory/large_entity/tests/the_two_halves_are_separate_populations.rs`).
        What is left is to make the copy the first allocation of its size class
        on a thread of its own, where the refusal is a block draw.
      correction 2026-09-18: "the first allocation of its size class on a
        thread of its own" is not enough, and the first form of both cases was
        flaky for it — whether a fresh thread's init left room in that class is
        the pool's warmth on the day, so the store succeeded whenever the suite
        ran before it. The cases empty the class first, with the copy's own
        call, through `barrier::tests::fill_the_class_until_refused`, and give
        every slot back after the window.
      handoff: `barrier/tests/the_owned_store.rs::`
        `a_refused_copy_leaves_the_mark_the_slot_and_the_counts_alone` and
        `the_ordinary_store.rs::a_refused_store_leaves_the_slot_and_the_count_alone`,
        each on a thread of its own under a block budget of zero, with the
        pool-request count read back so the refusal is named as a block draw
        rather than assumed from a `false`. Three mutations run: the budget
        raised so nothing refuses (both red), the retain kept instead of given
        back on the refusal (both red, count 2 against 1), and the slot written
        before the refusal returns (both red). A fourth stayed green and is
        recorded in the case itself — moving the mark before the store's answer
        is read changes nothing, because a refused store leaves the slot
        holding what it held, so `move_ownership_mark` sees one entity as both
        the displaced and the occupant. The early return is unobservable
        through the mark, and the case says so instead of claiming the clause.
        Suite 1043 passed, three times.
- [x] S37.10 The epoch counter is the collecting thread's
      handoff: the counter is `MutatorRecord`'s writer-line word, read by
        `epoch::current` for this thread and by `epoch::of_record` for the
        mutator a collector serves; the ruling and what it costs are
        `dev/DECISIONS.md`, "the epoch counter is the collecting thread's, in
        its record".
- [x] S37.11 The mirror a verdict-side deferral records
      done: a root deferred out of P at the close and one deferred out of R in
        the same collection carry the same turnover mirror, read by a case that
        defers one of each and re-offers both at one turnover
      tier: T1 · role: —
      correction 2026-09-19: that criterion cannot be read off one lane, and a
        case meeting it passes against the defect as well. The mirror is
        written where the deferred lane goes from empty to occupied
        (`queue::defer_entry`), and a collection defers out of R before its
        close disposes of P, so one lane holding both records carries the R
        side's count and nothing of the P side's. What discriminates is a
        collection that defers out of P alone, with the R side's reading named
        as the count it must record.
      note: found by the Critic of 2026-09-19 under S37.10, pre-existing.
        `collect::initial_disposition` reads the count before
        `Revalidation::close` increments it, while `CollectingThread::drop`
        reads it after and hands that later count to `compaction`'s
        verdict-side deferral, so a collection closing at commit 63 records 63
        for one root and 64 for the other — one whole epoch apart. What made it
        cheap was the process-global counter: the extra epoch was 64 commits
        the whole process contributed, and it is now 64 collections of this
        thread's own. The repair is to carry the collection's own reading to
        the drop rather than re-read there.
      handoff: `CollectingThread` carries `at_commits`, the count the last
        reading of its collection saw, and its drop records that instead of
        reading the counter again; the pressure loop takes it out of
        `PressureCommit`, which is what `commit_under_pressure` answers now.
        The case is `when_the_turnover_reoffers::`
        `a_pressure_collection_defers_its_verdict_at_the_count_its_reading_saw`,
        with the counter driven one short of the turnover
        (`epoch::close_commits_to_one_short_of_the_turnover`): red before the
        edit at 64 against 63, one whole epoch. Gate 1077 ×3 at eight threads,
        `hash-folding` 1077, `debug-journal` 1086 ×3, citations 739/0, release,
        bench and doc with no warning; Miri over the case 1 green in 29 s.

- [x] S37.12 The epoch case that its harness thread positions
      done: `a_collectors_trace_prunes_against_the_owners_epoch` passes from
        the worst position its harness thread can leave the counter in, one
        commit short of a turnover, which is red against the case as S37.10
        wrote it
      tier: T1 · role: —
      handoff: the case failed once in 14 `debug-journal` runs at eight threads
        on 2026-09-19, found in a control run of the tree before S37.11 and
        reproduced by starting it at the boundary (1 against 2, a stamp written
        before a crossing read after it).
        `epoch::stand_at_the_start_of_a_nonzero_epoch` puts the clock at a
        turnover's first commit, and the case opens at the boundary so that
        every run reads the alignment. The class and what hid it are
        `dev/POSTMORTEM.md`, "a case that reads the live epoch is positioned by
        its harness thread's history".

- [x] S37.5 The turnover constant, against a corpus   *(after S37.4)*
      done: the volume the deferred-candidate lane re-offers at the epoch
        turnover is measured on a parameterized test heap over `ll-model`'s own
        blocks, the entry naming every parameter the harness sets — the share of
        roots that read live inside an epoch above all — and reporting the
        re-offered volume as a response over them rather than as one figure;
        S37.1's 64-collection turnover is then replaced or confirmed, with the
        rule that picks it from a parameter value stated beside it
      tier: T2 · role: Bench
      handoff: split out of the density measurement, now S37.7, by the Sage of
        2026-09-04. The re-offered volume is the count of roots read live
        inside one epoch that are still registered at the turnover, and on a
        synthetic population the harness chooses the rate at which a root reads
        live, so the number is its own input read back. The step needs S37.4's
        buffer and a corpus, and the corpus is Phase-D-blocked in the same way
        S37.7 is.
        Y9's formula carries a question of its own, refused for S37.0 by the
        Sage of 2026-09-10 and landed here: a minimum taken over stamped
        members alone rather than over the whole component changes the formula,
        and the corpus reading is what answers it.
      handoff: **the blocking clause above is retired 2026-09-19** — Edmond
        ruled that no corpus over `ll-model`'s own heap is coming and the step
        gets test data. What the refusal guarded stands, which is why the
        criterion now asks for a response and not a number: the harness sets the
        live-read rate, so a single figure is that input read back. Naming the
        parameters and reporting the curve keeps the reading honest about what
        it depends on.
- [ ] S37.7 The traced share and `k`, against a corpus
      done: the share of a touched block's slots a collection traces is measured
        on a parameterized test heap over `ll-model`'s own blocks with the
        denominator named — occupied slots or all slots — and the pruned-edge
        share at `k` of 1, 2 and 3 is read off the same instrumented run; both
        are reported as a response over the harness parameters the entry names,
        and S37.1's provisional `k = 3` is then replaced or confirmed, with the
        rule that picks it stated beside it
      tier: T2 · role: Bench → Critic
      handoff: what is left of S40.1, whose stage was deleted on 2026-09-18
        with its goal discharged. The synthetic arm is closed and its figures
        are in `dev/BENCHMARKS.md`, 2026-09-04 and 2026-09-12; the instrument
        is the `#[cfg(test)]` walk over the touched list plus
        `note_phase_boundary` in `trace_batch`, whose release body is empty.
        This arm needs a driver over `ll-model`'s own heap — the recorded corpus
        instruments read PHP's heap, which has no blocks and no slots — so it is
        Phase-D-blocked, as S37.2 is blocked on the compiler.
      handoff: **the blocking clause above is retired 2026-09-19** — the driver
        stays required and the corpus behind it does not: Edmond ruled that test
        data is what the calibration gets. The traced share and the pruned-edge
        share depend on the graph's shape and on the block layout, so a test
        heap reads them more honestly than it reads the turnover; the parameters
        it is built from are named in the entry either way.
      handoff: closed 2026-09-19 with both responses in `dev/BENCHMARKS.md`,
        "what a turnover re-offers, and what a deferral costs". The volume is
        `rate × N` records, one per live root per epoch, measured exactly at
        rates 1, 2, 4 and 8; the recall is `N − d + 1` collections, five cells
        of six, the sixth being the empty-lane re-offer's one-shot rescue of a
        thread's first accumulation. The load is
        `mark::tests::the_volume_a_turnover_reoffers`, and the precondition that
        cost three shapes to find is that a close with no spare cell keeps the
        root in the active lane, so the poll refills the spares.
        **`N` is not replaced:** both costs are linear in it and pull opposite
        ways — a live root is re-traced once per `N` collections, a dead
        component waits up to `N` — so the reading gives the exchange rate and
        the side to pay is Edmond's ruling. Y9's minimum-over-stamped-members
        question is untouched and goes with the stamp, not with `N`.
      progress 2026-09-19 — both readings are taken and in `dev/BENCHMARKS.md`,
        "the pruned share against a named survival rate". The traced share
        reproduces 2026-09-04 on today's tree. The pruned share is
        `max(0, 1 − k·q)` of the standing population at retirement rate `q`,
        asserted rather than printed, by the new load
        `mark::tests::the_pruned_share_against_a_survival_rate` (32 units of a
        16-member held ring, `q` of 0, 0.125, 0.25 and 0.5, `k` of 1 to 3,
        three runs identical). **What the step still owes is the choice of `k`**:
        the spare falls by one `q` per step of `k`, so the smallest `k` spares
        the most rows and what pays for a larger one is recall, which the
        turnover prices. S37.5 priced it on 2026-09-19: a component read live
        waits `N − d + 1` collections, up to 64. So both sides of the trade are
        measured — rows spared at `1 − k·q`, latency at up to `N` — and what is
        left is the preference between them, which is Edmond's to state.
- [x] S37.2 The acyclic gate
      done: an entity of a class the compiler marked acyclic never enters the
        candidate set, the mark reaching `object::stamp_into` through the class
        descriptor and landing at bit 8, and a red test shows an entity of an
        unmarked class still registers
      tier: T2 · role: —
      note: read as blocked on the compiler until 2026-09-19, when Edmond
        named the reading wrong — the runtime honours a bit, and what fills it
        is the compiler's business. The proof stays the compiler's: the
        field-type closure over declared property types, with `mixed`,
        `array`, an untyped property, `#[AllowDynamicProperties]`, `__set` and
        a reflection write all conservative edges to anything
        (`rfc/model/memory/static-lifetimes.md`, "Level A — Acyclic classes").
      handoff: `class::CLASS_ACYCLIC` (bit 6) and `ClassBuilder::acyclic`;
        `object::acyclic_gate_of` copies it into the instance's
        `refcount::ACYCLIC_GATE` at `stamp_into`, so the candidate gate reads
        one header word rather than dereferencing the class, and both object
        factories share it. The case is
        `object::tests::what_the_factory_stamps::`
        `a_class_proven_acyclic_stamps_the_gate_into_its_instances`, red under
        a factory that stamps nothing (gate 0 against 256) and under one that
        stamps every class (the unproven arm, 256 against 0). `template.rs`
        builds its Object-kind entity without reading the class flags, so a
        template class marked acyclic would not carry the gate — conservative,
        and left alone rather than branching the template path.
- [x] S37.3 The ownership mark
      handoff: `memory::barrier::store_ptr_owned` / `store_box_owned` move the
        mark and `object::ll_owned_child_die` honours it (`dev/DECISIONS.md`,
        "the ownership mark is the owned store's to move and the holder's
        `dispose` to honour").

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
  "Mutator progress while collection is unavailable"). The collector-thread
  accelerator this entry used to carry is void (`dev/DECISIONS.md`, "the free
  path's reading of the token is fenced against the take, and the accelerator
  question of 2026-09-15 is void").
- [ ] **The birth count and the unique-owner policy.** The text went with the
  file on 2026-08-26 and is on `archive/pre-rc-cycle`; two composition stubs
  in `rfc/model/gc/pure-destructors.md` still cite it. The move rule is owed a
  home outside `rfc` by the ruling of 2026-08-23. Gated on a Phase D
  measurement of the share of dynamic publications with compiler-provable
  targets.
- [ ] **Per-structure GC memory, behind a feature.** Which structure holds
  collection's logical bytes is not carried in a production build (Edmond,
  2026-09-01); the breakdown is an axis A feature designed with
  `dev/design/debug-modes.md` §8, and what it needs before it is built is a
  question that wants it.
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
- [ ] **The rest of the language runtime.** Seven are in `rfc/BACKLOG.md`:
  exceptions, actors, closures, enums, generators and fibers, resources,
  generics; `rfc/stdlib/README.md` and `rfc/io/README.md` are placeholders.
- [ ] **Phase D, the vertical slice** — hello-world through the whole stack,
  PHP to IR to executable, on the simplest memory setup. It validates the
  central bet and unblocks every calibration item here; it waits on the
  execution-pipeline decisions (`rfc/BACKLOG.md`, "The big one") and on the
  C++/LLVM front end, both outside this crate.

## Residual / carried-over items

- [ ] **A quiet thread's garbage is taken after X.** Edmond, 2026-09-18: the
  GC takes a thread's garbage of its own accord once some time X has passed.
  Today a collector serves a mutator whose R holds `worker::SOFT_THRESHOLD`
  (64) records or more, the round's threshold is constant in the working
  build (`worker::threshold_for_rounds`), the timer sets the rounds' cadence
  and not their threshold, and a thread below the threshold, under no pressure and making
  no explicit fire keeps its cycle garbage until it crosses the threshold or
  exits (`cycle::collect::tests::what_the_byte_arms::`
  `an_unarmed_poll_leaves_a_completed_death_registered`). The cheapest form
  leaves the poll alone: a round serves a mutator it has not served for X at
  a threshold of one. X is not a crate constant — it comes from the pressure
  path or from an rfc ABI (`dev/DECISIONS.md`, "the safepoint poll takes the
  free path's road").
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
- [ ] **A bounded sweep of R, when a workload asks for it.** Built and taken
  out on 2026-09-18 (`0ceb364`, reverted the same day): `BlockSweep` over one
  block of the ring, retiring completed deaths in place. Not built because
  the `POSTED` fire already compacts R whole and the unarmed poll runs no
  retirement by ruling (`dev/DECISIONS.md`, "the safepoint poll takes the
  free path's road"); the hold it would shorten is at most sixty-three dead
  slots per thread. Comes back when a measured workload shows that hold.
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
- [ ] **Slot-typed pointer stores for provenance.** A box cell's +8 word is
  stored and read as an integer, so the bytes carry no provenance and Miri
  runs the pointer-heavy modules under permissive provenance
  (`dev/WORKFLOW.md`, Miri, "Known limits"); an `AtomicPtr` at every writer
  would keep it, at a cost nobody has priced (`rfc/dev/DECISIONS.md`, "A1
  closes on a discriminating word", the withdrawn provenance clause).
- [ ] **The per-process key's Windows randomness source.** `hash/process_key`
  is unix-only, `#[cfg(not(unix))]` a `compile_error!` naming this gap, until
  a session on the Windows box adds the source (`BCryptGenRandom` or an
  equivalent) and runs the gate there. Deferred by Edmond, 2026-08-17.
- [ ] **The collector's birth has no run off Linux.** `cycle::worker::birth`
  has a windows arm (`CreateThread`, `WaitForSingleObject`, `CloseHandle`)
  that type-checks against `x86_64-pc-windows-gnu` and has run nowhere, and
  a unix arm whose `pthread_setname_np` is declared for linux and android
  alone; the aarch64 target checks clean and has run nowhere. The Windows
  run waits on the same session as the key above. On the unixes std backs
  its `Mutex` and `Condvar` with pthread rather than a futex — macOS and
  the BSDs but freebsd, openbsd and dragonfly — the lock boxes itself on
  its first use, so the collector slots' wake words, the slot locks and
  `REFUSED_AT` would each make one global allocation on the first pressure
  collection there: the birth's deny reading is linux's and windows's, and
  a build for those targets owes the locks a first touch outside the
  pressure path or a futex the crate declares itself.
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
  raw C ABI from another thread. Order: a program that frees another
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
  the item needs a workload, not a mechanism: rpmalloc decommits free pages
  past a per-type threshold and caches freed huge mappings in a 32-slot cache
  evicted by age; ours never come back and `LARGE_RUN` unmaps on every free.
- [ ] Buffer *K* and memory-pressure mode thresholds — blocked on D: they need
  real workloads (`rfc/model/memory/buffers.md`).
- [ ] Per-block dense/sparse reset threshold calibration — blocked on D for
  the same reason (`rfc/model/memory/arena-reset.md`).

Object model, deferred by design:

- [ ] General interception Proxy — transparent method interception on an
  existing target without touching its class; prerequisite for
  proxy-mediated movability. Needs a mechanism discussion.
- [ ] Binary-level class interceptors (vtable-slot patching) — check whether
  this is the same mechanism as the deferred CHA-style optimistic
  devirtualization (`rfc/model/classes.md`, Deferred).
- [ ] Allocation telemetry layer 2 / debug mode — full design in
  `dev/design/debug-modes.md`, build order its section 10; item 1, the event
  journal, is built, the rest unscheduled.
