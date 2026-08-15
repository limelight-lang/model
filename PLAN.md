# Plan

Implementation plan for `ll-model`, re-sorted 2026-07-24 against the RFC
after the 2026-07-22 object-layout redesign landed in the `rfc` repo.

Design lives in `rfc` and is authoritative — read before coding, do not
re-derive: `model/classes.md`, `model/values.md`, `model/lowering.md`,
`model/gc/strategies.md`, `model/gc/satb.md`, `model/memory/ffi.md`,
`runtime/object-lifecycle.md`.

Updated: 2026-08-15 · Active: S24 — the prose sections after it are the backlog

**Closed stages are deleted whole** (rule 23.1.3), and what outlived each
of them is in the journals rather than here: `dev/DECISIONS.md` for a
decision and its reason, `dev/POSTMORTEM.md` for a trap,
`dev/BENCHMARKS.md` for a measurement, `dev/INDEX.md` and
`dev/ARCHITECTURE.md` for the map. Deleted so far: S4 through S23, so S24
below is the one open stage. A number is never reissued, so a
stage added later sits where it is to be done rather than where its
number falls, and the prose sections below are the backlog stages are
drawn from.

**`array::` has had its Miri run, in slices**, and the hour the module was
feared to cost was `array::entity`'s alone: `array::entry` took 4 seconds
over 7 tests and `array::table` 92 over 38, both clean, on 2026-08-13 at
`8d3728d`. A slice is still how the module is run — invocation and thread
cap in `dev/WORKFLOW.md`, Miri.

## S24 — the barrier's header reads: narrow where the writes are narrow

Goal: the rule this crate already states on `refcount::refcount_load` —
narrow stores demand narrow loads — holds on the barrier's path too, and
whether merging the remaining reads is worth anything is decided on an
instrument that does not manufacture the effect it measures.

Done when: the accessors read narrow, the merge has landed or been refused
with the figure that refused it, and every number here was taken on the
probe of S24.1 rather than on the single-entity arms of 2026-08-15.

What opened it: that entry, and the disassembly beside it. `escape_gain`
writes both halves of the header with plain 4-byte stores while
`header_flags` and `header_refcount` load the whole word through
`mutator_load_header` and discard the half they did not want — the pairing
that cost 3x on retain/release in the 2026-07-27 entry, sitting on the
escape path today.

Critic 2026-08-15 round 1: the draft ordered the merge ahead of the step
that decides its own trade-off, gated it on a load count whose payoff is
under the floor, and closed a door — a 4-byte atomic read of the flags
half — that `flags_load` walks through on every retain. Accepted, rewritten
around the rule that door was hiding.
Critic 2026-08-15 round 2: the instrument precedes the reading, so the
probe moved first; `ll_cow_separate` samples the two halves at two instants
and needs `header_pair`; `header_pair`'s own justification inverts after
this stage; and S24.3's arithmetic predicts refusal, which the step now
says out loud. Accepted. No dispute reached Sage.

- [x] S24.1 A probe over a working set, shaped like lowering
      done: an `#[ignore]`d release-mode probe inside the lib, the shape of
        `collector::tests::the_epoch_as_a_whole::measure_epoch_cost`, whose
        timed loop holds **no call instruction at all** (`objdump`) and takes
        `owner_cat` as a constant; it stores into one entity and into N ≥ 64
        distinct ones, each escaped before the clock, and both figures are
        recorded as their own baseline
      done also: the control, taken **before** S24.2 changes anything — under
        `rc-walk` the two working sets differ beyond the floor. They do not,
        the probe does not resolve the stall, and that is what it reports;
        S24.3 then has no gate and is dropped rather than guessed at
      tier: T2 · role: Critic
      why first: an instrument comes before a reading. The arms of 2026-08-15
        call across a crate boundary and hammer one entity, so they carry an
        indirect call per store and a loop-carried dependency through one
        header line that compiled code storing into a thousand slots has not.
      not comparable: a different binary from a different profile, so its
        figures are never tabulated beside the 2026-08-15 harness numbers
        (`dev/BENCHMARKS.md`, Method). Comparisons are drawn between probe
        variants inside one A→B→A bracket.
      rejected, recorded rather than retried: `#[inline]` on `store_box` or
        `ll_retain`, which changes the shipped artifact to serve a
        measurement; `lto = "fat"` on the bench profile, which moves every
        other arm in `benches/barrier.rs`.
      handoff: `memory::barrier::tests::what_a_store_costs_by_working_set`,
        and the figures in `dev/BENCHMARKS.md`, 2026-08-15, "the store path
        in the shape lowering emits". The control fired where it was aimed:
        the escape direction costs 4.68 ns per store over one child and 2.48
        over 64 under `rc-walk`, flat under `rc-trace`, so the harness's
        unexplained 3.7x was the chain through one header line and S24.3 has
        its gate. `heap → arena` is 2.2x dearer under `rc-walk` and **flat
        across working sets**, so that gap is the counted retain and the log
        rather than the stall — it is what S24.2 has to move. The criterion
        was read as "no call on the path the loop takes": the innermost loops
        are 13, 12 and 13 instructions and call-free, while the COW copy path
        and `grow_log` stay out of line and are reached by a forward branch.

- [x] S24.2 `header_flags` and `header_refcount` read narrow
      done: both take `flags_load` / `refcount_load` under `rc-walk` on the
        mutator store paths; the suite green in both GC configurations and
        under `debug-journal`; no arm regresses beyond the floor, and the
        improvement is recorded as a single-entity figure with its
        working-set counterpart from S24.1 beside it
      tier: T2 · role: Critic
      what stays wide, each with its reason in the code: `header_pair`, whose
        one caller (`array::entity::element_for_destination`) has no
        overlapping narrow store on the line it loads — and whose doc
        comment argues from load count and needs rewriting, the narrow pair
        being the preferred shape after this step; `object.rs`'s two direct
        `mutator_load_header` callers, each followed within a few lines by
        `mutator_update_flags`, which loads and stores the whole word anyway.
      outside the rule, and named so in the entry: `walk.rs`'s drain sites,
        which run once per component per epoch and sit in no forwarding
        window. Narrowing them would prove nothing.
      carried with it: `ll_cow_separate` moves to `header_pair`. It is the one
        predicate needing both halves (`cow_separation_needed(flags, count)`,
        and `ll_string_append`'s guard is the same pair), and today it samples
        them at two instants — safe only because nothing stands between the
        two lines, which no invariant records and no test defends.
      handoff: `dev/BENCHMARKS.md`, 2026-08-15, "the barrier's header reads go
        narrow". `heap → arena` fell from 4.82 to 1.53 ns per store under
        `rc-walk` and is now 29 % cheaper than `rc-trace` where it was 2.2x
        dearer; the escape direction lost its working-set effect entirely, and
        `arena → arena` did not move because that path stores no counter. Both
        gaps of this morning were one defect in two shapes — the overlap
        between iterations on the escape arm, and inside a single store on
        `heap → arena`, where `ll_retain` writes the counter half and the
        category test read all eight bytes after it. Full gate green in both
        configurations with the two feature legs; Miri clean over `refcount::`,
        `memory::barrier::` and the COW paths in both.

The two steps below were opened after S24.2, when the critic found four
header reads that bypassed the helpers (fixed in `6e5d137`). They sit here
because this is where they are done; their numbers say when they were opened.

- [x] S24.4 A guard against a header read that bypasses the helpers
      done: a test that reads this crate's own sources and fails on a direct
        read of `rc.flags` or `rc.refcount` outside `refcount.rs`, green on
        the current tree, and shown to fail when one of `6e5d137`'s four
        sites is put back
      tier: T1 · role: —
      why a source-reading test and not a lint: the defect is textual — a
        caller reached past the helper — and no type or visibility rule can
        express it while `RcHeader`'s fields stay `pub` for the layout
        contract. Its weakness is worth writing into the test: a rename or a
        read through a local evades it, so it defends against inattention
        and not against intent.
      what it would have caught: `object_constructed`'s category read,
        `ll_default_dispose`'s two, and `array::entity::needs_separation`.
      handoff: `refcount::tests::who_may_read_a_header`, two tests — the
        guard itself and one that feeds it `object_constructed`'s old shape
        beside an `rc-trace` block, so a guard that finds nothing anywhere
        fails instead of passing. It found a fifth site the review had not:
        `test_support::outside_block::install_block`, now on
        `object::header_category`. Exempt are `refcount.rs`, everything
        under a `tests/` directory, and any block opened by
        `#[cfg(not(feature = "rc-walk"))]`, which is found by brace counting
        from the attribute — sound only because `rustfmt` governs the file.

- [x] S24.5 ThreadSanitizer, the instrument this class actually needs
      done: `-Zsanitizer=thread` builds this crate and runs
        `collector::tests::the_epoch_as_a_whole::a_free_running_mutator_survives_concurrent_epochs`
        on this box, and one of `6e5d137`'s four sites, put back, is reported
        by it — or the attempt is recorded in `dev/POSTMORTEM.md` as refused,
        with what stopped it
      tier: T2 · role: Critic
      why: Miri cannot serve here. The only test that pairs a live collector
        with a mutator is ignored under it because the design's mixed-size
        atomics are rejected outright — a gap in the formal model rather than
        in the tool, and one ThreadSanitizer does not share: it reports a
        plain read against an atomic store, which is exactly this class.
      the risk that decides it: the crate brings its own allocator, and
        whether it runs under TSan at all is the first question the step
        answers rather than assumes.
      handoff: it runs, and the recipe is in `dev/WORKFLOW.md`,
        "ThreadSanitizer". `-Zbuild-std` is the part that is not optional —
        without it the build fails on an ABI mismatch against the prebuilt
        `core`. Validated both ways on 2026-08-15: `object_constructed`'s
        pre-`6e5d137` read was reported with the collector's
        `atomic_store::<u8>` on the other side, and the fix returned the run
        to silence. The window is thin — the mutator churns only while four
        epochs run — so a report is strong evidence and a clean run is weak.

- [ ] S24.3 One flags load for the whole store path, if the probe resolves it
      done: measured on S24.1's probe under the A→B→A bracket and landed only
        if the arena→arena and heap→arena directions move by more than 4 % of
        the probe's per-store figure; otherwise refused, with the figure, in
        `dev/BENCHMARKS.md`
      tier: T2 · role: Critic
      the prediction, stated before the work: refusal. After S24.2 the path's
        flags reads are a few independent narrow loads of one L1-resident
        line, on the order of 0.1 ns against a store of a few, so the branch
        is written to be measured and discarded unless it surprises.
      shape, if it does surprise: the snapshot is the **flags half only and
        never the count** — a twin carrying the count lets a caller decrement
        a stale value, and `drop_ref_deferred` → `escape_lose` → `ll_release`
        is where that lands first. `store_box` only; `store_ptr`,
        `publish_child`, `array::element::box_element` and `ref_store`'s owner
        read follow in a step of their own.
      what must hold: the snapshot answers what precedes any write to that
        header — counted, category, COW, and `IS_ESCAPEE` inside
        `escape_gain`, which is where one load per store has to reach — and
        dies at the first such write: `escape_gain` sets bit 11, and
        `escape_copy` republishes children through the barrier before it
        returns.
      not a door to close: narrowing an atomic read of the flags half is not
        forbidden here. `flags_load` does it on every retain and
        `collector_stamp_epoch` stores one byte into the same word; what
        decides a load's width is which store precedes it. The formal gap in
        mixed-size atomics is a whole-design property and its own stage if it
        is ever reopened.

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
- [ ] **Kinds 4 and 6 have no producer.** `ll_entity_die`'s switch serves
  five; Box waits on the FFI surface and Lazy on the compiler, and each
  reaches a `debug_assert!` meanwhile. `Lazy` is nevertheless in
  `CANDIDATE_KINDS`, on the argument recorded in `dev/DECISIONS.md`,
  2026-08-07.
- [ ] **The collector's escalation ladder**, build order 5 of
  `rfc/model/gc/rc-walk.md`, and the trigger thresholds beside it. Both
  are gated on a starvation measurement that does not exist, which is why
  a collection is still an explicit call.
- [ ] **`rc-satb` as a second build-time GC strategy**
  (`rfc/model/gc/satb.md`). The `WRITING` bit it waited on is pinned and
  the barrier's hook site is reserved; nothing else of it is built.
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

Owed to the array table by the map design, and owed before either map
class exists (`dev/DECISIONS.md`, 2026-08-13, "the flood ladder gets
kind-dispatched triggers"; `rfc/model/maps.md`, "What the flood ladder
becomes"):

- [ ] **The per-process key**, new work in `src/hash`: 32 bytes from the
  OS once per process in every build, outside `STAMP` and exempt from
  `hash-folding`, because nothing compiled may depend on it.
  `strong_hash`'s doc names the slot and stands in for it. Everything
  below waits on it, and so does `MapMixed`, whose key identity cannot be
  defined without it.
- [ ] **The ladder's repair in `array/table.rs`**: the equal-identity
  trigger becomes a tag-equality test rather than "not an integer key";
  `slot_hash` and `entry_slot_hash` dispatch on the tag with the
  byte-hashing branch asserted unreachable from any other; `draw_salt`
  draws under the per-process key instead of hashing a recyclable address
  under a foldable seed; `strong_hash` becomes the keyed function its own
  doc promises; and rung three, a refusal distinguishable from an
  allocation refusal, replaces the early returns that today make a spent
  ladder a dead one.
- [ ] **The key word's tag**, also `array/table.rs` and `array/entry.rs`,
  because the trigger above presupposes it: one encoding of the key field
  for every owner, the readers listed in `rfc/model/maps.md` under "The
  key word gains a tag, for every owner".

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
  **Settle separately:** entities past 8 KiB take the same path and live
  outside the walk on purpose; a uniform stride would make them walkable,
  which `rfc/model/gc/rc-walk.md` decided the other way.

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
