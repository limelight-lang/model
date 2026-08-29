# Plan

Implementation plan for `ll-model`, re-sorted 2026-07-24 against the RFC
after the 2026-07-22 object-layout redesign landed in the `rfc` repo.

Design lives in `rfc` and is authoritative — read before coding, do not
re-derive: `model/classes.md`, `model/values.md`, `model/lowering.md`,
`model/gc/rc-cycle.md`, `model/gc/cycle/questions.md`, `model/memory/ffi.md`,
`runtime/object-lifecycle.md`.

The `rfc` repository carries its own plan at `dev/PLAN.md` for work that lands
in the specification rather than in this crate.

Updated: 2026-08-27 · Active: S34 — the sections after S40 are the backlog

**Closed stages are deleted whole** (rule 23.1.3), and what outlived each
of them is in the journals rather than here: `dev/DECISIONS.md` for a
decision and its reason, `dev/POSTMORTEM.md` for a trap,
`dev/BENCHMARKS.md` for a measurement, `dev/INDEX.md` and
`dev/ARCHITECTURE.md` for the map. Deleted so far: S4 through S33. A number is never reissued, so a
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

**The crate collects no cycles.** S30 deleted `rc-walk`, `rc-trace` and
`rc-satb` on 2026-08-26 and the design in force is unbuilt, so a garbage ring
is retained and acyclic garbage dies by counting. The stages below build
`rc-cycle`; what the deletion took, what it kept and why is `dev/DECISIONS.md`
under that date, and the old code is on `archive/pre-rc-cycle`. S28 was
abandoned rather than closed by the same ruling, and S29 was split — its
second half is carried as S39.

**The stages below went through a Critic round and four Sage rulings on
2026-08-26**, on Edmond's instruction, and are the amended form. The rulings and their reasons are in `dev/DECISIONS.md`.

**Verification is one configuration** since 2026-08-26: the GC axis went with
the collectors, `hash-folding` and `debug-journal` are what remains, and
`cargo bench --no-run` is part of the gate because `cargo test --lib` builds
no bench target while `benches/lifecycle.rs` imports the GC ABI
(`dev/WORKFLOW.md`).

## Fog

Empty.

---

## S34 — The root queue, enrolment and parking

Goal: candidates reach the collector without the mutator paying for a data
structure, and an entity that dies while enrolled leaves no dangling pointer.

- [x] S34.1 The queue against Y12's contract
      done: **all eight** clauses hold, and where a clause constrains a reader
        that does not exist yet "hold" means the code contradicts none of it and
        says which half it does not build — clause 6 is the one that needs
        saying, its reserve draw being built and its reserve *mode* not, there
        being no collection to walk a root; so a failed growth never drops a
        root, no allocation
        happens on the enrolling thread's hot path, proven by a `#[cfg(test)]`
        allocation counter bracketing the enrolment call rather than by defining
        the growth path as not hot, and a second reader is either supported or
        refused by construction rather than by a `debug_assert!`; clause 4's
        second half is superseded by S34.2 and the step says so; the one arm the
        runtime keeps is rebuilt here — a growth refusal or a reserve draw
        during enrolment sets the pending flag, and the poll fires at the next
        clean point, returning 0 until S36.7 wires the collection
      tier: T2 · role: Critic
      Critic 2026-08-27: eleven findings, and the two that changed the code are
        the first two. `critical::draw` is a `try_with` on a thread-local with
        drop glue, so its first touch registers a destructor and S34.1 put that
        call on the release path where nothing can report — `ll_thread_init`
        now fills all three reserves before the heap allocation that returns
        early, and the residue is S34.5. And thread exit strands `ENROLLED` on
        entities that outlive the thread through the abandoned list, which the
        gate then refuses for ever: a permanent miss and not the "loss of
        candidates" the comment called it, so the comment is rewritten and
        S39.1 is told what it is choosing against.
      Critic 2026-08-27: the rest were the instrument and the fixtures, and
        four are now mutation-checked regressions that were not detectable
        before — the drain test held no spare at the drain, the allocation
        probe bracketed the growth path twice and the hot path never,
        `FORCE_OOM` in the refusal test closed a door the path never knocks on
        and hid an "ask the pool" fallback, and `fill_live_segment` set a count
        over 8159 recycled words that S34.3's corpse rule would have
        dereferenced. Also taken: the second `arm()` was unreachable, so the
        draw and the refusal now arm independently and each has a test; a held
        spare read `BLOCK_KIND_FREE`, now stamped at acquisition; and three
        comments said what had stopped being true. Nothing was rejected.
      handoff: the clause this step could not have been built against was
        clause 3, and it was ruled on 2026-08-27 (`rfc/dev/PLAN.md` S8.2, and
        the entry it names in `rfc/dev/DECISIONS.md`). What it hands the code: a
        segment is one 64 KiB pool block and the queue is a chain of them; the
        owner holds two spares in two pointer cells filled at thread init and at
        every poll through the ordinary door, the live segment being a cell the
        first enrolment swaps in; an overflow with both cells null draws the
        critical reserve. What no ruling reaches is the reserve being spent too
        — `rfc/dev/PLAN.md` S8.5 — so this step builds the reserve draw and
        stops at its edge.
      handoff: closed 2026-08-27. `cycle::queue` is the owner's side of the
        contract: a chain of 64 KiB pool segments, two spare cells filled at
        `ll_thread_init` and at every `ll_gc_maybe_collect`, the live segment a
        cell the first enrolment swaps in, `critical::draw` when both cells are
        empty, and the enrolled bit undone when both doors refuse. `refcount`'s
        release path is its one caller and the edge is in `dev/ARCHITECTURE.md`.
      handoff: verified at 514 tests — one run, three at four threads,
        `hash-folding` 514, `debug-journal` 520 three times, release with no
        warnings, `cargo bench --no-run`, `fmt --check`. Miri over `cycle::queue`
        is clean at 10 tests, 10.73 s on its own clock. Every new test was seen
        failing against a mutation of what it names, fifteen mutations in all.
        Miri found the stage's one defect and it was the fixture's: a raw
        pointer taken before a `&mut` call, invalidated by the retag —
        `dev/WORKFLOW.md`'s rule about that was scoped to reentrancy tests and
        is widened.
      handoff: what this step did **not** build, and who owns it. The read side
        is S35.1's and the accelerator's swap S38.1's, so nothing yet agrees
        with a detaching reader about the fill cell — `rfc/dev/PLAN.md` S8.7.
        The drain at thread exit returns blocks and drops entries, which S39.1
        turns into a chosen fate. The corpse rule and the marks a reader writes
        into an entry's low bits are S34.3's and S35's; the four bits are free
        and nothing writes them today.
- [x] S34.5 Decide what the critical reserve's first touch may cost
      done: `memory::critical`'s thread-local is reachable from `ll_release`
        without the process depending on what registering a TLS destructor
        costs — either its payload loses its drop glue and the blocks a thread
        that never ran `ll_thread_exit` holds are returned another way, or the
        cost is measured on this platform and recorded as acceptable with the
        measurement named
      tier: T2 · role: Critic
      handoff: raised by the Critic on S34.1, 2026-08-27. `critical::draw` is a
        `try_with` on a `thread_local!` whose payload has a `Drop`, so its
        **first** touch on a thread registers a destructor, and on glibc that
        registration allocates and terminates the process when it cannot. S34.1
        put that call on the release path, where nothing can report.
      handoff: what S34.1 did about it, and what it did not. `ll_thread_init`
        now fills all three reserves before the heap allocation that can return
        early, so every thread it runs on has touched the reserve while a
        refusal was still reportable. What is left is the thread that never
        calls `ll_thread_init` at all — a population `Critical::drop` and
        `ThreadCache::drop` both say the crate serves — and for that one the
        first touch is still `ll_release`'s. The claim about glibc is the
        Critic's reading and is **not verified on this box**; verifying it is
        part of this step.
      handoff: S34.8 added a second registration on the same path, and it is
        the same question. The lazy floor draw asks
        `heap::thread_exit_will_run`, whose call is the arming of `EXIT_GUARD`
        — a `thread_local!` with drop glue, so its first touch registers a
        destructor exactly as `critical::draw`'s does, and from the same
        release path on a thread that never ran `ll_thread_init`.
      Critic 2026-08-29: five findings against the decision, not the code. The
        two that mattered were about the record: the entry claimed a
        registration death at init belongs to the class the floor's ruling
        accepts, when that ruling accepts a *thread* refusal and a registration
        failure kills the process; and it called the registration and the
        floor's abort "the same edge", when one answers an empty block pool and
        the other an empty glibc heap. Both rewritten. The census pin, the
        second test's framing and the unpriced `pthread_key` arm were the other
        three, all taken. One round: nothing was disputed.
      handoff: closed 2026-08-29, and the arm taken is the second — the cost is
        measured and recorded rather than removed. Verified on this box, not
        assumed: the binary carries a weak `__cxa_thread_atexit_impl`, the
        toolchain's `linux_like::register` discards its result, and Ubuntu
        GLIBC 2.39-0ubuntu8.7 `calloc`s 32 bytes there and calls `__libc_fatal`
        on a null — "Fatal glibc error: failed to register TLS destructor: out
        of memory". It never returns a failure, so there is nothing for Rust to
        have checked.
      handoff: what the decision rests on, in `dev/DECISIONS.md`, "what the
        first touch of a thread-local with drop glue may cost". Four
        thread-locals in this crate have drop glue and `ll_thread_init` touches
        all four, so the death is deterministic in place rather than scattered
        over the release path — it is **not** converted into a refusable one,
        and the entry says so. Three tests in
        `memory::critical::tests::where_the_first_touch_happens`, the third a
        census of every `thread_local!` in `src/` against a literal list, so a
        fifth cannot appear unnoticed.
      handoff: not priced, and named rather than dismissed: a guard built on a
        `pthread_key_create` key taken once at process start, where the failure
        is reportable. It would remove the unreportable class from both paths
        if `pthread_setspecific` allocates nothing per thread, which was not
        read. Whoever picks it up owns the per-target story too.
- [x] S34.6 Make the enrolment unfailable, and delete the undo
      done: `enrol` answers nothing and every door refusing lands the entry in
        an escrow the same thread-local holds; the release path has no branch
        left in which a set enrolled bit names no entry; the poll refills,
        drains the escrow and only then fires, and a drain with no room leaves
        the entries where they are rather than losing them or looping
      tier: T2 · role: —
      handoff: Edmond ruled on 2026-08-28 that nothing may be lost — the
        mutator either collects itself or waits for the collector — and the
        mechanism was ruled beside it (`rfc/dev/PLAN.md` S8.5, and the entry it
        names in `rfc/dev/DECISIONS.md`). S34.1 had shipped the branch the
        ruling forbids: it undid the bit and lost the edge, which is Y6's
        permanent miss whenever that decrement was the ring's last external
        release.
      handoff: the escrow is one segment's capacity, 65 280 bytes of
        thread-local per thread, sized on clause 3's own poll argument.
        Overflowing it aborts, which is the funded class's last resort and is
        the one state this module has no answer for.
      handoff: the cost is measured and it is eager. `readelf -S` puts the test
        binary's `.tbss` at 65 680 bytes, so the escrow is 99.4 % of the crate's
        zero-initialised TLS image, and glibc allocates and zeroes that image
        per thread. Trimming it is one constant and wants the ABI's poll bound
        first (`rfc/runtime/exceptions.md`).
      handoff: role `—` rather than Critic, unlike S34.1: the shape is a
        ruling's and not a choice, and what a Critic would attack here — the
        escrow's size and the abort behind it — is named in the ruling and in
        `dev/DECISIONS.md` rather than decided in this step.
      handoff: *(storage amended 2026-08-28.)* The escrow leaves the TLS image
        for one allocator-issued floor block held for the thread's life; the
        rework is S34.8, the ruling `rfc/dev/DECISIONS.md` "the escrow's floor
        is allocator-issued". The 99.4 % TLS figure above stands as the
        measurement that motivated the move.
- [x] S34.7 Give the bulk release loop a poll of its own
      done: `ll_release_vector` polls on its backedge every `POLL_STRIDE`
        iterations, so a run of any length refills the queue's funding and
        drains its escrow mid-run; a test releases a vector longer than the
        stride with every door spent and finds the escrow empty and every
        candidate queued at the end
      tier: T2 · role: —
      handoff: found by the consolidation pass on the ruling, not by a test.
        The escrow was sized on "a whole segment cannot fill between two
        polls", and that argument quantifies over loops the compiler emits;
        `ll_release_vector`'s count is the caller's and the compiler polls only
        after the call, so a container clear enrolled without bound. Before
        this, a clear of some ninety thousand shared elements aborted with
        memory free — eleven segments of funding and then the escrow.
      handoff: the backedge is a legal fire point because iteration `i - 1` has
        fully returned and `entities[i]` is unread, which is
        `rfc/model/gc/strategies.md`'s own "between mutator operations". It
        rests on a precondition `rfc/model/memory/bulk-operations.md` now
        states: the caller severs every traced edge to an entry before
        submitting the vector.
- [x] S34.8 Move the escrow out of the TLS image into an allocator-issued floor
      done: the escrow's storage is one 64 KiB pool block drawn at
        `ll_thread_init` before the best-effort reserve fills and returned in
        `retire_the_journal` after `queue::drain`; a refused draw fails thread
        init through `ll_thread_init`'s new status return; a thread that never
        ran init draws its floor lazily at first enrol, through the ordinary
        door, and its refusal aborts; the lazy draw checks the exit phase and
        aborts past `ll_thread_exit` instead of drawing; the block is stamped
        `BLOCK_KIND_ARENA`; `ESCROW_ENTRIES`, `POLL_STRIDE` and the poll order
        are unchanged, and the accounting tests name the floor block
      tier: T2 · role: Critic
      handoff: the ruling is `rfc/dev/DECISIONS.md` "the escrow's floor is
        allocator-issued" (2026-08-28): per-life floor, a re-birth refusal
        refuses the new life through the status return, and memory-hard
        thread creation is a recorded trade, not a derivation.
      Critic 2026-08-29 round 1: five findings, every one verified before it was
        executed. `draw_floor` was not re-entrancy-safe and leaked a block per
        registered thread under `debug-journal`; the ignore on the unregistered
        thread's test gave a false reason; the status return had no enforcing
        mechanism; a superseded TLS figure was quoted fresh in the module doc;
        the drain test passed for a shallower reason. All five repaired.
      Critic 2026-08-29 round 2, against those repairs: `ll_thread_init` funded
        the thread without first asking whether anything would run its
        teardown — the same question `draw_floor_or_abort` asks three lines
        away — so a thread past the destruction of its guard's slot kept a
        floor, two spares and two reserves for the life of the process; the new
        leak test's exact count was unstable under a full `debug-journal` run,
        the registry's deferred ring frees landing inside its bracket; and
        `#[must_use]` stops at the crate boundary, where every `bench-external`
        probe called the new signature bare. All three repaired; the device is
        dropped here, at two rounds.
      handoff: closed 2026-08-29 at 530 tests — three runs at four threads,
        `hash-folding` 530, `debug-journal` 535, release with no warnings,
        `cargo bench --no-run`, `cargo build --examples`, `cargo fmt --check`
        clean. Every new test was seen failing against a mutation of what it
        names: the floor never returned, the floor unstamped, the drain taking
        it, init ignoring the refusal, the lazy draw arming nothing, the
        re-entrancy re-check deleted, the teardown check deleted.
      handoff: the measurement the ruling asked for is `dev/BENCHMARKS.md`,
        "the escrow's move out of TLS": `.tbss` 65 784 bytes before against 496
        after, both arms taken back to back on this box. What replaces them is
        one 64 KiB block per live thread, so every exact `blocks_out` counts one
        more per thread — the accounting tests say so where it matters.
      handoff: `ll_thread_init` now answers `bool`, and `false` has two causes:
        a refused floor, and a thread whose teardown will never run. Three
        self-initialising paths are exempt from reading it
        (`dev/DECISIONS.md`, "`ll_thread_init` answers"); everything else
        asserts. The re-entrancy trap that produced the round-one leak is
        `dev/POSTMORTEM.md`, "a draw re-entered itself through the journal".
- [x] S34.9 Take the pool's memory from the OS, and delete Rust's allocator from its path
      done: `carve_region` and the large-run path map their memory from the
        operating system — `mmap` on unix, `VirtualAlloc` on windows — and
        answer null when it refuses; no path reachable from `BlockPool::get`
        calls Rust's global allocator, the refill batch and the thread cache
        being fixed arrays and the region registry living in mappings
        of its own rather than in a growable collection; a test forces the operating system to refuse
        both mappings this path makes — the region and the registry's
        chunk — and reads a report instead of losing the process, and a
        refused carve is driven through `BlockPool::get` so the refill
        loop's own accounting runs on the refusal branch
      tier: T2 · role: Critic
      handoff: ruled by Edmond 2026-08-29. `BlockPool::get`'s own contract says
        "nothing on this path aborts", and three sites break it, because a
        `Vec` that cannot allocate calls `handle_alloc_error`:
        `Vec::with_capacity(REFILL_BATCH)` in `take_block`, the thread cache's
        `blocks.append`, and `regions.push` in `carve_region`.
        `memory/critical.rs` and `memory/heap.rs` already refuse that failure
        mode by hand and say why.
      handoff: it also pays a debt `memory/stdapi.rs` records in its own module
        doc: while regions come from `std::alloc::alloc`, this manager cannot
        be installed as Rust's `#[global_allocator]`, because region carving
        would re-enter `ll_alloc` with an alignment it refuses.
      handoff: S34.8 depends on it. That step reports a refused floor block
        through `ll_thread_init`'s new status return, and the report is only
        true once the draw can refuse without killing the process.
      handoff: closed 2026-08-29 at 524 tests — three runs at four threads,
        `hash-folding` 524, `debug-journal` 530, release with no warnings,
        `cargo bench --no-run`, `cargo fmt --check` clean once rustfmt was
        installed on this toolchain. Every new test was seen failing against a
        mutation of what it names: the chain built newest-first, the registry
        refusal ignored, a refused region reported as carved, the `blocks_out`
        undo deleted, the short batch's remainder dropped.
      handoff: two Critic rounds on Fable, and the second found the first's
        repairs wanting, which is why the device is dropped here rather than
        after one. Round one: the `MAP_ANONYMOUS` fallback was wrong on
        android and solaris, the criterion's refusal test did not exist and
        had no seam to be written through, `bases()` rebuilt the `Vec` abort
        under the registry lock, and `CachedBlocks::extend` could fill the
        slot `put` needs. Round two, against those repairs: the visitor form
        ran arbitrary code under a lock the allocator takes — the rule
        `memory/large_entity.rs` states — so the registry's read path became
        lock-free, `len` and `next` atomics published by release stores;
        `mmap`'s `offset` was declared `i64` where 32-bit unix has a 32-bit
        `off_t`; linux mips defines `MAP_ANONYMOUS` as 0x800 and slipped
        through the gate built to stop it; the fault arming leaked on a panic
        and became RAII.
- [ ] S34.10 Give Miri back a tree it can run
      done: a targeted Miri run over a module that carves a region completes
        instead of reporting undefined behaviour at `memory::os::map_aligned`,
        and `dev/WORKFLOW.md`'s Miri section states what the arm costs — which
        UB class Miri stops seeing in exchange
      tier: T2 · role: Critic
      handoff: found on 2026-08-29 while running Miri over S34.8's own pointer
        writes. `map_aligned` over-maps and trims the head and the tail with
        two partial `munmap`s, which is correct POSIX and which Miri's shim
        does not model: it reports "incorrect layout on deallocation" and stops
        the run. The first `BlockPool::get` of any test carves a region, so
        this reaches every test that allocates — `refcount::tests::`
        `who_may_read_a_header` passes only because it never asks the pool.
      handoff: it is S34.9's debt, found after that step closed. Before it,
        regions came from `std::alloc::alloc` and Miri ran; the WORKFLOW's Miri
        section still describes a capability the tree has not had since.
      handoff: what was tried and works, as a local patch that was **not**
        committed: `#[cfg(not(miri))]` around the two trims and a `#[cfg(miri)]`
        no-op `unmap`, which leaves the over-map in place and leans on
        `-Zmiri-ignore-leaks`, the flag the WORKFLOW already prescribes. Under
        it, `cycle::queue` ran 18 tests and `memory::heap` 20, both clean. The
        cost is that Miri stops seeing anything about unmapping, which is the
        trade the step has to state rather than assume.

- [ ] S34.3 Parking a slot that dies enrolled
      done: death runs in full — weak cells cleared first, then `__destruct`,
        then children released — and the slot is withheld from the allocator
        while a queue entry names it; the drain reads the refcount, retires a
        zero-count entry, clears the bit and returns the slot without touching
        the body; the return is the crate's single slot-return path and the
        block's `used` falls **there and never at the parking**, proven by a test
        that empties a block around a parked corpse and shows the block reaching
        the pool only at the return
      tier: T2 · role: —
- [ ] S34.4 Prove the corpse rule against arena reuse
      done: a red-first test enrols, kills, resets the arena and drains, and the
        category-zero clause is what makes it pass
      tier: T1 · role: —

- [ ] S34.2 The law: only the owner reduces state
      done: no dirty pass clears an enrolment bit, drops a queue entry or
        returns a slot; a reader may mark an entry a corpse and pass it on; the
        bit is cleared only at death — the drain's corpse rule, or commit's
        free — and **never at acquittal**, which supersedes Y12 clause 4's
        "cleared after the root is walked"; a test proves the acquittal case —
        ring A↔B with an external X→B that is released after the trace read the
        count — does not lose the ring, and the assertion is that a later
        collection reclaims it, not merely that the bit is still set
      tier: T2 · role: Critic
      handoff: clause 4 and the law of 2026-08-26 contradicted each other, and
        both were in the plan. The Sage ruled for the law: clearing on acquittal
        is the permanent miss, because enrolment is edge-triggered.
      handoff: the instant this step's test waits for was ruled on 2026-08-27
        (`rfc/dev/PLAN.md` S8.3, and the entry it names in
        `rfc/dev/DECISIONS.md`). An acquitted root parks in the owner's own
        suspects buffer with its bit set, and the owner splices that buffer onto
        its live queue at the first safepoint poll that finds the maturation
        epoch counter moved. The test therefore forces the counter forward
        through a `#[cfg(test)]` shorthand, runs the poll, runs a collection and
        asserts the ring reclaimed, which is the assertion this step demands
        instead of "the bit is still set".
      handoff: **it waits on three later stages, and the plan's order had it
        first.** Every clause but one is about code that does not exist: the
        dirty pass is S35's, the corpse mark S34.3's and commit's free S36.6's.
        The one clause that is about today's code — the bit is never cleared at
        acquittal — holds vacuously, nothing in the crate clearing `ENROLLED`
        at all (checked 2026-08-29; `refcount.rs` only sets it). And the test
        the step demands needs a collection to assert reclamation, which
        `gc::ll_gc_collect_cycles` does not have until S36.7, plus the
        maturation counter of S37.1 and the suspects buffer of S37.4 for the
        instant it waits for. Moved last in the stage for that reason; the work
        order takes it after S37.4.

## S35 — Mark and scan

Goal: trial deletion runs entirely in the shadow rows.

- [ ] S35.1 Mark
      done: the trace decrements children's working counts in their rows and
        writes nothing into any entity; children are enumerated through
        `cells::trace_cells` — the tracer moved at S30.2, not a second stride —
        and `cycle::row::edge_to` runs per yielded child in the
        collector's visit; an aborted mark leaves the heap byte-identical,
        proven by hashing every block on the touched list before and after, with
        the abort forced at a depth past the first descent rather than at the
        first instruction
      tier: T2 · role: —
      handoff: this clause stands verbatim against S37: the maturation stamp is
        written by commit and only read by the trace, so no write into an entity
        happens during a mark. A retained block's edge still takes the registry
        mutex once per edge in `retained::occupant_index`, and this is the step
        that gives the trace a per-block visit to hold the index's `Arc` over —
        the row array already holds the length that visit would bound against. The "retired on contact" clause that contradicted
        it was withdrawn 2026-08-26. Open, from the Critic round over the
        block-kind dispatch: `Edge` has
        two variants, and the age prune is a third answer — a child inside the
        GC heap, with a row, that must not be descended into. Either the mark
        resolves the prune before calling `edge_to`, and the "one dispatch per
        child" clause the dispatch was built under becomes two, or `Edge`
        grows a variant and the touched-block list can still tell a matured
        child from a child outside the heap.
- [ ] S35.0 Repoint what `dev/INDEX.md` says about tracing an array
      done: the paragraph on the array's tracing stride names a mechanism that
        exists — no `collector::Epoch::storage_versions`, no per-walked-row
        version kept by an epoch, no `collector::tests::…::measure_parked_memory`
        — and says instead what the version bracket is read by now, or says that
        nothing reads it yet and which step gives it a reader
      tier: T1 · role: —
      handoff: found 2026-08-27 by the pass-2 check of `dev/WORKFLOW.md`, which
        the previous run reported empty. Two of the four sites were the
        mechanical rename `walk::trace_cells` → `cells::trace_cells` and are
        repaired; the other two describe the deleted epoch's own machinery, and
        repairing those means saying what reads a storage version in `rc-cycle`,
        which is this stage's question and not a rename.
- [ ] S35.2 Scan
      done: a non-zero working count marks its reachable set live, a zero one
        leaves it white, and the pair is proven on two graphs — a ring with an
        external reference into its middle, which must survive, and the same
        ring without that reference, which must go white — because a scan that
        marks everything live passes the first alone
      tier: T2 · role: —

## S36 — Commit

Goal: only the owning thread frees, and it frees what the judge condemned and
the exact test confirmed. The teardown is here in full: the Critic round of
2026-08-26 found the stage claiming the frees while building none of them.

- [ ] S36.1 The exact test on the owner's thread
      done: current fields are re-read on the owning thread before any free, and
        the test opens with the corpse rule — a member read at count zero drops
        the component whole before any guard or field write — exercised by a
        test in which tearing down one component releases into a second already
        judged white; the refusal path is exercised by a mutation racing the
        verdict, and by a positive control in which the same scenario without
        the mutation does free
      tier: T2 · role: Critic
- [ ] S36.2 The epoch parking
      done: a slot freed inside a collection waits for its end, releases into
        S34.3's single return path, and that path refuses while **either**
        window is open — a queue entry naming the slot, or a collection in
        flight; a red-first test shows the defect it prevents, a reused slot
        inheriting the dead occupant's row, and overlaps the two windows in both
        orders
      tier: T2 · role: —
- [ ] S36.3 The guard and the weak window
      done: after the exact test confirms, every member takes the teardown
        guard, then every weak cell naming any member is nulled, all members
        before any destructor; a condemned ring A↔B with a weak cell on A, whose
        `__destruct` on B loads that cell, reads null inside the destructor —
        seen red against the per-member order before the component-wide call
        lands; a component the exact test refuses leaves every cell resolving
      tier: T2 · role: Critic
      handoff: the guard is needed even single-threaded — a destructor releasing
        an internal edge would otherwise drop a member to zero and start
        ordinary death inside the teardown. The window is the one PEP 442 exists
        to close.
- [ ] S36.4 Destructors and the resurrection re-verify
      done: `__destruct` runs per object member on the owning thread; when any
        ran, the exact test runs again with the guard discount; a failure
        releases the guards through the counted path, so survivors keep true
        counts with destructors still ahead of them, and the component is
        abandoned with its cells nulled; a destructor that stores `$this` into
        an external root proves both the acquittal and the nulled cell — the
        divergence from PHP that `weak-references.md` records
      tier: T2 · role: Critic
      handoff: the re-verify survives the shortlist framing rather than
        contradicting it. Garbage is monotone only while no reference to the
        component exists outside it, and the destructor runs holding `$this`, a
        reference the teardown itself handed to user code.
- [ ] S36.5 Sever, free and the deferred drops
      done: internal edges are severed with external children collected; the
        guards come off through the counted release and each member reaching
        zero dies through the ordinary death path into S36.2's parking; the
        deferred-drop queue — severed children and the weak notify's displaced
        map values — drains only after the last member's free, the order proven
        by a test-only sequence probe; a weak cell re-created on a condemned
        member is cleared by the free-time `HAS_WEAK_REFERENCES` notification
      tier: T2 · role: Code Reviewer
- [ ] S36.6 Commit writes the maturation stamp
      done: on the owning thread, after judgement, each proven-live component is
        stamped as a unit — current epoch and `min(age) + 1` saturated at 3, one
        single-byte store per member, never inside a wider access; a condemned
        or unjudged component is never stamped; in the accelerator form the
        posted proven-live components are stamped by the owner at its drain, so
        the stamp byte has one writing thread in both forms; a test matures a
        live ring across two collections and shows the third pruning it, read
        off the S37.1 counter
      tier: T2 · role: —
      handoff: commit is the only writer because a mature stamp suppresses
        descent, which is a reduction of future suspicion and therefore the
        owner's by the law of S34.2 — and because S35.1's zero-write mark is
        what makes an aborted collection free.
- [ ] S36.7 Wire the collection into the ABI
      done: `ll_gc_collect_cycles` runs a collection and reports what it
        reclaimed, and `ll_gc_maybe_collect` fires on the armed pending flag and
        nowhere earlier; a test arms the flag, shows nothing collected before
        the next poll, and shows the collection at it — restaging the
        deferred-fire contract the dying `gc/tests/where_a_collection_may_fire.rs`
        carried
      tier: T2 · role: —

## S37 — Maturation and the two class gates

Goal: the trace stops following the whole heap. On a booted Laravel corpus the
subgraph reachable from a median candidate root is 381 of 381 objects, so this
stage is what makes a trace affordable rather than what tunes it.

- [ ] S37.1 The maturation stamp is an edge-side prune
      done: mark's descent reads the stamp with one single-byte load; a member
        whose stamp epoch equals the heap's current epoch (mod 4) and whose age
        has reached `k` is treated as an **opaque live external and is not
        descended into**, the same test skipping a mature popped root entirely;
        a stale-epoch stamp reads as age 0 and is never cleared in place, so the
        trace writes no stamp; the per-thread-heap collection counter advances
        the epoch every 64 completed collections and `k = 3`, both named
        provisional after YRC's only known values, with the measurement owed at
        S40.1; `#[cfg(test)]` counters report edges pruned and roots skipped
        mature per collection
      tier: T2 · role: —
      handoff: the root-side reading — "traced only after it has stayed a
        candidate across `k` collections" — was struck from this step and from
        `rc-cycle.md`'s summary bullet on 2026-08-26. It is not a second
        mechanism: it filters which roots start a trace and does nothing to the
        closure, and its real content falls out of the prune at depth zero.
        Y9 calls the prune the only mechanism in this design that bounds the
        closure.
      handoff: carried from S31 before that stage was deleted. **Two producers
        hand a member a stamp byte nobody wrote.** A recycled `heap::FreeSlot`
        preserves the dead entity's final header, so the slot arrives carrying
        the previous occupant's byte 6; and promotion rewrites a survivor's
        category with a two-byte store, so the byte leaves the arena exactly as
        it went in. Either one reads here as a mature stamp of the current
        epoch and this step prunes a live subgraph permanently and silently.
        The zeroing belongs to S38.0; this step's counters are what would show
        it missing.
- [ ] S37.4 The suspects buffer and the turnover re-offer
      done: acquittal never clears the enrolment bit; a proven-live root parks
        in the suspects buffer with its bit set, and every suspect is re-offered
        at the first collection after the heap's epoch advances; red tests prove
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
- [ ] S37.2 The acyclic gate
      done: the factory stamps bit 8 from the class's own answer — waits on
        `rfc` `model/classes.md` declaring a target per pointer slot
      tier: T2 · role: —
      handoff: blocked outside this repository; the step is listed so the
        dependency is visible rather than discovered.
- [ ] S37.3 The ownership mark
      done: a proven-owned entity never enters the candidate set, and the
        compiler's stamp is honoured at bit 9
      tier: T2 · role: —
      handoff: `refcount::ENROLMENT_GATE_MASK` already tests bit 9 and no
        compiler stamps it, so this step's remaining content is the
        factory-side and FFI-side write.

## S38 — The claim and concurrency

Goal: a collection runs either in a collector thread or in the mutator, never
both, and the losing side never deadlocks.

- [ ] S38.0 The collector's reader
      done: a second `CellReader` impl reads `word` and `ptr` as relaxed atomics
        and answers nothing else — no storage version and no give-up, because a
        torn read costs at most a phantom edge or a missed one, and a child
        mapping to no GC-heap block already ends the descent as an external live
        reference (`cycle::row::edge_to`); the collector-thread trace
        instantiates `trace_cells`
        with it, and a Miri slice drives it over an object with outside cells
        and over an array mid-move
      tier: T2 · role: Critic
      handoff: `RelaxedCells` and its re-check plumbing died at S30.2 because
        they existed for `rc-walk`'s precision. What the accelerator needs is
        strictly smaller, and the `CellReader` trait is the socket it plugs into.
      handoff: two debts carried from S31 before that stage was deleted. **The
        stale stamp byte** is this step's to zero, at both producers — a
        recycled `heap::FreeSlot` and a promoted survivor — because this is
        where the second thread arrives and the byte stops being inert; S37.1
        is what it breaks. And **`dev/WORKFLOW.md`'s ThreadSanitizer run has
        selected no test since 2026-08-26**, its only one having lived in the
        deleted `collector::`, so the instrument that reports
        plain-against-atomic is         unavailable until this step gives it a pairing
        to watch.
      handoff: the collector thread's birth is this step's to name — startup
        or first pressure — with its floor refusal following it; a mandatory
        floor drawn at first pressure is the worst moment
        (`rfc/dev/DECISIONS.md`, "the escrow's floor is allocator-issued").
- [ ] S38.1 The claim
      done: one flag **per mutator thread**, free or held, taken by CAS and
        released by one store; a waiter blocks on a mutex rather than spinning;
        it covers the **trace** over that thread's graph — the arena, the block
        triples and the touched list — while each owner's exact judgement runs
        at its own checkpoint; a test proves a held flag blocks collection
        entry on the claimed thread alone while enrolment, release, allocation
        **and a second collector's trace of another thread** proceed
      tier: T2 · role: Critic
      handoff: the word was per process until 2026-08-29, when Edmond ruled the
        exclusion per thread (`rfc/dev/DECISIONS.md`, "a trace stays inside the
        blocks of the thread it claimed"). What licenses the narrowing is the
        transfer rule: `thread_move` and `thread_clone` require the graph
        arriving in a thread to hold no reference to an object that stays in
        the source, so no thread names an entity in another thread's blocks,
        and a block belongs to one thread's heap. Two collectors therefore
        never meet in a block, a triple or a row.
      handoff: the mechanism is Edmond's, 2026-08-29: the flag is the mutator
        thread's own, a collector going to judge that mutator takes it, and CAS
        with a mutex to wait on is the whole of it. That settles a
        contradiction this step had carried since it was written — it asked for
        three states and a thread-local held flag, which the ruling of
        2026-08-27 (`rfc/dev/DECISIONS.md`, "the trace token covers the trace
        alone") had already rejected by name. The narrowing to one thread does
        not revive them: a thread meets its own flag held only by a collector,
        never by itself, because mark and scan run no user code and the
        teardown that does runs with the flag released and the entry gate shut.
- [ ] S38.4 The entry gate and the slow-path fire   *(before S38.2)*
      done: the GC-heap slot allocation slow path, on a forced refusal that
        names which allocation refused, waits on this thread's own claim while
        it is held, takes it, runs the in-line collection, retries once and
        reports null only after; a shortage at teardown depth ≥ 1 collects
        nothing and reports; a heap of one size class full of cyclic garbage
        serves the allocation with no explicit collect call
      tier: T2 · role: Critic
      handoff: Y14's clause "a thread that finds the token taken does not wait"
        was argued from the handshake deadlock, and the amendment of 2026-08-26
        deleted the handshake, so the Sage retired the clause with its reason
        and generalised the wait to any non-self holder. That generalisation is
        recorded in `rfc` (Y14 and `rc-cycle.md`, Concurrency) as well as in
        `dev/DECISIONS.md`; it is a decision of the round, not of the design of
        record as it stood.
      handoff: "any holder but itself" and "a claim this thread already holds"
        were the process-wide word's phrasing, where a holder could be another
        thread's collection. With the flag per thread (S38.1, Edmond
        2026-08-29) the only holder of this thread's flag is a collector
        judging it, so the wait needs no holder identity and the self-held arm
        has no entrant; what stops a collection at teardown depth is the gate,
        which is unchanged.
- [ ] S38.2 The working wait
      done: an in-line collection needs no verdict list, no handshake and no
        second phase — it is exact with respect to the counts because the owner
        re-reads its own current fields — and a mutator that cannot allocate
        while the claim is held in either state waits for the trace to end
        rather than preempting; the test's running collection is staged by
        S38.1's harness seizure and reaches the wait through S38.4's path, with
        a `#[cfg(test)]` counter past the wait asserted non-zero, because a test
        that merely terminates terminates most easily when the wait is never
        taken
      tier: T2 · role: Critic
- [ ] S38.3 Parking the mutator's frees during a trace
      done: while a collection is in flight over a thread's blocks, that
        thread's frees park until it ends; the cost is measured as the churn
        held across one collection
      tier: T2 · role: —

## S39 — Thread exit  (carried from S29.2)

- [ ] S39.1 Exit drains its own queue
      done: `ll_thread_exit` retires its queue before handing the heap over, and
        the fate of a live enrolled entity at exit is **chosen** — which of
        collect, hand over or leak, and why — rather than described in a
        comment, with the test that observes the chosen fate named; a red-first
        test kills a thread between enrolment and collection
      tier: T2 · role: —
      handoff: the criterion previously read "named rather than left to the
        reader", which a doc comment saying "these leak" satisfies with S29.2's
        defect intact.

## S40 — Measure the trace's density and decide the row form

Goal: the one number the design still lacks.

- [ ] S40.1 Measure
      done: the share of a touched block's slots that a real collection traces
        is measured on the corpus and on a synthetic load, with the denominator
        named — occupied slots or all slots, which differ by two at the design's
        assumed half occupancy — and with the instrument checked against a
        synthetic block whose traced share is fixed by construction; the same
        instrumented run records the pruned-edge share and the suspects re-offer
        volume at `k` of 1, 2 and 3, which is what settles S37.1's two
        provisional constants
      tier: T2 · role: Bench
      handoff: the corpus arm needs a driver over `ll-model`'s own heap. The
        recorded corpus instruments read PHP's heap, which has no blocks and no
        slots, so this arm is Phase-D-blocked in the same way S37.2 is blocked
        on `classes.md`; the synthetic arm is not, and it is what the row form
        can be decided on if Phase D is far.
- [ ] S40.0 Redraw `docs/architecture.md`'s diagrams
      done: the four modules the diagrams show that no longer exist are gone
        from them, `rc-cycle`'s own boundaries are drawn as built, and the
        banner added on 2026-08-26 comes off with them; `dev/ARCHITECTURE.md`
        stays the source of truth and the two agree
      tier: T1 · role: —
      handoff: the debt of S30.5, which left the diagrams standing under a
        banner rather than redrawing them — a diagram of an unbuilt collector
        reads as structure that exists. It sits here rather than earlier
        because the boundaries are not real until S38 closes; S40 is simply
        the last stage that will still be open.
- [ ] S40.2 Decide chunks or not
      done: below 29 % density the chunked form replaces the flat array and the
        measurement is quoted in the decision, with its denominator; above it
        the flat array stands and the alternative is recorded as refused with
        its number
      tier: T2 · role: —

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
check's reversal threshold, the compaction threshold taken from Zend at
about 3 %, and the flood ladder's two, `EQUAL_HASH_LIMIT` and
`CHAIN_LIMIT`. None of the four can be settled on this box —
`dev/BENCHMARKS.md` puts its noise floor at 1.5–3 %, and every effect in
question is smaller than that. So this is not a task waiting for someone
to pick it up: it waits for a machine that can resolve it, and measuring
here would produce a number indistinguishable from noise and harder to
retract than to publish.

## The vocabulary

The crate's identifiers and test file names follow the glossary
`rfc/dev/PLAN.md` S9 builds; nothing here is renamed before S9.1 rules on it.
Two words are already marked for rename. Counted in this crate on 2026-08-29,
after commit 8208815: `door` at 110 occurrences in the code and 80 in the
documents, `escrow` at 88 and 34.
`ResetWindow::escrow` in `src/memory/reset_window.rs` names deferred count
corrections rather than the queue's overflow buffer and takes a different name.

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
  future. "Only the GC reads `traced_runs` as data" holds once generated
  disposes replace the stand-in. `rfc/runtime/object-lifecycle.md`.
- [ ] **A7, no zeroing by default.** `ll_object_new` zero-fills the whole
  body unconditionally; which slots need a defined initial state is the
  factory's to decide (`rfc/BACKLOG.md`, deferred optimizations).
- [ ] **`Lazy` (code 1) and `Box` (code 10) have no producer.**
  `ll_entity_die`'s switch serves five; Box waits on the FFI surface and
  Lazy on the compiler, and each reaches a `debug_assert!` meanwhile.
  `Lazy` nevertheless answers yes to `EntityKind::closes_a_ring`, on the
  argument recorded in `dev/DECISIONS.md`, "a kind's ring classification is
  written at its declaration, before a factory stamps it". `StringDynamic`
  (code 9) is not carried here: `string::reserve` stamps it whenever the
  placement is out of line, so the kind has a producer.
- [ ] **The threshold arming policy and the collector-thread accelerator.**
  What is left of the old escalation ladder after S38.4 builds the entry gate
  and the slow-path fire. The arming policy is the compiler's
  (`rfc/model/gc/strategies.md`, arm/fire); the accelerator carries the third
  claim state's production entrant, the proposal machinery that turns a dirty
  trace into a shortlist, and the owner-checkpoint judgement that reads it
  (`rfc/model/gc/rc-cycle.md`). Gated on a measured in-line pause that a
  collector thread would shorten — until then the in-line collection at a
  failed allocation is the whole trigger, and that is the design in force
  rather than a stopgap (Y14, amended 2026-08-26).
- [ ] **The birth count and the unique-owner policy**
  (designed 2026-08-17 in the old collector's document, sections "The birth
  count" and "Unique ownership"; S30.5 moves the text into `rc-cycle`'s
  documents or it goes with the file) — gated on a Phase D measurement of
  the share of dynamic publications with compiler-provable targets; the
  move rule (copy, barrier, or a never-moved proof) is the open design
  question.
- [ ] **Pure destructors, and the hand-off drain** — proposed by
  Edmond 2026-08-18, analyzed the same day in
  `dev/design/pure-destructors.md` through three lenses and two Critic
  rounds. The runtime-only step (the specialized P0 dispose and the
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
  named `proof-horizon` until 2026-08-20) — closed, and no pre-D step can
  change that status: the scan is kill-only, the census is undated, every
  verification artifact needs the compiler. Pre-D work is instrument
  preparation: the graded corpus scan, the census channel list owed to
  `dev/DECISIONS.md` before the census is specified, the summary-language
  question. Three Critic rounds, the granularity ruling of 2026-08-18
  (`dev/DECISIONS.md`), Edmond's corpus names and his
  family-borrow-analysis and summary-language rulings, and the five
  questions the 2026-08-20 case book opened — the weak cell's uncounted
  edge, promotion in the arena and immortal categories, raise sites in the
  placement rule, the COW-unique intersection, and runtime entries read as
  calls — were all written in documents S30.5 deletes. The arguments that
  outlive them move where S30.5 names; the rest is at
  `archive/pre-rc-cycle`.
- [ ] **Strategy 1, the typed vector.** No producer, so the 1 → 2
  transition waits on one — `dev/DECISIONS.md`, 2026-08-13, which also
  says what to confirm against `arrays.md` before opening it.
- [ ] **The rest of the language runtime**, listed in `rfc/BACKLOG.md`:
  exceptions, actors, closures, enums, generators and fibers, resources,
  generics, stdlib, I/O.
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

- [ ] **The ladder's refusal has nowhere to go.**
  `InsertOutcome::RefusedByLadder` is answered inside the crate — a null
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
  `Table::reseed`, `InsertOutcome::RefusedByLadder` to `CHAIN_LIMIT` —
  which `rustdoc` warns about unless private items are documented too.
  Crate-wide practice rather than one site, so it is a ruling and not a
  fix: either the links stay and `--document-private-items` becomes how
  the crate's documentation is built, or they become plain names. Raised
  by S27's Code Reviewer, 2026-08-18.
- [ ] **The per-process key's Windows door.** S27.1 lands unix-only,
  `#[cfg(not(unix))]` a `compile_error!` naming this gap, so the
  Windows build refuses until a session on the Windows box adds the
  door (`BCryptGenRandom` or an equivalent OS draw) and runs the gate
  there. Deferred by Edmond, 2026-08-17.
- [ ] **No ABI entry creates or mounts an arena.** `LLContext` is
  `#[repr(C)]` with one public pointer and a null context is legal, so an
  external caller can build one and reach the store barrier; what it
  cannot obtain is an `*mut Arena`, every arena in the crate being made
  by Rust code inside tests. An embedder needs that door before anything
  outside this crate exercises the arena paths.

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
  it; `deferred_free::dispose` is the existing shape for that.

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
  `stdapi::ll_realloc` allocates, copies and frees on every call, so 40
  bytes to 48 costs a block, a `memcpy` and a free to move inside one
  48-byte slot. `stdapi::ll_usable_size` already reads the class size out
  of the block header, so the test is one comparison on a path that is
  cold anyway. rpmalloc also declines to move a huge block that shrinks
  by less than half, and overallocates to 1.375x on a small growth so
  that a loop growing a few bytes at a time stops reallocating at every
  step (`rpmalloc.c:2402`, `2413`, `2429`).
  **What comes first:** a harness. `rptest` in `benches/standard.rs`
  frees and allocates rather than reallocating, so this path has no
  measurement at all, and nothing in the runtime calls it either — it
  serves the raw C surface.

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
  (`cycle::row::edge_to`);
  a uniform stride would make them walkable, which `rc-walk` decided the
  other way and `rc-cycle` re-decided by dispatching on the block's kind.

- [ ] **A flag saying the block already reads zero.** `Heap::refill`
  writes eight bytes into every slot of an entity block unconditionally —
  up to 4080 stores at the 16-byte class, and at a 16-byte stride that
  dirties every line of the 64 KiB block. The invariant is narrower than
  the pass: the walker reads only slots below `bump` and tests one field.
  Two sources of the same knowledge exist and neither is used. A region
  taken with `alloc_zeroed` instead of `alloc` is untouched kernel
  memory. A block returned empty from an entity heap already satisfies
  the invariant, because `FreeSlot` preserves the dead entity's final
  header and an entity dies at refcount 0. What breaks it is a block that
  served as raw or arena memory in between, or a recommissioning at a
  different stride, so the flag has to name the stride it holds for.
  **What comes first:** the case that shows the cost. Amortised over the
  steady-state benchmarks it is small, refill running about 0.00003 times
  per allocation; the workload to measure is a growing one, where the
  pass is one extra store per object created.

- [ ] **Return memory to the OS, and cache huge mappings** — blocked on
  the prerequisite the head of `stdapi.rs` already names: regions come
  from `std::alloc::alloc`, not from mmap. rpmalloc lets free pages
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
