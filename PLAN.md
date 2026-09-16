# Plan

Implementation plan for `ll-model`, re-sorted 2026-07-24 against the RFC
after the 2026-07-22 object-layout redesign landed in the `rfc` repo.

Design lives in `rfc` and is authoritative — read before coding, do not
re-derive: `model/classes.md`, `model/values.md`, `model/lowering.md`,
`model/gc/rc-cycle.md`, `model/gc/cycle/questions.md`, `model/memory/ffi.md`,
`runtime/object-lifecycle.md`.

The `rfc` repository carries its own plan at `dev/PLAN.md` for work that lands
in the specification rather than in this crate.

Updated: 2026-09-16 · Active: S37, S38, S40, S49 and S50 (opened 2026-09-15 on Edmond's ruling that `ll_thread_init` is called once). S49 opened the same night on Edmond's ruling that restores his read-behind queue, and replaces the outbox form S38.5–S38.7 built. S38.0 and S38.3 closed on
2026-09-15 — the collector's reader with its fence, and the owner's returns
withheld under a foreign holder of its token; the worker, unblocked the same
evening by `rfc` S8.7, is S38.5 through S38.7 — the record, the offer and the
pickup, the thread — all three closed the same night, so S38's steps are
done and the stage waits on Edmond's answer to the Sage's question in S38.7
and then on its Code Reviewer before it is deleted; every other open step is
blocked outside this repository or on a corpus. **S48 closed and was deleted
on 2026-09-14**, the ValueBox relayout to `rfc/model/values.md`, "ValueBox
Layout", in three steps, its close read by the Code Reviewer, whose
findings (a duplicated mask, two selectors for one fact, a spill on the
store path, stale sentences) landed the same day; what outlived it is
`dev/BENCHMARKS.md` under
2026-09-14 (the instrument's calibration and the A/B/A reading, whose
figures Edmond took as the ruling's price), `rfc/dev/DECISIONS.md`, "A1
closes on a discriminating word" (the ruling, the withdrawn provenance
clause and the measured price), the crate's `value.rs` module doc, and the
backlog line "Slot-typed pointer stores for provenance" below. **S36
closed and was deleted on 2026-09-14**, its last act being the stage-close
Code Reviewer over the modules no review had read since 2026-09-01, which
found three defects at the seams between the driver's two paths and inside
the finalization chain; what outlived it is `dev/POSTMORTEM.md`, 2026-09-14,
the six backlog lines below headed "What S36 left without an owner", and the
journals of 2026-08-29 through 2026-09-13 its steps wrote as they closed.
**S47 closed and was deleted on
2026-09-14**, its last step being S47.9, the COW reconciliation as three
linear passes over the window's log; what outlived it is `dev/DECISIONS.md`
under 2026-09-13 (six entries, the survivor-cell refusal and the count-word
accumulator among them), `dev/BENCHMARKS.md` under the same date for the
grouping, the COW rows and the three passes, and `dev/POSTMORTEM.md`,
2026-09-13, for the branch only the journal observed and the
write-provenance rule broken in `refcount`'s fixtures.
S40's one open step is
S40.1's Phase-D-blocked corpus arm. **S44 closed and was deleted on 2026-09-12**, its last step being
S44.7, the window's tests under `tests/` by group in the stack's vocabulary;
what outlived it is `dev/DECISIONS.md` under 2026-09-05 and 2026-09-06 and
`dev/BENCHMARKS.md`, "S44.4 the close against the chain". **S45, closed
on 2026-09-07, was deleted on 2026-09-12**; what outlived it is
`dev/DECISIONS.md`, "the ring fixture is two functions rather than one
builder with parameters", and the correction above it. S40.2
closed on 2026-09-12: the flat row form stays, and Edmond ruled the same day
that no stage builds the chunked candidate (`dev/DECISIONS.md`, "the flat
row array stays"). S40.5 closed the same day with the chunked form specified
and the census replayed through both forms; S40.3 closed the same day with
the census and the hardware arm, its three Sage rulings now in
`dev/DECISIONS.md`, "the census is one report at two boundaries"; the Sage's
rulings of the same day on `dev/SHADOW-ROW-REPRESENTATION-ANALYSIS.md`
reshaped S40.3, added S40.5 and rewrote S40.2, and are recorded under S40.5
and S40.2. **S46 closed and was deleted on 2026-09-12**, one step on
Edmond's ruling of the same day: an exit a destructor asks for is recorded
and runs at the thread's top (`dev/DECISIONS.md`, "an exit requested inside a
collection runs at the thread's top"). **S39 closed and was deleted on 2026-09-12**, its last step
being S39.1 — the exit's wait on the thread's token and its bounded rounds of
collection over every chain, the residue reported as a journal record; what
outlived it is `dev/DECISIONS.md` under 2026-09-09 and 2026-09-12 and
`dev/BENCHMARKS.md` for the early-return measurement. S38.2 closed the same
day on code S38.1 and S38.4 had built — the wait on a held token, reached
through the allocation refusal and counted at the wait. S38.4 closed on 2026-09-11 —
the entry gate with the teardown depth as its second input, a refused poll
keeping its arming, and the slow path's refusal named by size class. S38.1
closed the same day — the per-thread trace token, taken around the trace and
waited on through a mutex. S37.3 closed the same day — the ownership mark is moved by the
barrier's owned store and honoured by the holder's `dispose`. S37.0, S37.6
and S37.1 closed on 2026-09-10 — the
live-component stamp producer, the per-root disposition and the edge-side
prune they were built for — so the descent stops at the mature live core and
S40.1's pruning arm has a counter rather than a simulation; the corpus arm of
S40.1 still waits on the Phase-D driver, and S37.5 waits on the same corpus.
Of what is left in S37, S37.2 is blocked outside this repository.
**S34 closed and was deleted on 2026-09-10**,
its last step being the law that only the owner reduces state; what outlived
it is in the journals, and the two debts it carried without an owner are in
`## Fog` and in the backlog below.
The commit — the exact validation, the guards and the weak window, the
destructors and the revalidation, the sever, the frees and the deferred
drops, the maturation stamp, the collection behind the ABI and the one an
allocation failure starts — was S36, built between 2026-08-29 and
2026-09-13 and deleted whole; Edmond's ruling of 2026-09-12 that the reset's
exemption does not reach the frames a destructor's `ll_arena_reset` enters is
`dev/DECISIONS.md`, "the reset window's memory comes from the manager".
S43 closed the withheld-return window: past its region the module draws
nothing, a death in memory the collection never met is returned at once, a
marked slot of another thread's block is stacked rather than listing its
block, and an unwind out of the close returns every mark it can reach. What
outlived it is in the journals — `dev/DECISIONS.md` under 2026-09-04 and
2026-09-05, `dev/BENCHMARKS.md` for the walk against the chain, and
`dev/POSTMORTEM.md` for the fixture trap it hit twice. The sections after S40
are the backlog.

**Closed stages are deleted whole** (rule 23.1.3), and what outlived each
of them is in the journals rather than here: `dev/DECISIONS.md` for a
decision and its reason, `dev/POSTMORTEM.md` for a trap,
`dev/BENCHMARKS.md` for a measurement, `dev/INDEX.md` and
`dev/ARCHITECTURE.md` for the map. Deleted so far: S4 through S35
and S39 through S46. A number is never reissued, so a
stage added later sits where it is to be done rather than where its
number falls, and the prose sections below are the backlog stages are
drawn from.

**`array::` is run under Miri in slices**, never whole — invocation and
thread cap in `dev/WORKFLOW.md`, Miri. What each slice costs, measured
2026-08-18 at two threads and quoted on Miri's own clock: `array::table`
without the flood ladder 32 tests in 79 s, the ladder's own module 14 in
273, and `array::entry` with the tracer and ring tests 13 in 179. All
clean. `array::entity` is the expensive one and is taken by test rather
than whole; the copy tests of that module ran 25 in 59 s.

**The crate collects cycles in-line.** `cycle::collect` is the collector
and the heap's slow path its allocation-pressure caller. Ordinary collections keep rows through
teardown; pressure collections harvest a bounded member list. The worker is
still S38's. S30 deleted `rc-walk`, `rc-trace` and `rc-satb` on 2026-08-26;
that code is on `archive/pre-rc-cycle` and its removal is recorded in
`dev/DECISIONS.md`. S28 was abandoned by that ruling, and S29's second half
was carried as S39, closed on 2026-09-12.

**The stages below went through a Critic round and four Sage rulings on
2026-08-26**, on Edmond's instruction, and are the amended form. The rulings and their reasons are in `dev/DECISIONS.md`.

**Every cycle-GC improvement has one review gate, and the Sage is the
escalation** (Edmond, 2026-09-10 and again 2026-09-12: the Critic first, the
Sage only for what the model cannot answer itself). The pre-change baseline the
step would otherwise erase — its operation count, manager-allocation budget,
cache working set, lifetime and refusal model — is taken by the model before
the first code edit and recorded in the step; a red test is then seen failing;
after the implementation, the Critic reviews the repair and its mutations
before the step can be checked, and a finding the model can neither accept
with a repair nor refuse with a reason goes to the Sage. The findings are
recorded in the step's handoff. This applies to every step of S37, S38 and
S40; one broad review does not waive a later step's gate. Until 2026-09-12 this paragraph put the Sage before the first edit.

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

The six the review of 2026-09-01 raised over `52b2cbf` and `0416e83` left the
same day — four by Edmond's rulings, recorded in `dev/DECISIONS.md` and in the
`done:` clause of S38.3, and two by the repairs they prompted. The `dev/` sweep of the same day raised one more — `FORCE_OOM` against
the guard rule of `dev/POSTMORTEM.md`, 2026-08-13 — and it was fixed rather
than carried: the flag is raised only through `block_pool::force_oom`, whose
guard lowers it on the unwind as well as on the return.

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

- **`exact test` is a term the glossary retires** in favour of *exact
  validation* (`rfc/dev/GLOSSARY.md`, "Deprecated terms"), and it stands 45
  times across the tree — `cycle::validation`'s own module doc among them. The
  three vocabulary guards do not carry it, so nothing goes red and the count
  only grows; `cycle::finalization` and its cases were written in the ratified
  word, which is the whole of what has moved. No step owns the rest.

- `promote` keeps `corpse` for the reset's torn-down entity, which the
  glossary names a *torn-down entity* (`rfc` `9ca669c`,
  `dev/CYCLE-TERMINOLOGY-AUDIT.md`, "Glossary check"); the exemptions in
  `cycle::tests::the_metaphors_the_names_still_carry` and
  `..._the_comments_still_carry` say so. `memory::reset_window` took the
  glossary's words on 2026-09-12; no step owns `promote`'s.
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
      done: at every collection off the poll, each entity the scan coloured
        `Live` carries this commit's epoch and an age one more than the minimum
        current-epoch age over its **strongly connected component in the traced
        live subgraph**, written by a descent over the rows that runs after the
        scan and before the first guard; two rings joined by one one-way edge
        under a single keeper stamp at different ages inside one closure and at
        one age within each ring; a member joined to a ring between two commits
        holds that ring at age 1 while the other ring climbs; a commit under a
        pinned later epoch writes age 1 with the new epoch; a segment refusal
        injected inside the descent leaves every component stamped whole or
        untouched, read off a `#[cfg(test)]` counter of components closed, rows
        visited, stack high-water and refusals; a collection under pressure
        stamps through the exact validation alone, read off the same counter; a
        root whose row read `Live` and a root of a set the commit read as
        externally referenced each keep their one token and stand in the
        deferred lane at the close, a root whose set was freed is retired, and
        every other root is back in the active lane; the mark's and the scan's
        dispatch counters stand at the recorded baseline, the descent's own
        counted under a phase of its own
      tier: T2 · role: Critic
      Critic 2026-09-10: five findings, no broken arithmetic in the descent
        itself; all five repaired in the same step. The component stack holds
        the whole live population where the live core is one component, and it
        draws its segments before `reclamation` reserves room for the children
        a sever displaces — so a descent that emptied the pool turns a
        confirmed teardown into a refused reservation; the peak and that
        ordering were named nowhere, and are now in the module doc and in
        `dev/DECISIONS.md`. The descent's "own phase" ran to the end of the
        commit, so it counted the exact validation's and the teardown's
        dispatches as its own, and the one case reading it agreed by the
        accident that its fixture tore nothing down: the phase closes at the
        descent's end now (`row::note_descent_end`) and the case commits over a
        ring it frees. The journal entry claimed a narrowing of
        `cycle::density`'s contract that had not been made, and past a commit
        that module's census reads a component index as an in-edge count and
        raises: the contract is narrowed to "before the commit" for real.
        `write_maturation_stamp` and `cycle::epoch` still named one producer.
        And `clear_touched_rows` checked one of the two chains that carry row
        pointers.
      correction 2026-09-10: **the disposition clause moves to S37.6**, before
        anything was built to it, and this step closes on the rest. The
        producer and the disposition are two results and the second is a
        change to the lane discipline S37.4 closed the same week: the batch
        would have to be partitioned per root, which is a third destination in
        `queue::compaction`'s unwind-safe pass, and a step whose verdict can
        read "half done" says nothing. The Sage's ruling is unchanged and S37.6
        carries the clause word for word; what is lost until it lands is that a
        root read live is offered to every collection instead of waiting for
        the turnover, which is the cost this crate already pays today.
      Sage 2026-09-10 (S37.1's pre-change gate, which created this step): the
        unit is the strongly connected component of the traced live subgraph,
        and the descent is Pearce's single-index algorithm run iteratively over
        the rows, `rindex` held in the row word's 30-bit count — a field no
        production reader of a `Live` row touches past the scan. The frames and
        the component stack draw the worklist's segments from the arena's bump,
        so a refusal ends the descent with every closed component stamped whole
        and the heap in a state a later collection reads correctly. It runs on
        the ordinary path alone: the pressure path has given its blocks back
        before the commit, and the descent would draw the memory that
        collection exists to return. Refused by name: the whole `Live`
        population as one unit, because any reachable allocation and any member
        unmet at the previous reading pin the minimum at 0 and nothing matures;
        the per-root first-reach partition, which equals the closure on the
        corpus and moves with the batch order; per-entity ageing, and its
        disguise as a partition by current age; a minimum taken over stamped
        members alone, which changes Y9's formula and is S37.5's question
        rather than this step's; stamping during the mark or the scan, which
        breaks the byte-identical abort; stamping after the exact validation,
        which writes into slots user code may have freed; folding the component
        index into the mark, which taxes the collections the prune makes cheap;
        and a second array beside the rows.
      note: this is the producer S37.1's correction of 2026-09-09 required be
        built before the edge-side read, and what S40.1's pruning arm waits on.
        `rfc` carries the definition of "component" for the stamp first — Y9 and
        `rc-cycle.md`'s age-based pruning bullet use the word without defining
        it, and this crate calls those normative.
      handoff: `cycle::maturation`, called from `commit_before_drops` after
        `Finalization::begin` and before the first guard, on a `Membership`
        that carries rows. Pearce's single index lives in the working count of
        the entity's own row, which `shadow::write_live_index` writes and
        `cycle::density`'s narrowed contract stays clear of; the frames are the
        trace's worklist and the component stack is the arena's new
        `components` chain. Verified on the final tree: 845 passed, 0 failed,
        10 ignored, three times at eight threads, `hash-folding` once and
        `debug-journal` 849 three times; `--list` diffed against the pre-change
        tree, six additions and no removal; release, `cargo bench --no-run`,
        `cargo +1.94 fmt --check` and `cargo doc` (44 warnings, all the
        pre-existing private-item class) clean, `citations.py` 504 with the
        same seven residues. Five source mutations seen red: the maximum age
        for the minimum, per-entity ageing, a stale-epoch stamp keeping its
        age, an edge into a visited vertex lowering nothing, and the root test
        removed. Miri at two threads on the final tree, three slices:
        `cycle::maturation` 6 passed, 11.26 s on Miri's clock and 14.6 s of
        wall; `collect::tests::what_a_live_reading_leaves_registered` 1 passed,
        24.71 s and 18.8 s; `collect::tests::when_the_turnover_reoffers` 3
        passed, 226.19 s and 8 m 23 s. The descent adds no integer-to-pointer
        cast — the frame's kind tag keeps the pointer's provenance and the
        visit index is `ptr::without_provenance_mut`. What the step does not
        do is S37.6.

- [x] S37.6 The close disposes of a batch per root   *(after S37.0, before S37.1)*
      done: at the close of a collection off the poll, a root whose row read
        `Live` and a root of a set the commit read as externally referenced
        each keep their one token and stand in the deferred lane, a root whose
        set the commit freed is retired, and every other root is back in the
        active lane; a batch mixing the three is what a red test drives, read
        through `collect_lane_tokens` and `candidate_count` rather than through
        a count of one lane; and the deferred lane's fill bound is respected on
        the mixed batch as it is on the whole one
      tier: T2 · role: Critic
      correction 2026-09-10: the criterion says a live root stands in the
        deferred lane, and the Sage's mechanism makes that true **only while a
        spare segment stands**: an append that finds both cells empty sends the
        record to the active lane instead, which is the one destination that
        cannot refuse. Read the clause with that proviso — a live root stands
        in the deferred lane where the lane can take it, and is offered to the
        next collection where it cannot. The fill-bound clause is answered by
        the reading rather than by a case of this crate: a lane takes 8,160
        records per segment and the widest population any case builds is 4,077,
        so the append's growth arm is exercised by nothing and the debt is in
        the residual list below.
      note: split out of S37.0 on 2026-09-10, whose correction says why. The
        mechanism is the Sage's of that day, unchanged: a mark in the entry's
        reserved low bits, set by a walk over the batch after the commit and
        while the rows still stand, and one pass of `queue::compaction` with
        three destinations. S37.4's whole-batch deferral is its special case,
        so the two cases of `cycle/collect/tests/when_the_turnover_reoffers.rs`
        keep their expected outcomes.
      Sage 2026-09-10 (a price the gate ruling did not name): the pass has one
        read cursor and one write cursor in the one segment list, and
        `Compaction::drop` re-runs it to completion, so a third destination
        cannot be a second output cursor there — splitting one chain of
        segments into two needs segments neither input supplied. Ruled: the
        deferred lane becomes a **side exit** beside `ll_free`, its head taking
        a spare segment, and **an append that finds both spare cells empty
        sends the entry to the in-place output instead**, which is Y12 clause 8
        read literally and keeps a token out of no lane, the one destination
        that cannot refuse being the fallback. Refused: two output cursors,
        which would draw segments out of list order and re-argue all eight
        checkpoints for a deficit the two cells already cover; and a
        whole-batch rule with a sharper predicate, because it is right only on
        `Unreachable` and `ExternallyReferenced` and leaves the live roots of a
        `ZeroCountMember` or a refused teardown re-traced at every collection —
        a correctness that would rest on an unmeasured frequency. `Free`
        outranks `Deferred`; the reserve is never drawn for the lane; the
        pressure path keeps `defer_candidates` and its lift, the cells being
        emptiest exactly there.
      Critic 2026-09-10: no path found on which a token reaches the wrong lane,
        no lane or two, and nine findings beside it; seven repaired here. The
        close made **two** full compaction passes where it used to make one —
        the disposition's own and the guard's retirement over the same lane —
        so the guard now skips its pass where the close is itself a retiring
        one. Checkpoint 9 and the whole deferred arm were reachable by no
        injection, the one harness passing `deferred_at: None`:
        `owner_retirement::a_deferring_pass_survives_an_unwind_at_each_of_its_boundaries`
        drops the pass on each of the ten boundaries over a batch whose records
        are marked, dead, both and neither, and the Critic's own mutation —
        the boundary raised above the clear — reads back as one entity twice.
        The mixed batch reached two destinations of three; the third is the
        teardown's own registration, which arrives while the batch is out and
        therefore unmarked. A module doc claimed 4,077 roots were more than a
        segment holds, which is 8,160. `DEFERRED_MARK` took bit 0 out of the
        four the module doc reserves for a dirty reader's marks, and the ledger
        there now says which bits are whose. Two readers were missing from
        `walk_chain`'s list, and `finish`'s early return carried a clause no
        caller can reach. Two findings became debts in the residual list
        instead: the close no longer sweeps the deferred lane, and the lane's
        second segment is written by no case. Its finding on the order of the
        marks against the disposition arrived repaired — the same defect was
        found from this side and the two lines reordered.
      handoff: the mark is `queue::DEFERRED_MARK`, bit 0 of a stored entry,
        written by `InFlightBatch::mark_for_deferral` from
        `ActiveTrace::mark_roots_for_deferral` while the rows still stand and
        read once, by `compaction::stage_entry`, which masks it off before
        anything else sees the pointer. The disposition is selected before the
        first mark, so an unwind out of the marking walk still routes the batch
        through the masking pass. Verified on the final tree: 849 passed, 0
        failed, 10 ignored, three times at eight threads, `hash-folding` once,
        `debug-journal` 853 three times; `--list` diffed against the pre-step
        tree, four additions and no removal; release, `cargo bench --no-run`,
        `cargo +1.94 fmt --check`, `cargo doc` (44 warnings, all the
        pre-existing private-item class), `citations.py` 505 with the same
        seven residues. Four mutations seen red: a live root not marked, the
        turnover mirror moved at every append, the fallback's condition
        inverted, and the mark not masked off the entry — that last aborts the
        process rather than failing a case. Miri at two threads on the final
        tree, three slices: the deferring-pass case 1 passed, 24.97 s on Miri's
        clock and 19.8 s of wall; the three disposition cases 20.33 s and
        16.8 s; `queue::tests::the_tokens_every_lane_holds` 7 passed, 72.49 s
        and 53.7 s. A run over `cycle::queue::tests::owner_retirement` whole was
        killed at 25 minutes and left an empty log, so nothing is claimed from
        it. Three existing cases moved with the contract rather than being
        muted, each named in the commit.

- [x] S37.1 The maturation stamp is an edge-side prune
      done: mark's descent reads the stamp with one single-byte load; an **edge
        target** whose stamp epoch equals the current epoch (mod 4) and whose
        age has reached `k` is treated as an **opaque live external and is not
        descended into**; a **queue root is never pruned**, whatever its stamp,
        and a red test shows a ring every one of whose members is at the
        threshold collected at the trace that meets it; a stale-epoch stamp
        reads as age 0 and is never cleared in place, so the trace writes no
        stamp; the epoch counter is one process-global full-width word advanced
        by a collection's commit every 64 collections, read against a
        full-width per-thread mirror, and `k = 3`, both named provisional after
        YRC's only known values, with `k` owed a measurement on a real workload
        and the turnover's owed at S37.5; a `#[cfg(test)]` counter reports edges
        pruned per collection
      tier: T2 · role: Critic
      Critic 2026-09-11: no path on which a still-referenced entity is freed or
        a row reads lower than the whole batch would leave it; the predicate is
        stable inside one mark, the scan, the exact validation and the
        teardown all read a pruned target as never met, and the byte-6 read
        before the dispatch is legal for every population a counted child can
        be. Seven findings beside it, all repaired here. The module doc argued
        the root exemption from the ring it does not save — a ring one of whose
        mature members never observed a non-final decrement is pruned at that
        member and waits for the turnover, and that shape is ordinary; the doc
        says so now and the test says which shape it is right on.
        `MarkResult::Complete`, `mark`'s own contract and two sentences of the
        module head still said every reached entity is met and the meeting is
        the whole terminator. The prune cuts the subgraph `cycle::maturation`
        ages — a mature member has no row, so its mates close without it and
        it keeps its stamp — which `maturation`'s "same membership at every
        reading" and `finalization`'s youngest-member sentence did not say. One
        assertion of the first case could not fail with the prune off and is
        marked a fixture check. "A root of some batch" was false of a deferred
        entry. Its next attack — the exact validation's `RC − IN` over an edge
        from a pruned target — read here: `validate_component` counts an edge
        internal only where the child is a member, so a pruned target's edge
        raises the sum and the component reads externally referenced.
      correction 2026-09-04: the criterion carried "the same test skipping a
        mature popped root entirely" and a counter of such roots, which
        `rfc/model/gc/rc-cycle.md` forbids by name and for soundness — "the rule
        applies only to edge targets, never to queue roots; otherwise a
        reference cycle at the threshold could be skipped until the epoch
        changes", repeated in the summary bullet. The step's own handoff already
        recorded the root-side reading as struck on 2026-08-26 and the criterion
        kept it. Struck here, before anything was built to it. The counter of
        skipped roots goes with it: it counted an event that may not happen.
        The epoch counter's residence is corrected in the same pass —
        `rfc/dev/DECISIONS.md` closing Y12 clause 8 makes it process-global and
        full-width against a per-thread mirror, and the full width is what keeps
        a wrapped stamp from hiding a turnover.
      correction 2026-09-07: the counter is built, so this step reads it rather
        than founds it — `cycle::epoch`, one process-global `AtomicU64` of
        closed commits advanced at every commit's close, the epoch being
        `(commits / 64) % 4`. What is left here is the descent's read,
        the per-thread mirror S37.4's re-offer needs, and `k`. The maturation
        case moved here from the commit's tests with its arithmetic corrected: with `k = 3`
        and an age of `min + 1`, a live ring reaches age 3 at its third commit
        and the fourth collection is the one that prunes its edges, read off
        this step's counter.
      correction 2026-09-09: **the write-side prerequisite is not built.** Y9
        requires a commit to stamp each component it reads as externally
        referenced, so that a later edge stops at the mature live core. The
        current production membership contains only rows scan left
        `PotentiallyUnreachable`; `Finalization::confirm` and the destructor
        revalidation can stamp that membership when an exact reading changes,
        but no path presents the ordinary `Color::Live` population to
        `stamp_component`. Before adding the edge-side read, this step must
        build or name the component membership and owner disposition that
        stamps those live rows, together with S37.4's one-token deferred-lane
        transition. A one-entity fixture cannot discharge the requirement:
        the test must include unequal member ages and show the component-wide
        `min(age) + 1`, plus a turnover that makes the stamp stale. This is a
        prerequisite correction, not permission to stamp rows individually.
      Sage 2026-09-10 (pre-change gate): the write-side producer that
        correction demands is a step of its own, S37.0, and the edge-side read
        below waits on it. Nothing of this step's own criterion moved.
      handoff: the root-side reading — "traced only after it has stayed a
        candidate across `k` collections" — was struck from this step and from
        `rc-cycle.md`'s summary bullet on 2026-08-26. It is not a second
        mechanism: it filters which roots start a trace and does nothing to the
        closure, and its real content falls out of the prune at depth zero.
        Y9 calls the prune the only mechanism in this design that bounds the
        closure.
      handoff: `cycle::mark::visit_child` tests the target before the block
        dispatch: a stamp carrying this collection's epoch at
        `TRAVERSAL_AGE_THRESHOLD` (3) over a target `CANDIDATE_BIT` does not
        stand on is an opaque live external — no subtraction, no expansion, no
        dispatch. **A queue root is the bit and not the batch**: the bit is the
        wider set and the batch is not addressable from the mark, so every
        entity it spares beyond the rfc's population costs a descent and never
        a collection (`dev/DECISIONS.md`, 2026-09-10, "a queue root is the
        candidate bit"). The epoch is read once per `mark` call, `current`
        being a division over a process-global counter. `take_edges_pruned`
        is the counter, thread-local and cleared as it answers. Verified on
        the final tree: 852 passed, 0 failed, 10 ignored, three times at eight
        threads, `hash-folding` once, `debug-journal` 856 three times;
        `--list` diffed against a worktree at `15ce2ca` in all three
        configurations, three additions and no removal; release,
        `cargo bench --no-run`, `cargo +1.94 fmt --check`, `cargo doc` (45
        warnings, the same 45 the pre-step tree prints) and `citations.py` at
        508 with the same seven residues. Four mutations seen red: the prune
        removed, the candidate test dropped, the epoch test dropped, and the
        threshold lowered to 2. Miri at two threads on the final tree,
        `cycle::mark::` 9 passed, 15.79 s on Miri's clock and 16.1 s of wall.
        What the step does not do is the case in the residual list: the pair
        of the prune's recall loss and the turnover that ends it.
      handoff: carried from S31 before that stage was deleted. **Two producers
        hand a member a stamp byte nobody wrote.** A recycled `heap::FreeSlot`
        preserves the dead entity's final header, so the slot arrives carrying
        the previous occupant's byte 6; and promotion rewrites a survivor's
        category with a two-byte store, so the byte leaves the arena exactly as
        it went in. Either one reads here as a mature stamp of the current
        epoch and this step prunes a live subgraph permanently and silently.
        The zeroing belongs to S38.0; this step's counters are what would show
        it missing.
- [x] S37.4 The deferred-candidate buffer and the turnover re-offer
      done: a reading of `ExternallyReferenced` never clears the candidate bit;
        the component's root keeps its **one existing token** and the trace's
        batch moves into the owner's deferred lane, and every deferred
        candidate is re-offered at **the owner's first safepoint poll that
        finds the epoch counter moved**, never by registering or copying the
        entity a second time; red tests prove
        that a matured ring losing its last external reference mid-epoch is
        collected at that re-offer and not before, and that a ring whose mates
        carry unequal ages is likewise collected, so maturing apart costs recall
        rather than a permanent miss
      tier: T2 · role: Critic
      handoff: this is the backstop the withdrawn "retired on contact" clause
        was supposed to be and never was — eager clearing fires only when a
        trace touches the entity, and the stamp that wraps is exactly the one no
        trace touched for four epochs. It also collects YRC's 56 % saving on
        re-registration.
      handoff: `CANDIDATE_BIT` means exactly one logical token in exactly one
        state: `active → in-flight → deferred`, or consumer-retired after
        death. A decrement while the token is deferred sees the standing bit
        and cannot add a duplicate. Store original registered
        roots only — adding every traced live member manufactures tokens, and
        collapsing two roots in one component can miss it after a later split.
      correction 2026-09-04, amended 2026-09-10: the criterion re-offered
        "at the first collection after the heap's epoch advances", by
        detaching the deferred lane beside the active one.
        `rfc/dev/DECISIONS.md`, closing Y12 clause 8, chooses the poll over
        the collection **by name and with the failure case**: "a thread whose
        only garbage is a deferred ring has an empty queue … waiting for a
        judgement would wait for ever, which is Y6's permanent miss by another
        road". The whole-segment splice is no longer the contract either: only
        a lane head carries a fill bound, so the re-offer is the bounded merge
        of the deferred lane into the active one at the poll. The retired word
        "suspects buffer" goes with it (`rfc/dev/GLOSSARY.md`).
      closed 2026-09-10: the lane is `queue.rs`'s `deferred_segment` beside the
        write segment, with its own fill bound and a full-width
        `turnover_mirror`; `defer_candidates(batch, at_commits)` moves a batch
        into it and `reoffer_deferred_if_epoch_moved` brings the whole lane
        back at a poll whose commit count stands in a later epoch than the
        mirror. `gc.rs` wires that call into `ll_gc_maybe_collect` and arms the
        thread when it moves records, because a deferred ring can be a thread's
        only garbage. The two criterion tests are
        `cycle/collect/tests/when_the_turnover_reoffers.rs`; they stage the
        reading with `InjectedVerdictRace`, the fourth injection of the crate's
        own kind, because one thread's trace and its exact validation are a
        call apart and no other seam produces the disagreement between them.
      note 2026-09-10 — what the Critic round changed. Three defects and two
        test holes, all repaired in the same step. The re-offer compared raw
        commit counts, so any commit of any thread ended the deferral; it
        compares turnovers now (`epoch::turnovers_of`). The mirror was read at
        the disposition, which on the two paths falls on opposite sides of the
        commit's own close and put the same event 0 or 64 commits apart; the
        count is taken at the reading and passed in. A bounded round of the
        pressure path deferred the whole lane including the roots its trace
        never read — 2048 of them in the case now standing — so
        `commit_under_pressure` takes `whole_lane` and restores instead. The
        deferral also carried this thread's overflow buffer into the lane,
        found while repairing the above; the buffer is withheld from the pass.
        The lane's own name went with the round: the glossary's word is
        deferred-candidate buffer, and `dormant` was a second name for one
        thing.
      note 2026-09-10 — the deferral retires completed deaths on the way in.
        Nothing reads the deferred lane before the turnover, so a record that
        names a dead entity withholds its slot for a whole epoch, and
        `retire_candidates` reads the active lane alone. The parking pass is
        therefore the retirement pass (`compaction::finish(batch, true)`),
        which is `rfc` Y12 clause 8's own remedy made mandatory. Teaching
        retirement to walk the deferred lane instead was refused: retirement
        runs at every collection and every pressure round, and its work would
        become proportional to the accumulated deferred set, which is the work
        the deferral exists to remove.
- [ ] S37.5 The turnover constant, against a corpus   *(after S37.4)*
      done: the suspects re-offer volume is measured at the epoch turnover on a
        corpus, and S37.1's 64-collection turnover is replaced by a number or
        recorded as confirmed with its measurement; a synthetic reading is
        refused and the entry says so
      tier: T2 · role: Bench
      handoff: split out of S40.1 by the Sage of 2026-09-04. The re-offer
        volume is the count of roots acquitted inside one epoch that are still
        enrolled at the turnover, and on a synthetic population the harness
        chooses the acquittal rate, so the number is its own input read back.
        The step needs S37.4's buffer and a corpus, and the corpus is
        Phase-D-blocked in the same way S40.1's corpus arm is.
- [ ] S37.2 The acyclic gate
      done: the factory stamps bit 8 from the class's own answer — waits on
        `rfc` `model/classes.md` declaring a target per pointer slot
      tier: T2 · role: —
      handoff: blocked outside this repository; the step is listed so the
        dependency is visible rather than discovered.
- [x] S37.3 The ownership mark
      done: a proven-owned entity never enters the candidate set, and the
        compiler's stamp is honoured at bit 9
      tier: T2 · role: —
      Edmond 2026-09-11: the mark is not the factory's — a store into a slot
        the compiler proved moves it, the displaced entity losing it and the
        new occupant gaining it, and the holder's `dispose` destroys a marked
        child instead of releasing it, the count unread. Contract in `rfc`
        first (`model/gc/strategies.md`, "The store barrier, as
        micro-operations"; `dev/DECISIONS.md`, "the ownership mark is moved
        by the store into a proven slot, and honoured by the holder's
        `dispose`"), where the consolidation reader's ten findings were
        repaired before the code: the rationale this session had attached
        to the ruling ("the proven sites may have dropped their counting
        pairs") contradicted the bound of 2026-08-26 and went; the proof
        gained its third part, no ring closing through proven slots alone,
        without which a ring of marked entities registers nowhere.
      handoff: `memory::barrier::store_ptr_owned` / `store_box_owned` and
        the ABI pair `ll_store_ptr_owned` / `ll_store_box_owned`; the move
        is `move_ownership_mark`, and the mark lands only on a GC-heap
        occupant of a GC-heap holder. `object::ll_owned_child_die` — mark
        off, count written to zero, the ordinary death path — is what
        `ll_default_dispose` calls for a marked child and what a generated
        `dispose` owes. `refcount::is_owned`, `set_ownership_mark`,
        `clear_ownership_mark`. Eleven cases in three new groups:
        `barrier/tests/the_owned_store.rs`,
        `object/tests/what_dies_with_its_holder.rs`,
        `collect/tests/when_a_member_is_owned.rs`. Six mutations seen red:
        the move removed, the dispose ignoring the mark, the mark not
        cleared at the child's death, the count not zeroed, the gate
        without bit 9, and the same-entity early return — which stayed
        green, clear-then-set giving the same word, so the branch went.
        Untested: the owned store's refusal (the plain store's refusal has
        no barrier test either; `force_oom` does not reach a copy the
        thread's block already has room for). Two reviewers on the final
        tree (execution and intent, rule 11): the intent axis found the
        collector's sever leaves a marked member's mark standing, which is
        harmless because the guard-release free runs no destructor (step 4
        ran it) — recorded in the decision's cost clause; the execution axis
        gave ten findings, eight repaired (the owned forms' `# Safety`,
        `pub(crate)`, `Value::entity_or_null` for the eighteenth copy of one
        shape, `ll_owned_child_die` exported for a generated `dispose` with
        the obligation on `ClassBuilder::dispose`, `DeadInPlace` per member
        in the collect case, trimmed comments, the hot-path inventory) and
        two accepted with a one-line comment (a flags word loaded twice on
        the teardown path and once more in the owned store, unmeasured:
        neither is a listed hot path). Verified on the final tree: 863
        passed, 0 failed, 10 ignored, plain and three times at eight
        threads; `hash-folding` once (863); `debug-journal` three times
        (867, 12 ignored); release; `cargo bench --no-run`; `cargo +1.94 fmt
        --check`; `cargo doc` 45 warnings, the same per file as the pre-step
        tree's 45; `citations.py` 514 with the same seven residues; `--list`
        diffed against the pre-step tree in all three configurations, eleven
        additions and no removal. Miri two threads over the three new groups
        and `the_ordinary_store`: 14 passed, 29.30 s Miri's clock, 31.0 s
        wall.

## S38 — The claim and concurrency

Goal: a collection runs either in a collector thread or in the mutator, never
both, and the losing side never deadlocks.

**What the token has to cover, as of 2026-09-05.** Three clauses have gathered
on the premise that one window is open in the process at a time, and every one
of them is a correctness clause rather than a performance one. A block's
`marked_link` word, which two windows splicing at once would put on two lists.
A block's shadow pointer, which the free path reads past the region to decide
whether a death has to be withheld at all: a stale null returns a slot under
another window's rows. And `DEAD_IN_PLACE` itself, which carries no owner — a
block walk cannot separate its own window's marks from a foreign window's, so a
thread that stacks a slot of a stranger's block depends on that stranger not
walking the block meanwhile (`dev/DECISIONS.md`, "a death the collection never
met is returned at once, and a foreign slot is stacked", its last paragraph). All three
are unreachable while `ActiveTrace::open`'s per-thread assert is the only
window there is.

- [x] S38.0 The collector's reader
      handoff: `cells::AtomicCells`, every load atomic and `Acquire`, paired
        with the `fence(Release)` after the header store in
        `refcount::publish_header`; `OutsideCells::walk_concurrent`; `mark`,
        `scan` and `trace_batch` generic over the reader; the trace from a
        second thread is `cells::tests::what_a_collector_thread_reads` and
        the fixture `cycle::testing::traced_from_a_collector_thread`. Records:
        `dev/DECISIONS.md`, "the collector's reader loads with `Acquire`";
        `rfc/dev/DECISIONS.md`, "the publication fence lands before its ARM64
        price"; `dev/BENCHMARKS.md`, "S48.2 the box's price after the
        relayout" for the reader's price. Commits `b6839ca`, `6c1225e`.
- [x] S38.1 The claim
      handoff: `cycle::token` — `TraceToken` (a CAS flag, a futex `Mutex<()>`
        and a `Condvar`), `HeldToken` taken around the trace on both paths,
        `this_thread_token` for a holder on another thread; five cases in
        `token/tests/who_may_trace_this_thread.rs`, six mutations seen red;
        why the exclusion is per thread is `rfc/dev/DECISIONS.md`, "a trace
        stays inside the blocks of the thread it claimed".
- [x] S38.4 The entry gate and the slow-path fire   *(before S38.2)*
      handoff: the gate is `cycle::collect::may_collect` over `gate()` — the
        collecting flag, the reset window and `object::teardown_depth` — read
        by `CollectingThread::take` and by `ll_gc_maybe_collect` before
        `take_due`; the refused allocation is named per size class by
        `heap::take_refused_entity_refills`; nine cases in
        `heap/tests/the_collection_a_refusal_starts.rs`, one in
        `cycle/collect/tests.rs`, eleven mutations seen red. Decisions:
        `dev/DECISIONS.md`, "the entry gate reads the teardown depth, and a
        poll it refuses keeps its arming"; `rfc/dev/DECISIONS.md`, "the entry
        gate's third input is the reset window".
- [x] S38.2 The working wait
      done: an in-line collection needs no verdict list, no handshake and no
        second phase — it is exact with respect to the counts because the owner
        re-reads its own current fields — and a mutator that cannot allocate
        while the claim is held waits for the trace to end
        rather than preempting; the test's running collection is staged by
        S38.1's harness seizure and reaches the wait through S38.4's path, with
        a `#[cfg(test)]` counter past the wait asserted non-zero, because a test
        that merely terminates terminates most easily when the wait is never
        taken
      tier: T2 · role: Critic
      Critic 2026-09-12, over the claim that S38.1 and S38.4 already meet
        this criterion: no clause is unevidenced, and two wanted narrowing.
        The case asserted `waited == 1` where `Condvar::wait` may return
        spuriously and the loop counts each wait, a contractual flake; both
        cases that read the count assert it moved now, which is the
        criterion's own wording. "Past the wait" has two readings and the
        code holds the sound one — the count moves on the wait path before
        the block on the condition variable, and the holder's `release`
        takes the mutex the waiter holds until it blocks, so a moved count
        followed by the collection proves the wait was entered and ended by
        the release; a counter after the block would give the holder no
        signal. Recorded: the case's
        holder is a stand-in that does no trace's work, which the criterion
        accepts by naming the harness seizure; the design's own worker
        empties the lane it traces, so "and is then served" in the case's
        name holds against the stand-in alone, and the collector's case is
        S38.0's. Refuted as a hole: the rfc's "Worker-to-owner handoff"
        inbox is the worker path's, and the in-line form has no second
        tracer. Drift repaired in `rfc`: "Concurrency" placed the release
        at the end of scan on both paths while the pressure path holds the
        token through the harvesting sweep; the sentence names both
        instants now.
      handoff: no new code; the wait is `HeldToken::take` in
        `trace_and_harvest`, reached from `entity_alloc`'s refusal, and the
        case is `a_refusal_under_a_held_token_waits_for_the_release_and_is_then_served`
        with `HeldByACollector::take(.., true)` releasing only once the count
        moved. Seen red with the count's increment deleted, after the holder's
        10 s bound. The exactness clause is `cycle::validation`'s, in-degree
        from the members' current cells on the owner after the release.
- [x] S38.3 Deferring the mutator's frees during a trace
      order, agreed with Edmond 2026-09-15: (1) the gate and the stack for a
        foreign holder of the token, the first test red by construction;
        (2) who makes the returns — the owner, at its next free, the poll
        and the exit — and the sweep-before-release order on the collector's
        side; (3) the four addresses beside the slot — a buffer chunk
        (`buffer_free_longlived_payload`), a retained block
        (`release_emptied`), an OS-direct run, an arena block the reset
        returns under a held token — each with its link and its return;
        (4) the cost, the churn held across one collection on the census
        loads, in `dev/BENCHMARKS.md`.
      progress 2026-09-15: (1) and (2) landed — `dev/DECISIONS.md`, "a
        foreign holder of the token withholds every death, and the owner
        makes the returns"; six cases in
        `deferred_slot_reuse/tests/what_a_foreign_trace_withholds.rs`, the
        first red before the gate, the exit's pop and the reclaim's gate
        each seen red by mutation; Miri green over the six and the reader's
        four; ThreadSanitizer reports the fence-ordered publication in the
        churn case because it does not model `fence` (`dev/WORKFLOW.md`,
        "ThreadSanitizer"). The collector-thread fixture moved to
        `cycle::testing::traced_from_a_collector_thread`. What (2) left
        open: the collector's sweep-before-release order, because the
        stand-in's arena reset follows its release today and the owner's
        pop after it reads a shadow pointer the sweep is nulling — the
        block's shadow word is atomic both ways, so the reading is a stale
        stamp at worst and the owner under a foreign holder reads no stamp;
        the order is the worker's (S38.5) to fix when the worker exists.
        (3) landed the same day: the chunk arm, the pool's `put` and the
        run arm each read the token once, three cases in
        `what_else_a_foreign_trace_withholds.rs`, each gate seen red by
        mutation, Miri green over the deferral module, `stdapi`, the pool's
        and the buffer arena's tests (`dev/DECISIONS.md`, the same entry's
        extension). (4) the same day: `dev/BENCHMARKS.md`, "S38.3 what a
        foreign holder costs the owner" — 2.6 ns to withhold a death, 4.2 ns
        more to return it later, 12 ns more per chunk, and the churn held as
        a bound from the trace lengths of the census loads, the corpus's own
        death rate being unmeasured.
      handoff: the three windows `ll_free` asks are the queue entry, the
        owner's own trace and a foreign holder of the token; the third
        withholds every death, every chunk at `buffer_free_longlived_payload`,
        every block at `BlockPool::put` and every run at `ll_free`'s run arm,
        on three per-thread stacks the owner makes at its next free, the poll
        and the exit (`cycle::deferred_slot_reuse`, "A foreign holder of the
        token"; `dev/DECISIONS.md`, "a foreign holder of the token withholds
        every death, and the owner makes the returns"). What it left to
        S38.5: the collector's sweep-before-release order, and the exit-phase
        word a holder arriving after the exit's last pop needs. Commits
        `ee12c47`, `23c98c5` and `b81a95d`.
      note: `cycle::deferred_slot_reuse` is the owner-side substrate for one
        thread, where nothing frees inside the window: mark and scan only read, and the trace window
        ends before the user-code teardown by the decision of 2026-08-31. The
        hazard is this step's, and it is the one rc-walk already paid for — a
        collector reading an entity another thread frees underneath it
        (Edmond, 2026-09-01).
      done: while a collection is in flight over a thread's blocks, that
        thread's frees are deferred until it ends, whichever thread performs
        them, and the deferral covers every address the trace holds rather than entity
        slots alone — an array's table storage in a buffer chunk
        (`cells::trace_cells` strides it, and
        `buffer_arena::buffer_free_longlived_payload` returns it past the
        gate), a retained payload whose block goes home through
        `retained::release_emptied`, an OS-direct run, and an arena block
        `ll_arena_reset` returns while the token is held (added 2026-09-14 by
        the Sage's answer to the A1 Critic round, `rfc/dev/DECISIONS.md`,
        "A1 closes on a discriminating word": a recommissioned block under a
        trace would let `resolve_edge_target` charge a phantom in-edge to
        whatever occupies the address; `rfc/model/gc/rc-cycle.md`,
        "Concurrency" carries the deferral's contract once `rfc` S8.11
        lands); the cost is
        measured as the churn held across one collection
      tier: T2 · role: —
- [x] S38.5 The owner's record
      done: the trace token, the outbox, the inbox and the request word stand
        in one per-thread record whose storage is a chain of GC-metadata
        blocks the process never returns; a thread takes a record at init or
        at its first need and its exit claims the record's token for good
        before the record goes back to the free list; a second thread claims
        a live record's token through the record and reads the free path's
        withholding as before; a claim on a released record fails, and a
        record reused by a later thread is released to claimants only once
        that thread's init is complete; the free path's cost of reading the
        token through the record rather than the thread-local is measured
        against the pre-step tree (`dev/BENCHMARKS.md`)
      tier: T2 · role: Critic
      handoff: carried out of S38.0 on 2026-09-15, which built the reader and
        the fence and left the worker's four duties here. The `expect(dead_code)`
        on `cells::AtomicCells`, on `OutsideCells::walk_concurrent` and on
        `cycle::token::this_thread_token` name the worker's caller.
      handoff: unblocked 2026-09-15 by `rfc` S8.7 (`rfc/dev/DECISIONS.md`,
        "the owner detaches at its poll, and the worker takes the chain from
        a one-word outbox"): the detach stays the owner's, so the worker moves
        no queue word from another thread and `queue.rs`'s single-mover
        invariant holds under it. The exit-phase read the old criterion named
        went with the ruling: the worker acts on a record only under its
        token, and the exit's final claim is what refuses it. The token today
        was a `thread_local!` (`cycle::token::TOKEN`), which no worker could
        address; the record is this step's build, and the one step of S38.5
        as first written became S38.5 through S38.7 on 2026-09-15 because
        each of the three closes on a result of its own.
      baseline 2026-09-15, before the first edit: the token a `thread_local!`
        of three fields, no manager memory, the free path's read one
        thread-local load (`this_thread_token_is_held`), the exit taking and
        releasing the token per round with nothing between the rounds
        refusing a claim; the free path's figures under "S38.5 the token
        through the record" in `dev/BENCHMARKS.md`, column A.
      Critic 2026-09-15: six findings, two accepted with a repair and a case
        each. A recordless thread's exit could draw a record in a round's
        nested take and release it, so the record reached the free list free
        — `ensure_thread_record` answers null while the exit runs, pinned by
        `an_exit_draws_no_record`. A pool thread's second life was unpinned
        — `a_thread_living_twice_takes_a_record_per_life`, red with the
        release leaving the locator set. The count and identity assertions
        could move under other tests' threads — read as membership of one
        record. The module doc said a claim fails on "a thread that is
        exiting", true only from the exit's second step — reworded, with the
        thread that never exits named as keeping its record claimable. The
        registry lock held across the pool's draw was raised and refused:
        `BlockPool::get` refuses and starts no collection. The free-path
        read from a second thread while the owner holds is untested; the
        owner's own reading is.
      handoff: closed 2026-09-15. `cycle::owner_record` (`OwnerRecord`, the
        registry's block chain and free list, `ensure_thread_record`,
        `initialize_thread_record`, `release_thread_record`); the token's
        thread side rebuilt over it (`token::HeldToken` with nested takes
        and `keep`, `held_by_a_foreign_holder` in place of
        `this_thread_token_is_held`); `ll_thread_init` draws the record last
        and tolerates a refusal, `collect_before_exit` claims first and
        keeps, `retire_the_journal` gives the record back after the base
        block. Seven cases in `owner_record/tests.rs`, six mutations seen
        red (the exit releasing; init not releasing; the foreign reading
        ignoring the note; a nested drop releasing; the exit drawing; the
        locator kept). `dev/DECISIONS.md`, "the token stands in a record the
        process keeps"; `dev/BENCHMARKS.md`, "S38.5 the token through the
        record": no difference on the free path at the probe's resolution.
        Untested: a thread whose record the pool refuses at its first
        collection (no fixture refuses one draw of the registry alone).
        Miri over the tests the diff's `unsafe` lines select — the record's
        seven, the token's five and the exit's eight — 20 passed, 62 s of
        Miri's clock, 73 s wall at two threads. The `mark` case seen red once
        in the gate is in the backlog's flake line.
- [x] S38.6 The offer and the pickup   *(after S38.5)*
      done: a poll whose record carries a request detaches its lane into the
        outbox with one release store, after the pickup and behind the entry
        gate, and only into an empty outbox; an owner about to collect in
        line reclaims the outbox first; the pickup takes the inbox chain,
        re-enqueues every unmarked record and traces the marked roots in line
        as an ordinary collection whose batch is that chain; a stand-in
        worker on a test thread takes the outbox under the token by one
        acquire exchange, traces through `cells::AtomicCells`, marks, and
        posts before its release, and the roots it marked are collected at
        the owner's next poll while the ones it acquitted are back in the
        lane; an offer taken back by the exit and one taken back before a
        pressure collection each keep every root
      tier: T2 · role: Critic
      baseline 2026-09-15, before the first edit: the poll's duties ended at
        the re-offer and the fire; a batch was two words the window held from
        `detach_candidates` to its close; a lane entry carried the close's
        bit 0 alone; no in-line collection read a record.
      Critic 2026-09-15: six findings, three accepted with a repair and a
        case each. The fire and the pressure path never read the inbox, so
        a posted chain of garbage stood through the collection that ran
        short of memory — every lane collection drains the inbox before its
        take (`an_in_line_collection_drains_the_inbox_before_it_traces`). A
        panic in the worker's trace dropped the taken chain unposted, every
        root of it stranded — the chain lives in a guard that posts from the
        unwind (`a_trace_that_panics_still_posts_its_chain`). A posted chain
        with no mark opened a window to trace nothing — it goes back by a
        walk (`a_chain_with_no_proposed_root_goes_back_without_a_window`).
        Refused: the worker's `arena.reset` leaving shadow pointers standing
        — `reset` sweeps the rows. Recorded as cost: the treadmill of a root
        the worker reads live, and the offer's starvation on a thread armed
        at every poll. The rfc's "marked as far as the trace got" was
        amended to what the code does.
      handoff: closed 2026-09-15. `owner_record`'s six word operations;
        `queue::{offer_lane, reclaim_offer, take_proposal, merge_proposal}`,
        the batch's word form and `PROPOSED_MARK`; `collect::Roots` and
        `collect_proposal_off_the_poll`; `gc`'s poll picks up then offers,
        armed it fires; `cycle::worker::serve` is the round's body, driven
        by a case's thread. Ten cases in `worker/tests.rs`, nine mutations
        seen red and one green (an unread merge keeping its marks: every
        reader masks, the strip is the invariant's). Miri over the worker's
        and the batch's cases: 17 passed, 82 s Miri's clock, 3 m 34 s wall.
        Untested: any interleaving where the owner moves while the worker
        serves — every case joins the collector first; a chain across a
        segment boundary through the word form; the pickup ending live.
- [x] S38.7 The collector thread   *(after S38.6)*
      done: a collector thread born at startup or at first pressure — which
        is named, with its floor refusal after it (`rfc/dev/DECISIONS.md`,
        "the baseline overflow segment is allocator-issued") — rounds over
        the records, sets a request, and on a set outbox claims the token,
        checks the inbox empty, takes the chain, traces it through its own
        workspace and posts it, walked or not, before the release; a held
        token, an empty outbox and a full inbox are each a skip; `shadow`'s
        `count >= edges` assertion is conditioned on whose pass it is,
        because a worker's row starts from a count the mutator moves under
        it; and `cycle::token::note_last_row_read` reads the owner's token
        rather than the tracing thread's own
      tier: T2 · role: Critic
      baseline 2026-09-15, before the first edit: no collector thread;
        `worker::serve` is driven by a case's thread alone; the registry's
        block chain is walked by two test readers and enumerated by nothing;
        `collect_under_pressure` starts no thread; `note_last_row_read` reads
        the tracing thread's own record; `shadow::subtract`'s assertion is
        already conditioned on the reader (`mark` passes `!R::CONCURRENT`,
        S38.0), with no case exercising the concurrent arm.
      Critic 2026-09-15: five findings. Two went to the Sage: the request
        set on every record every round re-traces every live lane per
        interval and holds the owner's token for the trace, so the duty
        cycle grows toward one with the heap; and a timer that requests is
        the triggering policy the decision had refused startup-birth for.
        Accepted with a repair and a case each: a refused birth retried at
        every pressure collection spawns a thread per refused allocation
        under starvation — `BIRTH_RETRY_INTERVAL`; a panic in a round left
        the word at alive for good — `UnbornOnDrop`, and the retire
        tolerates a panicked join; the `PostedUntraced` arm and the walk past
        the caller's record were reached by no case — two cases. Named as
        met by mechanism and asserted by nothing: the STARTING exclusion
        (one CAS), "through its own workspace" (`TraceScratchArena::open`
        lends the calling thread's). The "born after the collection so that
        the base block comes from returned memory" argument was a suspicion
        and was withdrawn: the placement keeps the birth's draws off a
        collection in flight, no more.
      Sage 2026-09-15: the request is a relay of the owner's own shortage —
        the pressure path notes it on its record, the round takes the note
        and sets the request; the timed round serves and relays and
        originates nothing; the birth at pressure stands. Refused: the
        per-round request, a backoff over it, the owner writing its own
        request, deferring on the worker's reading. Final. Recorded in
        `dev/DECISIONS.md`, "the worker relays the owner's shortage into its
        request". Three things named as Edmond's: Y12 clause 8; whether the
        accelerator's reach under this ruling — one concurrent pass per
        shortage — is acceptable for the stage or an outside driver (an ABI
        for the compiler's arming or the embedder) is specified first; and
        `rfc/model/gc/strategies.md`'s "never fires on its own", which now
        reads a pickup as the tail of the shortage that requested it.
      handoff: closed 2026-09-15 by its criterion, the stage's close waiting
        on Edmond's answer above. `cycle::worker`: `ensure_thread` (called
        at every ending of `collect_under_pressure` through
        `note_shortage_for_the_worker`), `thread_body`, `round`,
        `ROUND_INTERVAL` 10 ms, `BIRTH_RETRY_INTERVAL` 1 s; the test
        switches in `worker/testing.rs` (births permitted, rounds confined
        to one record, the next birth's base block refused by a zero block
        budget, a panic at the next visit, retire); `owner_record::
        {for_each_record, note_shortage, take_shortage}`;
        `token::note_traced_owner`. Eight cases, seven in `worker/tests.rs`
        and the clamp in `shadow/tests/what_a_row_word_holds.rs`; nine
        mutations seen red (no birth; a refused birth leaving the word;
        no request; the probe not naming the owner; a wrapping subtract;
        a request without a note; no note on the pressure path; a refusal
        retried at once; no unwind guard). Miri over the seven worker
        cases: 7 passed, 158 s Miri's clock, 1 m 39 s wall at two threads.
        ThreadSanitizer over the birth, the served-once case, the probe and
        the reader's four: silent. Untested: a registry of more than one
        block (1020 records), a record mid-init met by a round, the
        `PostedUntraced` arm from a trace the rows refused (only the
        workspace refusal is pinned), the STARTING exclusion.

## S49 — The candidate ring read behind its writer, and the collector's rounds

Goal: the collector reads a mutator's candidates while the mutator registers
into the same ring, without a safepoint of the mutator and without either
thread touching the other's index, and answers by a verdict ring the mutator
reads at its poll; the registration stays two plain stores. Edmond's design
of 2026-08-25, restored 2026-09-15 (`rfc/dev/DECISIONS.md`, "the candidate
queue is read behind its writer, and the collector's verdicts come back by a
second ring"; `rfc/model/gc/rc-cycle.md`, "Worker-to-owner handoff"; Y12
clauses 2 and 8) after the outbox form S38.5–S38.7 built on it replaced it
without his word; amended by one Critic round the same night, whose findings
are folded into the steps. What the outbox form built and this stage keeps:
the token, the owner record, the collector thread and its round,
`worker::serve`'s trace through `cells::AtomicCells`, the deferred lane, the
spare cells and the overflow buffer.

Done when: a collector thread traces roots a mutator registered since its
last batch while that mutator keeps registering; no entry of R is consumed
twice and no root is dropped; the owner's reading of the verdicts frees a
proposed ring, defers a root read live for an epoch, retires a zero-count
entry whose death completed and keeps one whose destructor resurrected it;
the mutator's registration is two plain stores on the probe; and `src/`
carries no outbox, no offer, no pickup walk-back and no request relay.

- [x] S49.1 Delete the outbox form with its cases   *(closed 2026-09-15)*
      done: `offer_lane`, `reclaim_offer`, `take_proposal`, `merge_proposal`,
        `PROPOSED_MARK`, the outbox and inbox words, the request word, the
        shortage note and its relay, `Roots::Proposal`,
        `collect_proposal_off_the_poll`, `note_shortage_for_the_worker` and
        the poll's pickup and offer are gone, and so are the cases that
        drove them (a refuted mechanism costs its own tests; S49's Done-when
        re-pins what survives); `worker::serve` takes and posts nothing and
        `worker::round` serves nothing, and the collector thread is not
        born until S49.7; the in-line paths still detach and merge as
        today; the `S38.x` citations are swept; the gate is green
      tier: T1 · role: —
- [x] S49.2 The record at 256 bytes, drawn beside the base block   *(closed 2026-09-15)*
      handoff: Critic, one round, six findings — the re-take case pinned the
        reset only when it won the free list's race (the taking thread now
        names its record, `take_this_record_for_test`); the never-started
        case had gone from exact figures to process-wide bounds (the carves
        are counted on the thread now); a link to the deleted
        `initialize_thread_record`; the exit's order after the base block
        had no pin (a `cfg(test)` assertion in `release_thread_record`); the
        model journal still described the 64-byte record and the tolerated
        refusal (new entry); a rollback keyed on presence would release a
        base block the call found rather than drew (guarded). Trap met on the
        way, recorded in the journal entry: the initialisation's hold read as
        a foreign holder and the rollback's returns were withheld. Miri
        green over `cycle::owner_record`.
      done: `OwnerRecord` is 256 bytes — the token's line; the reader's line
        with R's `frontBlock`, P's `tailBlock` and the per-owner batch size;
        the writer's line with R's `tailBlock`, P's `frontBlock`; a spare
        line — reset in place at every re-take, `RECORDS_PER_BLOCK` at 255
        and the census guard's figures moved; the record is drawn in
        `ll_thread_init` beside the base block, before the heapless return,
        its refusal a thread that never starts, and an unregistered thread
        draws it at its first registration through the registry's lock,
        pinned by a case and named in the queue's module doc as clause 3's
        one exception; the exit releases the record after the last
        registration its rounds can make; nothing reads the new lines yet
      tier: T2 · role: Critic
- [x] S49.3 The ring: blocks in a circle, the reader's API, the in-line reader, the compaction, the splice   *(closed 2026-09-16)*
      handoff: `src/ring.rs` (Writer/Reader/Quiescent/Packing/Chain), the
        registration through `Writer::push`, `Batch` as counts over entries
        that stay in place, `compaction::compact` in place, the deferred lane
        as a `ring::Chain` spliced back; gate and Miri green (the three
        block-scale shapes of `owner_retirement` skipped, each over an hour).
- [x] S49.4 The verdict ring and the owner's disposition   *(closed 2026-09-16)*
      handoff: `src/cycle/queue/verdicts.rs` — P's encoding, the poll's
        prefix reading, the close's disposition in `compaction::dispose_verdicts`;
        `Batch { len, verdicts }` traces P's roots from P's slots; two Critic
        rounds folded, the design record `dev/DECISIONS.md`, 2026-09-16, "the
        owner writes the slots of P it has read"; commit `33856dd`.
- [x] S49.5 The collector's batch   *(closed 2026-09-16)*
      handoff: `src/cycle/worker.rs` — `serve` (work and room by loads before
        any claim, then the claim, an owner collecting in line skipped),
        `batch` (K clamped to P's room and R's count, `BATCH_BOUND` 1024,
        `TRACE_BLOCK_BUDGET` 8, one verdict per root, `AdvanceOnDrop`),
        `verdict_for`; cases `worker/tests/the_batch.rs`; two Critic rounds
        folded, the record `dev/DECISIONS.md`, 2026-09-16, "the collector's
        batch is bounded by three unmeasured figures"; commits `3353283`,
        `2edf2a6`. Named and left: a component past B whose owner-side trace
        the pool refuses circles P and R under pressure; the collector's
        arena draws its own thread's critical reserve; the exit returns P's
        block while a collector may read it pre-claim (for Edmond).
- [x] S49.6 The poll's shrink   *(closed 2026-09-16)*
      done: the poll unlinks the empty block after R's tail block into a
        spare cell or the reserve's return path, never the front block,
        pinned by a case whose ring grew under a burst and shrank after it
      tier: T2 · role: Critic
      Critic 2026-09-16: the collector's pre-claim idle test walked R's chain
        to a stale tail, and the unlink nulls the link of a block such a walk
        still holds. Accepted: the idle test reads the front block alone
        (`ring::Reader::has_unread`), the walk is the token holder's. The
        front-block case could not fail under the front check's deletion.
        Accepted: rebuilt over an emptied front block, red as a SIGSEGV
        under it. The discharge was pinned by nothing. Accepted: the ledger
        read across the poll. Unlinking at every poll with full cells traded
        the circle's free reuse for a pool put and get per fill. Accepted:
        the unlink runs only where a cell is short. The `gc` row's duty
        count. Fixed.
      handoff: `cycle::queue::unlink_surplus_block`, first in
        `refill_and_drain`, one block per poll, only into a short spare cell
        (`dev/DECISIONS.md`, 2026-09-16, "the ring's surplus goes into a
        short spare cell"); the collector's idle test is `Reader::has_unread`,
        no link followed; cases `what_the_poll_owes_the_queue` (three) and
        `ring::tests::the_work_test_reads_the_front_block_alone`, five
        mutations red; gate green in every configuration, TSan silent over
        the collector-reader and batch cases, Miri green over the poll's
        six, the ring case, the batch cases minus the budget one, and the
        collector-reader cases.
- [x] S49.9 The collector's pre-claim reading holds the blocks it reads   *(closed 2026-09-16)*
      done: the idle test reads P's index words and R's front block's only
        while the collector holds those blocks, taken to itself the way the
        memory manager moves a block between owners (Edmond, 2026-09-16,
        `rfc/dev/DECISIONS.md`, "the collector takes P's block to itself for
        the reading it makes before its claim"); an exit that finds a block
        held leaves its return to the holder, which makes it at the
        hand-back, so no block the collector reads reaches the pool under
        the reading; a re-taken record installs no block over one still
        held; pinned by a case whose owner exits between the take and the
        reading and whose blocks reach the pool exactly once, after the
        hand-back, and by a case whose collector reads a re-taken record
      tier: T2 · role: Critic
      Critic 2026-09-16: the exit's "nobody holds" answer was a plain load,
        so a take between that load and the return was invisible to the
        exit, which returned the block under the reading. Accepted: both
        answers store into the word (a returning flag on the clear path),
        with a case that holds the exit between its load and its store, red
        under the plain load. The base block's release asserted the ring
        empty before it read the flag, which a concurrent hand-back could
        fail. Accepted: the flag is read first. A running thread's queue
        reset could leave a live R to a hand-back. Accepted: the leave runs
        on the exit path alone. A reading held a free-list record off the
        registry. Accepted: the take is refused on a returned record by the
        returning flag; the registry skips on a hold or a left ring. The
        owner's arena dropped beside the case's pool reading. Accepted.
      handoff: `cycle::owner_record::HoldLine` and `take_for_reading` /
        `hand_back_reading` / `leave_to_holder_if_held`; `worker::serve`
        reads under the hold with a drop guard; the exit's two leaves in
        `queue::release_queue_segments` and `owner_record::release_thread_record`;
        cases `worker/tests/the_reading_before_the_claim.rs` (three), four
        mutations red; the record `dev/DECISIONS.md`, 2026-09-16, "the
        collector holds the rings' blocks for its pre-claim reading"; rfc
        amended in place. Gate green in every configuration, TSan silent over
        the three cases, the batch cases and the collector-reader cases, Miri
        green over the three cases, the batch cases minus the budget one,
        the record's tests and the exit's drain case.
- [ ] S49.7 The wake channel and the fallback timer   *(after S49.9)*
      done: the collector is born at the first pressure collection as today
        and parks with a timeout that is its fallback interval, adapted
        between two named bounds — lengthened after an empty round,
        shortened when an owner's poll wrote that its last disposition freed
        something — and unparked by a mutator's soft signal, its poll having
        registered N entries since its last signal, counted by its own writes
        and reading no word of the reader's, and by its pressure path; a
        wake only starts a round, and the round takes a batch from an owner
        whose unread count the collector itself reads at or above the
        threshold (Edmond, 2026-09-15, `rfc/dev/DECISIONS.md`, "the collector
        traces on the count it reads itself"), pinned by a case whose wake
        finds every count below it and makes no batch; a case shows a
        signalled collector serving within one round and an unsignalled one
        making no round through an interval set above the case's wait, read
        by a rounds probe; `gc.rs`'s sentence that puts every threshold
        outside the crate names the soft threshold as the runtime's own
      note: the count the collector reads at or above the threshold is read
        under the token, or off the front block alone — a walk of R's chain
        before the claim follows the link of a block the poll's unlink has
        taken out (`ring::Reader::has_unread` and `unread`, S49.6's Critic)
      tier: T2 · role: Critic
- [ ] S49.8 Siblings   *(after S49.7)*
      done: each owner record names its collector; a collector with backlog
        after two consecutive rounds births a sibling — through the same
        `ensure_thread` path, retry interval included — hands it half of its
        owners by rewriting their words and wakes it; a sibling idle for a
        named number of rounds is stripped of its owners and ended by the
        elder; a cap the embedder sets; a mutator's signal reaches the
        collector its word names, and a wake sent to an ended sibling is
        lost until the next poll; cases for the birth, the handover, the end
        and the lost wake
      tier: T2 · role: Critic

## S50 — `ll_thread_init` is called once, and a refusal closes the thread

Goal: `ll_thread_init` is the one initialisation a thread gets, made once by
whoever starts the thread; a refusal is a thread that never starts, and no
path inside the crate calls it for a thread that skipped it. Edmond's ruling
of 2026-09-15 (`dev/DECISIONS.md`, "`ll_thread_init` is called once, and a
refusal closes the thread"), given when the rollback of a repeated init was
found returning a live thread's queue.

Done when: the three self-initialising calls — `stdapi::ll_alloc_init`,
`heap::entity_alloc_init` and the journal's ring open — are gone; a second
`ll_thread_init` on a started thread is refused by a `debug_assert` and
answers true in release without touching anything; a thread that reaches an
allocation, a registration or a record site without its init is handled the
one way S50.1 decides; `cycle::queue`'s "thread the runtime never registered"
paragraph and `ensure_queue_base_or_abort` go with the lazy draw; the cases
that drove the lazy paths go with them and the contract is re-pinned.

- [x] S50.1 Decide what an uninitialised thread gets at its first allocation or registration: an abort, or a null from the allocator with the registration refused   *(closed 2026-09-16)*
      done: Edmond's answer recorded in `dev/DECISIONS.md` under the ruling
        above; the question was put on 2026-09-15 and he said to go on
      handoff: Edmond, 2026-09-16: `ll_thread_init` is called first, and a
        thread without it does not start — no such thread exists for the
        crate to serve. S50.2 reads that as an abort with a named reason at
        each entry point, the state being impossible by contract; that
        reading is the model's and is named in S50.2's `done:`.
      tier: T1 · role: — (Edmond decides)
- [ ] S50.2 Remove the lazy calls and pin the once-only contract   *(after S50.1)*
      done: the three call sites and `ensure_queue_base_or_abort` are gone;
        `ll_thread_init` refuses a second call under `debug_assert`; an
        entry point reached on a thread with no init ends the process with
        a named reason (S50.1: no such thread exists for the crate to
        serve), pinned by a case per entry point; `a_thread_that_cannot_arm_its_exit_guard_is_given_no_ring`
        and the unregistered-thread cases are rewritten or deleted with the
        mechanism; `ll_thread_init`'s doc says once
      tier: T2 · role: Critic

## S40 — Measure the trace's density and decide the row form

Goal: the readings the row form is decided on, and the decision.

- [~] S40.1 Measure
      done: the share of a touched block's slots that a real collection traces
        is measured on the corpus and on a synthetic load, with the denominator
        named — occupied slots or all slots, which differ by two at the design's
        assumed half occupancy — and with the instrument checked against a
        synthetic block whose traced share is fixed by construction; the same
        instrumented run records the pruned-edge share at `k` of 1, 2 and 3,
        which is what settles S37.1's first provisional constant
      tier: T2 · role: Bench → Critic
      Sage 2026-09-04 (pre-change gate): the step splits by the kind of number
        rather than by the mechanism. **The pruned-edge share is taken today by
        simulation**, and the simulation is honest: an entity's age is the
        scan's own verdict across `n >= k + 2` collections, kept in the
        harness's own side table, and the internal in-edges the trace found are
        `refcount - shadow::count`, readable after the trace because mark and
        scan write into no entity. **The suspects re-offer volume is refused**
        on a synthetic load at any depth of instrumentation: on a fixed
        population with a harness-chosen acquittal rate that number is the
        harness's own input read back. It goes to S37.5 with the corpus as a
        stated prerequisite, and S37.1's 64-collection turnover stays
        provisional there while `k` is settled here. What the simulation may
        not claim, stated beside its numbers: it bounds edges from above and
        the saving from below, the subtree an edge alone reached depending on
        the descent order a prune would have changed; it says nothing about
        recall; a saturated row contributes no in-edge count and is reported
        apart rather than folded in at zero; and the ages are the harness's
        ledger, flags bits 16-19 being reserved and unwritten. The instrument is a
        `#[cfg(test)]` walk over the touched list that the collector never
        calls, plus one `note_phase_boundary` in `trace_batch` whose release
        body is empty: zero operations per edge and per entity in a release
        build, one thread-local read and one store per trace in a test build.
        The walk bounds at `row_count` and never at the group rounding, reads a
        row only where its group bit stands, and meets no row. Both
        denominators are reported per population and never averaged across
        them: all slots is `RowArray::row_count`, occupied is
        `BlockPrivate::used` for `Slotted` and the `holds` word's low half for
        `Retained`, and a large entity is 1 by construction and marked
        arithmetic. Since S39.3, both of the first two occupancy readings count
        a dead candidate slot whose physical return is withheld: `used` by not
        decrementing yet, and `holds` by the retained count established at
        reset publication. Groups met are recorded beside rows met, the chunked form's
        directory being one entry per group of eight. Calibration is four
        anchors and a negative one; the load is S40.3's own population, sizes
        2, 16, 256 and 381, dense and one-entity-per-block, ordinary and
        retained; eight collections per load are recorded per collection rather
        than totalled, the first being the one that draws the workspace.
        Refused with reasons: counters inside `mark` and `scan` (they buy
        nothing the final row state lacks and would make `written_bytes` and
        `take_edge_dispatches` unreadable against their baselines), a
        `cfg(test)` per-edge callback (an indirect call per edge in the build
        Miri walks), a feature (a second leg on the commit gate for a
        measurement that runs once), an unconditional instrument (it would
        falsify the claim that an abandoned trace writes nothing), a
        `cfg(test)` field on `TraceScratchArena`, the workspace or `RowArray`
        (four `const` assertions and four `dev/BENCHMARKS.md` entries pin those
        layouts), the collector line as an entity block's occupancy source (it
        carries none; the low half of `holds` is the retained block's),
        `for_each_entity_slot` as a denominator (a process-wide walk the
        control arm would not pay, kept for one second opinion in the
        calibration), repeats of a deterministic count, and any timed run.
        Taken whole.
      note 2026-09-10 — the pruned-edge arm has a built instrument now, and
        the simulation the ruling above priced does not model one of its
        terms. `cycle::mark::take_edges_pruned` counts the edges a real
        collection pruned, and the prune spares every target `CANDIDATE_BIT`
        stands on — a bit cleared at death and at no other point, so it marks
        every entity that ever observed a non-final decrement. A simulation
        that ages by the scan's verdict alone counts those edges as pruned and
        reads high. Reading the counter at `k` of 1 and 2 needs a seam over
        `TRAVERSAL_AGE_THRESHOLD` that the constant does not have today.
      progress 2026-09-04 — the traced-slot instrument, and the range over the
        design's own size classes. `cycle::density` walks the touched list after
        a trace and before the arena's reset, reporting per block the index
        space, the occupancy, the rows met, the saturated rows among them and
        the groups met — kept apart by population and never averaged, an entity
        block's occupancy coming from `BlockPrivate::used` and a retained
        block's from the low half of its `holds` word. The traced path gains
        nothing in a release build; a test build gains one thread-local read
        and one store per trace, at `trace_batch`'s phase boundary, which is
        the only place the mark's own resolution count can be read.
        **The measured range is 0.1 % to 74.7 % over classes 32/64/128/256**,
        and the design's 29 % crossing lies inside it: one component of 381
        members — the corpus's median closure — allocated back to back reads
        18.7 % at class 32 and 37.4 % at class 64, with nothing about the
        collector changed between the two. The two inputs that decide the
        figure are the size class and the allocation interleaving, and the
        collector supplies neither, so **the synthetic arm cannot settle
        S40.2** (`dev/BENCHMARKS.md`, 2026-09-04). Ten calibration cases, seven
        source mutations each caught by the case that owns it, and the walk
        itself makes 0 allocations, 0 pool requests and moves no `gc_metadata`
        figure. This does not close S40.1: the pruned-edge share at `k` of 1, 2
        and 3 remains, and the corpus arm stays Phase-D-blocked.
      progress 2026-09-09 — the pruned-edge simulation. A harness-owned side
        table ages each entity from the scan's own verdict over eight full
        traces; the reading before the verdict reports internal in-edges as
        `refcount - shadow count`, with saturated rows excluded from both
        numerator and denominator and reported apart. Four one-edge components
        become live at collections 1, 2, 3 and never. Across all 32 traced
        edges, `k = 1` would prune 18 (56.25 %), `k = 2` 12 (37.5 %), and
        `k = 3` 8 (25 %); the per-collection ladder and the construction are in
        `dev/BENCHMARKS.md`, 2026-09-09. These are edge upper bounds and saved-
        work lower bounds, not recall or a subtree count. This closes the
        synthetic work of S40.1; the step remains open only on its Phase-D-
        blocked corpus arm, so the figures do not settle S37.1's production
        `k` by themselves.
      Critic 2026-09-09: the simulation stays test-only, adds no operation to
        a production trace, reads each met row once after the scan and owns
        only its harness `HashMap`. Its result is a policy calibration whose
        liveness schedule the harness chose, not workload evidence; the plan
        and benchmark record refuse to promote it into a choice of `k`.
        Saturation is excluded rather than read as zero internal edges, queue
        roots are absent from the numerator, and the age is tested before the
        current verdict advances it. Three source mutations were seen red:
        suppressing the live-age increment, changing `age >= k` to `age > k`,
        and charging a saturated row as one internal edge. The targeted Miri
        slice is clean at 12 passed, 5 ignored, 78.18 s on Miri's clock.
      correction 2026-09-09 — **the pruning result and the Critic verdict
        above are withdrawn.** The simulator incremented age for `Color::Live`,
        but the built driver constructs membership only from
        `Color::PotentiallyUnreachable`, and both `stamp_component` calls stamp
        only that membership. The three externally held self-cycles that
        supplied every reported prune are therefore never stamped; the fourth
        enters membership but is unreachable and is not stamped. Against the
        producer in the crate, the load reads 0 / 32 at every `k`, not 18, 12
        and 8. The simulator also omitted the component-wide `min(age) + 1`,
        the 64-commit epoch turnover, and identity across slot reuse, keying
        history by a bare address. It and its test are removed. The table stays
        in `dev/BENCHMARKS.md` only as a withdrawn record, and the root cause is
        in `dev/POSTMORTEM.md`. S40.1's synthetic pruning arm is open again: it
        waits for S37.1 to build the live-component producer Y9 requires before
        it can calibrate the same byte the corpus run will read.
      correction 2026-09-09, round 2 — the first correction discarded too
        much and did not price that choice. The age/threshold policy and its
        ladder stay removed, but the independent completed-mark census is
        restored as `density::internal_edges`: sum `refcount - shadow count`
        for exact rows and report saturated rows apart. It holds no state
        between collections, accepts no `k`, reads no epoch or stamp and cannot
        claim a pruned share. Its 16-edge ring and saturated-row calibrations
        sit inside the ordinary gate, and the allocation/GC-ledger bracket
        covers the census beside density. S40.1 therefore retains its valid
        denominator while its production-derived numerator remains blocked on
        S37.1. Sage and Critic independently accepted this boundary; their
        review added a target visitor private to the test-only measurement
        module, checked accumulation, the explicit synthetic-row safety clause
        and positive retained/large coverage. It is not a production hook:
        S37.1 owns the traversal in `mark`, and only a later measurement may
        reuse this visitor after that path writes real stamps. Four source
        mutations were seen red: using refcount without
        subtracting the shadow count, ignoring the initialized-group bit,
        treating a large entity's array row count as its index space, and
        failing to exclude a saturated row. The targeted Miri slice is clean
        at 11 passed, 5 ignored, 73.64 s on Miri's clock.
      progress 2026-09-12 — the pruned-edge arm, read against the built
        producer at `k` of 1, 2 and 3. `cycle::mark::pin_threshold` is the seam
        the note of 2026-09-10 asked for: a test-build guard over this thread's
        threshold, read once per `mark` beside the epoch, so no edge pays a
        read in any build and the constant does not move. The load is a
        registered ring of two under a keeper with a held ring of `n` hanging
        off it, `n` being S40.3's 2, 16, 256 and 381, eight real collections
        each; at every `k` the collection after the commit that wrote `k` is
        the first to prune, exactly one edge per collection after it, and the
        mark's rows fall from `n + 5` to 4 — 382 of 386 spared at 381. The scan
        still dispatches the pruned edge once and finds no row, so the trace's
        rows fall to 9 rather than to 8. All 96 readings as constructed, three
        mutations seen red, recorded in `dev/BENCHMARKS.md`, 2026-09-12. What
        the reading is: a calibration of the counter and the stamps against
        `rc-cycle.md`'s arithmetic, at every `k` the age field holds. What it is
        not: a share of any workload, or recall, since the load never loses its
        external reference; nor evidence of the component-wide minimum, which
        every member of this load reads the same under a per-entity age. The
        synthetic work of this step is closed with it; the step stays open on
        the corpus arm alone, which is what settles S37.1's `k`.
      Critic 2026-09-12: the seam cannot reach a production build, cross a
        thread or outlive a panic, and per-mark is the epoch's own granularity;
        the counts `n + 5` and 4 are fixed by the construction and each
        misplacement of the prune reads a different pair. Three findings, all
        repaired here: "one component, one age" was a fixture check presented
        as evidence of the minimum — a per-entity age reads the same on a load
        every member of which is met exactly when the entry is — and the
        record says so now, the minimum staying `cycle::maturation`'s case;
        the trace-row column was printed and not asserted, and is asserted;
        and the scan's cost for a pruned edge was undercounted, the row word
        being read where the group is initialised. The scan's dispatch of the
        pruned edge is a cost to record and not a defect: the stamp test moved
        into the scan would load the stamp and the flags on every edge it
        expands.
      verified 2026-09-12, on the tree with the Critic's repairs: 888 passed,
        0 failed, 11 ignored, plain and three times at eight threads;
        `hash-folding` 888; `debug-journal` 892/13 three times; release;
        `cargo bench --no-run`; `cargo +1.94 fmt --check`; `cargo doc` 45
        warnings; `citations.py` 545 with the same seven residues; `--list`
        diffed against a worktree at the tree before the step, two additions
        and no removal (the day's work lands squashed over `ea51cbe`).
        Miri at two threads: `cycle::mark::` 10 passed, 1 ignored, 17.85 s on
        Miri's clock and 22.8 s of wall; the ignored load itself, 136.68 s and
        3 m 5 s of wall, clean.
      handoff: the corpus arm needs a driver over `ll-model`'s own heap. The
        recorded corpus instruments read PHP's heap, which has no blocks and no
        slots, so this arm is Phase-D-blocked in the same way S37.2 is blocked
        on `classes.md`; the synthetic arm is not, and it is what the row form
        can be decided on if Phase D is far.
- [x] S40.0 Redraw `docs/architecture.md`'s diagrams
      handoff: closed 2026-09-09; the diagrams show the built `cycle` boundary
        and no deleted collector, all five parsing under PlantUML `-checkonly`.
- [x] S40.3 Count the workspace and the cache traffic   *(before S40.2)*
      handoff: closed 2026-09-12. `cycle::census` (two seams, counters), the
        loads `cycle::loads`, the driver `benches/census_driver.rs` with
        `dev/tools/census_perf.sh`; the record `dev/BENCHMARKS.md`, 2026-09-12
        (S40.3), the three Sage rulings `dev/DECISIONS.md`, "the census is one
        report at two boundaries".
- [x] S40.4 Price the trace a refused allocation repeats
      handoff: closed 2026-09-09. 127 refused allocations over 1, 64 and 1,024
        live roots trace exactly that many roots each, 0.45, 3.08 and 42.7 us
        per refusal (`dev/BENCHMARKS.md`, 2026-09-09); no remembered empty
        result (`dev/DECISIONS.md`, "do not cache an empty pressure
        collection").

- [x] S40.5 Specify the chunked form and replay the census through both forms   *(after S40.3, before S40.2)*
      done: `dev/SHADOW-ROW-REPRESENTATION-ANALYSIS.md` §3 is answered in
        that document: what a directory entry encodes and how absence reads;
        the representable range and what happens when it is exhausted; where
        directory and chunk memory come from, their alignment, and how growth
        across arena blocks keeps every row's address; the load chain of a
        first and of a repeated lookup; the enumeration of initialised
        groups, the membership probe, the cleanup and the publication order
        on a refused allocation; and every supporting structure priced in
        `A`
      done: a test-only replay over S40.3's readings reproduces the flat
        form's observed grants, tails and draws exactly on every load, and
        then answers the specified chunked form's requests, grants and draws
        on the same loads, with the draw count given as a bound where the
        order of group first touches is not recorded — every chunked request
        is at most one directory, so the tail a block abandons is bounded by
        that width
      tier: T2 · role: Critic
      Sage 2026-09-12: the specification is a step of its own and stays in
        `dev/`, because a candidate that may be refused is analysis and not
        design, and the rfc takes the amendment only if S40.2 adopts it
        (`dev/DECISIONS.md`, a normative table is a precondition). The replay
        is arithmetic over the census and not a build: the flat form's
        observed draws calibrate it, and the chunked form's requests follow
        from the specification and the per-block `G` and `T` the census
        reads. Final.
      Critic 2026-09-12: seven findings, all repaired. The directory's own
        clearing was unspecified and unpriced — a dirty entry would place a
        row past the block — so §3.1 zeroes the directory whole at placement
        and the replay counts first-touch writes for both forms, which is
        where the chunked form loses: 68 to 486 bytes more per block. "The
        collector's order is fixed by its code" was false past a fan-out of
        256; restated as the ring's order, asserted per load. The flat
        bitmap byte is behind `row_count`, so the load-chain claim is the
        depth at which the row's address is known, two against three. The
        largest row-side request is 1,088 at the smallest class, not 576;
        the class-64 and class-128 directory sizes were swapped; three
        sentences of the record overstated the replay (the rfc's MiB figure
        is in written bytes, the bitmap is 32 bytes and not 255, four loads
        draw and not three) and the large-entity arm meets no counter, which
        the record now says.
      handoff: closed 2026-09-12. The specification is
        `dev/SHADOW-ROW-REPRESENTATION-ANALYSIS.md` §3.1: a `u16` entry in
        eight-byte units from the directory's own address, zero for absence,
        a continuation directory placed with its first chunk where the bump
        has left the chain's tail's block, the directory cleared whole at
        placement. The replay is `cycle::census::replay`, its calibration on
        the gate (`census/tests/the_replay.rs`) and its matrix ignored; the
        record `dev/BENCHMARKS.md`, 2026-09-12 (S40.5): the flat replay reads
        the census to the byte on all 36 loads, the chunked form draws 1
        against 6 and reserves 12 % on the sparse 381 while writing 2.1 times
        the bytes at first touch, and the bracket is one number wide on every
        load. Commit `b8010df`. What S40.2 weighs from it: reserved bytes and draws per load
        for both forms, first-touch writes per load for both, and the range
        in `T/G` where the chunked form reserves less. One divergence from
        this step's second `done:` clause: the tail bound the bracket uses is
        the collection's largest request, a 4,160-byte segment wherever a
        chain draws, so "bounded by one directory's width" holds on the rows'
        account alone.
- [x] S40.2 Decide chunks or not
      done: the decision and its reason are in `dev/DECISIONS.md`, quoting
        per load the flat form's observed draws and reserved bytes beside the
        chunked form's replayed ones (S40.3, S40.5), the row lookups per
        collection both forms pay and the flat form's measured marginal
        lookup, and the rfc's full-trace write volumes, each with its
        denominator; the refused form is recorded with the range, in `T/G`
        and in draws, over which it would have won
      done: a decision to adopt the chunked form is taken only on a built
        candidate measured against S40.3's baseline, and that build is a
        stage of its own which this step opens rather than performs; a
        decision to keep the flat form names the draw count it accepts and
        the loads on which the chunked form would have drawn fewer
      tier: T2 · role: Critic
      handoff: **narrower than it was, and still open.**
        `rfc/model/gc/rc-cycle.md` decides the flat array, and exactly one of
        its figures bears on chunks: 717 MiB against the chunked form's 762 on
        a full trace, plus an unquantified further dependent load per edge. Its
        2.6 ns against 10.4 ns compares the flat array with an open-addressed
        hash, which is a third form this step does not decide between. Against
        the flat array stands the figure 2026-09-04 measured and that
        comparison never took: a sparse trace's manager draws. A 381-member
        component one per block at class 256 makes the flat form ask for six
        blocks where the chunked form asks for none, and each draw is a point
        at which the collection can be refused. One measured figure a side, and
        the per-edge load neither.
      handoff: the density is not the input it was taken for. Over the design's
        own classes one component of 381 members reads 18.7 % at class 32 and
        37.4 % at class 64 (`dev/BENCHMARKS.md`, 2026-09-04), so the 29 %
        crossing sits between two adjacent classes of the same component. The
        class and the allocation interleaving decide it, the collector supplies
        neither, and a single number for "the density" does not exist.
      Sage 2026-09-12 (the document's arithmetic): every source claim of
        `dev/SHADOW-ROW-REPRESENTATION-ANALYSIS.md` that was checked stands
        against the tree it reviewed, the one after S40.1's pruning arm closed
        on 2026-09-12 (inside the day's squashed commit over `ea51cbe`). The
        flat request is `24 + 32 G + ceil(G / 8)` (`shadow::bytes_for`),
        granted at the next multiple of 8 (`TraceScratchArena::alloc`); the
        two placements of 32 met rows in 256 read 216 against 1,112 bytes
        under the two-byte directory at a flat 1,052, so slot density does
        not choose the form and `T/G` does; the descent over a ring of `n`
        holds `2 n` frames and `n` component entries at its peak
        (`maturation::open` pushes a finish frame and an edge frame per
        vertex, `take_edge` a post frame per descent), which at `n` of 381 is
        three worklist and two component segments of 4,160 bytes, 20,800 in
        all, out of the 56,960-byte bump; so the load the handoff above
        quotes draws under the chunked form too, since 45,720 + 20,800
        exceeds the bump, and "asks for none" is true of the trace alone,
        which is what the 2026-09-04 run drove — `stamp_live_components`
        runs on every ordinary commit, an empty membership included
        (`collect::commit_before_drops`). `written_bytes` counts the prologue,
        the bitmap and 33 bytes per group and no row store after them, and
        keeps that contract. The release test binary's assertion is as the
        document says (`row.rs`, the `cfg(test)` `assert!` in
        `resolve_edge_target`). Not verified by a run: the size of `RowArray`
        is read as 24 from the module's layout and the recorded 1,052, and no
        collection was executed for this ruling. Final.
      Sage 2026-09-12 (scope): the decision is taken from the census, the
        replay and the specified model, and the chunk candidate is not built
        inside S40. The two sides are not symmetric. The memory side is exact
        without a build: the flat form's draws are observed and the chunked
        form's follow from the specification to within the tail bound. The
        time side is not: the chunked form's cost is one further dependent
        load per lookup, and no number for it exists until the form exists.
        A decision to keep the flat form can therefore be taken here, quoting
        the draws it accepts and the lookups and marginal lookup cost the
        flat form pays; a decision to adopt the chunked form cannot, and a
        build that touches `shadow`, `arena`, `row`, `mark`, `scan`,
        `maturation`, `membership` and `density` is a stage with a baseline
        and a gate of its own, for which S40.3's readings are the baseline.
        What a decision without the build cannot claim: a speed ratio between
        the forms, or a cache figure for the chunked form; what it can: the
        draw count per load for both forms, the lookup count both pay, and
        the range in `T/G` where the chunked form reserves less. The role is
        `Critic`, as on every T2 step; the Sage is the escalation and not a
        role. Final.
      Critic 2026-09-12 round 1: eight findings, all repaired. "Written bytes
        decided it" was a cache claim the record disclaims, and the flat
        form's writes are spread over 98 pages where the chunked form's are
        contiguous, so it decides nothing by itself; the 2026-09-04
        refusal-surface argument was outweighed by assertion and "sparse is
        synthetic" is asymmetric, the dense ring being synthetic too; six
        loads draw and the chunked form draws fewer on five, not four and
        two; the draw thresholds were 47 and 6 on a live ring, not fifty and
        six; three loads at `T = G` reserve more, and the crossing at class
        256 is `T ≤ 29`; 289 instructions is the marginal edge and not the
        lookup; the reopening reading had to be one a release build can
        take; the rfc's 29 % sentence stands contradicted. The entry was
        rewritten: the flat form stays, the build stage is not opened by the
        model, and the case for it is recorded for Edmond.
      Critic 2026-09-12 round 2, scoped to the rewrite: every per-load
        figure and both crossings check; the window's upper edge was 380 on
        a ring's segments the table gives as 20,800, and is 282 by the
        segment formula, which the table cannot pin between 256 and 381;
        two lesser corrections in the drawing loads' enumeration. Repaired.
      handoff: closed 2026-09-12. `dev/DECISIONS.md`, "the flat row array
        stays, and the case for building the chunked form is recorded for
        Edmond": the flat form keeps its 27 draws over six loads, the
        chunked form would have drawn fewer on five, reserves less below
        `T/G` of 0.91 at class 256 and 0.94 at class 32, and writes 68 to
        486 bytes more per block. Edmond, 2026-09-12: «выбери то что
        быстрее» — the stage is not opened, the flat form stays; the rfc's
        29 % sentence was amended the same day on his ruling that refuted
        text may be changed (`rfc` `8683096`).

---

## Cross-cutting (every stage)

- The old collectors are reachable at `archive/pre-rc-cycle` and nowhere else.
  Nothing is copied back without a decision entry.
- Every fix carries a regression test verified to fail on the bug
  (`dev/WORKFLOW.md`, Tests).
- Miri runs in slices, never whole (`dev/WORKFLOW.md`, Miri).
- A claim about speed is a measurement or it is not made.

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
here. It leaves the parked set.

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

Two residues have no owner, and both are named here rather than in a step:

- **The row-initialization bitmap's accessors have no ratified name.**
  `groups`, `group_bit` and `group_bytes` are described in
  `dev/CYCLE-TERMINOLOGY-AUDIT.md` and were never put to the glossary, so the
  crate is naming them for itself, which the rule against that forbids
  (`dev/DECISIONS.md`, "an uncovered term is a gap rather than a local
  ruling").
- **The comment guard reads fourteen words of a ninety-one-row mapping.** It
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
- [ ] **The threshold arming policy and the collector-thread accelerator.**
  What is left of the old escalation ladder after S38.4 built the entry gate
  and the slow-path fire. The arming policy is the compiler's
  (`rfc/model/gc/strategies.md`, arm/fire); the critical reserve's third
  customer, the mutator whose gate is closed, is answered null today and
  draws nothing, because which runtime progress operations a reserve would
  fund is what the ABI does not yet name (`rfc/model/memory/critical-reserve.md`,
  "Mutator progress while collection is unavailable"). The collector thread
  exists since S38.7 (`cycle::worker`, born at the first pressure collection,
  serving every 10 ms the offers that its relay of an owner's shortage
  produced), and its reach is one concurrent pass per shortage: what would
  make it an accelerator is a request from outside the model — the
  compiler's arming policy or the embedder — through an ABI the rfc has not
  written, which is Edmond's to specify or to leave (`dev/DECISIONS.md`,
  "the worker relays the owner's shortage into its request"); whether an
  in-line pause is shortened by it is the corpus measurement S37 waits on.
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
  member list, deferred slots, deferred drops, suspects — is not carried in a
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
- [ ] **The horizon's borrow elision** (Edmond's algorithm, 2026-08-18,
  named `proof-horizon` until 2026-08-20) — **the documents are gone and the
  work with them**. `gc-horizon.md`, `gc-horizon-cases/`, `gc-horizon-v2/`,
  `rc-walk.md`, `rc-walk-model.md` and `walk/compiler-proofs.md` were deleted
  on 2026-08-26 and are on `archive/pre-rc-cycle`; the ruling that took them
  says why — the proof logic left `rfc`'s scope on 2026-08-23 and nothing in
  force cites it. So the pre-D instrument work this item scheduled — the
  graded corpus scan, the census channel list, the summary-language question —
  has no document to serve and no owner. What outlived the deletion is named
  in that ruling and is where it says; the algorithm itself is Edmond's and is
  on the branch. Kept as one line so the name is findable, not as a task.
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

What the map design owed by the array table — the per-process key, the
ladder's repair and the key word's tag — was S27, closed 2026-08-18 and
deleted with its steps; the decisions it leaves are in `dev/DECISIONS.md`
(2026-08-17 and 2026-08-18), the traps in `dev/POSTMORTEM.md` and the map
in `dev/INDEX.md`. What it did not do is below.

- [ ] **A gate flake, measured 2026-09-03 and pre-existing.** The case that
  reached the gate is fixed and measured; five cases that cannot take the
  same fix are named below and stay open.
  `promote::tests::where_a_survivor_list_is_placed::`
  `lists_with_no_room_anywhere_share_one_fresh_block_the_reset_retains`
  failed 4 of 100 runs of `cargo test --lib -- --test-threads=16`, asserting
  that `gc_metadata::stats()` is unchanged across `arena_reset_full`. What
  differed was the high-water pair alone — a queue base block and the
  `OwnerCycleState` control line another thread charged inside the window,
  twenty-nine test files never taking `block_pool::test_guard`. The ledger
  now answers a per-thread reading beside the process one, every exact
  assertion takes it, and the same loop ran 500 times with no failure. Three
  mutations were run and each was caught: `thread_stats` made to answer
  `stats()`, the mirror deleted from `charge`, and the mirror deleted from
  `released`, which a test over the module's own source names
  (`dev/DECISIONS.md`, "the test-facing reading of the GC ledger is per
  thread"; `dev/POSTMORTEM.md`, "an exact assertion cannot be made against a
  process-global ledger").
  **What is left** is the five cases whose claim is about memory a thread
  that no longer exists gave back, which no per-thread figure can answer:
  `gc_metadata::tests::a_threads_exit_ends_every_block_it_acquired`,
  `what_gc_owns::a_threads_base_block_is_in_use_from_its_draw_until_its_exit`,
  `the_workspace_stands_between_collections_and_goes_back_at_exit`, and the
  two refusal cases in `the_base_block_a_thread_holds_for_its_life`. Each
  reads the process figures across a child thread's whole life and drifts if
  a third thread draws GC memory in that window; none was seen to fail in the
  500 runs. What would close them is a reading of a named thread's figures
  that outlives the thread, which is a structure rather than a patch — worth
  its cost only if one of them is seen to fail. A sixth was seen once, on
  2026-09-15 in one plain run of the gate of some twenty-five that day:
  `mark::tests::an_aborted_mark_writes_nothing::`
  `a_refusal_two_entities_deep_leaves_the_heap_byte_identical`, whose
  `force_oom` is process-wide and whose reserve reading is asserted at zero;
  ten further runs were green and no cause was established. The defect one of the five
  carried is closed: `a_thread_nothing_will_tear_down_is_not_funded` read the
  same process figure into two variables and asserted both, so its segment
  claim had no reading behind it, and it now reads `gc_metadata::thread_stats`
  on the child thread itself — the peak exactly at the base block and both
  spare segments, in both builds, and both current figures at zero. Seen red
  on a build whose `ll_thread_init` refills no spares.
- [ ] **A root the worker read live is never deferred.** The pickup puts a
  root the collector thread read live back in the active lane, where the
  in-line close would have deferred it for an epoch, because Y12 clause 8
  lets no speculative reading move an entry to the deferred lane. A
  long-lived root is therefore re-offered at every request and re-traced by
  the worker until an in-line collection reads it. Whether the owner may
  defer on the worker's reading — a delay of one epoch at most for a garbage
  root the worker misread — is the rfc's question; the crate carries the
  treadmill until it is answered (S38.6's Critic, 2026-09-15). Its rate is
  once per shortage the owner suffers, not once per request, since the
  worker's request is a relay of the owner's pressure collection
  (`dev/DECISIONS.md`, "the worker relays the owner's shortage into its
  request").
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
  code nobody has written. Named by S38.7 on 2026-09-15.
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

## Cross-cutting (every phase)

- Correctness tests per the project style (`test_guard`, scenario-per-test)
  and criterion benchmarks per `dev/BENCHMARKS.md` — follow the protocol,
  do not improvise. Benches do not cross the C ABI; ABI-entry work is shown
  by IR/asm.
- `dev/ARCHITECTURE.md` — the crate's knowledge map: layers and their
  sanctioned edges, the per-module "does not know" table, the header-bit
  ledger, the five end-to-end paths. Written; it moves with behaviour
  like any other document (`dev/WORKFLOW.md`).
