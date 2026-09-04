# Plan

Implementation plan for `ll-model`, re-sorted 2026-07-24 against the RFC
after the 2026-07-22 object-layout redesign landed in the `rfc` repo.

Design lives in `rfc` and is authoritative — read before coding, do not
re-derive: `model/classes.md`, `model/values.md`, `model/lowering.md`,
`model/gc/rc-cycle.md`, `model/gc/cycle/questions.md`, `model/memory/ffi.md`,
`runtime/object-lifecycle.md`.

The `rfc` repository carries its own plan at `dev/PLAN.md` for work that lands
in the specification rather than in this crate.

Updated: 2026-09-04 · Active: S43, from S43.2, the stage amended the same day
to the form Edmond ruled: the chain stays and the mark answers its refusal. The
rest of S36 stands behind it — S36.9's remainder and S36.12's slice (b) each
wait on a ruling named in their own step. The sections after S40 are the
backlog. S43 sits between S36 and S37, where it is to be done.

**Closed stages are deleted whole** (rule 23.1.3), and what outlived each
of them is in the journals rather than here: `dev/DECISIONS.md` for a
decision and its reason, `dev/POSTMORTEM.md` for a trap,
`dev/BENCHMARKS.md` for a measurement, `dev/INDEX.md` and
`dev/ARCHITECTURE.md` for the map. Deleted so far: S4 through S33, S35,
S41 and S42. A number is never reissued, so a
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

**Every cycle-GC improvement has two review gates.** Before the first code
edit, the Sage reviews its operation count, manager-allocation budget,
cache-working set, lifetime and refusal model, and records the pre-change
counter/benchmark baseline the step would otherwise erase. A red test is then seen failing;
after the implementation, the Critic reviews the repair and its mutations
before the step can be checked. Both findings are recorded in the step's
handoff. This applies to S36.9 onward and to the performance steps in S37/S40;
one broad review does not waive a later step's gate.

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

- `memory::reset_window` keeps a vocabulary of its own — `CORPSE_WALKS` and
  `park_large` beside `ResetWindow::escrow` — and
  `cycle::tests::the_metaphors_the_names_still_carry` and
  `..._the_comments_still_carry` exempt the two by name, with the reason in
  the exemption. The words exist now: `rfc`
  S9.1 named `escrow` the *deferred increment* list, `credits` the deferred
  decrement one, `park_large` a *deferred free*, the window's `corpse` a
  *torn-down entity* and the sweep-list `enrol` an *attachment to the touched
  list* (`rfc` `9ca669c`, `dev/CYCLE-TERMINOLOGY-AUDIT.md`, "Glossary check").
  No step owns the rename, and the four exemption reasons that said the
  glossary was silent now say this instead.

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
        is the collection's, S36.7, and the accelerator's swap S38.1's, so
        nothing yet agrees
        with a detaching reader about the fill cell — `rfc/dev/PLAN.md` S8.7.
        The drain at thread exit returns blocks and drops entries, which S39.1
        turns into a chosen fate. The corpse rule and the marks a reader writes
        into an entry's low bits are S34.3's and the trace's; the four bits are free
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
        doc: while regions came from `std::alloc::alloc`, this manager could not
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
- [x] S34.10 Give Miri back a tree it can run
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
      Critic 2026-08-29: four findings. The no-op `unmap` above would have
        disarmed three tests in `promote::tests::the_reset_reads_no_corpse`
        that name Miri as their whole regression, which is rule 4's
        prohibition applied to a tool rather than an assertion; the stated
        cost was incomplete, an untrimmed mapping leaving a 64 KiB readable
        apron at each end where an overrun past a region becomes invisible;
        the evidence exercised only the mapping half; and the `unmap`
        comment gave the wrong reason for the shim's refusal. All four taken.
      handoff: closed 2026-08-29, and **not** by the patch the line above
        describes. Under `cfg(miri)` the trims are skipped and a table in
        `os.rs` remembers `(aligned, base, over)`, so `unmap` hands back the
        whole mapping — an exact-layout deallocation the shim accepts, which
        keeps an unmap an unmap. Verified by running: `cycle::queue` 20 tests,
        `memory::stdapi` 14, `memory::large_entity` 5 — the last being the
        module that returns mappings, and neither an "incorrect layout" report
        nor the panic `unmap` raises on a missing table entry appeared.
      handoff: **what is not verified, and it is the Critic's first finding
        turned into a question.** The three `promote` tests can run again, but
        whether they still exhibit their defect is unknown: the reconcile one
        was run with `reset_window::park_large` returning false and passed in
        176 s. Either that is not the mutation their comments mean, or the
        arrangement needs a second half. They have been unrunnable under Miri
        since 2026-08-26, so the claim in their doc comments has been stale
        for three days and is not this arm's doing. Re-arming them is nobody's
        step.

- [x] S34.3 Parking a slot that dies enrolled
      done: death runs in full — weak cells cleared first, then `__destruct`,
        then children released — and the slot is withheld from the allocator
        while a queue entry names it; the retirement reads the refcount, clears
        the bit and returns the slot without touching the body; the return is
        the crate's single slot-return path and the block's `used` falls
        **there and never at the parking**, proven by a test that empties a
        block around a parked corpse and shows the block reaching the pool only
        at the return
      tier: T2 · role: —
      handoff: **the criterion was amended on 2026-08-29 and the clause that
        moved is named here.** It said "the drain reads the refcount, retires a
        zero-count entry"; what the drain is has no answer yet — the reader
        that drains a queue is the collection's, S36.7, and
        `cycle::queue::drain` is thread
        exit's, whose fate for an entry S39.1 owns. Wiring the retirement into
        that drain today would dereference entries, and the queue's own test
        fixture writes bare `RcHeader`s on the stack rather than allocated
        entities, deliberately and with a comment saying why: "nothing on this
        path dereferences the entry it writes". A drain that read them would
        read freed stack memory. So the mechanism is here and the wiring is
        S39.1's, together with the fixture change it needs.
      handoff: what landed. `memory::stdapi::ll_free` withholds an entity slot
        whose header carries `ENROLLED`, ahead of every route and after the
        reset window's own two guards — one door, because every slot return in
        the crate reaches that one. Nothing is recorded: the queue entry is the
        record, so `used` falls at the return and never at the parking, which
        is what keeps such a block out of the pool. `refcount::is_enrolled` and
        `clear_enrolled` are the two accessors, the second carrying an
        `expect(dead_code)` naming S39.1 until a caller arrives.
      handoff: two tests in `memory::stdapi::tests::`
        `the_slot_a_queue_entry_names`, both seen failing with the parking
        deleted: a block emptied around a parked corpse reaches the pool only
        when the last slot returns, and a parked body still reads the count the
        death left. Every `S34.3` citation in `src/`, `docs/` and the two maps
        was swept in the same commit — the sites that named two windows now
        name the one that is left, which is S36.2's.
- [x] S34.4 Prove the corpse rule against arena reuse
      done: a red-first test enrols, kills, resets the arena and drains, and the
        category-zero clause is what makes it pass
      tier: T1 · role: —
      handoff: closed 2026-08-29.
        `cycle::queue::tests::an_arena_entity_leaves_no_entry` builds a real
        arena object, takes it through the decrement the gate judges, resets
        the arena and watches a fresh one be handed the same block. Red against
        the gate with `MEMORY_CATEGORY_MASK` removed: an entry appears, naming
        a slot the reset then gives away.
      handoff: **the first fixture was green against that mutation** and had to
        be rebuilt. `release_word` returns before the decrement for a non-zero
        category unless `COW` is set, so a plain arena object never reaches the
        gate at all and the test proved nothing. The entity is shared on write
        for that reason, which is the construction `the_enrolment_gate` uses
        for the same clause; the test says so where it does it.
      handoff: what the clause is worth, in one line, since the step exists to
        record it: a GC-heap slot that dies enrolled is withheld by the free
        (S34.3), and an arena slot has no free to withhold it — `ll_arena_reset`
        returns the block whole — so an entry naming one would survive into the
        next request's memory.

- [ ] S34.2 The law: only the owner reduces state
      done: no dirty pass clears an enrolment bit, drops a queue entry or
        returns a slot; a reader may mark an entry a corpse and pass it on; the
        bit is cleared only by the owner consuming the one token that names a
        dead entity — the drain's corpse rule, or S36.5 commit for an in-flight
        root — and **never by death itself or at acquittal**, which agrees with
        Y12 clause 4 as narrowed on 2026-08-26, "cleared by the owner only when
        the entity reaches zero count", rather than superseding the "cleared
        after the root is walked" that clause carried before; a test proves the acquittal case —
        ring A↔B with an external X→B that is released after the trace read the
        count — does not lose the ring, and the assertion is that a later
        collection reclaims it, not merely that the bit is still set
      tier: T2 · role: Sage → Critic
      handoff: clause 4 and the law of 2026-08-26 contradicted each other, and
        both were in the plan. The Sage ruled for the law: clearing on acquittal
        is the permanent miss, because enrolment is edge-triggered.
      handoff: the instant this step's test waits for was ruled on 2026-08-27
        (`rfc/dev/PLAN.md` S8.3, and the entry it names in
        `rfc/dev/DECISIONS.md`). An acquitted root parks in the owner's own
        suspects buffer with its bit set, and the first collection after the
        maturation epoch counter moves detaches that dormant lane beside the
        active lane as one composite in-flight batch. The test therefore forces
        the counter forward through a `#[cfg(test)]` shorthand, runs the poll,
        runs a collection and
        asserts the ring reclaimed, which is the assertion this step demands
        instead of "the bit is still set".
      handoff: **it waits on three later stages, and the plan's order had it
        first.** Every clause but one was about code that did not exist when
        the step was written: the dirty pass exists now — `cycle::mark` and
        `cycle::scan` — the corpse mark landed with S34.3, and commit's free
        is S36.5's.
        The one clause that is about today's code — the bit is never cleared at
        acquittal — holds vacuously, nothing in the crate clearing `ENROLLED`
        at all (checked 2026-08-29; `refcount.rs` only sets it). And the test
        the step demands needs a collection to assert reclamation, which
        `gc::ll_gc_collect_cycles` does not have until S36.7, plus the
        maturation counter of S37.1 and the suspects buffer of S37.4 for the
        instant it waits for. Moved last in the stage for that reason; the work
        order takes it after S37.4.

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
- [ ] S36.9 The GC-memory contract   *(before S36.3)*
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
      handoff: S36.9 is executed as separately reviewed slices: (a) physical
        block contract and queue state; (b) logical ledger and current arena
        instrumentation; (c) manager-backed parking plus ordinary/abort deny
        gate, done 2026-09-01; (d) weak-table ownership and streaming arena
        drain; (e) retained
        index/registry/snapshot ownership plus the direct-large registry audit,
        done 2026-09-02.
        Only their composite source audit and deny test close this checkbox.
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
      tier: T2 · role: Sage → Critic
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
        The one site the composite audit exempts by name is
        `validation::member_counts_cover_internal_edges`'s in-degree `Vec`:
        it runs under `debug_assertions` alone, on the owning thread, and no
        release build allocates it (S42.1, 2026-09-01; the S42 Code Reviewer
        asked that the exemption stand here rather than in the closed stage).
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
        `parking`; S38.3, S39 and the backlog still carry it.
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
- [ ] S36.12 The in-flight batch and condemned membership   *(before S36.3)*
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
        are the head and the head's fill alone, with no tail, S37.4's composite
        detach adding one when it needs one. The overflow buffer is not part of
        the batch and the detach may not assert it empty, the pressure path
        sharing the code. The restore asserts an empty write position **in
        every build**, that assertion being the whole difference between "no
        user code runs between the detach and the restore" as an argument and
        as a check. And the step splits: slice (a) is the detach, the restore
        and the two-phase loop; slice (b) is the pressure path's harvest, which
        waits because its region capacity waits for S43.1. The instrument owed
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
      handoff: choose and test the commit unit here. A single condemned batch
        is safe under the aggregate exact sum but resurrection in one connected
        part conservatively retains the others; if teardown promises
        per-component behaviour, this step must extract components instead of
        silently passing their union to `validation::validate_component`.
        `rfc/model/gc/rc-cycle.md`, "Cycle finalization and reclamation", puts
        the guard on every member of every confirmed component before any user
        code, so no member can start ordinary teardown mid-commit whichever
        unit is chosen.
      handoff: the region's capacity is unchosen and **its expected budget is
        gone**. It was to be the 8,320 bytes the withheld returns would have
        left had S43 deleted their region; S43 no longer deletes it
        (2026-09-04), the chain staying as the ordinary path with the mark
        answering its refusal. Nor does S43.1 supply the other input: a
        synthetic load's death count is the fixture's own and was refused as a
        measurement. What is left to choose on is the bump's own 56,960 bytes
        and the pressure path's need, and `cycle::density` counts no death that
        could narrow it — `heap::block_occupancy` reads `used`, which drops at
        a slot's physical return rather than at its teardown.
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
        owner's by the law of S34.2 — and because the mark writes into no
        entity, which is what makes an aborted collection free.
- [ ] S36.7 Wire the collection into the ABI
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
- [ ] S36.8 Elide the redundant exact test after an in-line owner trace
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

## S43 — A refused withheld return becomes a mark on the dead slot   *(before S36.7)*

Goal: the chain keeps every death it has room for, and a death it has no room
for is marked dead in place and returned by the sweep that unstamps its block.
What goes: the growth past the workspace's region, the critical-reserve draw
behind that growth, and the `std::process::abort()` that answers a refusal
`ll_free` has no frame to report. The region itself, its capacity and the
ordinary eight-byte append all stay.

Why the mark answers it (Edmond, 2026-09-03): the fact the chain carries —
this address is dead and not yet on the free list — fits in the slot itself.
An entity block has no occupancy bitmap; occupancy is read from the slot's
first word (`memory::heap`, "why the bitmap lost"), and a dead slot's word is
nobody else's. The collector already walks the blocks it stamped, at
`TraceScratchArena::clear_touched_rows`, which is the instant its rows die —
so the window a mark has to survive is exactly the window the sweep closes.

Why the mark does not replace the chain (Edmond, 2026-09-04, on S43.1's
measurement): the sweep walks a block's slots where the chain writes eight
bytes, and that walk is dearer in cache lines at every design class. Breaking
even needs four deaths for every line the walk reads and no block holds that
many; on a component scattered one member to a block the walk costs about a
thousand times the chain (`dev/BENCHMARKS.md`, 2026-09-04, S43.1). The cost
does not move with the death count, only with the blocks a trace touched. So
the walk pays for what it removes rather than for what it saves, and it runs
where that trade is worth making: on the refusal, which is rare, and not on
every collection.

**The ordinary collection pays nothing for this.** No mark taken means no walk
made: `clear_touched_rows` stays the one store per touched block it is today.
The sweep walks the slots of a block that holds a mark and of no other, and
which mechanism carries that — a bit in the row array, a word in the block's
collector line — is S43.4's to choose.

**The rare path is exercised on every ordinary run.** A path that runs only
under starvation is a path nobody tests, and this one writes into other
occupants' blocks. The suite fills the region the way
`the_append_moves_into_a_block_when_the_workspace_region_is_full` already
does — by making `RETURNS_BASE_RECORDS` real deaths — so the mark and the
sweep run in the ordinary gate rather than only when memory is out.

**A mark is taken only where the sweep will find it.** Today every return is
withheld while a window is open, the block's own state unread; a mark in a
block the trace never touched would never be swept and the slot would be lost.
The test is the block's shadow pointer, read beside the block's owner: a stamp
another thread's trace wrote, or one standing on a block an exited thread
abandoned, is a stamp this thread's sweep will never reach. Both are cold-path
loads, the shadow living on the collector's own line rather than beside the
kind word. It is the right test rather than a cheaper one, a block with no rows
having no row for a new occupant to inherit.

**The sweep nulls before it returns.** Each marked slot goes back through
`stdapi::ll_free`, which makes that same test; a sweep that returned before
nulling would meet its own stamp and mark the slot again, and no slot would
ever return. Nulling first is safe on one thread at that instant: the rows are
dead there, which is what the token's release means.

**Three populations are withheld today and the mark owes all three.** An entity
block's slot is one; the other two are a retained block's whole-block return,
which `ll_free` reaches through `occupant_freed`, and an OS-direct run's unmap
— each pinned by a test of S36.2 (`a_retained_blocks_last_occupant_waits_for_`
`the_trace_row`, `a_pooled_large_entity_waits_for_its_header_row`,
`an_os_direct_large_entity_waits_for_its_header_row`). Neither has a slot's
first word to write into: a retained occupant's mark goes in its own header
and the sweep walks the survivor list, and a run's goes in its block header
with the sweep unmapping. A step that ships the entity-slot half alone returns
a retained block or a run under a live row, which is the defect S36.2 exists
to prevent.

- [x] S43.1 Measure the two populations   *(S40.1's instrument, this question)*
      done: one instrumented collection reports blocks touched and entities
        dying inside the window, on the synthetic load S40.1 defines, so the
        sweep's per-slot walk is priced against the list's per-death append
        before either is built
      tier: T2 · role: Bench
      Sage 2026-09-04 (pre-change gate): **the death count is the fixture's own
        input** and is refused as a measurement, standing as a construction
        check instead — the free path withheld exactly what the fixture freed
        (`dev/DECISIONS.md`, "the death count of a synthetic load is a check and
        not a measurement"). What is reported is what the crate chooses: blocks
        touched, the walk's two bounds, the clustering of deaths, and the
        break-even derived from them. The walk is bounded by `bump` and not by
        the slot count, `heap::for_each_entity_slot` running to the cursor, and
        both bounds are reported because they differ by 5.4x on the dense arm.
        Two units, because they disagree in direction: lines govern and
        operations are reported beside them. The control is the instant before
        the first free, a killing collection having no repeat. The measurement
        carries `Population::Slotted` alone, the retained and OS-direct death
        sides being S43.2's and S43.3's. Refused: any production edit, a timed
        run, a death set that could grow the chain past its region, class 16
        (`props_for(16)` is zero properties, so no ring can be built there),
        averaging across arms, and closing the stage on this number.
      progress 2026-09-04 — `cycle::density::tests::the_death_loads` runs
        S40.1's own populations at 381 members and lets the eighth collection
        tear the component down inside its still-open window. **The chain is
        cheaper in the line unit at every design class, and the sparse arm
        decides it**: 381 blocks holding one death each cost the walk 97,155
        line reads against the chain's 96 line touches. The break-even needs
        four deaths per line the walk reads, and a block cannot hold that many —
        the ceiling is below the break-even by a factor of two at class 32 and
        four above it. In the operation unit the crossing is reachable and this
        load passes it at classes 128 and 256. The walk's cost does not move
        with the death count at all, only with the blocks a trace touched
        (`dev/BENCHMARKS.md`, 2026-09-04, S43.1). Test-only: one `cfg(test)`
        reader of the bump cursor beside `heap::block_occupancy`, no production
        path edited, 677 tests unchanged and the two loads ignored.
      handoff: the number does not settle the stage and hands it back. What the
        sweep buys is not speed but the deletion of a refusal path — 8,320 bytes
        of every thread's workspace, the growth past that region, its draw on
        the critical reserve, and the `std::process::abort()` a refusal reaches
        — and that is Edmond's own reason of 2026-09-03. Whether it is worth a
        walk that costs a thousand times the chain on a scattered component is
        his to say, and S43.2 waits on it.
- [x] S43.2 The dead slot carries the mark
      done: a slot freed inside a trace window whose chain has no room for its
        record — the region full and both allocation paths refusing — in a
        block the trace has stamped, is left dead in place rather than
        returned, and reads as
        neither live nor free to every walker that reads a slot's first word —
        `heap`'s occupancy test, the census, and `describe_slot` — **while the
        refcount word still reads zero**, which is what the queue reader
        depends on; a test frees one such slot, walks the block by each of
        those readers and allocates against the same class without receiving
        it, and a second reads the refcount word back as zero
      tier: T2 · role: Sage → Critic
      Sage 2026-09-04 (pre-change gate): **the mark is flags bit 15**, in the
        mutator's half, and no contradiction with the rfc exists — the sentence
        the handoff feared governs the queue-parked slot, and the count stays
        zero under a flags bit anyway (`dev/DECISIONS.md`, "the dead-in-place
        mark is flags bit 15, and the owner clears it"). The occupancy test gets
        one definition, `refcount::slot_state`, and every walker goes through
        it — including `retained::is_occupied`, a fourth reader this criterion
        had missed and the only production one. **Two clauses struck**: the
        trigger is the region's capacity alone, "both allocation paths refusing"
        being memory starvation and unreachable in the ordinary gate; and
        S43.4's sweep-side clear of the per-slot mark moves to the owner, a
        collector worker having no business in the mutator's half of the flags
        word. The block-level flag stays S43.4's. Refused: a sentinel in the
        refcount word, `CANDIDATE_BIT` reused as the mark, the free-list link,
        any bit above 15, and a clearing store on the ordinary return path.
      progress 2026-09-04 — `refcount::DEAD_IN_PLACE` is flags bit 15 and
        `refcount::slot_state` is the one occupancy test, answering Live from
        the count alone before it reads the flags. Its readers: both walks of
        `heap::for_each_entity_slot` and the census over them, `describe_slot`,
        which now names the state, `retained::is_occupied`, and `row.rs`'s
        assertion that a listed retained block names no live occupant. The mark
        is taken on a refused push alone, behind `#[cold]`, for an entity slot
        of a block **this thread owns and a trace has stamped**; every other
        case still takes a record, so `grow` and its process end stand for the
        two populations S43.3 owns and for the unstamped block S43.5 owes an
        answer. `defer_reuse_if_tracing`'s success path is unchanged at three
        loads, two branches and two stores. Six tests: the mark taken with the
        pool healthy and nothing drawn, the unstamped block recorded beside a
        stamped one, a stamped block of an exited thread recorded because this
        thread cannot sweep it, and three over the predicate's order. Two
        `debug_assert`s stand at the free list's entrances, the owner's push
        and the remote post. This does not close S43: nothing sweeps a mark
        until S43.4, so a marked slot is held for the life of the process, and
        the retained and OS-direct marks are S43.3's.
      Critic 2026-09-04 round 1: ten findings, all taken, two of them as
        records rather than code — S43.5's deletion has no answer for a death
        in an unstamped block, and a thread exiting with a marked slot leaves
        it to abandonment and adoption. In code: `retained::is_occupied`'s
        conversion answered nothing by itself and gained the assertion that
        makes S43.3 arrive as a failure; the occupancy guard read one spelling
        of four; the unstamped test stamped no block, so its own contrast did
        not exist; two assertions of the marked test pinned the withholding and
        said they pinned the mark; `kind` reached an `unsafe fn` with no
        contract; the free list's second entrance had no guard; the predicate's
        order had no test; and four documents stated the pre-change build.
      Critic 2026-09-04 round 2: seven findings, all taken, and the
        load-bearing one was a defect round 1's own repairs left standing. **A
        mark could be written into a block this thread does not own**: a stamp
        is not proof of a sweep, an abandoned block keeps its kind and its
        collector line, and a cross-thread free past a full region would have
        marked a slot nobody returns. The mark now asks ownership beside the
        stamp, and a test builds the case out of a thread that exits holding a
        live entity. Also taken: `slot_state`'s doc named a reader that reads
        another word; the guard's allow-list exempted `row.rs` on a reason that
        described the opposite assertion, so that site was converted instead;
        the guard could not see `header_pair`; the third state test claimed a
        width it pins the outcome of; and the stage's own sentence priced the
        shadow load as a neighbour of the kind word, which is a different cache
        line. The device stops at two rounds.
      miri 2026-09-04 — `cycle::deferred_slot_reuse` at two threads, after the
        ownership load landed: 21 passed, 0 failed, 0 ignored, 47.70 s on
        Miri's clock against 2 m 17 s of wall. The run is proved alive by its
        own count, 21 being every `#[test]` of the module. The block's own run
        is S43's, at its close.
      handoff: the rfc owes two amendments, and carrying them is that
        repository's work. `rfc/model/gc/rc-cycle.md`, "A slot freed while the
        thread's own trace is open is appended to the trace's deferred-reuse
        list" gains the second case — a slot the list has no room for is marked
        in its own slot, its count still zero and its candidate bit still
        clear. And the paragraph beginning "The free path asks no allocation
        path past that region either" states the retracted form of 2026-09-03,
        that the list itself goes; it is wrong today rather than merely silent.
        `rfc/model/classes.md`'s "Flags layout" still calls bit 15 free.
- [ ] S43.3 The retained occupant and the OS-direct run carry theirs
      done: a retained block whose last occupant dies under a live row, and an
        OS-direct run whose entity dies under one, are held by a mark in their
        own headers rather than by a record; the three S36.2 cases that pin
        those two returns pass unchanged
      tier: T2 · role: Sage → Critic
- [ ] S43.4 The sweep returns everything marked
      done: `clear_touched_rows` nulls a block's shadow pointer and **clears
        the mark**, and the **owning thread** returns the slot on its own free
        path when it next finds no trace addressing the block — slots, the
        retained block itself, the run — a collection that aborts mid-trace
        clearing by the same path; **a block holding no mark is not walked**,
        and a collection that took no mark makes the one store per touched
        block it makes today, which a test asserts by counting the walk;
        tests read every marked slot back on the success path and on the abort
        path, with the block that emptied entirely retiring to the pool
      tier: T2 · role: Sage → Critic
      correction 2026-09-04 (S43.2's gate): the sweep clears the block-level
        flag and the shadow pointer; **the per-slot mark is cleared by the
        owner** as it returns the slot. Under S38 the sweep runs where the
        token is released, which is not the owning thread, and the mutator's
        half of the flags word has one writer — the same rule that refuses a
        worker the neighbouring clear of the candidate bit.
      correction 2026-09-04 (Critic round 1 on S43.2): the step also owes the
        thread that exits holding a marked slot. Such a block reads `used`
        above zero, so `heap`'s abandonment puts it on the abandoned list and
        an adopting thread claims it without zeroing a slot; the mark then
        stands in a block whose owner never made it, its `used` never reaches
        zero, and the adoption accounting counts it as a live slot the exited
        thread owed. Either abandonment refuses a block carrying a mark, or the
        block-level flag survives adoption and the new owner's sweep finds it.
      correction 2026-09-04: the criterion had the sweep itself free through
        `stdapi::ll_free`. The sweep runs where the token is released, and under
        S38's accelerator that is not the owning thread —
        `rfc/model/gc/rc-cycle.md` refuses exactly that: "A collector worker may
        label the queue entry as a zero-count entity, but it must not clear the
        candidate bit or return the slot", the reason being that the old
        entity's destructor may still be running on the owner. It is Edmond's
        own ruling of 2026-08-25, "the collector-side free is withdrawn; only
        the mutator frees". The sweep clears; the owner returns.
- [ ] S43.5 The growth, the reserve draw and the last resort are deleted
      done: `cycle::deferred_slot_reuse` grows past its region by nothing, asks
        the pool and the critical reserve for nothing, and holds no
        `std::process::abort()`; the region, its 1,024 records and the
        eight-byte append stay, and so does the workspace's 8,320-byte prefix;
        `dev/ARCHITECTURE.md`'s critical-reserve row names one borrower again
        and its withheld-returns row loses the blocks past the region
      tier: T2 · role: Critic
      handoff (Critic round 1 on S43.2): one case has to be answered before
        this deletion is possible, and it is the last resident of the growth
        path once S43.3 has taken the retained block and the OS-direct run: **a
        slot dying past a full region in a block no trace stamped**. It cannot
        be marked, a mark there being one no sweep walks to, and it cannot be
        dropped. Two ways out, and neither is chosen here: record the block
        rather than the slot, so that the region holds one entry per block and
        the close walks each recorded block once, which also retires the
        stamped/unstamped distinction; or keep the growth for this one
        population and narrow the step's promise to the reserve draw and the
        abort. Pinned by
        `an_unstamped_block_past_the_region_still_takes_a_record`, which asserts
        today's answer.
      note 2026-09-04: what makes the refusal answerable is the mark, not the
        deletion of the chain — S43.1 measured the walk that a full deletion
        would cost every collection and it is dearer than the chain at every
        design class. Under starvation a collection still ends itself and
        returns every block (`dev/DECISIONS.md`, "under memory starvation a
        collection ends itself and gives back everything"); with nothing left
        to grow, the free path asks an allocation path for nothing, so the
        regime needs no mechanism of its own here.
- [ ] S43.6 An unwind out of the close strands no dead slot
      done: a panic raised inside `TraceScratchArena::reset` — the profile that
        unwinds, since the release build aborts — still returns every marked
        slot before the arena's blocks go back, and the doc of whatever holds
        that duty states which panic it survives and which it does not; a test
        panics inside the reset and reads every marked slot back
      tier: T2 · role: Critic
      note 2026-09-03: raised by the S36.11 Critic's first round against the
        chain, and it survives the chain's deletion — `ActiveTrace::drop` runs
        `arena.reset()` before the returns are made, so an unwind out of the
        reset loses them whichever structure holds them.

## S37 — Maturation and the two class gates

Goal: the trace stops following the whole heap. On a booted Laravel corpus the
subgraph reachable from a median candidate root is 381 of 381 objects, so this
stage is what makes a trace affordable rather than what tunes it.

- [ ] S37.1 The maturation stamp is an edge-side prune
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
      tier: T2 · role: —
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
- [ ] S37.4 The deferred-candidate buffer and the turnover re-offer
      done: acquittal never clears the candidate bit; a proven-live root parks
        its **one existing token** in the owner's dormant lane, and every
        deferred candidate is re-offered at **the owner's first safepoint poll
        that finds the epoch counter moved**, never by enrolling or copying the
        entity a second time; red tests prove
        that a matured ring losing its last external reference mid-epoch is
        collected at that re-offer and not before, and that a ring whose mates
        carry unequal ages is likewise collected, so maturing apart costs recall
        rather than a permanent miss
      tier: T2 · role: Sage → Critic
      handoff: this is the backstop the withdrawn "retired on contact" clause
        was supposed to be and never was — eager clearing fires only when a
        trace touches the entity, and the stamp that wraps is exactly the one no
        trace touched for four epochs. It also collects YRC's 56 % saving on
        re-registration.
      handoff: `CANDIDATE_BIT` means exactly one logical token in exactly one state:
        `active → in-flight → dormant`, or consumer-retired after death;
        epoch turnover detaches active and due-dormant heads/tails in O(1), and
        an abort restores each sub-batch to the lane it came from without
        allocation. A decrement while dormant sees the standing bit and cannot
        add a duplicate. Store original enrolled
        roots only — adding every traced live member manufactures tokens, and
        collapsing two roots in one component can miss it after a later split.
      correction 2026-09-04: the criterion re-offered "at the first collection
        after the heap's epoch advances", by detaching the dormant lane beside
        the active one. `rfc/dev/DECISIONS.md`, closing Y12 clause 8, chooses
        the poll over the collection **by name and with the failure case**: "a
        thread whose only garbage is a parked ring has an empty queue …
        waiting for a judgement would wait for ever, which is Y6's permanent
        miss by another road". The rfc's mechanism there is a splice onto the
        live queue, one link per segment, in the poll's fixed order — refill,
        drain, splice. The batch-detach form this step proposed is what the
        handoff below argues for; it is an amendment `rfc` owes, and until it
        lands the splice is the contract. The retired word "suspects buffer"
        goes with it (`rfc/dev/GLOSSARY.md`).
      handoff: current queue segments cannot be spliced after filtering: only
        the live head has a fill bound and every segment behind it is assumed
        full. S36.12's per-batch/per-segment `read` and `used` bounds, or an
        equivalent in-place compaction restoring full segments, are a hard
        prerequisite. Tests count one token across active + in-flight + dormant
        after repeated decrements, acquittals, partial segments, abort and
        turnover; a dormant corpse keeps identity until its consumer retires it.
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
        where the second thread arrives and the byte stops being inert; S37.1
        is what it breaks. And **`dev/WORKFLOW.md`'s ThreadSanitizer run has
        selected no test since 2026-08-26**, its only one having lived in the
        deleted `collector::`, so the instrument that reports
        plain-against-atomic is         unavailable until this step gives it a pairing
        to watch.
      handoff: the collector thread's birth is this step's to name — startup
        or first pressure — with its floor refusal following it; a mandatory
        floor drawn at first pressure is the worst moment
        (`rfc/dev/DECISIONS.md`, "the baseline overflow segment is allocator-issued").
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
        while the claim is held waits for the trace to end
        rather than preempting; the test's running collection is staged by
        S38.1's harness seizure and reaches the wait through S38.4's path, with
        a `#[cfg(test)]` counter past the wait asserted non-zero, because a test
        that merely terminates terminates most easily when the wait is never
        taken
      tier: T2 · role: Critic
- [ ] S38.3 Deferring the mutator's frees during a trace
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

## S39 — Thread exit  (carried from S29.2)

- [ ] S39.1 Exit waits, collects, and drains its four chains   *(after S36.4)*
      done: `ll_thread_exit` **waits** while any trace holds rows over this
        thread's blocks, **collects**, and then retires its chains before
        handing the heap over — the queue, the overflow buffer, the
        deferred-candidate buffer and the inbox, all four, which for a
        zero-count entry means reading the refcount, clearing `CANDIDATE_BIT`
        and returning the deferred slot; what the collection could not take
        keeps its bit and is reported as a bounded leak with its cause; a
        red-first test kills a thread between registration and collection, and
        a second shows a thread that would have aborted inside an open window
        waiting instead
      tier: T2 · role: —
      ruling 2026-09-04 (Edmond): the wait, and collection before exit as the
        fate of a live registered entity. Recorded with its refused
        alternatives in `dev/DECISIONS.md`, "a thread waits for the trace,
        collects, and then exits". The wait replaces the process abort
        `dispose_thread_state` performs today, and closes the first clause of
        `rfc`'s A4. **The step moves after S36.4**: its destructors run on a
        winding-down thread, and an unwind across `ll_thread_exit`'s `extern
        "C"` boundary ends the process, so the policy for a throwing destructor
        has to exist first.
      correction 2026-09-04: the criterion named one chain where
        `rfc/dev/PLAN.md` names four — "`ll_thread_exit` drains the suspects
        buffer beside the inbox, the queue and the overflow buffer" — and the
        leak this step exists to close is not closed by the queue alone.
      handoff: the corpse half arrived from S34.3 on 2026-08-29, which built
        the deferral and the two accessors it needs
        (`refcount::clear_enrolled`, whose `expect(dead_code)` names this step)
        and could not wire them. Two obstacles, both this step's: a deferred slot
        that is never retired leaks for the life of the process, and the
        queue's test fixture writes bare `RcHeader`s on the stack, so a drain
        that dereferenced entries would read freed stack memory — the fixture
        has to allocate real entities first, and `cycle/queue/tests.rs` says in
        its own words why it does not today.
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
        instrumented run records the pruned-edge share at `k` of 1, 2 and 3,
        which is what settles S37.1's first provisional constant
      tier: T2 · role: Bench → Sage → Critic
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
        arithmetic. Groups met are recorded beside rows met, the chunked form's
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
- [ ] S40.3 Count the workspace and the cache traffic   *(before S40.2)*
      done: per collection counters report unique roots, `V`, `E`, touched
        blocks and groups, row bytes reserved and written, distinct row/group
        lines touched, worklist/member/deferred-slot high-water, arena requested,
        granted and abandoned-tail bytes, base hits, overflow draws and
        returns, deferred-candidate deferrals and re-offers, exact passes and probes,
        physical GC current/peak blocks by funding role,
        and logical current/high-water bytes by workspace consumer
      done: pinned back-to-back runs over component sizes 2, 16, 256 and 381,
        dense and one-entity-per-block, ordinary and retained, record
        instructions, cycles, L1D/LLC/dTLB and branch misses plus manager draws;
        the control protocol is `dev/BENCHMARKS.md`'s and every cache conclusion
        remains a hypothesis until these counters measure it
      tier: T2 · role: Sage → Critic
      handoff: a widest flat row array reserves 16,408 bytes, or 257
        line-equivalents and 257–258 physical cache lines depending on
        alignment. First touch is proven only to write 121 bytes; how many
        distinct lines those writes address is fixture-dependent and measured
        here. A persistent 64 KiB block removes manager churn, not cache fills
        and not the sparse-row cost. S40.2 changes representation only from
        these data.
- [ ] S40.2 Decide chunks or not
      done: the decision and its reason are in `dev/DECISIONS.md`, quoting a
        number on each side with its denominator — the full-trace write volume
        the rfc measured, and the manager draws a sparse trace costs each
        form — and the refused form is recorded with the range over which it
        would have won
      tier: T2 · role: Sage
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
  What is left of the old escalation ladder after S38.4 builds the entry gate
  and the slow-path fire. The arming policy is the compiler's
  (`rfc/model/gc/strategies.md`, arm/fire); the accelerator carries the third
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
  its cost only if one of them is seen to fail. One of the five carries a
  defect of its own, found in the same review and older than it:
  `a_thread_nothing_will_tear_down_is_not_funded` reads the same figure into
  `base_blocks_before` and `segments_before` and asserts both, so the segment
  claim its name makes has no reading behind it.
- [ ] **`docs/performance-case-decompositions.md`'s five citations point into
  a deleted file on a branch**, and whether that form is acceptable is the
  question left. The document carries its superseded banner and every one of
  the five names `archive/pre-rc-cycle` explicitly, so a reader is not misled;
  what `dev/tools/citations.py` cannot do is follow a citation onto a branch,
  which is why the five stand in its twelve known residues. Either the checker
  learns the branch form or the five lose their section names. Found by
  `dev/tools/citations.py`, 2026-09-01; the banner landed the same day.
- [ ] **A test that reads a file or spawns a process carries no guard
  requiring its `cfg_attr(miri, ignore)`.** The convention has been broken
  twice — `cycle::` on 2026-09-01 and `memory::` on 2026-09-02 — and each
  time it left a whole Miri slice unrun while `cargo test` stayed green. A
  guard reads each `#[test]` function's body for `read_dir`, `current_exe`,
  `File::open` and `read_to_string` and asks for the attribute above it. The
  work is one test; what it needs first is a decision on how it recognises a
  test function's extent, since the crate has no parser
  (`dev/POSTMORTEM.md`, 2026-09-02).
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
