# Plan

Implementation plan for `ll-model`, re-sorted 2026-07-24 against the RFC
after the 2026-07-22 object-layout redesign landed in the `rfc` repo.

Design lives in `rfc` and is authoritative — read before coding, do not
re-derive: `model/classes.md`, `model/values.md`, `model/lowering.md`,
`model/gc/rc-cycle.md`, `model/gc/cycle/questions.md`, `model/memory/ffi.md`,
`runtime/object-lifecycle.md`.

The `rfc` repository carries its own plan at `dev/PLAN.md` for work that lands
in the specification rather than in this crate.

Updated: 2026-09-12 · Active: S47, broken down on 2026-09-12 and owed a
Critic round before S47.0; S36's one open step is S36.9, which
closes on a deny run over a reset inside a collection that `promote`'s own
containers (S47) still fail; S36.17 closed on 2026-09-12 with the window's
memory under the manager, S36.18 the same day with the COW
reconciliation settling off the window's log, the walk that double-counted
a destructor's store into a promoted holder gone, and S36.19 the same day
with the re-trace running after every destructor round rather than after
an allocation. S40's one open step is
S40.1's Phase-D-blocked corpus arm, and S38 waits on `rfc` A1 for S38.0 and
S38.3. **S44 closed and was deleted on 2026-09-12**, its last step being
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
Of what is left in S37, S37.2 is blocked outside this repository;
S36 has S36.9 left, waiting on S47. **S34 closed and was deleted on 2026-09-10**,
its last step being the law that only the owner reduces state; what outlived
it is in the journals, and the two debts it carried without an owner are in
`## Fog` and in the backlog below.
S36 is the work in front:
its S36.9 has the deny run a wired collection owed — five cases, green, three
mutations seen red — and Edmond ruled on 2026-09-12 that the reset's
exemption does not reach the frames a destructor's `ll_arena_reset` enters;
S36.17 took the window's five sites out the same day, and what stands in
those frames now is `promote`'s own containers, S47's debt; S36.12 closed on
2026-09-06 with the pressure path's harvest, S36.3 closed the same day with the
guard references and the weak window, S36.4 the same day with the destructors
and the revalidation behind them, S36.5 on 2026-09-07 with the sever, the frees
and the deferred drops, S36.6 the same day with the maturation stamp and the
epoch counter under it, and S36.7 the same day with the collection behind the
ABI — `cycle::collect`, which is the first production caller the modules of
`cycle` have. Two of its steps are the debts that left with it: S36.15 puts the
collection under pressure behind the allocation failure that should start it,
and S36.16 carried the merge and the two paths back into `rfc` on 2026-09-07,
ahead of S36.15 because S36.15 needs `cargo` and a Miri run held the lock.
S36.15 lost half its criterion the same day and then closed: a collection
returns no slot of a **member**, every one being a registered candidate whose
slot `ll_free` withholds, so "served rather than refused" moved to S39.2
(closed 2026-09-09), which retires the entry at the owner's read (the Sage's ruling,
`dev/DECISIONS.md`, "the collection's yield and the retirement that unlocks it
are two steps"). Its Critic round left one open number, **S40.4**: what the
trace a refused allocation repeats costs at the memory ceiling.
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

**The crate collects cycles in-line.** S36.7 wired the collector and
S36.15 its allocation-pressure caller. Ordinary collections keep rows through
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
recorded in the step's handoff. This applies to S36.9 onward and to the
performance steps in S37/S40; one broad review does not waive a later step's
gate. Until 2026-09-12 this paragraph put the Sage before the first edit.

**Every byte owned for cycle collection comes from the memory manager and is
identifiable there as GC memory.** Production collection paths use no
allocator-owning Rust containers — no `Box`, `Vec`, `HashMap`, `BTreeMap`,
`Arc` backing allocation or hidden `GlobalAlloc`. Plain `#[repr(C)]` layouts,
fixed arrays, slices and raw links are representation, not ownership; their
backing blocks come through the manager. S36.9 makes this executable rather
than aspirational and audits the existing retained-index boundary before the
collector may cache it.

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
`done:` clause of S38.3, and two by the repairs they prompted, recorded under
S36.9. The `dev/` sweep of the same day raised one more — `FORCE_OOM` against
the guard rule of `dev/POSTMORTEM.md`, 2026-08-13 — and it was fixed rather
than carried: the flag is raised only through `block_pool::force_oom`, whose
guard lowers it on the unwind as well as on the return.

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
  S36.4 reads that as a member's pending `__destruct` at step 4, which is what
  `DestructorPass` records. The other reading is any user destructor at all, the
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
  glossary's words on 2026-09-12 (S36.17); no step owns `promote`'s.
- **An object handed to a survivor by a destructor of the same batch still
  runs its own `__destruct`.** The settle loop drains a round's destructor
  entries before the re-trace, so `$survivor->keep = $this->y` in one body
  does not spare `y`'s body later in the same batch: `y` is promoted
  already-destructed, its later `dispose` finding `DESTRUCTOR_RAN` and
  skipping. Zend would not destruct it, `y` never having become
  unreachable. Named by the Critic of S36.19 on 2026-09-12; whether the
  rfc's "survives already-destructed" paragraph admits it is the rfc's
  sentence to write.
- A destructor's `ll_thread_exit` waits for the thread's top
  (`memory::heap::thread_exit_pending`), and nothing tells the code above
  the destructor that a request stands: whether the emitted safepoint reads
  that word and unwinds on it, and under what name the ABI carries it, is the
  rfc's to say (`dev/DECISIONS.md`, "an exit requested inside a collection
  runs at the thread's top").

---

## S36 — Commit

Goal: only the owning thread frees. An in-line owner trace may commit what its
stable mark/scan proved; a speculative trace must first pass the owner's exact
test. The teardown is here in full: the Critic round of 2026-08-26 found the
stage claiming the frees while building none of them.

- [x] S36.1 The exact test on the owner's thread
      done: current fields are re-read on the owning thread before any free, and
        the test opens with the corpse rule — a member read at count zero drops
        the component whole before any guard or field write — exercised by a
        test in which tearing down one component releases into a second already
        judged white; the refusal path is exercised by a mutation racing the
        verdict, and by a positive control in which the same scenario without
        the mutation does free
      tier: T2 · role: Critic
      Critic 2026-08-29: eight findings. Taken — the safety contract names a
        member whose slot is still its own rather than a live entity, since the
        corpse rule reads a count of zero; the corpse test compares the state
        before the call against the state after it instead of asserting a
        residue the runtime never holds; the control arm builds the keeper it
        does not write into, so the two arms differ by the store alone; the
        sum's premise is checked member by member in a debug build, the sum
        being unable to see a defect that invents one in-edge and loses
        another; an empty component, a member outside the GC heap and a zero
        count under a guard are debug-asserted. Refused — naming the corpse in
        the answer: the disposition of an entry belongs to the queue drain,
        which sorts entries rather than members
        (`rfc/model/gc/cycle/questions.md`, Y12 clause 5). Verified and not a
        defect — `row::edge_to` places a `LongLived` entity on an interior row,
        which its own comment covers: the category is out of use.
      handoff: `src/cycle/exact.rs` is `judge(members, discount)`, and the
        tests are under `src/cycle/exact/tests/`; four source mutations were
        run and each was caught by the test that owns it. Two debts leave with
        this step. **Nothing derives a member list from the condemned rows**,
        no step of this plan owns that, and the design leaves the vehicle for
        that memory unnamed (`rfc/model/gc/rc-cycle.md`, "The release obliges
        a readership rule"). And a member of a kind other than an object or an
        array is untested — `Reference`, `Lazy`, a template, a class with cells
        outside itself.
- [x] S36.2 The trace-window parking
      note: S36.1's Critic round found the window this step has to cover. A
        member that never took a non-final decrement has no queue entry, so
        nothing names its slot; what keeps its header readable for the corpse
        rule is this step's trace window and not the entry.
      done: a slot freed while mark or scan may still address its row waits for
        the trace's end, releases into
        S34.3's single return path, and that path refuses while **either**
        window is open — a queue entry naming the slot, or a trace in
        flight; a red-first test shows the defect it prevents, a reused slot
        inheriting the dead occupant's row, and overlaps the two windows in both
        orders
      tier: T2 · role: —
      handoff: `cycle::parking::TraceWindow` owns the `ShadowArena`, so its
        drop order is not a caller convention: first reset and null every row,
        then lower the owner-local active flag, then replay the out-of-band
        returns through `stdapi::ll_free`. It is `#[must_use]`, cannot move to
        another thread, and a nested open fails in release as well as debug.
      handoff: all three row populations wait. A retained block has no slot
        free list, but its last occupant returns the whole block; pooled large
        entities return a block and OS-direct ones unmap a run. Seven tests
        cover those three routes, a slotted address inheriting a corpse's row,
        and both orders of the queue/trace windows. The retained work also
        repaired S34.3's older omission: its `ENROLLED` test had named only
        slotted and large entities. The block-return sentinel is deliberately
        distinguished from a retained entity pointer before any header read.
      handoff: this is the synchronous owner-side substrate only. S38.1/S38.3
        must replace the TLS active state and list with owner-addressable state
        before a worker traces another thread; the generation/handoff problem
        remains RFC audit A3 and is not claimed closed here.
      handoff: Critic and Sage reviews 2026-08-31 found the retained omission,
        the arena-before-replay ordering, an impossible first version of the
        reverse-overlap fixture, the movable/nestable guard and the exit order.
        All were taken; the old objection to a `Vec` allocation was withdrawn
        against the later decision that explicitly accepts the cold,
        trace-only allocation.
- [ ] S36.9 The GC-memory contract   *(waits on S47)*
      progress 2026-09-01 — S36.9a physical contract: the single
        `memory::gc_metadata` door owns pool/reserve adoption and return,
        `BLOCK_KIND_GC_METADATA` makes the bytes collection holds identifiable,
        and current/high-water block counts are observable. Queue control moved
        from TLS into one 64-byte floor line; TLS is one non-owning pointer,
        escrow capacity is 8,152 and `POLL_STRIDE` is re-derived as 4,076.
        This does not close S36.9: logical accounting and allocator-free
        parking, weak and retained storage remain.
      repair 2026-09-01 — the slice's `escrow` addressed the block through a
        `&OwnerCycleState`, which covers the control line alone; Miri fails the
        write on the `ll_release_vector` path and the parent tree passes. The
        floor pointer is threaded through `grow_and_write` and `escrow`
        instead, `escrow` is `unsafe` and states the precondition, and the
        overflow test that spawns a child now carries `cfg_attr(miri, ignore)`
        — without it Miri stopped at that test and ran none of `cycle::` after
        it. `dev/POSTMORTEM.md`, 2026-09-01. Miri over `cycle::`: 86 passed,
        0 failed, 1 ignored.
      repair 2026-09-01 — the slice's tests were rebuilt where they agreed
        with the code instead of constraining it. The capacity figures are
        asserted as the literals the documents name, and the escrow's last
        entry is asserted to end flush with the block, which is what makes the
        capacity exact rather than sufficient. The overflow test reads the
        child's signal rather than its exit status. The three refusals at the
        boundary have tests of their own — `BlockPool::put` and
        `critical::give_back` against a GC-stamped block, `adopt` against a
        source that is not the reserve — the reserve's aimed at the arm that
        keeps the block, since at capacity the pool answers first. Six source
        mutations were run and each was caught by the test that owns it.
      progress 2026-09-01 — S36.9b logical ledger: `gc_metadata::charge` and
        `discharge` keep one current and one high-water figure for the bytes in
        use inside the blocks collection owns. The figure moves at five
        structural transitions — a segment leaving the live position, an escrow
        landing, a floor's control line, an arena block leaving the bump, and
        the reset that publishes the block under the cursor before discharging
        the collection's whole total — so the enrolment write takes no added
        instruction. Two residues are documented granularity, each bounded by
        one payload: the live segment's own fill and the arena block still
        under the bump — and each is entered in the high-water figure by the
        transition that ends it, which is exact on one thread and can miss a
        maximum two threads stood in together. Thirteen source mutations were
        run and each was caught by the test that owns it.
        This does not close S36.9: allocator-free parking, weak and retained
        storage remain.
      Critic 2026-09-01: fifteen findings. Taken — `drain` released the live
        segment without charging its fill, so a thread that filled a segment
        and never overflowed was absent from the high-water figure while three
        documents called that figure exact; `drain_escrow`'s discharge was the
        one ledger site no test reached, and deleting it leaked eight bytes per
        entry for the life of the process; `current_bytes_in_use`'s contract
        stated a bound against the reservation that this step's own test
        contradicts, a spare segment and a block header being reservation; the
        peak assertions were absorbed by the process-global high-water, which a
        `#[cfg(test)]` door that lowers it to the current figure now makes
        exact; no test drove `enrol`'s ordinary write, the path the design
        exists to keep clear; `drain_escrow` took one read-modify-write per
        entry where `drain` took one for the run; `stats` could report a
        high-water figure below its own current one, and the byte axis was
        lifted — round two found the block axis still unlifted and it was
        lifted there; the
        payload charge rested on a fullness invariant with no assertion;
        `draw_floor`'s comment gave a reason that was not the reason, and the
        arena's claimed a single publish site it does not have. Refused —
        nothing: the two remaining findings are the `expect` on the discharge,
        which the Critic could not fire against the five sites, and the
        citation form, fixed in place. Known gap: the journal's re-entry into
        `draw_floor` is driven by no test, so a charge moved above its
        installed check would go uncaught.
      Critic 2026-09-01 round 2: eight findings, all taken, two of them
        defects the first round's repairs introduced. Batching `drain_escrow`'s
        discharge into one operation for the run left the escrow's bytes
        standing over entries already re-enrolled, so a recovery inflated the
        high-water figure by a whole payload for the life of the process — the
        discharge went back to one per entry, and `dev/BENCHMARKS.md` records
        the batch as tried and refused. `drain`'s charge-and-discharge pair for
        the live segment's fill was observable by another thread as a current
        figure holding a segment already gone, so a peak-only
        `gc_metadata::mark_peak` replaced it. Of the rest: the block axis was
        never lifted the way the byte axis was, and the contract claimed both;
        the high-water figure carries a residue only from the transition that
        ends it, which is exact on one thread and not across threads, and five
        texts claimed it exact — each now states the bound; every enumeration
        of the charge sites was one short of the code; the contract explained
        the cross-axis excess by the wrong read order; and four of the new
        tests passed under a mutation of the line they were written for, which
        two rewritten tests and two added assertions now fail. Not taken
        further: the device stops at two rounds.
      progress 2026-09-01 — S36.9c manager-backed withheld returns: the
        physical returns an in-line trace withholds move out of a
        `Box<Vec<_>>` and into a chain of `gc_metadata` blocks. The first is
        drawn at `ActiveTrace::open`, which now answers `Option<Self>`, so both
        doors refusing is a collection that does not start rather than a
        refusal met with a slot in hand; a growth past that block that both
        doors refuse ends the process, as the overflow buffer's bound does.
        TLS keeps one non-owning pointer to the head block and a null one is
        the closed window, which retires `TRACE_ACTIVE`. Measured on the day:
        the withheld-return path made 2 global allocations and now makes 0
        (seen red before the first edit), `.tbss` 480 bytes to 464, `cycle::`
        under Miri 104 passed to 113 with 0 failed, and the `lifecycle` timed
        run void on its own A-A control (`dev/BENCHMARKS.md`). Three of
        S36.11's `done:` claims land here, and its clause names what is left
        to it. This does not close S36.9: weak and retained storage remain.
      progress 2026-09-01 — S36.9d the weak table and the streaming drain: the
        per-thread weak table leaves the global allocator for an open-addressed
        table in one long-lived buffer payload — the mutator's storage class,
        not `gc_metadata`'s, because the ledger counts what collection holds
        and a thread that never collects fills this table. Sixteen-byte rows
        carry the target and a tagged subscriber word, capacity is a power of
        two at a load of one half, and every fallible step of
        `ll_weakref_create` runs before it holds anything, so a refusal answers
        null. `drain_arena_weak_log` notifies inside the drain's own walk
        instead of collecting into a `Vec`. Measured on the day: the first
        create made 2 global allocations and now makes 0, 200 creates across
        three growths made 8 and now make 0, the reset's weak walk made 2 above
        its control arm and now makes none, `.tbss` 464 bytes to 472 with the
        crate's thread-local set unchanged. `weak::` under Miri went 7 tests to
        19 with 0 failed, measured before the second Critic round's repairs:
        from 2026-09-01 Miri runs at the close of a logical block rather than
        of a step (Edmond; `dev/WORKFLOW.md`), so the run that covers this
        tree is S36.9's and is owed there over `weak`, `cycle` and `memory`. This does not close S36.9: retained storage remains, and
        the composite deny run over a wired collection waits for S36.7.
      progress 2026-09-02 — S36.9e retained index and registry ownership: the
        process-wide `Mutex<BTreeMap<usize, Index>>` with an `Arc<[usize]>` per
        block is deleted. The reset writes each retained block's sorted
        survivor list into memory the arena already holds — the block's own
        tail past its recorded fill, else the reset's current block, else one
        fresh pool block shared by the lists that missed — and publishes its
        address, its length and one atomic count word in the block's collector
        line, live occupants in the low half and pins in the high half. Every
        reader asks the block; the trace's retained arm takes no lock and the
        reset makes no global allocation (2 to 0, `dev/BENCHMARKS.md`).
        Built and reviewed on `work/s36-9e` by a Fable line and merged at
        `50dba6d`; the merge's own gate is 644 tests, 648 with `debug-journal`.
        This does not close S36.9: the composite source audit and the deny run
        over a wired collection remain, and the deny run waits for S36.7.
      Sage 2026-09-01 (slice d gate): the buffer layer is the consumer and
        `gc_metadata` is refused, the block kind being the answer to whose
        memory a block is; `array::table`, a cell pointer in the object header
        and a slot-indexed side array are refused with reasons; null at the ABI
        entry is the refusal answer, since `create` has a caller who can
        decline; the ledger gains no site and no residue; the deny gate is
        module-level zero and may not claim the collection-run leg; and no
        timed run is taken, this box's control having been void today. Taken
        whole, including the initial 64 rows and the half load.
      Critic 2026-09-01 round 1 (slice d): nineteen findings, none of them in
        the table's arithmetic, which the Critic put through a fuzz of its own
        against a model in its scratch directory — an instrument of that review
        and not of this repository. The load-bearing ones: neither the
        growth's nor the disposal's return of the payload was constrained by
        any test, and Miri cannot see it because the chunk is pool memory
        rather than the global allocator; the ledger test compared against a
        process-global high-water figure it had not lowered, so a charge would
        have passed it; the growth probe never asserted that a growth happened
        and counted its own `collect` inside the window; and the streaming
        drain's stated reason — "runs no user code" — is weaker than the
        condition it needs, which is that the callback may not reach the
        `Arena` at all while the walk holds `&mut` on it. All taken. Not a
        defect: `find`'s tag mask is behaviourally dead while the only tag is
        zero.
      Critic 2026-09-01 round 2 (slice d): fifteen findings, five of them in
        round 1's own repairs. The ledger test lowered one high-water figure of
        two, so a table drawn through `gc_metadata` would still have passed it
        — the test-only door lowers both axes now; the growth assertion could
        not fail, and pins the capacity three growths reach instead; the
        `drain_weak_log` doc gained a second summary and an order the log does
        not have, newest segment first being the chain's; and "the refusal is
        taken before anything is built" is false of the cell's refusal after a
        growth has already taken hold, which the prose now states. Also taken:
        the payload-return tests held the address and not the size the free
        recorded, which a second request at twice the size now pins; the
        OS-direct boundary was asserted against a copy of the arithmetic; and
        `chunk_from_the_free_list` restored the pressure mode it found rather
        than `Plenty`. The device stops at two rounds.
      progress 2026-09-02 — S36.9e the survivor list and registry ownership:
        the process-wide registry of retained blocks — `Mutex<BTreeMap>`,
        `Arc<[usize]>`, `snapshot` — is gone. The reset writes each retained
        block's sorted survivor list into the arena's own memory, the block's
        own tail when it fits past the block's recorded fill, else the
        reset's current block, else one fresh pool block shared by every list
        that missed, and publishes its address and length in the block's
        collector line beside one atomic count word: live occupants in the
        low half, pinned payloads and the lists of other blocks standing in
        the block in the high half. Every list is placed before any count is
        read, a holder is retained once and pinned once per list, and the
        decrement that reaches zero returns the block, spending its own
        list's hold on the holder first. The absorb keys on the count word,
        and the reset's empty-block return has an arm of its own in
        `ll_free`. Measured on the day: publishing a list made 2 global
        allocations and now makes 0 (seen red before the first edit), pool
        requests 0 in the first two tiers and 1 for two lists with no room
        anywhere, `gc_metadata::stats()` unchanged across a reset that lists,
        `.tbss` 472 to 472; the registry acquisitions per retained-only
        trace, `2E + V + B + 2R` by reading, are 0 by absence. A defect found
        by the gate's reading and seen red on the base: a block pinned for a
        payload alone whose payload died inside the reset stayed retained
        for the life of the process, its return absorbed as a corpse's free;
        the sentinel arm closes it. The direct-large registry audit:
        `large_entity::runs` is read by no production path — the large arm
        of `row::resolve_edge_target` reads kind and category — its readers
        are the test-only enumerator and `describe_slot`, and it sits on the
        mutator's OS-direct entity alloc and free path, outside the
        collection paths the deny gate covers; gating it `cfg(test)` is a
        backlog question, not this slice. Not changed: the reset keeps its
        `HashMap` and `Vec` (`dev/design/retained-index-ownership.md`'s
        disclosed decision), and the rfc's sentence "publishes them with the
        release store that stamps the block's kind" reads as one instant
        where the code has two, which is the `rfc` repository's to amend.
        This does not close S36.9: the composite deny run over a wired
        collection waits for S36.7.
      progress 2026-09-03 — S36.9f the OS-direct run registry: the first of
        the audit's two live sites is closed. `large_entity`'s
        `OnceLock<Mutex<BTreeSet<usize>>>` is now a doubly linked list
        threaded through the run headers — `prev` and `next` between
        `run_bytes` and `row`, a null head in a `static Mutex<Runs>`, linked
        under the lock after the kind's release store and unlinked under it
        before the unmap, `snapshot` walking under the lock and copying out.
        The instrument came first, as the gate required: the probe counted a
        free as nothing by design, so `take_heap_deallocations` and a
        counting `dealloc` were added and read against a dropped `Box`
        before `large_entity.rs` was touched, with a second calibration
        pinning that a reallocation is one allocation and no free.
        `take_all` is `take_allocations`, three counters having made the old
        name false. Measured on the day and seen red on `43951b4`:
        registering twelve runs made 3 global allocations and now makes 0,
        freeing the twelve made 2 global deallocations and now makes 0, pool
        requests 0 in both halves, `.tbss` 480 to 488 for the probe's own
        `cfg(test)` counter and nothing else. Each of the three writes an
        unlink makes was dropped in turn and the A/B/C test went red on all
        three, as a fault rather than a mismatch — a link left standing
        names an unmapped page and the next walk reads it. Recorded in
        `dev/DECISIONS.md`, `dev/BENCHMARKS.md` and `dev/INDEX.md`, and
        `rfc/model/memory/large-entities.md` is amended to the built shape.
        This does not close S36.9: the composite deny run over a wired
        collection waits for S36.7, and `reset_window::park_large` and
        `died_set` wait with it.
      Sage 2026-09-01 (slice c gate): the records live in a chain of manager
        blocks of their own, drawn at the open rather than at the first
        withheld return, because a refusal is answerable only before a slot is
        in hand; the trace arena is refused as the store, its own reset
        returning the record blocks before the replay reads them, and the
        queue base block is refused, its payload being exactly full and its
        lifetime the thread's. Refusal model: `None` at the open, no failure
        within capacity, process abort on a refused growth. Taken whole. Where
        the implementation departs: the append reads three loads rather than
        the ruling's two, the fill living in the block the cursor points into.
      Critic 2026-09-01 round 1 (slice c): thirteen findings, eleven taken.
        The load-bearing ones: an unwind out of the replay stranded the chain
        and left the ledger permanently inflated, `BlockPool::put` panicking on
        a poisoned mutex and nothing behind it re-entrant the way
        `arena::reset` is; `mark_peak` ran after the arena's reset, so the two
        residues of one collection were never in the ledger together while
        `gc_metadata` called the figure exact for one thread; the growth path's
        reserve accounting was constrained by no test; and the thread-exit call
        site still carried a comment about the `Vec` this step deleted.
        Refused: extracting the funding machinery this module now shares with
        `arena` and `queue` — that is S36.11's primitive, and building it here
        is that step done early.
      Critic 2026-09-01 round 2 (slice c): twelve findings; the first is a
        use-after-free round 1's own repair introduced. Moving the chain under
        its own `Drop` left the thread-local pointer naming it to the enclosing
        close, so an unwind out of the row sweep released the blocks with the
        window still standing, and the next free would have written a record
        through a stale cursor into memory the reserve had lent out again. The
        window is taken down by whoever releases the chain now, on both paths.
        Also taken: the ledger counted a later block's reserved control line as
        in use, against `current_bytes_in_use`'s own rule; the module cited
        Y14 for a claim Y14's 2026-08-26 amendment reverses; `critical`'s
        stamping enumeration was wrong in class and not only in count; and two
        prose claims contradicted the code they stood over. Refused: the
        finding that a test's failure message states the opposite of its
        contract — the message names the failure, which is what the crate's
        assertions do.
      Known gaps of slice c: no test drives an unwind out of the row sweep, so
        the second take-down of the window rests on reading; and nothing
        constrains `mark_peak` against a `charge`/`discharge` pair, which one
        thread cannot observe.
      Sage 2026-09-01: the charge belongs at a structural transition rather
        than per grant; the figure is bump consumption published at those
        transitions rather than a per-grant sum; the pre-change baseline is the
        gate, Miri over `cycle::`, `.tbss` and the enrolment operation count,
        the logical figure itself having no prior instrument. Taken whole —
        `dev/DECISIONS.md` carries the charge sites and the refused
        per-enrolment alternative, `dev/BENCHMARKS.md` the operation count and
        what `.tbss` cannot resolve.
      audit 2026-09-02 — the composite source audit ran over `cycle`, its
        parking and deferred-drop storage, the weak registry and disposal
        path, `retained`, `gc_metadata`, `promote`, `reset_window` and the
        three manager modules where a collection calls them. **The clause is
        not met**, and two live sites stand between it and met.
        `large_entity::free`'s run arm takes a process-global
        `Mutex<BTreeSet<usize>>` and frees its nodes at
        `large_entity.rs:165`, inside the collection's own close: an
        OS-direct large entity is inside `can_lose_trace_identity`, so it is
        withheld and replayed through `stdapi::ll_free` at
        `ActiveTrace::drop`. A deny run cannot see it, `remove` freeing
        rather than allocating, which is the case the clause reserves for
        this audit. Second, `reset_window::park_large` grows a `Vec` at
        `reset_window.rs:321` on an arm `ll_free` tests **ahead** of
        `defer_reuse_if_tracing`, and `record_death`'s `died_set` is a boxed
        `HashSet` on the same footing for S36.4 and S36.5. Both are reached
        from a collection's paths rather than from the reset's own frames.
        Clean, and named so the next audit need not re-read them: `cycle`
        itself, `weak`, `gc_metadata`, `retained`, `critical`, `block_pool`,
        `heap` and the dispose path; `promote`'s twelve container sites are
        the disclosed decision and no collection path calls into `promote`.
        Noted for S36.5 rather than found here: `cells::sever` is typed
        `unsafe fn(*mut RcHeader, &mut Vec<*mut RcHeader>)` across five
        modules, dead code today, and becomes a growth inside the denying
        window on the day S36.5 wires it — the manager-backed replacement
        belongs to that step's design, before its code.
      miri 2026-09-02 — the run this step owes over the modules its block
        touched: `weak::` 19 passed, `cycle::` 120 passed with 7 ignored,
        `memory::` 145 passed with 5 ignored, 0 failed anywhere, at two
        threads, 2 m 19 s, 46 m 43 s and 15 m 43 s of wall. `memory::` was
        unrunnable until this run:
        `critical::tests::where_the_first_touch_happens::`
        `the_crate_declares_these_thread_locals_and_no_others` reads `src/`
        and carried no `cfg_attr(miri, ignore)`, so the slice aborted after
        eleven tests and the other 134 had never run under the interpreter
        (`dev/POSTMORTEM.md`, 2026-09-02).
      ruling 2026-09-02 — `reset_window::park_large` and `died_set` wait for
        the deny run at S36.7 rather than for an argument now: no collection
        nests inside a reset until the collection is wired, and the reading
        that decides them — whether the disclosed reset exemption covers the
        frames a collection enters or only the reset's own — is Edmond's and
        is cheaper to take against a run than against a source path.
      Sage 2026-09-02 (the OS-direct run registry): the intrusive list, and
        it is slice (e)'s shape applied to this population — the index of a
        block lives in the block it describes. Refused with reasons:
        manager-backed storage (a second lifetime, a lookup on free, a
        refusable growth, a ledger question for storage no production path
        reads), a fixed-capacity array (a run is any entity above 65,280
        bytes and a full table would turn an entity allocation into a
        refusal), the `BTreeSet` with an exemption, `cfg(test)` gating, and
        deleting the enumerator's run leg, which the `promote` census tests
        stand on. **Doubly linked, two words, null-terminated, head under
        the mutex**, because `free` removes an arbitrary run inside the
        collection's close and a singly-linked list would walk the live runs
        under a process-global lock for every dead one. Layout, `#[repr(C)]`,
        pinned by `offset_of!`: `kind` 0, `_pad` 4, `size` 8, `run_bytes` 16,
        `prev` 24, `next` 32, `row` 40, size 48 within the 256-byte line;
        `row` stays last and its only accessor reaches it by field.
        `static RUNS: Mutex<Runs>` with a null head — `Mutex::new` is const,
        so the `OnceLock` goes, and std's futex mutex allocates nothing.
        Counts: the free path goes from a lock, a B-tree search and 0-2
        global deallocations to a lock, two loads and one or two stores;
        `alloc` loses the hidden abort a `BTreeSet::insert` can reach
        through the allocator's error handler on a path whose contract says
        null. Nothing to refuse, and that is right: the words are in the run
        the mapping already owns. The invariant is that the list is exactly
        the set of runs between `alloc`'s return and `free`'s entry into the
        unmap — linked under the lock strictly after the kind's release
        store, unlinked under it strictly before the unmap, and the mutex
        rather than the kind is what publishes the links. `snapshot` keeps
        its signature and its `Vec`, walks under the lock and copies out; a
        visiting `for_each_run` is refused, a visitor that frees or allocates
        re-entering the same mutex on its own thread. Its order becomes
        reverse registration and every reader is order-insensitive. Final.
      Sage 2026-09-02 (the baseline this step would erase): the real
        instruments are `test_support::allocation_probe` around twelve
        OS-direct runs allocated and freed under `test_guard`, and **the
        probe gains a deallocation counter first**, verified against a
        dropped `Box` before `large_entity.rs` is touched — without it the
        defect the audit named is invisible to every instrument in the
        crate, today's probe counting a free as nothing by design. Then
        Miri over `memory::large_entity`, `cycle::deferred_slot_reuse` and
        the four `promote` tests that read `snapshot()` after a free;
        `promote::` has no recorded Miri figure, so its slice is run at
        baseline or the Miri claim is limited to `memory::` and `cycle::`
        and says so. A one-run probe fixture is theatre, one insert into an
        existing empty root leaf allocating nothing; `.tbss` is a control
        rather than evidence, the change adding no thread-local; a timed run
        is theatre, no benchmark driving a run's free. Final.
      retracted 2026-09-02 — gating `large_entity::runs` `cfg(test)` was
        ruled and then refused on the facts, before any commit carried it.
        The registry's reader is `heap::for_each_entity_slot`, which is
        `pub`: gating deletes a public item or leaves a public enumerator
        skipping the OS-direct population in a release build, and the audit's
        phrase "test-only enumerator" described its callers rather than its
        visibility. What the refusal leaves is the shape the ruling should
        have taken — the run addresses thread through the runs' own headers,
        28 of whose 256 bytes are used, so the registry holds no memory and
        frees none. That goes through this stage's pre-change Sage gate.
      correction 2026-09-02 — slice (e)'s claim that `large_entity::runs`
        "sits on the mutator's OS-direct entity alloc and free path, outside
        the collection paths the deny gate covers" is false of the `remove`
        at `large_entity.rs:165`. The audit of its readers held; the writer
        on the free path was not checked against the replay.
      miri 2026-09-03 — the run this slice owes, at two threads:
        `memory::large_entity` 7 passed with 1 ignored, `cycle::`
        `deferred_slot_reuse` 16 passed,
        `promote::tests::the_reset_reads_no_zero_count_member` 9 passed and
        `promote::tests::the_memory_a_survivor_takes_with_it` 13 passed, 0
        failed anywhere — 368 s, 419 s, 295 s and 421 s of wall. The
        ignored one is the twelve-run probe test: under Miri
        `os::map_aligned` keeps a table of whole mappings, so mapping a run
        allocates on the probe's own counter. No baseline was taken for
        `promote::`, which had no recorded figure: the Sage allowed either
        a baseline run or a limited claim, and a clean run has nothing to
        attribute, so the baseline is owed only if a later run reddens.
      Critic 2026-09-03 round 1 (slice f): seven findings, all taken. Two
        were defects rather than wording. `take_heap_deallocations` was
        documented as "what a path gave back" while it counts `dealloc`
        calls, so a shrinking reallocation returns memory the counter does
        not see; the doc now says what it counts and a test pins the
        `realloc` arm. And `unlink` read a null `prev` as "this run is the
        head" while `commission` gives every block one, so an unlink of
        something never linked would have emptied the whole registry —
        under the ordered set the same mistaken call removed nothing. A
        `debug_assert!` restores the difference. Also taken: the `Send`
        justification argued pointee validity instead of the mutex
        discipline it exists for, two comments forty lines apart stated
        opposite facts about a run's link words, and the rfc still
        specified the ordered set.
      Critic 2026-09-03 round 2 (slice f): nine findings against round 1's
        repairs, all taken. One was a defect: `link`'s new assertion pinned
        the harmless half of its own contract, and a second link of the run
        already at the head passes a null-`prev` test and writes a
        self-loop, leaving `snapshot` walking forever under the
        process-global mutex with no fault and no output. The
        non-membership assertion is now the first of the two. The rest were
        sentences round 1 wrote that the code does not support: the
        poisoning paragraph denied an allocation `snapshot` makes and named
        `ll_free` as the abort site where the C-ABI frame above it is one;
        five citations named `retained.rs`'s index, lock and snapshot,
        which slice (e) deleted the day before; the rfc's rewritten
        invariant claimed the list holds every live mapping, which the
        window between `map_aligned` and `link` breaks; and its new "Ruled
        out" paragraph asserted the allocator-free collection clause the
        audit of 2026-09-02 records as unmet. Two rounds, and the device is
        dropped here.
      audit 2026-09-03 — the composite source audit re-run over `cycle`,
        `weak`, `gc_metadata`, `retained`, `large_entity`, `cells` and
        `reset_window`, after slice (f) closed the first of the two live
        sites. **The clause is still not met**, and every remaining site is
        in `reset_window`. `large_entity` is clean: the run registry
        allocates and frees nothing, and `snapshot`'s `Vec` has no
        production caller — `heap::for_each_entity_slot` reaches it, and
        every caller of that enumerator is `cfg(test)`, `cells::heap_census`
        and `heap::describe_slot` among them. Where 2026-09-02 named two
        sites in `reset_window`, this reading finds four allocator touches
        inside the teardown frames a collection enters: `park_large`'s
        `parked_large.push` (`reset_window.rs:321`), `record_death`'s insert
        into the boxed `died_set` (`reset_window.rs:220`), the
        `escrow.extend(edges)` two lines below it, and a nested close's
        `parked_large.extend` (`reset_window.rs:143`). A fifth frees rather
        than allocates and is invisible to a deny run for that reason,
        `snapshots.remove` (`reset_window.rs:225`) — the case the
        `large_entity::remove` finding reserved for this audit. All five
        turn on the one reading the 2026-09-02 ruling defers to the deny run
        at S36.7, and the ruling covers them unchanged: it asks whether the
        disclosed reset exemption reaches the frames a collection enters.
        Re-read and clean: the `thread_local!`s of `cycle`, `weak` and the
        queue are `const` `Cell`s that locate manager memory and own no
        backing, and the three `.extend(` sites in `cycle` are
        `RecordChain`'s over blocks the manager issued. The two exemptions
        stand unchanged — `validation::member_counts_cover_internal_edges`'s
        in-degree `vec!` under `debug_assertions`, and `cells::sever_cells`'s
        `&mut Vec<*mut RcHeader>`, dead code whose replacement S36.5 owes.
      audit 2026-09-06 — the composite source audit re-run, because S43 and
        S44 rewrote the modules it covers, `deferred_slot_reuse` and the record
        chain among them. Subject: `cycle` whole, `weak` and its table,
        `cells`, `object`'s dispose path, `gc_metadata`, `retained`,
        `large_entity`, `critical`, `block_pool`, `heap`, `arena`, `stdapi`,
        `reset_window`, and `promote` for reachability alone. **The clause is
        still not met**, and the five sites that break it are the ones
        2026-09-03 named, at today's lines: `park_large`'s
        `parked_large.push` (`reset_window.rs:326`), `record_death`'s insert
        into the boxed `died_set` (`:224`) and the `escrow.extend(edges)` below
        it (`:231`), a nested close's `parked_large.extend` (`:143`), and
        `snapshots.remove` (`:230`), which frees rather than allocates. The
        ruling of 2026-09-02 covers all five unchanged.
        `cycle` came through the rewrite clean: `validation`'s in-degree `vec!`
        under `debug_assertions`, whose `validate_component` has no non-test
        caller until S36.3, and `queue::collect_lane_tokens` under `cfg(test)`
        are its whole population, and the arena, the trace stack and the record
        chain extend over manager blocks. Clean too, and named so the next
        audit need not re-read them: `weak` and its table, `cells`'s live
        tracer, `gc_metadata`, `retained`, `critical`, `block_pool`, `arena`
        and `stdapi`; `heap`'s only production global-allocator traffic is
        `ll_thread_init`'s `alloc` and `ll_thread_exit`'s `Box::from_raw`, and
        no collection frame reaches either. Three `reset_window` sites the
        earlier reading did not name stand on the reset's own frames rather
        than on a death path, so a collection does not enter them: `opened`'s
        `Box::new` (`:105`), `corrections`' two clones (`:309`, `:310`), and
        `snapshot_edge` with `credit` (`:195`, `:209`).
      correction 2026-09-06 — "`snapshot()`'s `Vec` has no production caller"
        claims more than the code supports. `large_entity::snapshot` carries no
        `cfg(test)`, and `heap::for_each_entity_slot` is `pub` and ungated, so
        the `Vec` is compiled into a release build and callable from outside
        the crate. What holds is the narrower reading: every in-crate caller of
        that enumerator is test-gated, `cells::heap_census` among them, so no
        collection frame reaches it. The retraction of 2026-09-02 drew the same
        distinction over the same function.
      audit 2026-09-06, beside the path — `static_block::tear_down`
        (`static_block.rs:169`) owns a `Vec<*mut RcHeader>` and fills it
        through `object::sever_counted_slots`, in production, on the
        thread-exit release of static-block roots. No collection frame reaches
        it, so it is not a site of this clause. It is recorded because it is
        the live instance of the `&mut Vec<*mut RcHeader>` signature
        `cells::sever_cells` carries as dead code: the manager-backed
        replacement S36.5 owes has a caller already, and S39.1 puts a
        collection on that same thread-exit path.
      handoff: the step closes on three commits rather than one — `88ad136`
        built it, `ea2a941` carries round 1's repairs and `01d50cb` round 2's,
        and the last is what the built form is.
      handoff: S36.9 is executed as separately reviewed slices: (a) physical
        block contract and queue state; (b) logical ledger and current arena
        instrumentation; (c) manager-backed parking plus ordinary/abort deny
        gate, done 2026-09-01; (d) weak-table ownership and streaming arena
        drain; (e) retained
        index/registry/snapshot ownership plus the direct-large registry audit,
        done 2026-09-02.
        Only their composite source audit and deny test close this checkbox.
      Sage 2026-09-07 (the closing run's gate): the run is one module of five
        cases under `cycle::collect::tests`, each driving a production entry and
        each proving it collected by an observation the answer alone cannot
        make; the exempt figure is read off `premise_cell_walks` rather than
        written down, and a `cfg(test)` door in `validation`, a second exempt
        counter and a release-only case are refused with reasons
        (`dev/DECISIONS.md`, "a deny case subtracts the debug checks by reading
        them, not by writing the figure down"); frees are asserted beside
        allocations, the audit having reserved the sites that give memory back;
        the calibration of the exempt figure is taken before the first edit of
        the run, the instrument-first rule of slice (f); and no operation count,
        `.tbss` figure or timed run is owed, the step adding no production code.
        Taken whole. Where the run departs: the Sage expected the ordinary arm's
        three member returns to be popped by the close, and they are not — a
        member is a registered candidate, and `ll_free`'s candidate arm answers
        ahead of the trace window's, so exactly one return is withheld and it is
        the child's, which takes its holder's edge as its creation reference and
        never reaches the candidate gate. The child's run is unlinked at the
        close as the Sage said.
      progress 2026-09-07 — S36.9g the composite deny run:
        `cycle::collect::tests::what_a_collection_asks_the_allocator`, five
        cases over the two production entries. Ordinary and parking are one
        case, the ordinary path's close being the parking: a three-ring with an
        OS-direct child whose free is withheld by the trace window alone and
        replayed at the close, where `large_entity::free` unlinks the run and
        returns the mapping. Neither counter sees that return, an unmap being no
        global free, so the registry is what the case asks before and after —
        which is also what the audit's reserved free-shaped sites need, none of
        them being reachable from these five. Weak
        nulls a cell a destructor then reads. Retained collects two survivors a
        reset promoted, `resolve_edge_target` answering `Population::Retained`.
        Pressure drives `collect_under_pressure`, whose close gives the blocks
        back before the teardown. The abort refuses a growth mid-trace — both
        allocation paths closed, the pool by this thread's budget and the
        reserve by draining it — over a ring sized from `WORKSPACE_BUMP_BYTES`
        and `shadow::bytes_for` so that the row arrays pass what the workspace
        holds; it answers zero, gives back every block, and the same entry over
        the same graph collects the ring once the budget is lifted. Measured:
        every arm draws the exempt figure and no more, frees exactly it, and the
        abort arm draws `(0, 1)` — one refused pool request and no heap
        allocation. Three mutations were seen red and restored:
        `record_death`'s null-window return deleted reddens three of the four
        collecting arms; the retained arm passes, alone as well as beside the
        others, and **why it does is not established** — the reading that the
        boxed `died_set` was already drawn does not survive `close_and_flush`,
        which frees it at every outermost close, so the arm either does not
        reach `record_death` or reaches it at no cost, and which of the two is
        open; a
        `Vec::push` in `dispose_withheld` reddens the arm with a withheld
        return, and a fourth allocation in `member_counts_cover_internal_edges`
        reddens every validating arm and the calibration case.
      repair 2026-09-07 — the run's first reading was fourteen where the
        exemption explains six, and both halves of the excess were the
        instrument's rather than the collection's. Eight came from
        `cycle::row::stands_where_a_block_can`, a `cfg(test)` assertion on every
        edge dispatch that called `large_entity::snapshot` to ask a membership
        question; it asks `holds_run` now, which builds nothing
        (`dev/DECISIONS.md`, "the trace's edge-target assertion asks the run
        registry for membership rather than a list"). The remaining six are the
        exemption itself, whose text named one site while the code has three —
        a text error rather than an allocation, and the three are production
        source under `debug_assertions` rather than `cfg(test)`
        (`dev/DECISIONS.md`, "a deny case subtracts the debug checks by reading
        them, not by writing the figure down").
      gaps 2026-09-07 — what the five cases do not enter, named by the Critic
        round so a later case is written for them rather than assumed covered:
        `ValidationResult::ZeroCountMember` and `ExternallyReferenced`, and with
        them the maturation stamp — every arm's component is confirmed on both
        readings, and the zero-count arm costs one exempt allocation rather than
        three and records no walk, so a case that reaches it owes a calibration
        of its own; a destructor that resurrects a member; the sever's displaced
        children, every child of these five graphs being a member, so the queue
        the pressure path's second arena exists for stays empty; the pressure
        path's overflow and its two arming endings, which
        `a_population_past_the_harvest_region_is_collected_over_several_traces`
        drives without a probe; a destructor that allocates, and the
        `ll_arena_reset` the open ruling turns on; an entity with array cells,
        which is where `array::entity`'s debug `HashSet` stands; and a
        cross-thread free out of a collection. Two limits of the instrument
        beside them: the probe's counters are per thread, so work moved to a
        helper thread would be off the counter, and a release test build asserts
        `(0, 0)`, which a dead counter also satisfies — the debug build's
        positive figure is what proves the counter live, and the calibration
        case the exempt figure rests on asserts `(0, 0)` in a release build too,
        so nothing pins the instrument there. And the cases keep their memory:
        each takes a width nothing else builds, so the blocks their withheld
        slots hold are never adopted either — four blocks at the wide classes,
        eight at the abort case's, and one retained block with two occupants,
        out of circulation for the life of the process.
      audit 2026-09-07 — the composite source audit re-run over `cycle` whole,
        `weak` and its table, `cells`, `object`'s dispose path, `gc_metadata`,
        `retained`, `large_entity`, `critical`, `block_pool`, `heap`, `arena`,
        `stdapi`, `reset_window` and `promote` for reachability, and `array`
        for the one site the reachability walk found there. **The collection's
        own frames hold no owning container**: none exists in the non-test
        source of any of the modules above outside `reset_window` and the two
        exemptions below, and the
        `.extend`/`.push` sites of `cycle` are `RecordChain` and `LazyChain`
        over manager regions. What stands between the clause and met is one
        reading and two exemptions. The reading: since S36.15
        `CollectingThread::take` refuses while `reset_window::is_open()`, and
        the only opener is `promote::arena_reset_full`, so a collection's own
        frames never run under a window — the sites of `reset_window` fire only
        when a step-4 destructor calls `ll_arena_reset`
        (`memory/context.rs:126`), and the deaths that reach them there are the
        reset's drain's rather than the collection's reclamation. Whether the
        disclosed `promote` exemption covers those frames is the ruling of
        2026-09-02, still Edmond's. The site list is **six** rather than five:
        `died_set`'s `Box::into_raw(Box::new(HashSet::new()))`
        (`reset_window.rs:175`) stands on the same death path as the insert at
        `:224` and was named by neither earlier audit. The second exemption is
        new and is the instrument's: `array::entity::separate` keeps an
        `entered: HashSet` under `debug_assertions` (`array/entity.rs:506`),
        reached from a destructor's store through `barrier` and
        `object::escape_copy`, so a deny case whose destructor writes an array
        into a longer-lived slot reads a figure this run does not subtract —
        none of the five does, and the hole is in the instrument rather than in
        the code.
      audit 2026-09-07, beside the path — two further C-ABI entries a step-4
        destructor reaches that allocate globally, of the same class as
        `ll_arena_reset`: `static_block::ll_static_block_register`
        (`static_block.rs:96`, `:107`, `:111`) and the lazy `BufferArena` of
        `buffer_arena::with_buffer_arena` (`:818`), once per thread on the first
        long-lived payload, which `weak::table::draw` can reach. The free side
        of the buffer arena touches neither, so a teardown cannot trip it.
      done: every block owned by the candidate queue or a collection is drawn
        through one memory-manager wrapper, carries
        `BLOCK_KIND_GC_METADATA` while held, and is counted once — the kind is
        the whole answer to whose memory a block is, and collection is not
        split by use (Edmond, 2026-09-01, `dev/DECISIONS.md`); pool and
        critical-reserve handoffs restamp both directions, physical
        current/peak blocks and bytes are observable without double counting,
        and thread exit returns the direct count to zero. Beside it one pair
        of logical figures — bytes in use inside those blocks, current and
        high-water — which is what says how much of a reserved block is
        working memory. Which structure holds them is not carried in a
        production build (Edmond, 2026-09-01); the per-structure split is a
        build-time feature of its own, designed with
        `dev/design/debug-modes.md` axis A and owned by the backlog line
        below
      done: the production collection path contains no allocator-owning Rust
        container or hidden global allocation. The source audit covers
        `cycle`, its parking and deferred-drop storage, the weak registry and
        disposal path S36.3 reaches, and every registry the collector proposes
        to retain or cache; a collection run under a denying/counting global
        allocator covers ordinary, retained, weak, parking and abort paths and
        performs zero global allocations. A source/ownership audit catches
        backing allocated before the denying window opened
      tier: T2 · role: Critic
      handoff: this supersedes S36.2's acceptance of `Box<Vec>` parking. A
        manager-issued block stamped merely `ARENA` is not enough: the manager
        must be able to answer how many bytes GC owns.
        Control headers live in the manager block's header/payload; TLS holds
        only the non-owning pointer that finds the owner state. The present
        queue `Cell`s therefore move out of TLS into its floor header, and the
        queue capacity, poll stride and between-polls guarantee are re-derived
        and statically checked against the resulting layout.
      note 2026-09-01 — slice e is worked out in full before its code:
        `dev/CYCLE-COLLECTOR-REVIEW.md` finding 3 records that every production
        reader of the retained registry asks about one block whose address it
        already holds, and that its one enumeration has no production caller;
        `dev/design/retained-index-ownership.md` moves the index into the
        retained block's own collector line with the array in a per-thread
        chain of manager blocks, and names four questions for `rfc`. That is a
        design change rather than a backing move, so the fork below stands
        before the slice.
      handoff: the ownership audit must settle the retained registry before a
        cache is built. Its present `BTreeMap`, `Arc<[usize]>` and snapshot
        `Vec` may not be smuggled into the collection under the claim that an
        `Arc` clone itself allocates nothing. Either their backing moves under
        the manager or the registry is redesigned at its owning layer; there
        is no cycle-path exemption.
        The sites the composite audit exempts are `cycle::validation`'s two
        `debug_assert!`s, which allocate **three** `Vec`s per
        `validate_component` — the sorted list `members_in_address_order`
        builds for each of the two checks, and the in-degree array of
        `member_counts_cover_internal_edges` between them. They run under
        `debug_assertions` alone, on the owning thread, and each is freed
        before the call returns (S42.1, 2026-09-01, which named one of the
        three; the S42 Code Reviewer asked that the exemption stand here
        rather than in the closed stage). The figure is
        `validation::EXEMPT_ALLOCATIONS_PER_VALIDATION` and the deny run
        subtracts it per validation rather than asserting a bare zero
        (`dev/DECISIONS.md`, "a deny case subtracts the debug checks by reading
        them, not by writing the figure down").
      note 2026-09-12 — S36.17 took `reset_window`'s six sites out: the
        window holds no container and its own guard reads none, so the
        composite audit's "outside `reset_window` and the two exemptions"
        reads "outside the two exemptions" for the collection's frames. What
        still allocates in a frame a collection can reach is `promote`'s own
        twelve sites, entered when a destructor inside a collection resets
        an arena — S47's debt by Edmond's ruling of the same day — so the
        deny run over that shape is not green until S47, and this step waits
        on it rather than exempting it.
- [x] S36.14 Decide the retained index's owning layer   *(before S36.9's slice e)*
      done: the choice is recorded in `dev/DECISIONS.md` with the rejected side
        and its reason — either the present registry keeps its shape and only
        its backing moves under the manager, or the index moves into the
        retained block and the registry goes; and if the second wins, the four
        open questions of `dev/design/retained-index-ownership.md` are answered
        in `rfc` before any code, which is what makes them answerable at all:
        an owner word in the block, the index chain at thread exit, when a
        chain block is released, and what `for_each_entity_slot` may read
      tier: T2 · role: Sage
      note: the proposal's own working already refuses a block per index and
        an index beside its retained block, with reasons; what it does not
        settle is the block-owner word, which is the same prerequisite the
        collector worker waits on (`rfc/dev/ALGORITHM-AUDIT.md`, A4)
      Sage 2026-09-01: option B in a narrower form — the list into a
        per-thread chain of fresh pool blocks, the count word atomic because
        `ll_free` is ABI, the chain outside the ledger; A refused because
        stable Rust gives the containers no allocator parameter.
      Edmond 2026-09-01: the registry was a leftover of `rc-walk`'s census
        and the list belongs to the arena that produced it — the retained
        block's tail, else the reset's current block, else a fresh block.
        Final; the atomic count word stands as disclosed.
      handoff: `dev/DECISIONS.md`, "a retained block's survivor list lives in
        the arena's own memory". Slice (e) waits on the `rfc` entry the
        decision lists the questions for. Structure agreed with Edmond on
        2026-09-01: no new document — one section of
        `rfc/model/gc/rc-cycle.md` beside "Where the shadow count lives",
        four paragraphs: the retained block's header words and who publishes
        them; the block on no thread's list, neither abandoned nor adopted,
        its last death returning it from any thread; the list-holding block
        returned when its last list and last occupant are gone; the quiescent
        enumerator reading the list without a lock. Written: `rfc`
        `0f638f4`, "The survivor list of a retained block", after the
        consolidation reader's six findings were taken — a list standing in
        another block counts as a pin in that block's payload half, and the
        atomic count word is stated as independent of the disjointness
        premise of "Concurrency".

- [x] S36.10 The persistent per-owner workspace   *(before S36.3)*
      done: the first collection on a thread draws one 64 KiB workspace base
        through `gc_metadata::acquire` from the ordinary block pool, and the
        thread holds it from then until exit; a refusal is a collection that
        does not start, `None` at the open as today, and thread init draws
        nothing new. The base is rewound at every trace close and returned
        at thread exit, after the queue's blocks and before `critical::drain`.
        It is never drawn from the critical reserve; overflow asks the pool
        and then the reserve and returns after every close or abort; the base
        stands outside the arena's returnable block list
      done: the workspace is a typed `Idle → Trace → Idle` ownership
        transition with one representation of "a trace is open". Trace end
        sweeps every block shadow and replays the withheld returns, then
        rewinds; an abort keeps its own sweep and rewinds the same way.
        Nested use and a phase-invalid pointer fail in every build. The
        Commit phase — bytes the commit still names after the trace close —
        is S36.12's, taken when its commit unit is chosen
      tier: T2 · role: Sage → Critic
      Edmond 2026-09-01: a dedicated block for the algorithm is acceptable on
        one condition — every block is explicitly requested from the memory
        manager. Mandatory-at-init or first-collection was left to the
        architect: first collection, because the `rfc`'s one-mandatory-block
        contract then stands as written and a refused draw loses no guarantee
        a collection did not already lack (`dev/DECISIONS.md`, "the workspace
        base is drawn at the first collection"). The Commit phase moves to
        S36.12 on the same ruling: it had no consumer here (F6). Final.
      note 2026-09-01 — L2 read the step and the code and stopped before the
        first edit at Edmond's close; its settled design, untyped: the base
        pointer is `OwnerCycleState::_future_workspace` renamed
        `workspace_base`, no new thread-local; `TraceScratchArena::new()`
        becomes `open() -> Option<Self>` — null pointer → `gc_metadata::acquire`
        (pool only), refused → `None`; cursor at the payload start, `left =
        BLOCK_PAYLOAD`, no control line in the base, the base outside `blocks`
        and `from_reserve`, `reset` rewinds and returns overflow only;
        `ActiveTrace::open` draws the workspace before the chain head; drop
        order `mark_peak`, sweep, close window, replay, rewind; the base's
        consumption is a residue (`base_used` at the crossing) and `reset`
        does `mark_peak` then `discharge`; the return at exit goes inside
        `release_queue_base` after the segments and before the base block;
        `block_pool::test_guard` warms the base after `ll_thread_init` so the
        before/after block counts hold, a test-only upward edge for
        `ARCHITECTURE.md`. The red test: a spawned thread, `ll_thread_init`,
        open + one `ensure_row` + drop → pool requests (0, 2), again → (0, 1),
        `current_blocks` baseline +1 while the thread lives and baseline after
        exit — red on `ecc9379` because the second trace makes 2 requests.
        Edmond's condition asserted as: the base's kind is
        `BLOCK_KIND_GC_METADATA`, `current_blocks` counts it, and `blocks_out`'s
        delta equals the probe's request count. Assertions that move:
        `blocks_held` 2/1/4/0 one fewer where the first block was the base,
        the reserve's 8-then-refused becomes 9, the crossing's `in_use +
        BLOCK_PAYLOAD` becomes unchanged with the peak kept.
      Sage 2026-09-01 (pre-change gate, run by session L2, every cited line
        checked against the file): nine findings and three escalations, no
        code touched. Taken as rulings for the code — one representation of
        "a trace is open", the chain head pointer or a phase word but not
        both, since slice c retired `TRACE_ACTIVE` and an unwind out of the
        row sweep would leave the two disagreeing (F2); the rollback of a
        refused base ends with `critical::drain`, because `release_queue_base`
        alone parks the queue base in the refused thread's reserve and a test
        reading `blocks_out` after `join` cannot see it (F3); the base lives
        outside the arena's `blocks` list and outside `from_reserve`, or
        `reset`'s count-based return hands it to the reserve on an abort (F4);
        base consumption is a high-water residue entered through `mark_peak`
        at the rewind, never a current-figure charge (F5, the choice the
        Sage proposed); the abort path keeps its second shadow sweep (F9);
        the handoff's 262,144 is the empty-queue figure, not a maximum — a
        polled thread with one candidate holds five blocks (F7); and the step
        text's `floor`, `parking`, `active flag` are retired words (F8).
        Escalated to Edmond: **the design of record has no workspace** —
        `critical-reserve.md` "Allocation paths" says one mandatory block at
        init and a thread that cannot obtain it does not start,
        `rfc/dev/DECISIONS.md` calls the floor "the one stock" that is
        mandatory, and the only authority for a second mandatory block is
        this crate's `dev/DECISIONS.md` of 2026-08-26; the alternative that
        meets the 08-28 reasoning is a base drawn at the first collection
        and retained from then, `None` at the open as today (F1); and
        whether the Commit phase is built here with no consumer, S36.12 not
        having chosen its commit unit (F6). Baseline measured by L2 on
        `f895272`: `.tbss` 480 (1.96.0) / 472 (+1.94); init draws 13 blocks
        (base 1, barrier reserve 2, critical 8, spares 2) and the step adds
        one; every ledger relation the tests pin is listed in L2's report of
        2026-09-01 and reproduced. Code waits on the two escalations.
      Critic 2026-09-02 round 1: eight findings, none of them in the ledger
        arithmetic. The load-bearing one: an unwind out of the arena's reset
        inside its own `Drop` skipped the workspace return, so the cell stayed
        lent and `release_queue_base`'s assertion fired inside a thread exit
        that was already unwinding — one failing test became a process abort
        naming no test, shown here by a counter mutation that panics on the
        second reset, SIGABRT before the repair and one reported failure
        after. The workspace is a holder with a `Drop` of its own now, which
        is the shape `deferred_slot_reuse` already uses for its chain. Also
        taken: the test accessor masked the lent bit and so hid a bit standing
        over a null block; nothing asserted the rewind itself, one incidental
        block count being all that pinned it; `None` at the open also means a
        thread the runtime never registered while three comments said the pool
        refused; `docs/memory-manager.md` gained the retired word `door`, which
        no guard reads outside `src/`; and thirty copies of the opener's
        message moved into `cycle::testing::open_arena`.
      Critic 2026-09-02 round 1, three divergences it found that stand: the
        ledger rule departs from the Sage's F5, and the departure with its
        reason is `dev/DECISIONS.md`, "the workspace is charged when the bump
        leaves it and marked when the reset rewinds"; the rewind sits inside
        `reset` rather than after the replay, where the first `done:` clause
        puts it — the sweep still precedes it and the replay reads no arena
        memory, so the clause's order is a statement about this code rather
        than a constraint on it; and `Idle → Trace → Idle` is a holder with a
        destructor over a tagged cell rather than a typed state machine, which
        is the untyped form the note above settled on.
      Critic 2026-09-02 round 2: five findings, three of them in round one's
        own repairs. The abort round one recorded was explained wrongly: it is
        not a second panic inside an unwinding exit but `ll_thread_exit` being
        `extern "C"`, so any panic under it aborts whether or not anything is
        unwinding — shown by forgetting an arena and returning normally, which
        aborts all the same. The holder therefore converts one path, an unwind
        out of the reset, and the comment says so now instead of claiming the
        class. `return_workspace_base`'s assertion on a missing thread state
        could fire on exactly one sequence — `release_queue_base` had already
        taken the state out of the thread-local and then failed an assertion of
        its own — so it was a second panic on that path and nothing anywhere
        else; it returns instead, and the same misuse now reports one failing
        test where it aborted the binary. "Ends the process in every build" was
        false of an `assert_eq!` the test profile unwinds, which the
        `should_panic` case depends on. Two stale comments taken: the thread
        exit named one block held for a thread's life, and `cycle::stack`'s
        stale-segment warning described another thread's block where the
        quieter failure is now this thread's next collection.
      Critic 2026-09-02 round 2, what it cleared by running rather than by
        reading: the ledger reasoning of `dev/DECISIONS.md`, "the workspace is
        charged when the bump leaves it", which it tried to refute and could
        not — guarding the crossing charge fails exactly the one test the entry
        names; the unmasked accessor's three callers; the residue assertion and
        the shared opener; the `should_panic` case's state; and every citation.
      Critic 2026-09-02 round 2, one finding that is not a repair and waits on
        Edmond: **the first-collection draw loses a guarantee the record says it
        does not.** On `ea5e208` a collection could start with the pool
        refusing — the arena and the withheld-return chain both asked the pool
        and then the critical reserve, which is the reserve user
        `rfc/model/memory/critical-reserve.md`, "Collection working memory",
        exists for. A thread that has never collected now answers `None` under
        the same pressure, because the workspace has the ordinary path alone.
        Five untouched cases in `cycle::deferred_slot_reuse::tests` state the
        old claim in their prose and pass only because
        `block_pool::test_guard` draws the workspace ahead of them; with that
        warm removed, `the_critical_reserve_funds_a_window_the_pool_refuses`
        fails on its own message. No production caller exists until S36.7. The
        alternative that keeps both the ruling and the guarantee is an arena
        that opens without a workspace when the draw is refused and owns its
        blocks for that collection, as it did before this step.
      handoff: mandatory direct cycle memory becomes 131,072 bytes per
        registered thread — one 65,536-byte queue floor and one workspace
        base. The two best-effort queue spares make the nominal/maximum direct
        baseline 262,144 bytes when both are present. The separate critical
        reserve has capacity up to 524,288 bytes and is not guaranteed resident
        or workspace capacity. Initially exactly one workspace block is
        retained; retaining warm overflow waits on S40.3.
      handoff: a dense 381-entity shape needs 23,568 bytes (about 23.0 KiB)
        for the widest block's rows, one present-day stack segment and member
        pointers, so it fits the base by calculation; one entity in each of
        381 widest blocks reserves 6,251,448 row-array bytes, or 6,258,608
        bytes (about 5.97 MiB) with that stack and 381 member pointers. Neither
        number is a workload measurement.
      handoff: closed 2026-09-02, `ea7f5c1` and the three commits after it.
        A thread draws one 64 KiB workspace at its first collection through
        `gc_metadata::acquire` and holds it to exit; `TraceScratchArena::new`
        is `open() -> Option<Self>`, the bump rewinds over the workspace at
        every close, and `cycle::queue` lends it out of the word
        `OwnerCycleState` had reserved, tagging the cell's low bit while an
        arena holds it. Measured the same day: a second collection asks the
        pool once against twice, `.tbss` 480 on both arms
        (`dev/BENCHMARKS.md`).
      handoff: what the step leaves unconstrained, for whoever needs it. No
        test separates `mark_peak` from a `charge`/`discharge` pair — one
        thread cannot observe the difference, which is S36.9's recorded gap
        and now covers the reset too. Nothing pins the order in
        `ActiveTrace::open` that draws the workspace before the chain: both
        orders pass, and only the comment carries the reason. A lent cell that
        reaches thread exit any way but an unwind out of the reset still
        aborts the binary (`dev/POSTMORTEM.md`, "an assertion under
        `ll_thread_exit` aborts the binary and names no test").
- [x] S36.11 The managed lists and the small worklist   *(before S36.3)*
      done: one manager-backed segmented primitive serves collection-owned
        pointer records with explicit `read`/`used` bounds and no drop glue;
        the withheld returns, condemned members and S36.5's deferred drops use
        it. **The clause's second half — a small fixed worklist in the
        workspace, growing into managed segments only on overflow — is struck**
        by the note at the foot of this step
      done: a worklist entry carries the pair (entity, row pointer) rather
        than the entity alone, so the scan's pop reads the colour through the
        pointer instead of resolving the row a second time — the pointer and
        not the colour, because another path can recolour the row between push
        and pop (`dev/CYCLE-COLLECTOR-REVIEW.md`, finding 2). Mark reads no row
        at its pop and carries the pointer for one entry shape
      done: the Sage gate names the pre-reserved withheld-return capacity from
        the 65,280-byte payload before code begins; boundary tests exercise
        exactly capacity and capacity plus one, and the documented budget
        accounts for the other base-workspace residents
      done: the withheld returns take their base capacity from the workspace
        payload rather than from a block of their own, and the chain never
        writes a link into a corpse. What S36.9 slice c built and this step
        inherits rather than repeats: no `Box<Vec>`, an overflow that asks pool
        then critical, a documented hard failure rather than a lost physical
        return, and a replay through `stdapi::ll_free` after the row sweep
      tier: T2 · role: Sage → Critic
      Sage 2026-09-03 (pre-change gate, no code touched). Capacities, which
        the fourth clause owes this gate: the worklist takes 256 entries of
        16 bytes and the withheld returns 1,024 records of 8, each region
        behind a 64-byte line of its own, so the workspace's fixed prefix is
        at most 12,480 bytes and its bump region at least 52,800. The
        justification offered — that a prefix under 16,056 bytes costs
        nothing, three widest row arrays still fitting — was struck as true of
        class 16 alone: at class 32 the workspace holds six arrays where it
        held seven. What holds instead is a comparison against today, where
        every window's open draws a whole block: per-collection draws never
        rise, and fall by one for every trace whose rows and overflow segments
        fit 52,800 bytes. The primitive keeps a `cursor`/`limit` pair as the
        chain has today, a segment carries its own capacity in its header
        because base and overflow differ, LIFO pop and the replay's walk are
        two methods of one chain, records are `Copy` with no drop glue, and no
        `read` bound is persisted until S36.5 needs one. The worklist moves
        into `TraceScratchArena` rather than `ActiveTrace`, the arena already
        owning every byte it uses: `mark(arena, root)`, `scan(arena, root)`,
        and `TraceStack::reset` goes with the separation that made it
        necessary. The abort threshold is the gate's 1,024 rather than
        Edmond's: clause 5 as written already lowers it, 8,152 records not
        fitting the payload beside anything.
      Sage 2026-09-03, the findings the slices owe: the chain's drop releases
        every block from its head, which after slice (d) is the thread's
        workspace, and its replay reads every block but the append one as
        holding 8,152 records, so a chain that grew once replays 7,128 words
        of worklist and rows into `ll_free`; `ActiveTrace` declares `arena`
        before `returns`, so the workspace goes back to the thread before the
        replay reads the records inside it, and the field order reverses;
        `grow`'s crossing charge is `BLOCK_PAYLOAD - self.left`, which
        overstates by the prefix once the bump opens at 52,800;
        `a_window_neither_allocation_path_can_fund_does_not_open` and
        `the_critical_reserve_funds_a_window_the_pool_refuses` state a refusal
        model the open no longer has and are rewritten rather than weakened;
        `resolve_edge_target` has no counter, so the operation the second
        clause removes is instrumented first and its four calls per scan of a
        two-entity chain seen before the change (`shadow::WRITTEN_BYTES` is
        the pattern); `clear_touched_rows` is `pub(crate)` and must assert an
        empty worklist once the two share an owner; `parking` is a retired
        word and both surviving clauses use it; and no chain link is written
        into the workspace's block header, which `BlockPool::put` writes at
        thread exit.
      progress 2026-09-03 — the pair entry, `841d763` and `77bd48a`. The
        instrument first: `cycle::row::take_edge_dispatches` counts the calls
        into `resolve_edge_target` per thread under `cfg(test)`, on the
        pattern `shadow::WRITTEN_BYTES` set, and priced one scan of the
        two-member ring at seven dispatches. `WorklistEntry` is then the pair
        of an entity and the row its meeting found, sixteen bytes, with
        `SEGMENT_ENTRIES` at 256 so the entries still fill one page behind the
        segment's two links and `SEGMENT_BYTES` stays 4,112, now pinned by a
        compile-time assertion. The scan's loop head reads the colour through
        the carried pointer, the mark carries a pointer it does not read, and
        the seven fell to four — one per classification, none at a pop — seen
        red at 7 against the 4 written before the code changed
        (`dev/BENCHMARKS.md`, `dev/DECISIONS.md`). The stale 512 went from
        `dev/ARCHITECTURE.md`'s map with it. This does not close S36.11: the
        segmented primitive, the fixed worklist in the workspace and the
        withheld returns' base capacity remain.
      progress 2026-09-03 — slice (c), the segmented primitive and the fixed
        worklist region. `cycle::records::RecordChain` is a chain of segments
        over memory its owner supplies: it allocates nothing and answers a
        full append position instead, each segment carries its own capacity in
        a 64-byte header line, and the records are `Copy` with no drop glue.
        The worklist's first 256 entries are the workspace's own region, so
        the bump opens at 61,120 bytes of the 65,280-byte payload and a trace
        of that depth draws nothing — seen red on `4baee36` at one segment
        against zero. `TraceScratchArena` holds the worklist, `mark(arena,
        root)` and `scan(arena, root)` carry no second object, and
        `TraceStack::reset` is gone with the separation that needed it. Three
        of the findings the slices owe are paid here: `clear_touched_rows`
        asserts an empty worklist, the reset rewinds the worklist ahead of
        that sweep, and `grow`'s crossing charge measures against what the
        bump may grant rather than against the whole payload
        (`dev/DECISIONS.md`, `dev/BENCHMARKS.md`). This does not close S36.11:
        the withheld returns' base capacity in the workspace payload remains,
        with the findings that belong to it.
      progress 2026-09-03 — slice (d), the withheld returns' base in the
        workspace. The chain's first 1,024 records are the workspace's second
        fixed region, control line included, so the prefix is 12,480 bytes and
        the bump opens at 52,800 — the two figures the gate fixed, now pinned
        by compile-time assertions. `WithheldReturns::open` is infallible and
        `ActiveTrace::open` draws nothing: a thread's second collection asks
        the memory manager for nothing at all, where it asked once before
        (`cycle::arena::tests::the_workspace_a_thread_holds_for_its_life::`
        `a_second_collection_on_the_same_thread_draws_no_workspace`, (0,1) and
        (0,0) against (0,2) and (0,1)). The chain is a `RecordChain` over that
        region, so the replay reads each segment's own capacity and the drop
        releases only the segments past the base. The remaining findings are
        paid with it: the field order reverses so `returns` dies before the
        arena hands the workspace back, and the two refusal-model tests are
        rewritten — the refusal left is a thread's first collection, which is
        the only one that can be told there is no workspace. The withheld
        returns' region enters neither byte figure, being memory the thread
        holds between collections. Two clauses above lost the retired word
        `parking`; S38.3 and the backlog still carry it.
      progress 2026-09-03 — the Critic's two rounds, `d8d7c2c` and the commit
        after it. Round one: the sweep's guard fell from `assert!` to
        `debug_assert!`, so an S36.7 abort path that swept before it reset no
        longer ends a release process on the path this module documents as
        free; `defer_reuse_if_tracing` stopped dropping the answer of the push
        after a growth; the aborted-window case said "the abort gave the chain
        back" over a chain that had never drawn one; `dev/ARCHITECTURE.md`
        still described the chain as blocks for the length of a trace. Round
        two found three defects in those repairs: the new assert would unwind
        out of an `extern "C"` frame where the `abort()` beside it does not,
        the new case's final assertion held whether or not the grown segment
        was replayed, and `RecordChain::extend` stated a precondition no
        caller checked — `deferred_slot_reuse::grow` satisfies it only by
        never popping, and a list that pops would strand a segment and
        under-discharge its bytes. It also found that no case reached the
        reset with both lists occupied, which is the abort's own state and
        the one the rewind ordering exists for; there is one now, seen red
        against the swap. Both rounds are spent, and what the second one cost
        the first is `dev/POSTMORTEM.md`, "a stricter repair can be worse than
        the failure it replaces".
      progress 2026-09-03 — Miri over `cycle::` at the close of the block:
        123 passed, 0 failed, 7 ignored, 434.71 s on Miri's clock against
        17 m 00 s of wall, at two threads with `-Zmiri-ignore-leaks`. Clean,
        and it covers every case of `records`, `stack`, `arena` and
        `deferred_slot_reuse` this step wrote.
      miri 2026-09-03 — re-run at `fd1ecce`, which the figure above predates by
        two commits: the worklist region's revert and the root-count guard.
        126 passed, 0 failed, 7 ignored, 441.22 s on Miri's clock at two
        threads. The 126 and the 7 account for every `#[test]` under
        `src/cycle` at that commit, 133 of them; the run above accounts for 130
        of the 132 the tree carried at `0c64602`, and what the other two were
        is not established.
      note 2026-09-03 — **the two-cursor clause is struck**, by Edmond's word
        over the Sage gate's ruling. It read: the arena bumps row arrays from a
        block's front and worklist segments from its back, growing when the two
        cursors meet, so the 16,056-byte tail a fourth 16,408-byte array cannot
        use at the smallest size class is spent rather than abandoned. The
        mechanism is byte-identical to the arena it would replace.
        `TraceScratchArena::alloc` grows on `bytes > self.left`, and a
        two-cursor arena's free space `back - front` is that same number, so
        both forms draw a block at the same request and abandon the same tail —
        20,000 shuffled traces over all 32 size classes, equal block counts in
        every one. The waste the review measured is real, and two other places
        account for it: the base block's tail is spent by the residents the
        clauses above put in the workspace, and the drawn blocks' tail is
        counted by S40.3, whose `done:` already names abandoned-tail bytes, and
        decided by S40.2, which may replace the flat row array that creates the
        tail. The alternative that does spend it — keeping the tail a growth
        abandons and serving later requests from it — was refused at its
        measured size: 0.2 % to 2.1 % of blocks drawn over all classes, and
        4.9 % on a heap of nothing but the smallest class
        (`dev/CYCLE-COLLECTOR-REVIEW.md`, finding 1).
      note 2026-09-03 — **the fixed worklist region is struck and reverted**,
        after the design review of the same day. It was justified against a
        baseline that does not exist: the segment it replaced came from the
        arena's bump, whose first block has been the workspace since S36.10, so
        both forms take the same bytes out of the same block. With `n` worklist
        segments the bump has `65,280 - n * 4,112` left for rows in the old
        form and `65,280 - n * 4,160` in the new, the region being worse by the
        48 bytes the segment's header grew and better in no case. The
        measurement that passed it read the bump during the trace and did not
        count the region's own bytes, taken at the arena's open. What survives
        the revert: `cycle::records::RecordChain` and the arena's ownership of
        the worklist. What may still be worth taking, once S40.3 has the
        worklist high-water, is a region *smaller* than a segment — at 64 entries it
        costs the bump 1,088 bytes and leaves 64,192 for rows against the
        61,168 a segment leaves (`dev/DECISIONS.md`, "the worklist's fixed
        region is retracted").
      handoff: Red tests show a small trace makes no manager overflow draw, two
        collections reuse the same base, corpse bytes remain intact, critical
        capacity is restored, and success and abort both return GC bytes to the
        per-thread baseline.
- [x] S36.12 The in-flight batch and condemned membership   *(before S36.3)*
      done: collection detaches the active candidate chain as one in-flight
        batch whose bounds travel with it, and all roots mark before any root
        scans. **The member list is the pressure path's alone** — the ordinary
        collection off the poll keeps its arena through the teardown and reads
        the rows directly, appending no record and allocating nothing for one.
        A collection an allocation failure started harvests its condemned
        entities in the sweep that nulls the blocks' shadow pointers, into a
        fixed region of the thread's workspace, and gives every block back
        before the first destructor runs; what does not fit the region keeps
        its candidate bit and is the next trace's, which under pressure
        follows immediately on the memory the teardown returned
        (`dev/DECISIONS.md`, "the member list is the pressure path's alone")
      done: refusal after detach or after any member append aborts the whole
        trace, sweeps its rows and restores every in-flight token to its source
        lane without allocation. No `CANDIDATE_BIT` bit is left without exactly one
        logical record and no record exists in two lanes
      tier: T2 · role: Sage → Critic
      Sage 2026-09-03 (pre-change gate): **the detach draws no segment**, which
        refuses the proposal put to the gate and Y12 clause 2's swap with it —
        recorded in `dev/DECISIONS.md`, "the detach of a candidate chain draws
        no segment", and carrying the amendment to `rfc` is that repository's
        work. The batch is a two-word move-only value owned by the collection
        frame, refused a home in `OwnerCycleState`'s reserved word and in the
        workspace: nothing outside that frame ever has to find it. Its bounds
        are the head and the head's fill alone, with no tail; S37.4's deferred
        lane keeps a fill bound of its own rather than adding one here. The overflow buffer is not part of
        the batch and the detach may not assert it empty, the pressure path
        sharing the code. The restore asserts an empty write position **in
        every build**, that assertion being the whole difference between "no
        user code runs between the detach and the restore" as an argument and
        as a check. And the step splits: slice (a) is the detach, the restore
        and the two-phase loop; slice (b) is the pressure path's harvest, whose
        region capacity waited on the withheld-return region's own fate and is
        answered by S44. The instrument owed
        before the first edit is a walk over every lane, `candidate_count`
        answering one of two and the clause being about entities rather than
        counts.
      progress 2026-09-03 — slice (a): `queue::detach_candidates` moves the
        chain's head and fill into an `InFlightBatch` and leaves the write
        position null; `restore_candidates` puts them back under the
        every-build assertion; `ActiveTrace` holds the batch and its drop
        restores one nothing disposed of, before the row sweep. `cycle::trace`
        runs both phases over the batch in one function, so "all roots mark
        before any root scans" holds by construction rather than by a caller
        remembering it — its test shows the interleaved order reading a ring as
        held from outside, and the two-phase order reading the same ring
        unreachable. Measured on the day: the detach and the restore each make
        0 global allocations and 0 pool requests, move neither
        `gc_metadata::stats` figure and spend no spare cell. Ten tests, the
        walk over both lanes calibrated against `candidate_count` and
        `overflow_len` before the batch existed, and the two assertions —
        a restore over a lane that grew again, and a batch dropped instead of
        restored — in child processes. This does not close S36.12: slice (b)
        remains, and with it the first clause's harvest sentence and the
        member-append half of the refusal clause.
      Critic 2026-09-03 round 1 (slice a): eleven findings, ten taken. The
        load-bearing one is a trap the slice would have sprung on the first
        real collection: the restore lives in `ActiveTrace`'s drop, and the
        ordinary path keeps its rows through the teardown, so the severing that
        releases a condemned member's live children registers candidates into
        the write position the detach emptied — and the restore's own assertion
        would then end the process. `ActiveTrace::take_batch` is what a
        disposition uses instead, and the ordering it owes is stated at both
        ends and named against S36.7. Also taken: the restore emptied the batch
        before it asserted, so a refusal stranded the chain in a local, and the
        batch's drop was blind to exactly that path; `scan`'s safety contract
        said "a live entity header" while the batch offers roots that were torn
        down, which is the case `mark` admits by rule; `walk_chain` claimed to
        be the only place that knows how a chain is bounded, where
        `release_queue_segments` and `append_with_new_segment` rest on the same
        rule, and `candidate_count` now goes through the walk so the count is
        one place fewer; the walk's order was documented as newest first, where
        it is the newest segment first and the oldest entry inside each; the
        ordering test's control arm ran an order the batch does not produce,
        and under the real order the interleaving is harmless, so the fixture
        registers the other way round and the batch's order is pinned; the
        tests never read a candidate bit; and the citation for the ordering
        rule named an `rfc` section that states the trial-deletion arithmetic
        and not the ordering, in `scan.rs` as well, both now stating the
        derivation instead. Refused — that the probe cannot see a pool request:
        `allocation_probe::take_allocations` answers heap allocations and pool
        requests as a pair, and the assertion is against `(0, 0)`. Two source
        mutations were run and each was caught by the test that owns it: the
        two phases interleaved per root, and the restore's assertion deleted.
      Critic 2026-09-03 round 2 (slice a): twelve findings, all taken, and four
        of them defects round 1's own repairs introduced. `take_batch` was the
        worst: it hands a disposition a batch with no terminal operation — the
        restore refuses a lane a destructor refilled and the drop refuses a
        chain — so it moved the process kill rather than closing it, and it
        broke the paragraph that argued a thread cannot exit with a batch out.
        It is withdrawn, and what S36.7 owes is the pair, taking the batch and
        giving its segments back. Round 1's reordered assertion was
        observationally a no-op, the batch being taken by value and dropped by
        the same failing frame either way, and the `panicking()` guard it came
        with went on the wrong drop: the assertion that fires from drop glue is
        the restore's, so that is where the yield is now, and what the guard
        costs — an unreported dropped batch during an unwind — is stated in
        `dev/DECISIONS.md` rather than left implied. `candidate_count` had been
        folded onto `walk_chain`, which made the walk's calibration a tautology;
        it goes back to its own arithmetic, and the comment names all five
        readers of the rule including `write_segment_entry`, which round 1
        missed eleven lines from the bottom of the file it was editing. The
        trace fixture freed neither entity, `release_queue_segments` dropping
        records without clearing bits and `ll_free`'s candidate arm withholding
        both slots for the life of the process; it clears them as the
        `deferred_slot_reuse` suite does. The new candidate-bit assertions could
        not fail, so the lane now carries a second record whose bit is down and
        the pair constrains both directions. "Neither ledger figure moved" read
        the current figures alone; the peak is lowered first and the whole
        `stats()` compared. `scan`'s two inner contracts still said "a live
        entity header" under a widened outer one; `roots_of`'s doc still said
        "newest first"; and `ll_free`'s comment promised a check nobody wrote,
        which is the arm below it and now says so. The device stops at two
        rounds.
      miri 2026-09-03 — the run slice (a) owes, at `e63c235` and at two threads:
        `cycle::` 134 passed, 0 failed, 9 ignored, 457.95 s on Miri's clock. The
        two added ignores are the slice's child-process cases, which Miri's
        isolation forbids; 134 and 9 account for every `#[test]` under
        `src/cycle` at that commit.
      Sage 2026-09-06 (slice b gate): **a second fixed region of the
        workspace, behind the withheld returns' control line** — a 64-byte
        control line of its own and 1,024 eight-byte records, so the prefix is
        8,320 bytes and the bump 56,960, which is the layout the crate ran on
        from 2026-09-03 until S44.2 returned those bytes. What keeps the list
        readable from the last append to the last destructor is that nothing
        else writes the region: the arena's open sets its cursor past the
        prefix, its reset rewinds to it, and `return_workspace_base` hands the
        block to the thread's own cell rather than to the pool. The capacity is
        derived rather than round — the floor is two median closures of 381
        (S37's corpus figure), the ceiling is the three widest row arrays the
        bump must still hold (3 × 16,408 = 49,224 against 56,960), and 1,024 is
        the count whose records are exactly the bytes S44.2 gave back; S40.1's
        corpus arm revises it. The harvest walks the group bitmap and reads
        only the groups a trace met, reconstructing each address by population
        — the slot stride from the collector line's own `size_class`, the
        retained block's survivor list, the large entity's own block — and the
        ordinary path reads no row at all, which a `cfg(test)` count asserted
        at zero pins. **The region is all-or-nothing per trace**: an overflow
        zeroes the fill, answers `Overflowed`, tears nothing down and leaves
        every candidate bit standing, and the re-trace over fewer roots is
        S36.7's driver. Slice (b) may close on slice (a)'s fixtures, and may
        not stand on a teardown, a reclaimed count or the two-trace collection.
        Taken whole. Where it departs: no pre-change Miri run, `dev/WORKFLOW.md`
        putting Miri at the close of a logical block, and the figure the gate
        names — 134 passed at `e63c235` — predates S44.
      note 2026-09-06 — the gate reads "what does not fit the region keeps its
        candidate bit" as the trace rather than the entity. `dev/DECISIONS.md`,
        "the member list is the pressure path's alone", says the entities past
        the region stand in the queue for the next trace; the gate narrows that
        because the scan condemns a row only where no live referrer raised it,
        so every referrer of a condemned entity is itself condemned, and
        freeing a prefix of that set leaves the rest naming freed slots. Under
        the narrower reading nothing is torn down after an overflow, so every
        bit stands and the ruling's sentence holds of the whole batch. Edmond's
        to overturn.
      baseline 2026-09-06, read at `c2a3af0` before the first edit:
        `WORKSPACE_PREFIX_BYTES` 64 and `WORKSPACE_BUMP_BYTES` 65,216;
        `the_workspace_a_thread_holds_for_its_life` — a thread's first
        collection makes 0 heap allocations and 1 pool request, its second
        makes neither, and the one block it keeps goes back at thread exit; a
        block's first touch writes 121 bytes, 24 of prologue, 64 of bitmap and
        33 for one group, against the 16,320 its rows reserve;
        `touched_blocks()` 3 over a three-block chain; the suite 718 passed at
        one thread. `.tbss` is a control rather than evidence here, the region
        being heap memory the section cannot see, and a timed run is theatre
        while nothing collects.
      progress 2026-09-06 — slice (b): the sweep that nulls the blocks' shadow
        pointers harvests, into the workspace's second fixed region — a
        64-byte control line and 1,024 eight-byte records, prefix 8,320 and
        bump 56,960, the layout S44.2's bytes paid for. `cycle::members` owns
        the region and the one-list-per-thread rule, `shadow::for_each_unreachable`
        reads only the groups a trace met, and `row::entity_at` is the dispatch
        backwards over the three populations, on two new accessors —
        `heap::entity_slot_at` off the collector's own line and
        `retained::occupant_at`. An unarmed close reads no row, counted where
        the read is. Thirteen cases, and six source mutations each reddened
        the case that owns it: a kept partial list, a sweep that harvests
        unarmed, a slot address one stride off, a second arming granted, a
        sweep that gives up where the list does, and a walk that reads unmet
        groups. Verification on the day: 731 at one thread and three times at
        four, 731 `hash-folding`, 735 `debug-journal` three times, release and
        `cargo bench --no-run` clean, `cargo +1.94 fmt --check` clean,
        `citations.py` 443 with the same seven residues. Miri at two threads
        over the modules it touched: `cycle::members` 10 passed in 9.4 s of
        Miri's clock and 14 s of wall, `cycle::row` 7 in 337.8 s and 3 m 54 s,
        `cycle::arena` 29 in 185.8 s and 4 m 20 s, `cycle::shadow` 12 in 6.5 s,
        `cycle::deferred_slot_reuse` 42 in 31.5 s, 0 failed anywhere.
      Critic 2026-09-06 round 1 (slice b): nine findings, all taken, and the
        first is the defect the module's own doc denied. The sweep gated on
        `members::is_armed()`, which answers "a list stands on this thread" —
        and a list stands from the arming until the driver releases it, across
        the whole teardown — so a collection a destructor of that teardown
        started swept, read its own blocks' rows and appended them to the outer
        driver's list, which would then have torn down entities the nested
        collection had already freed. The flag moved onto the arena, which is
        per collection, and a case drives that sequence. Also taken: the
        touched list was emptied before a walk the harvest had made able to
        panic, so an unwind stranded the rest of the chain with their shadow
        pointers standing, and it is popped as the walk goes now; a row the
        dispatch cannot place gave a partial set with `overflowed()` false,
        where the closed-under-in-edges argument requires the whole harvest to
        be given up; `push` and `end_harvest` were safe functions guarded by a
        `debug_assert` alone and are `unsafe` with their precondition stated;
        thread exit did not refuse a standing list; the overflow fixture was
        one block, so the claim about what the sweep still owes past a refusal
        was made over the block that refused; the single-entity arm, the
        multi-group walk and the rounding bound had no case; the rows-read
        counter missed the sweep's second reading site; and a `const`
        assertion's comment did not describe what it checked. Repairs in
        `ea2a941`, with the gate re-run on the day: 737 at one thread and three
        times at four, 737 `hash-folding`, 741 `debug-journal` three times,
        release and bench builds clean, and Miri at two threads over
        `cycle::members` 12 passed, `cycle::shadow` 16, `cycle::arena` 29 and
        `cycle::deferred_slot_reuse` 42, 0 failed.
      Critic 2026-09-06 round 2 (slice b): nine findings against round 1's
        repairs, all taken, and two were defects the repairs themselves
        introduced. Round 1 popped each array off the touched list before
        reading it, so an unwind out of the harvest left that array's block
        stamped while the arrays behind it were swept — and the second sweep of
        the same close, finding the flag still raised, went on harvesting and
        ended with a list holding part of a set and `overflowed` false. The
        array leaves the list only after both halves are done now, and the
        harvest is a four-state value on the arena, so a sweep that meets
        `Running` knows it is the first one's unwind and gives the list up.
        `InjectedHarvestFailure` stages that, the state having no other way in:
        what raises it in production is a retained block whose survivor list no
        longer holds a row's position, and a debug build ends on `entity_at`'s
        assertion first. The second defect was mine, found while the round ran
        and reported by it too: a refused second arming assigned its own answer
        over the flag, disarming the collection that had armed. Also taken: the
        sweep asks the list as well as its own state, a driver that released
        early leaving a null control line; the overflow case refused on its
        last array, so the claim about work past a refusal is made at a
        capacity of zero now; the nested case asked for no harvest at all,
        where the sequence it names is a collection that asks and is refused;
        `rows_read`'s doc named one of its two sites and no case pinned the
        other, which the armed close now does at nine — eight rows of one met
        group and one large entity's own word; the rounding case wrote through
        `row` outside its stated bound; `entity_at`'s doc still said a caller
        passes an unplaceable row over; and one order claim stood in three
        cases. Repairs in the same commit as this entry, and the harvest before
        the store is forced rather than chosen: a large entity's row **is** the
        word the sweep nulls. Three mutations, each caught: a re-entered sweep
        that goes on harvesting, an array popped before it is handled — which
        aborts rather than fails, the stranded stamp reaching a `debug_assert`
        inside an `extern "C"` frame — and the harvest moved after the store.
        Gate on the day: 739 at one thread and three times at four, 739
        `hash-folding`, 743 `debug-journal` three times, release and bench
        builds clean, `fmt --check` clean, and Miri at two threads —
        `cycle::members` 14 passed in 110.8 s of Miri's clock, `cycle::arena`
        29 in 186.0 s, `cycle::shadow` 16 in 7.6 s, 0 failed.
      Known gaps of slice (b): the `None` arm of `harvest_rows` — a retained
        block whose survivor list no longer holds a row's position — is
        release-only, `entity_at`'s assertion ending a debug build on that
        path, so no case reaches the arm itself; what a case does reach, since
        round 2, is the state it produces, `InjectedHarvestFailure` staging a
        walk that does not finish. The retained population is not driven
        through the sweep, its round trip being pinned at the row level
        instead. And a driver that unwinds between the arming and the release
        leaves the region in use for the thread's life, which thread exit now
        refuses and S36.7's driver owns.
      handoff: choose and test the commit unit here. A single condemned batch
        is safe under the aggregate exact sum but resurrection in one connected
        part conservatively retains the others; if teardown promises
        per-component behaviour, this step must extract components instead of
        silently passing their union to `validation::validate_component`.
        `rfc/model/gc/rc-cycle.md`, "Cycle finalization and reclamation", puts
        the guard on every member of every confirmed component before any user
        code, so no member can start ordinary teardown mid-commit whichever
        unit is chosen.
      handoff: the region's capacity is unchosen, and **the budget it expected
        came back with S44.2**: the withheld returns' 8,320-byte region is gone,
        leaving a 64-byte prefix and 65,216 bytes of bump
        (`dev/DECISIONS.md`, "one stack through the dead entity holds every
        withheld return"). This slice waited on that ruling and no longer
        does. Nor does the density instrument supply the other input: a
        synthetic load's death count is the fixture's own and was refused as a
        measurement (`dev/DECISIONS.md`, "the death count of a synthetic load
        is a check and not a measurement"). What is left to choose on is the bump's own 65,216 bytes
        and the pressure path's need, and `cycle::density` counts no death that
        could narrow it — `heap::block_occupancy` reads `used`, which drops at
        a slot's physical return rather than at its teardown.
      handoff: closed 2026-09-06 by `88ad136`, after slice (a)'s `e63c235`. What
        S36.7 owes is named in its own `done:` and repeated here for the reader
        who arrives at the driver: arming the harvest after `TraceOutcome::Complete`
        and never before, taking the batch and giving its segments back as one
        pair, reading `members::take_standing` after the window has closed, and
        the trace over fewer roots that follows an overflow. The region holds
        one list per thread, so a collection a destructor starts finds it in use
        and harvests nothing.
      handoff: an enrolled death does not clear the bit or retire its record.
        Teardown finishes and physical return waits; only the consumer that
        still owns the record may observe count zero, clear `CANDIDATE_BIT`, return
        the slot and retire the token. The reverse order creates a dangling
        queue pointer that can name a new occupant.
- [x] S36.13 The retained-block visit   *(after S36.12, before S36.7)*
      done: closed 2026-09-02 by S36.9 slice (e) without code of its own —
        there is no registry to acquire once. The retained arm of
        `row::resolve_edge_target` reads two words of the block's own header
        and binary-searches the list, the block's index space is a third word
        read at its first touch, and no lock stands anywhere on the path.
        Nothing of the visit remains to build, and there is no handle to keep
        past the trace token.
      tier: T2 · role: Sage → Critic
      handoff: the counter this step asked its gate to record was never an
        instrument. The old cost, `2E + V + B + 2R` registry acquisitions per
        retained-only trace, was taken by reading the five call sites and
        stands in `dev/BENCHMARKS.md`, 2026-09-02.
- [x] S36.3 The guard and the weak window
      done: after the exact test confirms, every member takes the teardown
        guard, then every weak cell naming any member is nulled, all members
        before any destructor; a condemned ring A↔B with a weak cell on A, whose
        `__destruct` on B loads that cell, reads null inside the destructor —
        seen red against the per-member order before the component-wide call
        lands; a component the exact test refuses leaves every cell resolving
      tier: T2 · role: Critic
      Sage 2026-09-06, the stage's pre-code gate: the module is
        `cycle::finalization`, which is the glossary's own name for the
        protocol, and the guards and the invalidation run inside the call that
        validates, so a component the exact validation refuses is structurally
        unwritten; the batch-wide half of step 3 is carried by the type, `seal`
        being the only source of the value a destructor pass takes. Costed at
        three header accesses per member and about six for a weak-bearing one,
        a manager-allocation budget of zero — the debug premise check's `Vec` is
        the exact validation's and inherited — and no refusal of its own.
        **Ruled the `done:` clause "seen red against the per-member order"
        unmeetable in this step**, which builds no destructor loop, and moved
        it to S36.4; source mutations run by hand stand in its place here.
        Edmond's to overturn. Final.
      Critic 2026-09-06 round 1: twelve findings, all taken. An unsealed
        `Finalization` lost its guards silently, `#[must_use]` firing only on
        an unused expression — the value now carries a drop that refuses, in
        the shape `cycle::queue::InFlightBatch` already uses. The exact
        validation's answer could be discarded, so `ValidationResult` is
        `#[must_use]`. "Every member takes the guard" had no assertion at all,
        and a mutation guarding all but the last member passed the whole group;
        three cases now read every member's count. The rest were doc claims
        that did not hold: a standing invariant on `Invalidated` that user code
        can falsify, an order the type does not establish, a fit with the
        harvest that does not exist, step 5 cited for a refusal step 1 answers,
        and a fixture comment naming the counted release for the counter's
        twin.
      Critic 2026-09-06 round 2: fifteen findings, fourteen taken, three of
        them defects the first round's repairs introduced. `seal` moved the
        obligation to `Invalidated` and left it unguarded, so the leak stood
        one call further on — the sealed value carries the same drop now, and
        `guards_released` is what discharges it. The drop's account named the
        premise check, which raises before the first guard, where the assertion
        that raises after all of them is `weak::notify_death`'s. And
        `dev/ARCHITECTURE.md` asserted the order the module doc disclaims. Of
        the rest: "no other thread reading these headers" contradicted
        `refcount`'s own reason for a narrow store; the path off the poll
        derives no member list at all, which the fit section did not say;
        `dev/ALGORITHM-AUDIT.md` resolves in `rfc` and not here; "exact test"
        is a term the glossary retires, and the new files say "exact
        validation"; `dev/INDEX.md`'s production-caller column named a caller
        no production build runs; and the `cycle` row's module count was one
        short of the tree before this step and stayed one short after being
        incremented. Refused — a debug-only set of guarded members inside
        `Finalization`: the value holds a counter and no identity by design,
        and the disjointness of a partition belongs to the step that makes one.
      handoff: `src/cycle/finalization.rs` is `Finalization::begin`, `confirm`,
        `seal` and `Invalidated`, with the cases under
        `src/cycle/finalization/tests/`. `confirm` validates, guards and nulls
        as one act, so a refused component is left byte-identical; S36.4's
        destructor pass takes `Invalidated` by value, and `guards_released` is
        what S36.5 calls when the guards come off. Six source mutations were
        run against the repaired body and each was caught by the case that owns
        it; 746 tests at one thread and three times at four, 746
        `hash-folding`, 750 `debug-journal` three times, release, `cargo bench
        --no-run`, `cargo +1.94 fmt --check` and 451 citations with the same
        seven residues. Miri over `cycle::finalization` at two threads: 5
        passed, 0 failed, 2 ignored, 10.6 s of its own clock, the two ignored
        being the child-process cases.
      handoff: the guard is needed even single-threaded — a destructor releasing
        an internal edge would otherwise drop a member to zero and start
        ordinary death inside the teardown. The window is the one PEP 442 exists
        to close.
      handoff: what S36.7 inherits from this step, in one place: the commit
        unit, left open by S36.12's handoff and admitted either way by
        `confirm`'s signature; the mutable view of `members::StandingMembers` a
        sort needs, and what a sort costs the order that list documents; the
        path off the poll, which derives no member list at all and so has no
        caller for this signature; that one commit uses one finalization, which
        no type here holds; and the disjointness of whatever partition it
        makes, which `confirm` cannot check and states as a precondition.
- [x] S36.4 Destructors and the resurrection re-verify
      done: `__destruct` runs per object member on the owning thread; when any
        ran, the exact test runs again with the guard discount; a failure
        releases the guards through the counted path, so survivors keep true
        counts with their destructors already behind them, and the component is
        abandoned with its cells nulled; a destructor that stores `$this` into
        an external root proves both the acquittal and the nulled cell — the
        divergence from PHP that `weak-references.md` records
      tier: T2 · role: Critic
      note 2026-09-06 — this step owns the clause the Sage gate moved out of
        S36.3: the destructor loop is written per member first and seen red
        against S36.3's A↔B fixture, whose cell naming one member is loaded by
        the destructor of another, before the loop behind
        `cycle::finalization::Invalidated` lands. The pass takes that value by
        value, which is what keeps the invalidation ahead of every destructor
        of the finalization.
      correction 2026-09-06 — the `done:` clause read "with destructors still
        ahead of them", where the design retains a survivor "with their
        destructors already invoked" (`rfc/model/gc/rc-cycle.md`, "Cycle
        finalization and reclamation", step 5). The code follows the design —
        `DESTRUCTOR_RAN` stands on a survivor, so its later ordinary death runs
        no second body — and the clause is corrected to it. Raised by the
        Critic's first round, ruled by Edmond.
      Sage 2026-09-06, the stage's pre-code gate: **refused the batch-then-batch
        shape** — revalidate every component, then tear every one down — and
        ruled the revalidation of a component adjacent to its teardown. A
        destructor of component *j* may take a weak reference to a member of
        *j*, and the external children an earlier component's teardown drops run
        user code that can root what it resolves, so *j* would be severed under
        a root; the design's sequence is per component and only its steps 2, 3
        and 4 carry a batch-wide qualifier. The chain is `Invalidated →
        DestructorPass → Revalidation`, each close matching the members it was
        handed against the members the confirm guarded, and `GuardedComponent`
        borrowing the revalidation. The kind gate is
        `refcount::carries_a_class_word` rather than a test for the object kind,
        so the pass admits the kinds the death path dispatches to
        `ll_object_die`; the ordinary death of a member the release takes to
        zero is the revalidation's own, through the counted release rather than
        the counter's twin, its queue entry named as the price. Costed at one
        flags load per member, one indirect call per member that owes a
        destructor, one validation per component read again, and zero manager
        allocation. Baseline recorded: 746 tests, and S36.3's Miri count over
        the module. Final.
      Critic 2026-09-06 round 1: nine findings, all taken. Three were claims the
        code contradicts — the answer promised every member survives while the
        release frees one whose guard was its last reference; the module doc
        named the destructor pass as the only place user code runs, and an
        external child's `__destruct` runs inside the release; and the path a
        commit with no destructor takes returned without the sort its doc
        promised, which the sever's membership test reads by binary search. Two
        were obligations stated and unenforced: `guards_released` was safe while
        carrying the adjacency, and is `unsafe` now, and the skip's soundness
        was argued nowhere. The allocation account was S36.3's and stale — user
        code allocates without bound and the counted release can end the process
        through the overflow buffer. Two cases did not own their behaviour: the
        cross-component weak case passed with the invalidation moved into a
        per-component pass until the prober's component was confirmed first, and
        nothing read the queue entry the counted release exists to write. The
        rest were a dead parameter, a fixture reading its victim from a static,
        and a count of counters.
      Critic 2026-09-06 round 2: seven findings, all taken, and **both soundness
        arguments the first round asked for were false**. The second user-code
        site was said to be unable to reach a member of an unread component; it
        can, through anything step 4 published, and what keeps the component
        whole is the adjacency itself — the sentence as written licensed the
        loop shape the ruling forbids. The skip's warrant said no user code runs
        when no member owes a destructor; an external child's does and sets no
        flag, so an induction replaced it. Three surfaces still said the guards
        come off in S36.5 and nowhere else, which the release inside this module
        contradicts; the module doc still claimed the borrow orders the teardown
        where the type's own doc says it orders the readings alone; the two
        close assertions named a protection a member count cannot give — a
        component offered twice in place of another meets the sum; and the test
        module doc kept the sentence the first round removed from `run`. The
        ruling that reshaped the API had no case at all:
        `a_component_rooted_by_an_earlier_teardown_is_read_after_it` is it, and
        a mutation deferring the release out of `revalidate` is caught by it.
      handoff: `src/cycle/finalization.rs` gains `Invalidated::destructors`,
        `DestructorPass`, `Revalidation`, `Revalidated`, `GuardedComponent` and
        `release_guards`; the cases are `what_the_destructor_pass_runs.rs` and
        `what_the_revalidation_answers.rs` under
        `src/cycle/finalization/tests/`, and the three S36.3 cases drive the
        pass rather than `run_user_destructor` by hand. `Invalidated::`
        `guards_released` retired: the discharge is `GuardedComponent`'s, it is
        `unsafe`, and it is S36.5's entry.
      handoff: the red the plan asks for was taken as a source mutation of
        `confirm` — `weak::notify_members` moved into a per-member loop with the
        destructor — and `every_destructor_of_the_finalization_reads_null_`
        `through_the_other_s_cell` failed on a cell that still resolved. That
        case gives both members a cell and each destructor the other's, because
        under the forbidden order which member runs first is the sorted slice's
        answer. Twelve source mutations were run in all, that one among them,
        and each is caught by the case that owns it; the kind gate's ends the
        run rather than failing it, on a misaligned class pointer.
      handoff: the re-verify survives the shortlist framing rather than
        contradicting it. Garbage is monotone only while no reference to the
        component exists outside it, and the destructor runs holding `$this`, a
        reference the teardown itself handed to user code.
      handoff: 759 tests at one thread and three times at four, 759
        `hash-folding`, 763 `debug-journal` three times, `cargo build
        --release`, `cargo bench --no-run`, `cargo +1.94 fmt --check` clean and
        465 citations with the same seven residues. Miri over
        `cycle::finalization` at two threads: 14 passed, 0 failed, 6 ignored,
        18.1 s of its own clock against 30 s of the wall, the six ignored being
        the child-process cases.
      handoff: what S36.5 inherits. `GuardedComponent::guards_released` is its
        entry and states an ordering nothing can check: the sever and the free
        run before the next component is read. The sever's membership test
        reads the slice the revalidation sorted, on both of its paths. And a
        component read as externally referenced may leave a freed entity named
        in the caller's slice — a member whose guard was its last reference dies
        inside the release.
      handoff: untested, and its owner is S39.1, which waits on it: a destructor
        that throws. Release sets `panic = "abort"`, so there it ends the
        process; in a test build every value of the chain drops silently under
        the unwind and every guard it wrote is stranded.
- [x] S36.5 Sever, free and the deferred drops
      done: internal edges are severed with external children collected; the
        guards come off through the counted release and each member reaching
        zero dies through the ordinary death path into S36.2's parking; the
        deferred-drop queue — the severed external children — drains only after
        the last member's free, the order proven by a test-only sequence probe;
        a weak cell re-created on a condemned member is cleared by the free-time
        `HAS_WEAK_REFERENCES` notification
      tier: T2 · role: Code Reviewer
      correction 2026-09-07 — the `done:` clause named a second population for
        the queue, "the weak notify's displaced map values", and no producer of
        it exists: `rfc/model/weak-references.md` marks map subscription
        "(future)", and `weak::table::remove` answers one canonical cell and
        displaces no counted reference. Edmond ruled the map can wait, so the
        half is struck here and the work is a backlog item of its own — "A weak
        map, and the second kind of death subscriber", which carries the shape
        the mechanism has to take, the thread-local sink included.
      note 2026-09-06, superseded — it read the retirement of a candidate record
        as this step's, clearing `DEAD_IN_PLACE` before handing the slot back.
        The Critic's first round showed the clear is not available to a commit
        at all, and the teardown takes no such path: see the Critic record below
        and `dev/DECISIONS.md`, "the commit clears no candidate bit, and a
        member the queue names keeps its slot withheld".
      Sage 2026-09-07, the stage's pre-code gate: ruled the queue a
        `RecordChain<*mut RcHeader>` the collection arena owns — the record
        chain's second user, its segments the same bump's, emptied at the end of
        every component and drained oldest first; the refusal taken before the
        first cell is emptied, a refused component left whole with its guards
        off; the `Vec` sink replaced by `impl FnMut` at five signatures and
        `&mut dyn FnMut` at the `OutsideCells` function pointer; and the code in
        a module of its own, `cycle::reclamation`, entered through
        `GuardedComponent`. Two of its rulings were overturned by the Critic and
        both are recorded below. Baseline recorded: 759 tests, and S36.4's Miri
        figure over `cycle::finalization`.
      Critic 2026-09-07 round 1: nine findings, all taken. **The high one
        overturned the Sage's candidate-bit ruling**, which had the commit clear
        the bit ahead of each member's free: `mark::schedule_root_if_unvisited`
        reads a root's refcount out of the body and says it may only because the
        mutator does not free a slot an entry names, `ActiveTrace`'s close
        restores its batch rather than disposing of it, and an entry can be
        written into the live lane by a step-4 destructor and reach no
        disposition at all. All three were verified in the sources before the
        ruling was reversed. The rest: two cases of one file shared fixture
        statics and could overwrite each other at four threads; the
        outside-cells group had no stated obligation to sever what it walks; the
        refusal path's two docs promised a state the code does not leave, a
        member whose guard was its last reference being freed there; `attach`
        and a multi-segment drain had no case; and six comments the code
        contradicted, `release_queue_segments` as a lawful clearing among them.
      Critic 2026-09-07 round 2: eight findings, seven taken. **The high one
        overturned the Sage's costing of the reservation**: the bound read off
        the layouts is `2 × used` for a table, so a ring holding an array of a
        million integers asks for eight megabytes of records it would never
        write — and asks for them on the pressure path, where the refusal
        floats the very component whose release would relieve the pressure. The
        count is a walk now, exact, and the sever's own obligation is checked
        against it on both sides. The second high one: the candidate case's
        assertion passed under the defect it named, the free list being a stack
        whose head was the other member's slot; it takes two probes now. Also
        taken: the group contract stated one-sidedly, the sorted slice `reclaim`
        needs and the ordinary path may not have, the lane the drain refills
        against `restore_candidates`' every-build assertion, the cost of the
        candidate-bit overturn understated in `dev/DECISIONS.md` by a block per
        dead candidate and a lane that only grows, and three stale sentences.
        The eighth is a new `Fog` line, the reset window's absorb arm standing
        ahead of the candidate arm.
      Code Reviewer 2026-09-07, the step's declared role: six findings and four
        comment repairs in place, one of them a dangling fragment a doc rewrite
        had left in `refcount::clear_candidate_bit`. Two were taken as work.
        **The module had made `arena` depend on the teardown**, importing the
        queue type and its segment size out of `cycle::reclamation` while the
        teardown imported the arena; the worklist's own precedent is the answer,
        so the queue type moved to `cycle::drops` beside `cycle::stack` and the
        arena depends on a structure again rather than on a phase. And
        **`DeferredDrops` and `TraceStack` were two copies of one idea** — both
        an `Option<RecordChain<T>>` drawn at first use, with `new`, `is_empty`,
        `rewind` and `segment_count` identical: they are one type now,
        `records::LazyChain<T>`, and each user is a type alias over the verbs it
        reaches for. Also taken: `reserve_drops` read the chain's room once per
        drawn segment, which walks every kept segment, and reads it once now.
        Reported and not taken: the test fixtures of `cycle` copy a two-member
        ring in fourteen files rather than sharing one in `cycle::testing`, and
        `severed_edge_release` is a second name over one store.
      handoff: `src/cycle/reclamation.rs` is `reclaim(component, members, arena)
        -> Reclaimed` and `DeferredDrops`; the cases are four groups under
        `src/cycle/reclamation/tests/`; the queue itself is `cycle::drops`, a
        `records::LazyChain` the arena owns. `RecordChain` gains `room`,
        `attach` and `drain` with three cases of its own, and `LazyChain` is the
        lazily-drawn chain both it and the worklist are;
        `TraceScratchArena` gains `reserve_drops`, `push_drop`, `drain_drops`;
        `cells::
        sever_cells` and four signatures under it take a closure instead of a
        `Vec`; `refcount::severed_edge_release` is the narrow decrement an
        internal edge takes; `GuardedComponent::release` is the refusal path's
        discharge.
      handoff: 773 tests at one thread and three times at four, 773
        `hash-folding`, 777 `debug-journal` three times, `cargo build
        --release`, `cargo bench --no-run`, `cargo +1.94 fmt --check` clean,
        `cargo check --lib --tests` warning-free and 473 citations with the same
        seven residues. Miri at two threads: `cycle::reclamation` 11 passed in
        12.3 s of its own clock against 20 s of the wall, `cycle::records` 3 in
        5.4 s, `cycle::stack` 5 and `cycle::arena` 29 clean over the same
        change. Eight source mutations, each caught by the case that owns it:
        the child dropped where the sever meets it, the membership test skipped,
        the drain moved ahead of the frees, the candidate bit cleared at the
        commit, the reservation drawing nothing, the internal edge taking the
        counted release, the drain reversed, and the refusal discharging the
        component without releasing its guards.
      handoff: what S36.6 and S36.7 inherit. The drain's releases refill the
        thread's candidate lane, and `queue::restore_candidates` refuses in
        every build to restore a batch over a lane something has written since —
        so the driver owes the disposal the design names
        (`rfc/model/gc/rc-cycle.md`, "Concurrency"), and the built
        `ActiveTrace` close restores. `reclaim` reads a sorted member slice,
        which only the pressure path has. And a member the queue still names is
        freed into a withheld slot, so a commit reclaims the memory of every
        member no entry names and none of the rest until S39.1 retires the
        entries.
      handoff: untested, and no step owns it: the refusal path over a component
        holding a member whose guard is its last reference — that member is
        freed inside `GuardedComponent::release`, and the case that exercises
        the refusal has no such member.
- [x] S36.6 Commit writes the maturation stamp
      done: on the owning thread, after the exact validation, each proven-live
        component is stamped as a unit — current epoch and `min(age) + 1`
        saturated at 3, where a member whose stamp epoch is not the current one
        contributes age 0 — one single-byte relaxed store per member at header
        offset 6, never inside a wider access; a component read as unreachable
        is never stamped, nor is one the zero-count rule dropped; in the
        accelerator form the posted proven-live components are stamped by the
        owner at its drain, so the stamp byte has one writing thread in both
        forms; the epoch counter is one process-global full-width word of
        closed commits, advanced at the close of every commit, the epoch being
        its value past the turnover shift and the stamp carrying the low two
        bits; tests read the byte out of the headers — a live ring driven
        through four commits reads ages 1, 2, 3, 3, a ring one fresh member
        joined reads the minimum, a component a destructor resurrected is
        stamped at the second reading, a component read as unreachable keeps a
        zero byte by both roads to that answer, and past a turnover the next
        stamp carries the new epoch at age 1
      tier: T2 · role: —
      correction 2026-09-07 — the criterion ended "a test matures a live ring
        across two collections and shows the third pruning it, read off the
        S37.1 counter", which no work at this step could satisfy: the prune and
        the counter are S37.1's, and the crate had no epoch counter at all. The
        Sage ruled the split below and the clause was rewritten to the write
        half before anything was built to it. The pruning case moved to S37.1
        and was corrected on the way: with `k = 3` and an age of `min + 1` a
        component reaches age 3 at its third reading, so it is the fourth
        collection that prunes it rather than the third.
      Sage 2026-09-07, called by Edmond on the criterion: ruled the step the
        write half alone — `cycle::epoch` founds the counter, `cycle::finalization`
        writes the stamp at the two readings that prove a component live, and
        S37.1 keeps the descent's read, its counter and the pruning case. The
        write is testable on its own because the stamp is a header byte; the
        counter goes with the write because a stamp cannot be written without
        an epoch and its advance already has a site; and merging would put
        S37.1's change to `cycle::mark` under a step that carries no role
        (`dev/DECISIONS.md`, "the epoch counter is founded where the stamp is
        written").
      handoff: commit is the only writer because a mature stamp suppresses
        descent, which is a reduction of future suspicion and therefore the
        owner's by the law that only the owner reduces state
        (`rfc/model/gc/cycle/questions.md`, Y12 clause 4) — and because the mark writes into no
        entity, which is what makes an aborted collection free.
      handoff: `src/cycle/epoch.rs` is `current()`, `commit_closed()` and a
        `#[cfg(test)]` `pin`; `refcount::{read,write}_maturation_stamp` and
        `MaturationStamp` are the byte-wide pair at offset 6, the write a
        read-modify-write so the reserve at bits 20-23 stands;
        `finalization::stamp_component` is the private writer, reached from
        `Finalization::confirm` at step 2 and from `Revalidation::revalidate`
        at step 5, in the second case ahead of `release_guards` because a
        member whose guard was its last reference dies in that call. The cases
        are `cycle::finalization::tests::what_the_commit_stamps`,
        `refcount::tests::the_maturation_stamp_the_commit_writes` and
        `cycle::epoch::tests`; `dismantle_ring` in the finalization fixtures
        takes a ring of any size now.
      handoff: 785 tests at one thread and three times at four, 785
        `hash-folding`, 789 `debug-journal` three times, `cargo build
        --release`, `cargo bench --no-run`, `cargo +1.94 fmt --check` clean,
        `cargo check --lib --tests` warning-free and 478 citations with the
        same seven residues. Nine source mutations, each caught by the case
        that owns it: the stamp written on every refused answer, the component
        aged at its oldest member, a stale stamp keeping its age, the age
        unsaturated, a component read as unreachable stamped by either road,
        the close counting no commit, the stamp storing the whole byte, and the
        age read out of the epoch's bits. No Miri run: the change adds no
        pointer arithmetic and no `unsafe` past the two byte accessors, whose
        offset is the one `the_flags_half_the_mutator_leaves_alone` already
        writes by hand (`dev/WORKFLOW.md`, Miri).
      handoff: untested, and it is the ordering the whole step rests on: the
        stamp at step 5 is written before the guards come off. A stamp written
        after would land in a slot the allocator may have back, and no case can
        exhibit that — the byte reads the same either way and the free list
        keeps the memory mapped, so Miri answers nothing about it. What holds
        the order is the comment at the site.
      handoff: what S37.1 inherits. Its handoff names two producers of a stamp
        byte nobody wrote, and neither stands against the built form. A
        recycled slot is cleared by its next publication: `publish_header`
        writes all eight bytes, pinned by
        `a_published_entity_carries_no_stamp_of_its_own_slot`. A promoted
        survivor does keep its byte — `update_header_flags` sees bits 0-15 and
        `flags_store` writes two bytes at offset 4 — but an arena entity is
        never a candidate and no commit ever stamped it, so the byte it keeps
        is the zero its own publication wrote. What would revive the hazard is
        a path that publishes an entity without `publish_header`, or a stamp
        written outside a commit; the zeroing S38.0 owes is cheap either way.
- [x] S36.7 Wire the collection into the ABI
      done: `ll_gc_collect_cycles` runs a collection and reports what it
        reclaimed, and `ll_gc_maybe_collect` fires on the armed pending flag and
        nowhere earlier; a test arms the flag, shows nothing collected before
        the next poll, and shows the collection at it — restaging the
        deferred-fire contract the dying `gc/tests/where_a_collection_may_fire.rs`
        carried
      done: the driver takes the two paths apart. Off the poll it holds the
        arena through the teardown and reads the rows; started by an allocation
        failure it harvests, returns every block, tears down, and traces again
        while the queue still holds candidates — a test under a forced refusal
        collects a population past the harvest region's capacity in two traces
        and leaves none behind (`dev/DECISIONS.md`, "the member list is the
        pressure path's alone")
      tier: T2 · role: Sage → Critic
      note 2026-09-07 — the second `done:` is met in substance and not in
        staging, and the difference is named rather than smoothed. The
        population past the region's capacity is collected whole and leaves
        nothing behind, which the case pins; but it takes three traces rather
        than two — one that overflows and two that harvest — and it calls
        `collect_under_pressure` directly instead of reaching it through a
        forced refusal. Nothing can reach it that way yet: the allocation slow
        path does not call a collection, which is S36.15, and `FORCE_OOM` held
        over the trace itself refuses the rows rather than the teardown.
      Sage 2026-09-07: four rulings, all executed. The commit is the whole
        unreachable set rather than a partition into components — the identity
        the exact validation compares holds per member, so a union that meets
        the sum meets it member by member, and what the union costs is
        precision in the two arms that refuse. The membership is one type with
        two forms rather than a member list built for the ordinary path, which
        the rfc forbids, or a second implementation of the three modules that
        read one. The batch's disposition is a merge into the live lane, not
        the rfc's literal "gives its segments back": segments given back with
        their records strand every root in them. The driver is a module of its
        own with one entry per path.
      Critic 2026-09-07 round 2, over round 1's repairs: six findings, two
        taken as code. The repaired stop rule had an exit it did not cover — a
        trace an allocation path refused ended the loop with no arming, which
        is the one ending that reads no lane at all; it arms now, and a case
        under `force_oom` on a thread of its own pins it. The dead-prefix
        producer of a fruitless bounded round had no case; it has one. Taken as
        prose: the ledger paragraph of `merge_candidates` contradicted itself
        and missed the overflow buffer's own charge, the `CollectingThread`
        note had the unwind's order backwards — the inner frames' drops run
        before the `extern "C"` boundary aborts — and the membership's
        assertion comment said no counter of the chain would see a truncated
        walk, where `Revalidation::close` does see it, after the damage.
        Verified and not a defect: the full head's splice, against every reader
        of the chain and both sides of the ledger; the loop's termination; and
        that the arming never fires with nothing behind it.
      note 2026-09-07 — what the pressure path returns is a fraction of what
        the heap holds, and the fraction is unpriced. Round 2's worked example:
        3,000 garbage pairs and one component past the region freed 750 pairs
        over five traces and handed the rest to the poll, because the bound
        restarts from half the roots after every paying round while the freed
        entries stay in the lane at exactly the positions the next bound
        re-selects. The yield is S39.1's to change — retiring an entry is what
        empties the dead prefix — and S40.1's to measure.
      Critic 2026-09-07 round 1, two lenses: six findings, three taken. The
        pressure loop read an empty harvest under a bound as an empty heap and
        ended with garbage standing and the thread unarmed — a bounded round
        that ends the loop now arms it. A full batch head was copied through
        the ordinary write, which always takes the growth path, and is spliced
        with the segments behind it instead, taking the charge that growth
        would have made. The row form's walk gave up silently in a release
        build where a row named no entity; it refuses in every build now.
        Taken as prose rather than as code: two false sentences in
        `merge_candidates`'s contract, which said it moves no bytes in the
        ledger and has no last resort, and the claim that a destructor's unwind
        takes the collecting flag down — a destructor is `extern "C"` and
        cannot unwind into that frame. Verified and not a defect — the union's
        free decision, which the second lens attacked over six heap shapes and
        found sound; and the second arena the pressure path opens over the same
        workspace.
      handoff: `cycle::collect` is the order, and `gc`'s two collecting entries
        call `collect_off_the_poll`. `collect_under_pressure` is built and
        called by nothing — the allocation slow path is S36.15's — and it
        halves its roots on a harvest overflow, arms the thread where one root
        still overflows, and traces again after a bounded teardown that freed.
        Five cases in `src/cycle/collect/tests.rs`; four source mutations were
        run and each was caught by the case that owns it.
      handoff: three debts leave with this step and are steps of their own.
        S36.15 puts the pressure collection behind the allocation failure that
        should start it. S36.16 carries the merge and the two paths back into
        `rfc`, whose "Concurrency" section still says the segments go back.
        And `queue::merge_candidates` copies its part-filled head one record at
        a time, which is a memcpy nobody has measured against.
- [x] S36.15 The allocation slow path starts a collection   *(after S36.7)*
      done: the call's place is chosen — `memory::heap`'s refill or the entity
        factory above it — and recorded with its reason; a refusal on the
        entity allocation path runs `cycle::collect::collect_under_pressure`
        once per allocation and retries once, whatever that collection
        answered; a retry the allocator refuses goes back to the caller as the
        refusal it raises memory-exhausted on, with no second collection for
        that raise (`rfc/runtime/exceptions.md`, "Allocation failure is an
        ordinary exception"); under a test-only cap on the blocks the pool
        hands out, a heap holding one garbage ring reuses all three member
        slots after a collection, then refuses the request that exhausts those
        returns too; the run reads two collection entries and three pool
        requests, with the arming state beside them. A separate empty-queue
        control still collects once, asks the pool twice and refuses. The
        occupancy/address case verifies that all three member slots return
        *(criterion updated by S39.2 on 2026-09-09; the original withholding
        polarity was seen red before the assertions were flipped)*
      tier: T2 · role: Critic
      Sage 2026-09-07: the criterion "allocates past the pool's last block and
        is served rather than refused" could not be met here and moves to
        S39.2. A collection returns no entity slot: every member the ring
        fixture builds is a registered candidate, `ll_free` withholds such a
        slot, and the block's `used` falls at the return that never comes. The
        retirement stays where Y12 clause 7 puts it — the owner's read of a
        zero-count entry — rather than in the commit, which was refused the
        same day (`dev/DECISIONS.md`, "the commit clears no candidate bit").
        Blocking this step on a step nobody had written would leave the
        collection with no production caller, which is the debt S36.7 left.
        Final.
      correction 2026-09-07: the Sage's "a member whose creation reference went
        straight into a field takes no non-final decrement, so its slot
        returns" has no site in this crate. The barrier spends a creation
        reference with `ll_release` (`array::element::write_through`;
        `dev/DECISIONS.md`, "the creation reference is spent before the
        displaced original is dropped"), which is the decrement that registers
        the candidate. What a collection does return is a member's body — a
        string payload, an array's storage, an OS-direct run — and that is what
        keeps "returns nothing" from being the whole truth.
      note: `FORCE_OOM` is not the injection these cases can use. It refuses at
        the top of `take_block`, ahead of the thread cache and the global free
        stack, so a retry under it proves nothing about the collection while a
        retry with it dropped proves the pool. The cap is new, `cfg(test)`, and
        stands against `blocks_out`.
      note: the `entity_alloc` cases that already run under `FORCE_OOM` would
        start a collection they did not start before. What that disturbs — a
        lane detached and merged, a thread armed — is read at the first run
        rather than assumed.
      Critic 2026-09-07 (two lenses, the wiring and the cases). Taken from the
        wiring's lens — `ll_weakref_create` read "has this target a row?"
        before the allocation and inserted after it, so a destructor of the
        collection that creates a weak reference to the same target left a
        second row, a cell dangling at the target's death and a count the table
        never gives back; an arena reset runs destructors between
        `promote::retain_block` and `promote::place_survivor_lists`, where a
        promoted survivor stands in a retained block with no occupant list and
        `memory::retained::register` forbids a trace to read it; the claim "a
        collection returns no entity slot" is true of a ring of objects and
        false as a rule, the candidate gate never admitting a kind at or above
        eight; and the obligation a caller now carries was written on a private
        function that no caller reads. Taken from the cases' lens — the counter
        counted entries into the cold tail rather than collections opened, so a
        tail with the collection deleted still read one; both cases passed with
        `collect_off_the_poll` substituted for the pressure path; the
        withholding case read `DeadInPlace`, which a slot on its block's free
        list also reads; the injection was a process-wide threshold on a
        counter every thread moves; the loop's guard admitted 100,000 slots and
        panicked holding them; and no case took the branch where a collection
        frees nothing. Refused nothing.
      handoff: the call stands in `memory::heap::entity_alloc`, not in
        `Heap::alloc_no_block`: a `&mut Heap` is live there and the
        collection's destructors re-enter the allocator on the same thread.
        `cycle::collect::CollectingThread::take` refuses a collection while a
        reset window is open, which is a defect older than this step — the
        poll's own collection could already be reached from a reset's
        destructor — and `a_collection_reached_from_an_arena_reset_is_refused`
        is red without it. `weak::table::insert` answers the row that stands
        instead of writing a second one, and `ll_weakref_create` gives its cell
        back and hands out the canonical one.
      Critic 2026-09-07 round 2 (over round 1's repairs). Taken — the retry was
        gated on the collection's answer, and that answer counts members a
        commit freed rather than memory an allocator can use: a trace that
        ended in a refused allocation path has given its blocks back, and a set
        the revalidation read as live has already run the destructors, each of
        which can free a child whose slot returns at once. The retry is
        unconditional now, and the control case reads two pool requests rather
        than one. Also taken — all three cases built one size class, which is
        the unit `Heap::adopt` works in, so each case's three withheld slots
        landed in the next case's arithmetic; they take three classes now and
        the occupancy reading is a delta. And `weak/table.rs`'s module doc still
        described the order the repair inverted, `ll_weakref_create` still read
        the target's category before the allocation and used it after, the
        DECISIONS entry called a template instance a kind above the ring
        reserve when `ll_template_new` publishes it as an object, the budget's
        doc claimed a property its placement above the thread cache forbids,
        and the caller's obligation was stated on `entity_alloc` while seven of
        the nine factories reach it through `routing::entity_alloc_in`. All
        repaired. Verified and not a defect — the other factories satisfy the
        obligation today: `box_element`, `fill_from` and `flatten` each read a
        structure across the allocation and are safe because no other name can
        reach it, which is now written where each does it.
      handoff: the injection is `block_pool::budget_blocks`, a per-thread count
        of blocks this thread may still take. Three cases in
        `src/memory/heap/tests/the_collection_a_refusal_starts.rs`, one of them
        the control arm with no ring, and one more in `weak::` for the row a
        re-entrant creation finds taken. Seven source mutations were run and
        each was caught by the case that owns it — among them the poll's
        collection substituted for the pressure one, a second collection inside
        one allocation, and the candidate arm returning a member's slot.
        Verified at 805 tests, and Miri clean over `memory::heap` (24 in 271 s),
        `weak::` (21 in 42 s) and `cycle::collect` (9 in 720 s).

- [x] S36.8 Elide the redundant exact test after an in-line owner trace
      done: when mark and scan run synchronously on the owning mutator at one
        consistent point, a condemned component proceeds directly to the owner
        commit — guard acquisition, weak invalidation and finalization — without
        `validation::validate_component` re-reading the same counts and fields first; the
        speculative/off-thread path still calls `validation::validate_component`, because its
        shortlist may combine observations from different instants; tests count
        exact-test entries and prove zero before teardown for the in-line path,
        one for a posted speculative result, and no behavioural difference on
        a ring with an external keeper; a benchmark records the saved member
        and edge reads for component sizes 2, 16 and 256
      tier: T2 · role: Critic
      handoff: this removes only the pre-teardown confirmation. S36.4's exact
        re-verify after any `__destruct` remains mandatory in both paths: user
        code ran with a guarded `$this` and may have resurrected a member. The
        in-line shortcut is valid only while the owner has not released the
        collection's consistency window between the final scan decision and
        guard acquisition; encode that boundary in the API so a future caller
        cannot pass a stale condemned list as an in-line proof.
      note 2026-09-09 — the first owner-proof attempt (`195ddc5`) is withdrawn
        by `faad4f2`. A fixed heap did make the stale-count and zero-count arms
        unreachable, but `validate_component` also independently derives
        `IN(m)` from the members' cells and compares it with the row machinery's
        proposal before any destructor runs. Removing it turns a row-arithmetic,
        grouping, saturation, reverse-index or placement defect from a refused
        collection into irreversible user destructors. It also removes the
        ordinary path's `ExternallyReferenced -> stamp_component` producer,
        narrowing Y9 to post-destructor resurrection. A future shortcut must
        retain an equally early independent check and preserve that producer;
        the decision is in `dev/DECISIONS.md` and `rfc`'s journal.
      Sage 2026-09-11 (Final: refused): closed without the elision, on the
        ruling of 2026-09-09. The retained check is the second walk of
        `validate_component` in full: the internal in-degree is derived from
        the members' cells and every out-edge is classified against the
        membership, and no cheaper reading is independent of the rows, because
        the scan keeps an edge's resolution only in the row, and the set `IN`
        is defined over does not exist until the scan ends. The count's first
        walk is the smaller term, and folding it into the cell walk saves one
        traversal of eight or nine a collection makes; that is a T1 inside
        `validation.rs` behind an `rfc` amendment of finalization step 1, not a
        step here. The remaining criteria have no producer: the speculative
        count names S38's worker, the keeper ring is the injected verdict race
        in `collect.rs`, and the saved-reads benchmark measures a quantity the
        ruling fixes at zero.

- [x] S36.17 The reset window's memory comes from the manager
      done: `memory::reset_window` holds its parked bodies, its snapshots,
        its escrow and credits and the died set in memory drawn from the
        manager, no `Vec`, `Box`, `HashMap` or `HashSet` remains on its
        paths, a refused draw is answered to the reset as a refusal rather
        than an abort, and S36.9's composite deny run over a wired collection
        reads the five sites of 2026-09-06 as clean
      tier: T2 · role: Critic
      Edmond 2026-09-12: the reset's exemption does not reach these frames;
        the memory is the manager's and an allocation failure is never a
        panic (`dev/DECISIONS.md`, "the reset window's memory comes from the
        manager"). The ruling is global; `promote`'s own twelve container
        sites are the debt S47 carries.
      Critic 2026-09-12 round 1, on the design: five findings. Taken — the
        first refusal answer, one `ll_retain` of the child at the refused
        edge, settles a same-round COW child one low, the retain landing
        inside the `at` the arithmetic discards; the round's survivors are
        pinned after their counts are captured instead. Taken — the log
        drawn from the arena's bump would take the last bytes ahead of the
        survivor lists; it is drawn through `stdapi::ll_alloc`. Taken —
        clause 3(b) cited a retirement inside a window that cannot run;
        struck. Taken — the counter's meaning, a null-arena `open`, and the
        decision "the reset reads no corpse" as a precondition, amended
        first. The parts that stood: the frame-held window, the byte-8
        stack, the derived escrow.
      Critic 2026-09-12 round 2, on the code: five findings. Refused with a
        reading — `slot_state` answers `DeadInPlace` on the bit alone
        (`refcount.rs`, the zero-count arm), so a torn-down candidate reads
        torn down; the test's holder grew that third shape to pin it. Taken
        — the container guard is a spelling net and says so, with a longer
        list; the record functions' unused `bool` was a second channel for
        one fact and is gone; the log's slices over an uninitialised tail
        are raw-pointer reads now; the take counter's meaning is written at
        it. Its out-of-scope sentence on the delta term is in the handoff.
      handoff: the window is `ResetWindow` in `arena_reset_full`'s frame,
        `DEFERRED_FREES` one stack through byte 8 of the bodies popped at the
        outermost close, `is_torn_down` one `slot_state` reading, and a log
        of `(holder, child)` records in 4 KiB `ll_alloc` segments read by
        `for_each_correction`; a refused segment pins the round through
        `take_refused_promotion_edge`. `record_death` is gone with its four
        sites; the teardown counter is `note_slot_taken` at `ll_free`'s head.
        Seen red on their own mutations: `is_torn_down` false (a SIGSEGV —
        the walk follows byte 8 as a class pointer), no deferred increment
        (4 cases), a close that pops nothing (6), every close popping (1),
        no pin (1), the pin before the `at` capture (1), segments kept (1).
        The deny run's reading of a reset inside a collection waits on S47:
        the window's sites are gone from the source and the module's guard
        reads none, but `promote`'s containers stand in the same frames.
- [x] S36.18 The COW reconciliation settles off the log   *(after S36.17)*
      done: a destructor of a later round that writes `$keeper->s = null` on
        a promoted holder leaves the string at its remaining holders' count,
        and one that writes `$keeper->s = $s` into a promoted holder leaves
        it at its holders' count and no higher — both seen red on the walk;
        `reconcile_cow_counts` reads no holder and follows no slot; a refused promotion-edge
        record still settles no count low; the gate and a Miri run over the
        two modules
      tier: T2 · role: Critic
      progress 2026-09-12 — the Critic's out-of-scope sentence at S36.17
        reproduced in both directions: `edges_live + (now − at) + D − K`
        counts an event twice whenever a destructor changes a promoted
        holder's slot after the holder was counted — a release is in the
        delta and out of the walk (one low, zero under a living holder), a
        retain in the delta and in the walk (one high). The formula is
        `edges_at_promotion + (now − at) − K` now, the edges read off the
        window's log, the walk and `D` gone, and a refused record answered
        by one `ll_retain` of each COW child of the round after the round's
        captures (`dev/DECISIONS.md`, "the COW count is the log's edges plus
        the delta"). Seen red: the two new cases on the walk; with the
        refusal retain deleted, the refusal case; with the log answering
        torn-down holders alone, the two new cases, the corpse case and the
        two window cases. Gate: 906 plain and three times at eight threads,
        `hash-folding` 906, `debug-journal` 910/20 three times, release,
        `cargo bench --no-run`, `cargo doc` 46 warnings (46 at `218904d`),
        `citations.py` 547 with the same six residues, `fmt --check` clean,
        `--list` +4/−2 against `772eabe` (two renamed, two added). Glossary
        rows for the two correction terms amended in `rfc`.
      Critic 2026-09-12: nothing breaks under the shapes it ran — a child
        promoted a round before its holder, a transient and a total refusal,
        a nested reset, a COW write in a destructor, a resurrected holder
        that nulled its slot first; every edge a promoted holder gains after
        its count is a retain in the delta or an `escape_copy` with no edge,
        which is why the walk's extra edges were exactly the delta's events.
        Six findings, all taken. "Every record refused is exact" was false
        for a child of an earlier round counted again, the uncredited retain
        standing — narrowed in the entry and the case's doc; the zero-count
        survivor case passed vacuously, its count no longer witnessing a
        holder's fate — re-aimed at the re-trace, which now promotes a
        child the same destructor stores into the survivor, seen red with
        the re-trace skipping at count zero; the two new cases alone also
        accepted `now − the count pass's increments` — a third keeper that
        dies with the arena rejects it; the group's header still described
        the walk; "reads no survivor" was false of the COW rows it writes —
        "reads no holder and follows no slot", the 2026-08-14 argument
        cited for the read it makes; "in the log's order" dropped. Named
        beside the path and not this step's: a destructor storing an
        **existing** arena value into a marked survivor without allocating
        moves no bump cursor, so the re-trace never runs and the holder is
        promoted with an edge into the dying arena — reproduced the same
        day and opened as S36.19.
      handoff: `reconcile_cow_counts(at_promotion)` sums the delta and the
        log's terms per COW row and walks nothing; `for_each_correction` is
        a safe fn answering one increment per record with a holder;
        `arena_reset_full` retains the round's COW children on a refused
        record after the round's captures. The gate on the final tree, then
        Miri at two threads on it: `memory::reset_window` 5/1 (12 s),
        `promote::tests::what_a_destructor_does_during_the_fixpoint` 7/0
        (23 s), `promote::tests::the_reset_reads_no_zero_count_member` 12/0
        (5 m 29 s). Decision: `dev/DECISIONS.md`, "the COW count is the
        log's edges plus the delta".
- [x] S36.19 The re-trace runs after every destructor round   *(after S36.18)*
      done: a destructor of the settle loop that stores an **existing** arena
        object, reachable from nothing else, into an already-marked survivor
        without allocating leaves that object promoted and held by the
        survivor's slot — seen red on the bump-cursor trigger, where the
        survivor comes out naming memory the reset gave back; the trigger
        is the round having run a destructor, the bump cursor read by no
        one; the counters test of the re-trace's skip still holds; the gate
      tier: T2 · role: Critic
      note 2026-09-12: found by the Critic of S36.18 beside its path and
        reproduced in a worktree the same day: `FogHolder` escapes,
        `FogDying` holds `FogLeaf` and destructs unheld with
        `$survivor->keep = $this->y`, and after the reset the survivor's slot
        names the leaf at `RequestArena` — an entity the reset never
        promoted, standing here in the block retained for the survivor and
        elsewhere in memory the reset returns. `retrace_survivors`'s "cheap when
        nothing changed" is the whole cost of running it per round.
      progress 2026-09-12 — baseline on the cursor trigger, read off the new
        `cfg(test)` counter `promote::take_retrace_count`: the case's
        destructor round re-traced 0 times and the handed-over leaf read
        `RequestArena` after the reset. The trigger is `ran_a_destructor`
        now, `Arena::bump_cursor` deleted with its last reader, the case
        green with one re-trace and the leaf promoted at count 1. Mutation
        seen red: no re-trace at all reddens this case, the H2 fresh-object
        case, the zero-count survivor case and the retrace-skip counters
        case. `rfc/model/gc/pure-destructors.md`'s sentence on the re-trace's
        trigger updated (`dev/DECISIONS.md`, "the re-trace runs after every
        destructor round").
      Critic 2026-09-12: nothing breaks under A and B — a store into a
        survivor marked earlier in the round, into an unmarked holder a
        later destructor escapes, into a survivor of an earlier pass (the
        barrier escapes it), a COW child, a box or an array element as the
        slot; from the release drain every survivor is GcHeap already.
        Six findings, four taken and two named elsewhere. Taken: the rfc's
        dirty bullet named allocation alone, so the three classes did not
        cover a store-only destructor and the entry's "the rfc classes it
        as dirty" was false — the bullet is amended in place and the entry
        says so; the entry's cost sentence and its "the barrier already
        loads the holder's flags" were not what the code does — the loads
        are the child's and `store_ptr` takes the category as a parameter;
        the trigger counted a drained entry with nothing to run — it reads
        `run_user_destructor`'s answer now; the case asserted `>= 1`
        re-traces where the shape yields exactly one, and "memory the
        reset gave back" was not what the reproduction showed, the leaf
        standing in the survivor's own retained block — both texts
        corrected. Named elsewhere: an object handed over in a batch still
        runs its own destructor (Fog); a large survivor's address reused
        by a later large object reads live, not torn down, since the bit
        is the header's (no action).
      handoff: the trigger is `ran_a_destructor`, read off
        `run_user_destructor`'s answer, in `arena_reset_full`'s settle loop;
        `promote::take_retrace_count` is the `cfg(test)` instrument. Gate on
        the final tree: 907 plain and three times at eight threads,
        `hash-folding` 907, `debug-journal` 911/20 three times, release,
        `cargo bench --no-run`, `cargo doc` 46, `citations.py` 549 with the
        same six residues, `--list` +1. Miri at two threads: the destructor
        group 8/0 (33 s), the zero-count group 12/0 (6 m 56 s). The rfc's
        dirty bullet amended in `arena-reset.md`.
- [x] S36.16 Carry the collection's two paths into `rfc`   *(after S36.7)*
      done: `model/gc/rc-cycle.md`'s "Concurrency" says the ordinary path
        merges its detached chain into the live lane rather than giving its
        segments back, and says why a segment given back with its records
        strands every root in it; `cycle/questions.md` Y12 clause 5 names the
        merge as what performs it; and the commit's unit — the whole
        unreachable set rather than a partition into components — is written
        down with the precision it costs
      tier: T2 · role: —
      note: the `rfc` repository's plan owns the edit, and this step is the
        pointer at it from here (`dev/WORKFLOW.md`: a debt the other plan owns
        is never written as this plan's `S<n>`). What was built is
        `queue::merge_candidates` and `cycle::collect`.
      handoff: taken ahead of S36.15, which is a `cargo` step and stood behind
        a Miri run holding the lock. Three edits in `rfc`, one commit:
        `model/gc/rc-cycle.md`'s "Concurrency" says the close merges the
        detached chain and names the two refused answers by what each strands;
        the same file's "Cycle finalization and reclamation" opens with the
        unit the in-line collection commits — the whole confirmed set, with the
        precision that costs in the two refusing arms; and
        `model/gc/cycle/questions.md` Y12 clause 5 names the merge as what puts
        an unwalked root back, and ties a freed member's surviving entry to
        clause 7's retirement. `dev/tools/linkcheck.php` clean at 630 links,
        and this crate's citation check unmoved at 495 with the same seven
        residues.


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
        `(commits / 64) % 4` (S36.6). What is left here is the descent's read,
        the per-thread mirror S37.4's re-offer needs, and `k`. The maturation
        case moved here from S36.6 with its arithmetic corrected: with `k = 3`
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

- [ ] S38.0 The collector's reader   *(blocked: `rfc` A1)*
      done: the collector's `CellReader` implements one of the four resolutions
        `rfc/dev/ALGORITHM-AUDIT.md` A1 requires — an atomic slot
        representation, a versioned read for wide values, a write-barrier
        snapshot, or a stop-at-consistent-point protocol — and states which; the
        collector-thread trace instantiates `trace_cells` with it, and a Miri
        slice drives it over an object with outside cells and over an array
        mid-move
      tier: T2 · role: Critic
      correction 2026-09-04: the criterion read "no storage version and no
        give-up, because a torn read costs at most a phantom edge or a missed
        one". A1 rejects that justification in terms: a wide `ValueBox` is
        published as two stores, "this cannot be treated as harmless staleness
        because the resulting pointer need not have been valid at any instant",
        and "until then, only synchronous owner-side tracing is memory-safe".
        `rfc/model/gc/rc-cycle.md` repeats it — "This statement assumes a
        memory-safe protocol for concurrent slot reads; that protocol is
        currently an open blocker". So the step refused by name one of the four
        resolutions it is required to pick from. **The step is blocked until A1
        closes**, and that is a design ruling, not an implementation choice.
        `rfc/model/layouts.md` licenses relaxed atomics for the header, not for
        a two-word cell. The same block reaches the cross-thread arms of S38.1
        and S38.3; the several-collectors-at-once part is licensed separately
        (`dev/DECISIONS.md`, 2026-08-26) and only the reads are blocked.
      handoff: `RelaxedCells` and its re-check plumbing died at S30.2 because
        they existed for `rc-walk`'s precision. What the accelerator needs is
        strictly smaller, and the `CellReader` trait is the socket it plugs into.
      handoff: two debts carried from S31 before that stage was deleted. **The
        stale stamp byte** is this step's to zero, at both producers — a
        recycled `heap::FreeSlot` and a promoted survivor — because this is
        where the second thread arrives and the byte stops being inert; the
        prune in `cycle::mark` is what it breaks. Neither producer reaches a
        reader today: `refcount::publish_header` stores the whole eight-byte
        word, so a recycled slot carries no stamp of its previous occupant,
        and nothing writes byte 6 of an arena entity, so a promoted survivor
        arrives with it zero (read 2026-09-10). And **`dev/WORKFLOW.md`'s ThreadSanitizer run has
        selected no test since 2026-08-26**, its only one having lived in the
        deleted `collector::`, so the instrument that reports
        plain-against-atomic is         unavailable until this step gives it a pairing
        to watch.
      handoff: the collector thread's birth is this step's to name — startup
        or first pressure — with its floor refusal following it; a mandatory
        floor drawn at first pressure is the worst moment
        (`rfc/dev/DECISIONS.md`, "the baseline overflow segment is allocator-issued").
      handoff 2026-09-12, from S39.1: the exit waits on the thread's token
        once and then collects with takes of its own, so between and after
        those takes a collector could take the token of a thread whose exit
        has begun and trace blocks the exit is about to abandon — this step's
        collector reads the owner's exit phase before its take, through a
        word it adds: `memory::heap::thread_exit_running` reads the calling
        thread's own thread-local and cannot answer for another. And the
        inbox the rfc's handoff names is the fourth chain the exit would
        drain; today it has three (`cycle::collect::collect_before_exit`),
        and a fourth joins `queue::registered_count`, the offer before each
        round and `release_queue_segments` alike.
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
- [ ] S38.3 Deferring the mutator's frees during a trace   *(blocked: S38.0 — the tracer whose held addresses a free would pull out is the collector thread; the in-line trace runs no user code and frees nothing)*
      note: S36.2 built the owner-side substrate for one thread, where nothing
        frees inside the window: mark and scan only read, and the trace window
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
        `retained::release_emptied`, and an OS-direct run; the cost is
        measured as the churn held across one collection
      tier: T2 · role: —

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
- [x] S40.4 Price the trace a refused allocation repeats   *(after S36.15)*
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

## S47 — The reset's bookkeeping leaves the global allocator

Goal: `promote::arena_reset_full` and the passes under it hold their survivor
list, snapshot pairs, retained and pinned sets, per-block index, rounds and
settle map in memory drawn from the manager, and answer a refused draw as a
refusal.
Done when: no `Vec`, `HashMap`, `HashSet` or `Box` stands on a reset's path,
and S36.9's composite deny run reads `promote` as clean.

A debt Edmond named as such on 2026-09-12 when he ruled that no runtime path
may end the process on an allocation the manager could have refused
(`dev/DECISIONS.md`, "the reset window's memory comes from the manager");
S36.17 took the window's five sites first. Broken down on 2026-09-12 from a
reading of `promote.rs`'s twelve sites, the structure agreed with Edmond the
same day; the breakdown has not been through a Critic round (23.3), and that
round is the first act before S47.0.

- [ ] S47.0 Decide what a reset answers when the manager refuses it memory
      inside the fixpoint
      done: the answer and its reason are in `dev/DECISIONS.md`, with the
        refused alternative — the reset has no caller to report to, so the
        candidates are retaining the whole arena (every block retained, no
        block freed, counts intact: a bounded leak) and nothing else that
        the no-panic ruling admits; every later step's refusal arm is this
        answer
      tier: T2 · role: Critic
- [ ] S47.1 The drains hand over their log instead of filling a `Vec`
      done: `round`, `round_dtors` and `round_releases` are gone; each
        `drain_*` yields the log's own segments, walked after the arena's
        borrow has ended, so the settle loop takes no memory for a round and
        has no refusal to answer; the H5 reentrancy rule holds by reading
        and the fixpoint cases stay green
      tier: T2 · role: Critic
- [ ] S47.2 The survivor list and the mark worklist in `ll_alloc` segments
      done: `survivors`, `mark_subgraph`'s `stack` and `cow_at_promotion`
        stand in 4 KiB segments drawn through `stdapi::ll_alloc`, the
        window's log's shape; a refused segment is answered as S47.0 says,
        seen on a forced refusal; the reset's global allocations on the
        fixpoint cases go from their counted baseline to 0
      tier: T2 · role: Critic
- [ ] S47.3 A block's reset state stands in the block
      done: `retained`, `pinned`, `by_block`, `placed` and `emptied` are
        gone — "retained in this reset" is the kind stamp, the reset's own
        pin is a bit in the block header, survivors are grouped by block by
        sorting the chain by address, and the emptied blocks chain through
        their collector lines; the reset draws no memory for any of them
      tier: T2 · role: Critic
- [ ] S47.4 The COW rows without a `HashMap`
      done: `settled` is gone — `at` is one more record kind in the window's
        log and the sum per child is taken over the segments sorted by
        child; `reconcile_cow_counts` makes no global allocation; S36.9's
        composite deny run reads `promote` as clean and S36.9 closes
      tier: T2 · role: Critic

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
  "Mutator progress while collection is unavailable"); the accelerator carries the third
  claim state's production entrant, the proposal machinery that turns a dirty
  trace into a shortlist, and the owner-checkpoint judgement that reads it
  (`rfc/model/gc/rc-cycle.md`). Gated on a measured in-line pause that a
  collector thread would shorten — until then the in-line collection at a
  failed allocation is the whole trigger, and that is the design in force
  rather than a stopgap (Y14, amended 2026-08-26).
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
  its cost only if one of them is seen to fail. The defect one of the five
  carried is closed: `a_thread_nothing_will_tear_down_is_not_funded` read the
  same process figure into two variables and asserted both, so its segment
  claim had no reading behind it, and it now reads `gc_metadata::thread_stats`
  on the child thread itself — the peak exactly at the base block and both
  spare segments, in both builds, and both current figures at zero. Seen red
  on a build whose `ll_thread_init` refills no spares.
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
  object identity, marked "(future)" there, and S36.5's `done:` clause was
  written against it — "the weak notify's displaced map values" — so half that
  clause named a population no producer can make. The clause was struck and
  the work is here (Edmond, 2026-09-07: it can wait).

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
- [x] **The retained arm's per-edge registry lock.** Closed 2026-09-02 by
  S36.9 slice (e): the registry is gone, and the arm reads the survivor
  list's address and length from the block's own header line and searches
  the list (`memory/retained.rs`). The scan's second lookup per popped
  entity is a second header read rather than a second lock; the row-pointer
  alternative for that half stays unweighed in `dev/DECISIONS.md`, "the
  scan re-reads a colour it may have written".

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
