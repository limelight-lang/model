# Plan

Implementation plan for `ll-model`, re-sorted 2026-07-24 against the RFC
after the 2026-07-22 object-layout redesign landed in the `rfc` repo.

Design lives in `rfc` and is authoritative — read before coding, do not
re-derive: `model/classes.md`, `model/values.md`, `model/lowering.md`,
`model/gc/rc-cycle.md`, `model/gc/cycle/questions.md`, `model/memory/ffi.md`,
`runtime/object-lifecycle.md`.

The `rfc` repository carries its own plan at `dev/PLAN.md` for work that lands
in the specification rather than in this crate.

Updated: 2026-09-18 · Active: S37. Every open step is blocked outside this
repository or on a corpus: S37.2 waits on the compiler that computes the
acyclic proof, S37.5 and S37.7 on the Phase-D corpus. The prose
sections after S37 are the backlog, and the next stage is drawn from them.

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
through S52 — every number this plan has spent but S37. A number is never
reissued, so a
stage added later sits where it is to be done rather than where its
number falls, and the prose sections below are the backlog stages are
drawn from.

**`array::` is run under Miri in slices**, never whole — the invocation, the
thread cap and each slice's measured cost are in `dev/WORKFLOW.md`, Miri.

**The crate collects cycles in-line.** `cycle::collect` is the collector
and the heap's slow path its allocation-pressure caller. Ordinary collections keep rows through
teardown; pressure collections harvest a bounded member list. The collector
thread is `cycle::worker`, S49's, and the exclusion between it and the
in-line collection is the trace token, `cycle::token`.

**Every cycle-GC improvement has one review gate, and the Sage is the
escalation** (Edmond, 2026-09-10 and again 2026-09-12: the Critic first, the
Sage only for what the model cannot answer itself). The pre-change baseline the
step would otherwise erase — its operation count, manager-allocation budget,
cache working set, lifetime and refusal model — is taken by the model before
the first code edit and recorded in the step; a red test is then seen failing;
after the implementation, the Critic reviews the repair and its mutations
before the step can be checked, and a finding the model can neither accept
with a repair nor refuse with a reason goes to the Sage. The findings are
recorded in the step's handoff. This applies to every step of S37; one broad
review does not waive a later step's gate.

**Every byte owned for cycle collection comes from the memory manager and is
identifiable there as GC memory.** Production collection paths use no
allocator-owning Rust containers — no `Box`, `Vec`, `HashMap`, `BTreeMap`,
`Arc` backing allocation or hidden `GlobalAlloc`. Plain `#[repr(C)]` layouts,
fixed arrays, slices and raw links are representation, not ownership; their
backing blocks come through the manager. What makes this executable is the
deny run `cycle::collect::tests::what_a_collection_asks_the_allocator`, over a
counting allocator under the whole crate, and the ledger
`memory::gc_metadata` (`dev/DECISIONS.md`, "GC memory is counted once, and
the block kind is the split").

**Verification is one configuration** since 2026-08-26: the GC axis went with
the collectors, `hash-folding` and `debug-journal` are what remains, and
`cargo bench --no-run` is part of the gate because `cargo test --lib` builds
no bench target while `benches/lifecycle.rs` imports the GC ABI
(`dev/WORKFLOW.md`).

## Fog

A line here is an unresolved question rather than a step: it carries no
criterion, and it leaves when it gets one or when it is ruled on.

- **Whether an occupant of a retained block can be freed from another
  thread inside the reset that retained it.** `retained::occupant_freed`
  subtracts 1 from the low half of a count word whose high half holds the
  pins, so a free that arrives before the reset has established occupant
  counts borrows out of the pins: a `debug_assert` in a debug build, and in
  a release build one silently eaten pin and a block held for ever. With no
  pin standing either — the ordinary shape, a block retained for its
  occupants alone — the whole word wraps instead, and then
  `has_held_occupants` answers true for the life of the process. The
  guard that would refuse such a free reads `reset_window::is_open`, which
  is a thread-local, so it answers for the freeing thread rather than for
  the block's. Raised by the Critic of 2026-09-13 over the reset's pin chain,
  as a probe rather than a claim: whether a promoted survivor is reachable from
  another thread mid-reset is unestablished, and the probe is one red test.

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

## S37 — Maturation and the two class gates

Goal: the trace stops following the whole heap. On a booted Laravel corpus the
subgraph reachable from a median candidate root is 381 of 381 objects, so this
stage is what makes a trace affordable rather than what tunes it.

- [x] S37.0 The commit stamps the live components it read   *(before S37.1)*
      handoff: `cycle::maturation`, called from `commit_before_drops` after
        `Finalization::begin` and before the first guard; Pearce's single index
        lives in the working count of the entity's own row
        (`shadow::write_live_index`) and the component stack is the arena's
        `components` chain, so a segment refusal leaves every closed component
        stamped whole. The ruling and its refusals are `dev/DECISIONS.md`, "the
        live population is stamped by component"; the peak and the ordering
        before `reclamation` are in the module doc. Verified on the final tree:
        845 passed, 0 failed, 10 ignored, three times at eight threads, five
        source mutations seen red, Miri over `cycle::maturation` and two
        `collect` cases.

- [x] S37.6 The close disposes of a batch per root   *(after S37.0, before S37.1)*
      handoff: the mark is `queue::DEFERRED_MARK`, bit 0 of a stored entry,
        written by `ActiveTrace::mark_roots_for_deferral` while the rows still
        stand and masked off by `compaction::stage_entry`; the lane is a side
        exit of the one compaction pass and an append that finds both spare
        cells empty sends the root to the active lane instead
        (`dev/DECISIONS.md`, "the deferred lane is a side exit of the
        compaction pass"). Verified on the final tree: 849 passed, 0 failed, 10
        ignored, three times at eight threads, four mutations seen red, Miri
        over the deferring-pass case, the three disposition cases and
        `queue::tests::the_tokens_every_lane_holds`. The two debts it left are
        in the residual list.

- [x] S37.1 The maturation stamp is an edge-side prune
      handoff: `cycle::mark::visit_child` tests the target before the block
        dispatch — a stamp of this collection's epoch at
        `TRAVERSAL_AGE_THRESHOLD` (3) over a target `CANDIDATE_BIT` does not
        stand on is an opaque live external, with no subtraction, no expansion
        and no dispatch — and `take_edges_pruned` counts what it cut. `k = 3`
        and the 64-collection turnover stay provisional (S37.5, S37.7); why the
        prune cannot cost a live component its teardown is `dev/DECISIONS.md`,
        "the edge-side prune cannot tear down a live component". Verified on the
        final tree: 852 passed, 0 failed, 10 ignored, three times at eight
        threads, four mutations seen red, Miri `cycle::mark::` 9 passed. The
        recall-loss case it did not write is in the residual list.
- [x] S37.4 The deferred-candidate buffer and the turnover re-offer
      handoff: `queue::defer_candidates` parks the batch in `deferred_segment`
        and `reoffer_deferred_if_epoch_moved` re-offers it at the turnover,
        which `epoch::turnovers_of` reads; the two criterion cases are
        `cycle/collect/tests/when_the_turnover_reoffers.rs`, staging the
        reading with `InjectedVerdictRace`. Why the lane holds a registered root
        and never a traced live member is `dev/DECISIONS.md`, "the deferred lane
        holds a registered root"; YRC's borrowed 56 % is `dev/RESEARCH.md`. The
        sweep the close no longer makes is in the residual list.
- [~] S37.8 The review's simplifications in the collector's own modules
      done: `worker::serve`, `worker::thread_body`, `worker::batch`,
        `worker::round`, `collect::collect_under_pressure`,
        `collect::commit_before_drops`, `token::TraceToken::take_unless` and
        `deferred_slot_reuse::make_returns_withheld_under_a_foreign_trace` each
        stand under fifty code lines and nest control constructs no more than
        two deep — an `if`, `match`, `loop`, `while` or `for` counted where it
        opens — every lifted body named in the commit; the `--list` diff in all
        three configurations shows no case added or removed, and the poll probe
        `collect/tests/what_the_poll_costs.rs` is re-run and its reading
        reported beside the recorded one (`dev/BENCHMARKS.md`, "S51.5 the token
        handshake's instruments")
      tier: T2 · role: Critic
      note: the eleven places, their measured figures and the three proposals
        refused are the review of 2026-09-18 in this file's header. The three
        loops of the withheld-return drain are one algorithm three times, and
        `splice_behind_the_head` beside them already carries the parameter shape
        a shared body needs.
      Critic 2026-09-18: one defect, and it was mine to make — the extraction
        of `refused_under_pressure` went in between `collect_under_pressure`'s
        sixty-line doc block and the function, so the whole pressure path's
        contract and its `# Safety` clause stood over a helper that traces
        nothing and runs no destructor, while the function three `As
        [collect_under_pressure]` clauses point at had none at all. The block
        is back where it belongs and the helper carries a precondition it owns.
        Two findings accepted as costing nothing, both named where they are: the
        chunk drain reads the packed word twice per chunk, once for the link and
        once for the capacity, which one owner-only word cannot disagree on; and
        an arena refusal now drops the harvest inside `tear_down_the_harvest`
        rather than after the caller's `arm()`, which `StandingMembers::drop`
        makes unobservable. What it read and found sound is the wait under the
        claim, the drain's early answer against the old `return`, the advance
        guard's two points and the folded `Served` arms.
      handoff: eight functions cut and nine named ones lifted —
        `answer_a_refused_request` and `wait_for_consent` out of `serve` (80
        code lines to 42), `begin_the_thread`, `next_interval`,
        `grow_the_siblings` and `note_idleness` out of `thread_body` (77 to
        45), `post_the_verdicts` and `size_the_next_batch` out of `batch` (65
        to 45) with `AdvanceOnDrop` now a module-level guard,
        `read_one_record` out of `round`, `refused_under_pressure`,
        `after_the_harvest` and `tear_down_the_harvest` out of
        `collect_under_pressure` (79 to 46),
        `reclaim_what_the_second_reading_confirms` out of `commit_before_drops`,
        `wait_out_a_claim` out of `TraceToken::take_unless`, and one
        `drain_withheld` in place of the three withheld-return loops. Two
        `match` arms took guards instead of an inner `if`. Nothing of the eight
        is over 50 code lines or nests control constructs more than two deep.
        The poll is where it was: 7.46–7.61 ns against 7.58–7.71 before, two
        binaries A B A B (`dev/BENCHMARKS.md`, "S37.8 the review's cuts leave
        the poll where it was"). Gate on the final tree: 1041 passed, 0 failed,
        28 ignored, three times at four threads, `hash-folding` once,
        `debug-journal` 1050 three times, `--list` unchanged in all three
        configurations, release, `cargo bench --no-run`, `cargo +1.94 fmt
        --check`, `cargo doc` 49 warnings against the same 49 before.
        `citations.py` fell from 13 misses to 6: the owner-to-mutator rename
        had rewritten eight citations whose journal headings still read
        *owner*, and a citation keeps its heading. Miri at two threads:
        `cycle::token::tests` 7 passed (15.55 s, 42 s of wall) and
        `cycle::deferred_slot_reuse` 52 passed, 1 ignored (66.83 s, 8 m 30 s);
        `worker::tests::the_batch` and `collect::tests::what_the_byte_arms`
        were still running when this was written, each on the long case
        `dev/WORKFLOW.md` names.
- [ ] S37.5 The turnover constant, against a corpus   *(after S37.4)*
      done: the volume the deferred-candidate lane re-offers is measured at the
        epoch turnover on a corpus, and S37.1's 64-collection turnover is
        replaced by a number or recorded as confirmed with its measurement; a
        synthetic reading is refused and the entry says so
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
- [ ] S37.7 The traced share and `k`, against a corpus
      done: the share of a touched block's slots that a real collection traces
        is measured on a booted corpus with the denominator named — occupied
        slots or all slots — and the pruned-edge share at `k` of 1, 2 and 3 is
        read off the same instrumented run, which replaces S37.1's provisional
        `k = 3` with a number or records it as confirmed with its measurement
      tier: T2 · role: Bench → Critic
      handoff: what is left of S40.1, whose stage was deleted on 2026-09-18
        with its goal discharged. The synthetic arm is closed and its figures
        are in `dev/BENCHMARKS.md`, 2026-09-04 and 2026-09-12; the instrument
        is the `#[cfg(test)]` walk over the touched list plus
        `note_phase_boundary` in `trace_batch`, whose release body is empty.
        This arm needs a driver over `ll-model`'s own heap — the recorded corpus
        instruments read PHP's heap, which has no blocks and no slots — so it is
        Phase-D-blocked, as S37.2 is blocked on the compiler.
- [ ] S37.2 The acyclic gate
      done: an entity of a class the compiler marked acyclic never enters the
        candidate set, the mark reaching `object::stamp_into` through the class
        descriptor and landing at bit 8, and a red test shows an entity of an
        unmarked class still registers
      tier: T2 · role: —
      handoff: the proof is the compiler's and never this crate's — the
        field-type closure over declared property types, with `mixed`, `array`,
        an untyped property, `#[AllowDynamicProperties]`, `__set` and a
        reflection write all conservative edges to anything
        (`rfc/model/memory/static-lifetimes.md`, "Level A — Acyclic classes").
        What is left here is the channel: which class flag carries the answer
        and where `stamp_into` reads it. Blocked on the compiler, and listed so
        the dependency is visible rather than discovered.
- [x] S37.3 The ownership mark
      handoff: `memory::barrier::store_ptr_owned` / `store_box_owned` and the
        ABI pair `ll_store_ptr_owned` / `ll_store_box_owned` move the mark,
        which lands only on a GC-heap occupant of a GC-heap holder;
        `object::ll_owned_child_die` is what `ll_default_dispose` calls for a
        marked child and what a generated `dispose` owes. The ruling, its
        rejected forms and its cost clause are `dev/DECISIONS.md`, "the
        ownership mark is the owned store's to move and the holder's `dispose`
        to honour". Verified on the final tree: 863 passed, 0 failed, 10
        ignored, three times at eight threads, six mutations seen red, Miri 14
        passed over the three new groups and `the_ordinary_store`. The owned
        store's refusal has no case, and that debt is in the residual list.

---

## Cross-cutting (every stage)

- The old collectors are reachable at `archive/pre-rc-cycle` and nowhere else.
  Nothing is copied back without a decision entry.
- Every fix carries a regression test verified to fail on the bug
  (`dev/WORKFLOW.md`, Tests).
- Correctness tests per the project style (`test_guard`, scenario-per-test);
  benchmarks per `dev/BENCHMARKS.md`, and a bench does not cross the C ABI —
  ABI-entry work is shown by IR or asm.
- Miri runs in slices, never whole (`dev/WORKFLOW.md`, Miri).
- A claim about speed is a measurement or it is not made.
- `dev/ARCHITECTURE.md` is the crate's knowledge map — layers and their
  sanctioned edges, the per-module "does not know" table, the header-bit
  ledger, the five end-to-end paths — and it moves with behaviour like any
  other document (`dev/WORKFLOW.md`).

## Then: arrays as a performance problem

Opened 2026-08-07 at Edmond's request. What was representation work in it
is built — the generic element write, the strategy tag in the head, the
32-byte entry with its collision link inside the element Box, and the
2 → 3 migration — and the reasoning behind each is in `dev/DECISIONS.md`
(2026-08-07 for the entry, 2026-08-11 for the head). What is left is
measurement.

**Four constants stand on borrowed or invented numbers**: the string-key
check's threshold, the compaction threshold taken from Zend at about 3 %,
and the flood ladder's two, `EQUAL_HASH_LIMIT` and `CHAIN_LIMIT`. Three of
the four cannot be settled on this box — `dev/BENCHMARKS.md` puts its noise
floor at 1.5–3 % and every effect in question is smaller — so they wait for
a machine that can resolve them, and measuring here would produce a number
indistinguishable from noise and harder to retract than to publish.

**The string-key one is different and was mis-parked here.**
`rfc/model/arrays-hashtable.md` names it in advance as a **cancellation
threshold** of **1.5x**, not a percentage: "if the control-byte index wins
both lookups by 1.5x or more at N between 56 and 28 672 on string keys of
realistic length, without its deletion margin worsening, the default
changes." A 1.5x margin is thirty times the noise floor and is resolvable
here, so it leaves the parked set and takes an entry of its own:

- [ ] **The string-key cancellation threshold, measured here.**
  done: the control-byte index and the plain check are timed against each
  other at N of 56, 512, 4,096 and 28,672 on string keys of realistic length,
  with the deletion margin read beside them, and the default either changes or
  is recorded as kept with the readings; the figures go to
  `dev/BENCHMARKS.md` and the criterion is `rfc/model/arrays-hashtable.md`'s
  own, quoted above.
- [ ] **Whether an escalation raises an operations-visible signal.**
  done: `rfc/model/arrays-hashtable.md`'s open bullet carries this half and
  this plan never picked it up; the entry is either a decision recorded with
  its reason or a line saying the rfc answers it, and it names which.

`arrays-hashtable.md`'s own open bullet also carries a second half this
plan never picked up: whether an escalation raises an operations-visible
signal.

## The vocabulary

**The rename happened**, closed 2026-09-02 against
`rfc/dev/GLOSSARY.md`, whose deprecated table holds 46 rows and whose writing
rule names six metaphors rather than the two this section used to carry.
`ResetWindow::escrow` took its ratified name there. Counted at `f1ad00f`:
`door` 5 in the code and 320 in the documents, `escrow` 45 and 79 — the five
`door`s are inside the guard that retires the word, and every count in the
documents is a record of the rename rather than a use.

What remains is the standing net, not a task: three guards in
`src/cycle/tests/` fail on a retired identifier, a retired word in a comment,
or a metaphor outside a citation, and their failure messages name
`dev/CYCLE-TERMINOLOGY-AUDIT.md` and `dev/PROJECT-TERMINOLOGY-AUDIT.md` as the
tables to read. `rfc`'s own S9.1 is still open in that repository's plan and
carries the remaining cross-repository work.

Five residues have no owner, and all five are named here rather than in a
step. The sixth left on 2026-09-18: `exact test` is out of the crate's
comments — nine occurrences, rewritten as *exact validation* — and
`the_metaphors_the_comments_still_carry::no_comment_carries_a_retired_term_of_two_words`
holds the word out, the guards' unit having been one word until then. What
stands is the journals' record of the term and two review documents of
2026-08.

- **`promote` keeps `corpse` for the reset's torn-down entity**, which the
  glossary names a *torn-down entity* (`rfc` `9ca669c`,
  `dev/CYCLE-TERMINOLOGY-AUDIT.md`, "Glossary check"); the exemptions in
  `cycle::tests::the_metaphors_the_names_still_carry` and
  `..._the_comments_still_carry` say so. `memory::reset_window` took the
  glossary's words on 2026-09-12.

- **The row-initialization bitmap's accessors have no ratified name.**
  `groups`, `group_bit` and `group_bytes` are described in
  `dev/CYCLE-TERMINOLOGY-AUDIT.md` and were never put to the glossary, so the
  crate is naming them for itself, which the rule against that forbids
  (`dev/DECISIONS.md`, "an uncovered term is a gap rather than a local
  ruling").
- **The comment guard reads fourteen words and one term of a ninety-one-row
  mapping.** It
  walks the whole crate, so the gap is the list and not the reach: `refused`
  is not among the fourteen, which is why five files carried its retired sense
  until a Critic round found it by reading. The same gap leaves 65 comment
  occurrences of `colour` standing against the audit's US-spelling rule, one
  of them directly above `shadow::color`.
- **Neither metaphor guard refuses a stale exemption.** The identifier guard
  has a test that fails on a file which has stopped offending; the name and
  comment guards have none, so the day an exempted name is renamed its
  exemption goes on exempting whatever is spelled that way next.
- **`cycle::deferred_slot_reuse` outgrew its name.** `ActiveTrace` lives
  there and owns the scratch arena, takes the detached candidate chain and
  hands out rows, while the module header still describes the slot-return
  window alone. The name was right for the module of 2026-09-01; a reader
  looking for where a collection begins does not open it now.

## Beside the hashtable: the memory categories

Opened 2026-08-06, out of the same review chain, and independent of the
questions above. The routing item of that round is closed
(`memory/routing.rs`, and `dev/DECISIONS.md`); two are left, and the
second gates the first.

- [ ] **Rename the memory categories**, in the RFC where they are
  defined, through the documents that refer to them, and in the crate —
  **deferred 2026-08-06**, reasoning in `dev/DECISIONS.md`. `LongLived`
  is named after a duration rather than an owner, which is why its
  reclamation was never decided; `Region` would mark exactly the entities
  no region owns, a `#[Region]` class owning *arenas*; and `Arena` would
  make `arenas.md`'s "between two request arenas: forbidden" false before
  the mechanism justifying it exists. Meanwhile the category is marked
  out of use on the enum itself.
- [ ] **The region reset, and the refusal that waits on it.** The
  mechanism that would make a long-lived category mean something: what a
  region owns, when it resets, how the owner's O(1) death reaches its
  entities, and what promotion across a region boundary is.
  `rfc/model/memory/regions.md` is the starting point. It also gates
  `ll_string_new_dynamic`'s refusal of that category — today nothing
  would reclaim such a string. Blocked on design, not scheduled.

## What is left of the old phase lists

The A-chain of the 2026-07-24 status snapshot is finished but for two
items, and every rc-walk build step of Phase B is built, so both lists
were deleted with the snapshot that framed them. What survives is below,
each line verified against the code on 2026-08-13 rather than against its
own checkbox.

- [ ] **A3's factory half.** The descriptor carries `dispose`, and
  `ll_default_dispose` stands in until the compiler generates one.
  `factory` cannot be stood in for the same way: its signature is
  `factory(ctx, category)` with no class parameter, so it needs per-class
  generation, and the generic path stays
  `ll_object_new(ctx, class, category)`. `clone`, `deep_clone`,
  `thread_clone` and `thread_move` are reserved for the multi-threading
  future. `rfc/runtime/object-lifecycle.md`'s "Only the GC reads layout as
  data, through `traced_runs`" holds once generated
  disposes replace the stand-in. `rfc/runtime/object-lifecycle.md`.
- [ ] **A7, no zeroing by default.** `ll_object_new` zero-fills the whole
  body unconditionally; which slots need a defined initial state is the
  factory's to decide (`rfc/BACKLOG.md`, deferred optimizations).
- [ ] **`Lazy` (code 1) and `Box` (code 10) have no producer.**
  `ll_entity_die`'s switch serves five; Box waits on the FFI surface and
  Lazy on the compiler. Only Box reaches the `debug_assert!` meanwhile:
  `ll_entity_die` already routes `OBJECT | LAZY` to `ll_object_die`.
  `Lazy` nevertheless answers yes to `EntityKind::closes_a_ring`, on the
  argument recorded in `dev/DECISIONS.md`, "a kind's ring classification is
  written at its declaration, before a factory stamps it". `StringDynamic`
  (code 9) is not carried here: `string::publish_uninit` stamps it whenever the
  placement is out of line, so the kind has a producer.
- [ ] **The threshold arming policy.** What is left of the old escalation
  ladder after the entry gate (`cycle::collect::may_collect`) and the
  slow-path fire. The arming policy is the compiler's
  (`rfc/model/gc/strategies.md`, arm/fire); the critical reserve's third
  customer, the mutator whose gate is closed, is answered null today and
  draws nothing, because which runtime progress operations a reserve would
  fund is what the ABI does not yet name
  (`rfc/model/memory/critical-reserve.md`, "Mutator progress while collection
  is unavailable"). The collector-thread accelerator this entry used to carry
  is void: the collector wakes on the owner's count of its own registrations
  and on its timer and traces on the count it reads itself, so it works ahead
  of a shortage with no ABI (`dev/DECISIONS.md`, "the free path's reading of
  the token is fenced against the take, and the accelerator question of
  2026-09-15 is void").
- [ ] **The birth count and the unique-owner policy** — **the text went with
  the file** on 2026-08-26 and no section landed in `rc-cycle.md` or
  `cycle/questions.md`; it is on `archive/pre-rc-cycle`. What is left in the
  active tree are two composition stubs still citing it,
  `rfc/model/gc/pure-destructors.md`'s "With unique ownership" and "With the
  birth count". The move rule — copy, barrier, or a never-moved proof — is
  **not** the open question it was: the ruling of 2026-08-23 put "what can the
  compiler prove" outside `rfc`'s scope by name and left the move rule "owed a
  home outside these documents". Still gated on a Phase D measurement of the
  share of dynamic publications with compiler-provable targets.
- [ ] **Per-structure GC memory, behind a feature.** Which structure holds
  collection's logical bytes — shadow rows, the trace worklist, a component's
  member list, deferred slots, deferred drops, the deferred-candidate lane —
  is not carried in a
  production build (Edmond, 2026-09-01). The breakdown is an axis A feature
  designed with `dev/design/debug-modes.md` §8; what it needs before it is
  built is a question that wants it.
- [ ] **Pure destructors, and the hand-off drain** — proposed by
  Edmond 2026-08-18, analyzed the same day through three lenses and two
  Critic rounds; the analysis is `rfc/model/gc/pure-destructors.md`, with
  the 2026-08-23 amendment that withdraws the collector-side free. The runtime-only step (the specialized P0 dispose and the
  raw-sever drain arm) needs no ruling and no compiler; the hand-off
  drain waits on the residual-duties and tail-bound questions the
  analysis names, its external-child delay accepted by ruling
  2026-08-18; the child-release-order ruling landed the same day —
  specified, P2 keeps its call (`dev/DECISIONS.md`) — so the
  compiler tiers wait only on the compiler. The composition with the
  ownership pair — including the fast class that can block its own memory
  return — was `dev/design/owned-slots-and-the-walk.md`, deleted 2026-08-26
  and readable on `archive/pre-rc-cycle`; it is argued against the walk
  throughout, so it is a source to re-read rather than a conclusion to
  carry.
**The horizon's borrow elision** — Edmond's algorithm of 2026-08-18, named
`proof-horizon` until 2026-08-20 — is on `archive/pre-rc-cycle` with the six
documents deleted on 2026-08-26, and it is no task here: the proof logic left
`rfc`'s scope on 2026-08-23, nothing in force cites it, and the pre-D
instrument work it scheduled has no document to serve. The name is written
here so a search finds it; what outlived the deletion is in the ruling that
took it.
- [ ] **The three `promote` tests that claim Miri as their whole regression.**
  `the_reset_reads_no_zero_count_member`'s
  `a_large_survivor_killed_by_the_drain_is_not_read_by_the_reconcile` and its
  two neighbours guard the reset window against reading a large run after it
  was unmapped, and their doc comments say `cargo test` passes the defect by
  construction. They have run under Miri again since the `memory::os` arm that
  keeps an oversized mapping whole, and whether
  any of them still exhibits its defect is unverified: the reconcile one was
  run on 2026-08-29 with `reset_window::park_large` returning false — the
  mutation its neighbour's comment names — and passed in 176 s. Either that is
  not the mutation those comments mean, or a second half of the arrangement is
  missing. Each test is either seen failing under Miri against the mutation it
  names, or its comment is corrected to say what it does prove.
  `dev/WORKFLOW.md`, Miri, carries the same paragraph.
- [ ] **Strategy 1, the typed vector.** No producer, so the 1 → 2
  transition waits on one — `dev/DECISIONS.md`, 2026-08-13, which also
  says what to confirm against `arrays.md` before opening it.
- [ ] **The rest of the language runtime.** Seven are in `rfc/BACKLOG.md`:
  exceptions, actors, closures, enums, generators and fibers, resources,
  generics. Two are not, and exist only as a three-line placeholder each —
  `rfc/stdlib/README.md` and `rfc/io/README.md`.
- [ ] **Phase D, the vertical slice** — hello-world through the whole
  stack, PHP to IR to executable, on the simplest memory setup. It
  validates the central bet, that the compiler can prove escape,
  monomorphism and ARC-pairing on real PHP, and it unblocks every
  calibration item below. It runs as a parallel track rather than in
  turn, because it waits on the unwritten execution-pipeline decisions
  (`rfc/BACKLOG.md`, "the big one") and on the C++/LLVM front end, both
  outside this crate.

## Residual / carried-over items

- [ ] **The zero-refcount pass over R.** Edmond's fifth line of 2026-09-17,
  an option: the collection `POSTED` fires also walks R and retires the
  entries whose count already reads zero, one count load per entry and no
  trace, leaving every other entry to the collector. The Sage's bound if it
  is built: one block of R per fire by a cursor over the occupied run,
  the front-block-only and capped forms refused; a bench line before it is
  called free. What it buys: slot retirement no longer waits on the
  collector's throughput (S51's named cost).
- [ ] **What S51 named and left, 2026-09-17.** Two debts of the harness the
  stage-close Code Reviewer priced: every non-ignored case runs the serves
  at a request wait of 2 s (`worker::testing::HARNESS_REQUEST_WAIT`), a
  thousand times the crate's 2 ms, so the shipped bound is exercised by the
  ignored probes alone; and two fixtures — `worker::tests::Mutator::start`
  and `the_siblings`'s round wait — clear `POSTED` by hand so that the
  collector batches again without a collection between, which suspends the
  invariant "`FREE` promises an empty P" for those cases and would mask a
  disposition missing in production there. A form that runs the harness at
  the crate's wait needs the consent to come from a real poll on a loaded
  box; a fixture that disposes instead of clearing pays a collection per
  batch. Neither is priced.

- [ ] **What S50 left unpinned, 2026-09-16.** A second life whose `ThreadHeaps`
  allocation or slot store the OS refuses is set `Live` and its journal
  reopened before the heap is built (`heap::ll_thread_init`), so it frees and
  journals as a life; no seam refuses either on demand, so no case reads it.
  The seam is a test-only fault on the `ThreadHeaps` allocation, in the form
  `FORCE_GUARD_UNARMED` takes for the guard.
- [ ] **What S49 named and left, 2026-09-16.** A component past the
  collector's block budget whose owner-side trace the pool refuses circles
  P and R under pressure, arming a collection each round (Critic, S49.5); the
  collector's arena draws its own thread's critical reserve on a pool
  refusal, and no ruling says whether a collector thread may spend a
  reserve; a ring a burst grew while both spare cells were full keeps its
  blocks until a cell is spent, with no bound (`dev/DECISIONS.md`, "the
  ring's surplus goes into a short spare cell").
**A gate flake watch, not a step.** Six cases read the process-wide GC ledger
across a child thread's whole life, which no per-thread figure can answer, and
they drift if a third thread draws GC memory in that window:
`gc_metadata::tests::a_threads_exit_ends_every_block_it_acquired`,
`what_gc_owns::a_threads_base_block_is_in_use_from_its_draw_until_its_exit`,
`the_workspace_stands_between_collections_and_goes_back_at_exit`, the two
refusal cases in `the_base_block_a_thread_holds_for_its_life`, and
`mark::tests::an_aborted_mark_writes_nothing::`
`a_refusal_two_entities_deep_leaves_the_heap_byte_identical`, whose `force_oom`
is process-wide and whose reserve reading is asserted at zero. The last was
seen to fail once, on 2026-09-15, in one plain run of some twenty-five that
day; ten further runs were green and no cause was established, and none of the
other five failed in 500 runs. What would close them is a reading of a named
thread's figures that outlives the thread, which is a structure rather than a
patch, and it is worth its cost only once one of them is seen to fail. The
repair of 2026-09-03 that made the ledger answer per thread is in
`dev/DECISIONS.md`, "the test-facing reading of the GC ledger is per thread",
and its trap in `dev/POSTMORTEM.md`, "an exact assertion cannot be made against
a process-global ledger". **No work is scheduled here until a failure.**

- [ ] **The deferred lane is not swept, and the close no longer sweeps it.**
  `defer_candidates` lifted the active lane and ran the retirement pass over
  the deferred one, so a record whose entity had died gave its slot back on the
  way in; `dispose_candidates`, which the ordinary close now uses, reads the
  deferred lane not at all, and `retire_candidates` walks the active lane
  alone. A record deferred at one collection whose entity dies afterwards
  therefore withholds its slot until the turnover. The population this applies
  to grew with S37.6 from "the batch of an `ExternallyReferenced` commit" to
  "every root any collection read live". Teaching retirement to walk the
  deferred lane was refused under S37.4 — its work would become proportional to
  the accumulated deferred set — so what is owed is either a sweep bounded like
  the re-offer's or a statement that the turnover is the bound. At thread exit
  the lane is re-offered before the exit's last round, so a deferred record
  withholds nothing past the exit (`cycle::collect::collect_before_exit`).
  done: the interval a deferred record can withhold a slot for is stated with
  its bound, and a case shows it.

- [ ] **The deferred lane's second segment is written by no case.** A lane
  takes `SEGMENT_CAPACITY` — 8,160 — records per segment, and the widest
  candidate population this crate's cases build is the 4,077 of
  `queue::tests::where_a_full_segment_comes_from::`
  `a_bulk_release_polls_on_its_own_backedge`. So the growth arm of
  `compaction::Compaction::append_to_deferred_lane` and the `deferred_heads`
  term of the ledger beside it are exercised by nothing, and a mutation that
  deletes either stays green. `the_tokens_every_lane_holds::`
  `a_deferred_lane_of_two_segments_comes_back_whole` builds two segments
  through `defer_candidates`, which is the other machine.
  done: a case drives the append past one segment, or a `#[cfg(test)]` seam
  puts the lane at its bound without the population behind it.

- [ ] **The owned store's refusal has no case.** `store_ptr_owned` returns the
  plain publish's `false` before moving any mark, and nothing drives that
  branch: the copy a COW value takes leaving the arena is the only refusal a
  publish has, and `force_oom` does not reach a copy the thread's block
  already has room for. The plain store's refusal has no barrier case either.
  done: a refused owned store leaves the displaced entity's mark and the
  slot as they were, shown by a case whose refusal is the copy's.

- [ ] **The prune's own recall loss has no case.** The descent stops at a
  mature edge target, so a ring one of whose members never observed a
  non-final decrement — its creation reference moved into a cell rather than
  released — is read live at the collection that meets it and collected at the
  turnover that re-offers its root. Both halves are built and no case drives
  them together: `cycle::mark::tests::where_the_descent_stops` proves the stop
  and `cycle::collect::tests::when_the_turnover_reoffers` proves the re-offer,
  the second staging its reading with `InjectedVerdictRace` because the
  natural producer did not exist when it was written. What the fixture needs
  is a store that moves a reference instead of retaining one, which
  `test_support::store_prop` is not.
  done: one case drives a mature ring through the collection that reads it
  live and the turnover that collects it, with no injection.

- [ ] **A weak map, and the second kind of death subscriber.**
  `rfc/model/weak-references.md` names two subscriber kinds and the crate
  builds one: the canonical `WeakReference` cell. The other is a map keyed by
  object identity, marked "(future)" there, and the sever step's `done:`
  clause was written against it — "the weak notify's displaced map values" —
  so half that clause named a population no producer can make. The clause was
  struck and the work is here (Edmond, 2026-09-07: it can wait).

  What it is: an ordinary GC-heap entity over the same `array::Table` that
  `Map` reuses, its key uncounted and its value counted, so a trace reads one
  half of each entry and the map keeps no key alive. What it costs the weak
  table: a target's row stops being one subscriber and becomes a list — one
  cell plus one record per map naming that target — which is what the row's
  reserved tag bits were left for.

  **Where it meets this stage:** the death notification displaces a counted
  value per map entry, and the two sites cannot agree on when to drop it. An
  ordinary death may release it inline, being a cascade of releases already; a
  cycle death may not, because between the sever and the last free no user
  code runs (`rfc/model/gc/rc-cycle.md`, "Cycle finalization and
  reclamation", step 6). One body serves both — `weak::notify_death`, which a
  cycle teardown reaches through the ordinary death path — and it cannot be
  told which site it is on by a parameter, `dispose` being a C ABI function
  pointer. The shape that answers it: the teardown arms a thread-local sink
  for the length of its own run and the notification reads it, so a displaced
  value goes to `cycle::reclamation`'s queue there and to an inline release
  everywhere else. The queue takes it unchanged — the record is one entity
  pointer, and a second producer writes the same eight bytes.

  Two smaller obligations come with it: an arena-resident key belongs on the
  arena's weak log, the way a cell's target does, and the last subscriber
  leaving clears `HAS_WEAK_REFERENCES` so an ordinary death stops asking the
  table.
- [ ] **The ladder's refusal has nowhere to go.**
  `InsertOutcome::AdmissionDenied` is answered inside the crate — a null
  from `ll_cow_separate`, a `false` from `element::set` — because the
  crate has no error channel. `rfc/model/maps.md`, "Rung three,
  refusal", says the runtime raises it as a catchable error, and that
  waits on the exceptions work (`rfc/BACKLOG.md`). Until then a refused
  insert is indistinguishable from memory pressure to the program, which
  is the one thing the two-variant outcome exists to prevent.
- [ ] **The equal-identity trigger's tag test has no test of its own.**
  S27.3 changed the counter from "not an integer key" to "the tag equals
  the incoming string's", which in an array names the same set, so the
  change was verified by reading. `Map` is where the two sets differ —
  an object key is neither — and the test is owed there.

- [ ] **The long-key slot itself.** S27 re-keys `strong_hash`; it does
  not fill the slot `strong_hash`'s doc stands in for, which is
  HighwayHash-64 behind a length threshold `rfc/model/strings.md` says is
  unmeasured. Blocked on that measurement, and it belongs with the
  strings work rather than the table's.
- [ ] **The publication fence's ARM64 price.** `refcount::publish_header`
  emits one `fence(Release)` per entity built, a compiler barrier on x86-64
  (the release assembly's instruction multiset is identical with and without
  it, 2026-09-15) and a `dmb ish` on ARM64, whose allocation-path cost is
  unmeasured: no ARM64 machine exists in the project, and Edmond relaxed the
  rfc's "measured before it is emitted" to "before the first ARM64 build"
  (`rfc/dev/DECISIONS.md`, "the publication fence lands before its ARM64
  price"). The measurement is `benches/lifecycle.rs`'s create/release pair on
  an ARM64 box, both arms in one session, before that build ships.
- [ ] **Slot-typed pointer stores for provenance.** A box cell's +8 word is
  stored and read as an integer (`Value::entity` stores the address as a
  `u64`; `cells::counted_box_cell` reads it back and casts), so the bytes
  carry no provenance and Miri runs the pointer-heavy modules under
  permissive provenance (`dev/WORKFLOW.md`, Miri, "Known limits"). Storing
  the +8 word as `*mut u8` through an `AtomicPtr` at every writer would keep
  it, at a cost no one has priced; a step of its own if ever wanted
  (`rfc/dev/DECISIONS.md`, "A1 closes on a discriminating word", the
  withdrawn provenance clause).
- [ ] **Doc links that point at private items.** Public documentation
  links `pub(crate)` and private names — `Table::empty` to
  `Table::reseed`, `InsertOutcome::AdmissionDenied` to `CHAIN_LIMIT` and to
  `EQUAL_HASH_LIMIT` —
  which `rustdoc` warns about unless private items are documented too.
  Crate-wide practice rather than one site, so it is a ruling and not a
  fix: either the links stay and `--document-private-items` becomes how
  the crate's documentation is built, or they become plain names. Raised
  by S27's Code Reviewer, 2026-08-18.
- [ ] **The per-process key's Windows randomness source.** S27.1 lands
  unix-only, `#[cfg(not(unix))]` a `compile_error!` naming this gap, so
  the Windows build refuses until a session on the Windows box adds the
  source (`BCryptGenRandom` or an equivalent OS draw) and runs the gate
  there. Deferred by Edmond, 2026-08-17.
- [ ] **No ABI entry creates or mounts an arena.** `LLContext` is
  `#[repr(C)]` with one public pointer and a null context is legal, so an
  external caller can build one and reach the store barrier; what it
  cannot obtain is an `*mut Arena`, every arena in the crate being made
  by Rust code inside tests. An embedder needs that door before anything
  outside this crate exercises the arena paths.
What S36 left without an owner, 2026-09-14:

- [ ] **The collector thread's spawn allocates through the global allocator.**
  `std::thread::Builder::spawn` builds the thread's name and the handle's
  shared state with `String` and `Arc`, whose refusal ends the process rather
  than answering the caller, and `cycle::worker::ensure_thread` runs it at the
  end of the first pressure collection — the path on which the manager has
  just refused. The ruling that no runtime path may end the process on an
  allocation the manager could have refused (`dev/DECISIONS.md`, "the reset
  window's memory comes from the manager") covers it; what closes it is a
  spawn over `pthread_create` with a stack the manager issues, which is target
  code nobody has written. Named on 2026-09-15, by the step that built the
  thread.
- [ ] **The exit's own containers.** Three sites on paths a destructor or the
  thread's exit reaches allocate through the global allocator, under the
  ruling that no runtime path may end the process on an allocation the
  manager could have refused (`dev/DECISIONS.md`, "the reset window's memory
  comes from the manager"): `static_block::tear_down` fills a
  `Vec<*mut RcHeader>` through `object::sever_counted_slots` on the thread-exit
  release of static-block roots; `static_block::ll_static_block_register`
  grows the registry's `Vec`; and `buffer_arena::with_buffer_arena` boxes the
  thread's `BufferArena` on the first long-lived payload, which
  `weak::table::draw` can reach from a step-4 destructor. Named by the GC-memory
  audits of 2026-09-06 and 2026-09-07; none is on a collection frame, so the
  deny run does not see them. What closes each is the shape S47 gave the
  reset: a chain in memory the layer already holds, and a refusal answered to
  the caller.
- [ ] **The collection's journal kinds.** `journal/kinds.rs` carries no record
  for a collection's begin or end: the two kinds went with the collectors on
  2026-08-26, and `rc-cycle`'s were never named (`dev/design/debug-modes.md`,
  §9.5). `cycle::collect` records `KIND_EXIT_RESIDUE` alone. What a window over
  a collection would need is its two ends and which path it took, off the poll
  or under pressure.
- [ ] **A member of a kind other than an object or an array is untested through
  the commit.** A reference box, a template and a class with cells outside
  itself each have an arm in `cells::trace_cells` and `cells::sever_cells`, and
  no case drives one through validation, finalization and reclamation as a
  member; `Lazy` waits on a producer. Named at the exact test's close, 2026-08-29, and
  carried by no step since.
- [ ] **The refusal path over a component whose member's guard is its last
  reference.** Such a member is freed inside `GuardedComponent::release`, and
  the case that exercises the refusal has no such member. Named at the sever's
  close, 2026-09-07.
- [ ] **`queue::merge_candidates` copies its part-filled head one record at a
  time**, which nobody has measured against a `copy_nonoverlapping` of the run.
  Named at the ABI wiring's close, 2026-09-07; a figure, not a rewrite, is what is owed.
- [ ] **Two shapes the deny run over a reset inside a collection never enters**,
  named by the Critic of 2026-09-13 over the deny run: an emptied chain — `register`
  answering true for a block whose listed survivors all died inside the reset —
  and a destructor round that escapes something new. Each is one case in
  `cycle::collect::tests::what_a_collection_asks_the_allocator`.

Memory manager, still open:

- [ ] **Batch the cross-thread free, once a workload exists** — gated on
  measurement, and the gate comes first. Today `Heap::free_remote` posts
  each foreign slot with its own CAS onto the owning block's
  `remote_free` stack, and `buffer_arena::post_remote` does the same for
  a chunk, so the cost is linear in items freed. snmalloc gathers the
  same work into one message queue per owning allocator and pays one
  atomic operation per batch instead (`dev/RESEARCH.md`, 2026-08-08).

  The shape, if it is ever wanted: stage foreign frees in a bounded
  thread-local buffer with no atomics, group them by block on flush —
  `ptr & !BLOCK_MASK`, one AND, and a 64 KiB block holds thousands of
  slots, so a batch lands in a handful of blocks — link each group into a
  chain through the dead slots themselves, and CAS each chain onto its
  block's head once. No per-object memory: the links live in the freed
  slots, as they do now. The staging buffer is the only new memory, one
  fixed-size array per thread.

  What it costs is not memory but **return latency**: freed memory
  reaches its owner a batch late, so peak RSS rises by the batch, which
  is a real change of behaviour in a runtime whose ordinary free is
  immediate. A thread exiting with a staged batch must flush it or leak
  it; the thread-exit flush is the existing shape for that, and it lives on
  `archive/pre-rc-cycle` — `deferred_free.rs` went with `rc-walk`.

  Removing the atomic entirely means a per-thread-pair SPSC ring
  (`ck_ring`), which costs memory per pair. That is the trade snmalloc
  declines, and so should we.

  **Why not now.** Our CAS is already spread across blocks, which is
  mimalloc's contention argument, so the win would be in the count of
  atomic operations and not in contention. Nothing today drives the path:
  the crate is single-mutator, and the callers are one test group
  (`heap::tests::frees_arriving_from_another_thread`) plus whatever
  reaches the raw C ABI from another thread. Order: a program that frees
  another thread's objects in bulk, then a measurement, then this.

- [ ] Buffer *K* and memory-pressure mode thresholds — **blocked on D**:
  they need real workloads, and designing them further on paper is what
  the block is for (`rfc/model/memory/buffers.md`).
- [ ] Per-block dense/sparse reset threshold calibration — **blocked on
  D** for the same reason (`rfc/model/memory/arena-reset.md`).

Read from rpmalloc 2.0.1 on 2026-08-10 (`dev/RESEARCH.md`). Material to
think with, not decisions: none of it is measured here, and each entry
names what would have to be measured first.

- [ ] **Reallocate in place when the class does not change.**
  `stdapi::ll_realloc` refuses an entity first and then allocates, copies
  and frees on every remaining call, so 40 bytes to 48 costs a block, a
  `memcpy` and a free to move inside one 48-byte slot. `stdapi::ll_usable_size` already reads the class size out
  of the block header, so the test is one comparison on a path that is
  cold anyway. rpmalloc also declines to move a huge block that shrinks
  by less than half, and overallocates to 1.375x on a small growth so
  that a loop growing a few bytes at a time stops reallocating at every
  step (`rpmalloc.c:2402`, `2413`, `2429`).
  **What comes first:** a harness. `rptest` in `benches/standard.rs`
  frees and allocates rather than reallocating, so this path has no
  measurement at all. Nothing calls it in a running program either, but
  only because the allocator is not installed: `GlobalAlloc::realloc` calls
  it, and the `#[global_allocator]` install is still owed.

- [ ] **Size classes for the band between 8 KiB and one block.** Classes
  stop at `heap::MAX_SMALL` and everything above takes a whole 64 KiB
  block, so a 9 KiB request holds 64 KiB. Five classes divide the
  65280-byte payload without a tail — 10880, 13056, 16320, 21760 and
  32640, at six slots down to two — and hold the worst case to 1.33x at
  the bottom of the band and 1.5x at the top; past 32 KiB one object per
  block is already the two-times ceiling. The fast path need not move:
  `ll_alloc` routes anything past `MAX_SMALL` into a cold function, and
  the class is chosen there by a short comparison chain, so `CLASS_LUT`
  stays 514 entries instead of growing to 4082. Free simplifies, since
  these become ordinary heap blocks that the existing `BLOCK_KIND_HEAP`
  arm serves.
  **What it costs:** five more classes in three per-heap arrays and in
  the abandoned table, about 120 bytes per thread, and a high block
  switch rate on a two-slot class — against today's pool get and put per
  object, which is worse in every case. The routing list at the head of
  `stdapi.rs` and `docs/memory-manager.md` move with the change.
  **What comes first:** a footprint measurement, and there is none:
  `benches/alloc.rs` stops at 8192. The metric is `blocks_out` and RSS
  rather than operations per second.
  **Settle separately:** entities past 8 KiB take the same path and are
  reached by their own block header's row rather than by a stride
  (`cycle::row::resolve_edge_target`);
  a uniform stride would make them walkable, which `rc-walk` decided the
  other way and `rc-cycle` re-decided by dispatching on the block's kind.

- [ ] **A flag saying the block already reads zero.** `Heap::refill`
  writes eight bytes into every slot of an entity block unconditionally —
  up to 4080 stores at the 16-byte class, and at a 16-byte stride that
  dirties every line of the 64 KiB block. The invariant is narrower than
  the pass: the walker reads only slots below `bump` and tests one field —
  and that walker is `for_each_entity_slot`, which no production path calls
  since `rc-walk` went. One source of the knowledge is already paid for: a
  region is a fresh OS mapping and arrives zeroed. What is open is carrying
  the flag across recycling, since a block recycles through the pool and may
  have served at another stride. A region taken from the OS is untouched
  kernel
  memory. A block returned empty from an entity heap already satisfies
  the invariant, because `FreeSlot` preserves the dead entity's final
  header and an entity dies at refcount 0. What breaks it is a block that
  served as raw or arena memory in between, or a recommissioning at a
  different stride, so the flag has to name the stride it holds for.
  **What comes first:** the case that shows the cost. Amortised over the
  steady-state benchmarks it is small, refill running about 0.00003 times
  per allocation — a figure from `dev/RESEARCH.md` that `dev/BENCHMARKS.md`
  never took, so it is a reading rather than a measurement; the workload to
  measure is a growing one, where the
  pass is one extra store per object created.

- [ ] **Return memory to the OS, and cache huge mappings** — the
  prerequisite this was blocked on is met since `8208815`: a region is an
  OS mapping (`memory::os::map_aligned`) and `os::unmap` is already used
  by the large-run path. What the item now needs is a workload, not a
  mechanism. rpmalloc lets free pages
  accumulate to 16, 8, 4 or 2 per page type and then decommits down to 4,
  2, 1 or 1, keeping the header prefix committed (`rpmalloc.c:712`,
  `2003`, `1249`), and sends a freed huge mapping to a 32-slot cache
  bounded by committed bytes and evicted by age rather than straight back
  to the OS (`rpmalloc.c:1600`). Ours never come back, and `LARGE_RUN`
  unmaps on every free. Either way the block header line stays committed:
  the walker reads every block's kind across the region.

Object model, deferred by design:

- [ ] General interception Proxy — transparent method interception on an
  existing target without touching its class; prerequisite for
  proxy-mediated movability. Needs a mechanism discussion.
- [ ] Binary-level class interceptors (vtable-slot patching) — check
  whether this is the same mechanism as the deferred CHA-style optimistic
  devirtualization (`rfc/model/classes.md`, Deferred).
- [ ] Allocation telemetry layer 2 / debug mode — full design in
  `dev/design/debug-modes.md`, and the build order is its section 10.
  Item 1 of that order, the event journal, is built; the rest of the
  section is unscheduled.

