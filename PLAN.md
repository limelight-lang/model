# Plan

Implementation plan for `ll-model`, re-sorted 2026-07-24 against the RFC
after the 2026-07-22 object-layout redesign landed in the `rfc` repo.

Design lives in `rfc` and is authoritative — read before coding, do not
re-derive: `model/classes.md`, `model/values.md`, `model/lowering.md`,
`model/gc/strategies.md`, `model/gc/satb.md`, `model/memory/ffi.md`,
`runtime/object-lifecycle.md`.

Updated: 2026-08-15 · Active: S25 — the prose sections after it are the backlog

**Closed stages are deleted whole** (rule 23.1.3), and what outlived each
of them is in the journals rather than here: `dev/DECISIONS.md` for a
decision and its reason, `dev/POSTMORTEM.md` for a trap,
`dev/BENCHMARKS.md` for a measurement, `dev/INDEX.md` and
`dev/ARCHITECTURE.md` for the map. Deleted so far: S4 through S24, so S25
below is the one open stage. A number is never reissued, so a
stage added later sits where it is to be done rather than where its
number falls, and the prose sections below are the backlog stages are
drawn from.

**`array::` has had its Miri run, in slices**, and the hour the module was
feared to cost was `array::entity`'s alone: `array::entry` took 4 seconds
over 7 tests and `array::table` 92 over 38, both clean, on 2026-08-13 at
`8d3728d`. A slice is still how the module is run — invocation and thread
cap in `dev/WORKFLOW.md`, Miri.

## S25 — what the arena's write-side log costs, measured rather than subtracted

Goal: the release-at-reset record gets a figure of its own, taken with the
counted retain held constant instead of subtracted away.

Why today's number is not one: 0.45 ns is `heap → arena` minus
`arena → arena`, and those two arms differ in the retain as well as in the
log. How much that matters is already in the file — under `rc-trace`, where
the log code is byte-identical, the same subtraction is 0.66 and 0.71 ns
(`dev/BENCHMARKS.md`, 2026-08-15, the S24.1 and S24.2 brackets). A log cannot
differ by 0.2 ns between two builds that do not touch it, so the record is a
minority of the 0.45 and the rest is retain and codegen.

Done when: the probe carries the `heap → heap` direction and the sweep,
`dev/BENCHMARKS.md` states the record's marginal cost twice — with the log
hot and with it cold — and the latency-or-throughput question is answered by
the rule S25.2 registers in advance or reported unresolved with the figures
that failed to decide it.

Critic 2026-08-15 round 1: the drafted arm was impossible. The log fires on
`new_cat == GcHeap && owner_cat == RequestArena` while `ll_retain` returns
early exactly when the category is not `GcHeap`, so every store that logs has
taken a counted retain and no child category separates them; an `Immortal`
child is instruction-for-instruction `arena → arena`. Vary the owner instead,
and prefer a sweep to any subtraction. Accepted, rewritten.
Critic 2026-08-15 round 2: the timed loops are clean and the rounds are not —
a `GcHeap` owner's death releases its own slot, so `heap → heap` needs the
slot cleared where `heap → arena` must not, and the release build catches
neither the double release nor the drift; the reset's own drain warms exactly
the lines the next round's log writes, growing with k and flattening the
curvature the sweep is read for; and S25.2's rule has to stand on the
interaction of three arms, one outcome of which supports the framing it was
preparing to retract. Accepted, all folded into the steps. No dispute reached
Sage.

- [~] S25.1 The direction that holds the retain constant, and the sweep that
      prices the record
      done: two things in
        `memory::barrier::tests::what_a_store_costs_by_working_set` — a
        `heap → heap` direction (a `GcHeap` child into a `GcHeap` owner),
        which takes the same counted retain from the same allocator as
        `heap → arena` and appends no record; and a sweep in one timed region
        over k ∈ {0, 250, 500, 750, 1000} stores into an arena-category owner
        with the rest into a heap-category one, the same children throughout,
        so that d(ns)/d(k) is the record's marginal cost with no arm-to-arm
        subtraction at all
      done also: every figure taken twice, with the log cache-hot and with a
        few megabytes of scratch walked untimed between rounds to evict it.
        The difference between the two is how much of a record is
        instructions and how much is a cache line, and a request pays the
        cold one: it writes a record into a line it never revisits.
      tier: T2 · role: Critic
      Critic 2026-08-15 round 3, on the built step: the probe's own null pair
        — `sweep k=0` against `heap → heap`, the same publishes into the same
        slot — reads 0.05 ns per store hot and 1.22 cold, and the entry quoted
        it only in the half where it passes; the `rc-trace` override is
        contradicted by the endpoint it was built on, `sweep k=0` sitting
        0.61 below `heap → heap` there where the offered mechanism requires
        equality; the carve term is nearly collinear with `k`, so it biases
        the slope and the residual is blind to it; and the eviction runs after
        a round, so the next round's `children` re-warms every header before
        its timer starts. Checked against the run data and accepted. The cold
        figure and the override are retracted in `dev/BENCHMARKS.md`; two
        design findings are the step's remaining work, below.
      what the step still owes, both from that round: a `null_sweep_round`
        with both owners on the GC heap, whose slope is zero by construction
        and is the only thing that bounds the two-loops-two-slots term the
        sweep's slope currently carries; and an arm order rotated by the round
        index, because the arms run in a fixed order monotone in `k` with no
        eviction between them in the hot half, so each sweep point inherits
        the cache state of the point below it. Printing each arm's minimum,
        median and maximum decides the statistic question the same round
        opened.
      the sweep is the primary instrument and `heap → heap` the cross-check.
        Two directions are two loops at two alignments, and that layout term
        — 0.02 to 0.05 ns against an effect of 0.2 to 0.45 — is invisible to
        the repetition control, being constant across runs of one binary. The
        sweep holds both loops fixed and varies only k, so it absorbs the
        term; when the two disagree, the slope is what the entry calls the
        record's cost.
      the teardown is by measured count, not by arithmetic: a `GcHeap`
        owner's death releases what its slot holds (`ll_default_dispose`) and
        an arena owner's never does, so `heap → heap` clears the slot before
        the owner dies and `heap → arena` must not. Reset the arena first,
        then read each child's `header_refcount` and `assert_eq!` it before
        draining — an assert that survives the release build, where
        `debug_assert!` is gone and `panic = "abort"` turns a wrong count
        into a silent write through a freed slot. In the sweep the owed count
        is `1000 - k`, so an error there is linear in k and would pass the
        residual check as slope.
      and the untimed half stays symmetric: keep `arena_reset_full` in the
        `heap → heap` round although that direction never touches the arena,
        and interleave the arms round by round rather than running one arm's
        rounds and then the other's, so the block pool's LIFO state is common
        mode.
      the segment, to be stated with the number: at 1000 stores per region
        the cold `grow_log` fires twice (`LOG_SEG_RECORDS` = 500), so a
        per-record figure carries 1/500 of a segment carve and moves with
        `STORES`.

- [ ] S25.2 Whether a failed store-forward is latency or throughput
      done: the wide accessors of before-S24.2 — a two-line revert in
        `header_flags` / `header_refcount` on a scratch branch that is
        discarded — measured against the shipped narrow ones on both the
        `heap → heap` and `heap → arena` directions, and the entry answering
        by the rule below rather than by whichever account looks likelier
        afterwards
      tier: T2 · role: Critic
      the rule, registered before the run, on Δ = wide − narrow per direction,
        with Δ(`heap → arena`) = 3.28 ns already in the file: Δ(`heap → heap`)
        near 3.3 means the stall is intrinsic to the failed forward and
        `dev/BENCHMARKS.md`'s latency framing of the 2026-07-27 trap is wrong
        in general and gets corrected; near 0 means it was serialized with
        the log's cursor read-modify-write, the alternative the S24.2 entry
        names; meaningfully above 3.28 means the log's work ran in the shadow
        of the stall, which supports the latency framing rather than
        retracting it and yields a second estimate of the record from the
        latency budget. The cells come from two binaries, so the layout term
        cancels only if it is additive: the repetition control bounds it and
        nothing makes it zero.
      what is not the instrument, and why: moving `ll_retain`'s counter store
        after the barrier's read removes the failed forward instead of
        pricing it, so both accounts predict the same figure. It would also
        need the flags snapshot threaded through `ll_retain` — the invasive
        half the 2026-08-15 refusal deliberately never wrote — and would
        measure a shadow copy of the store path rather than the path.

Both steps: run the probe once in a **debug** build before any figure is
believed, and take the control by repetition rather than by a cross-binary
bracket — the arms live in one binary and one process, so the whole probe
runs three times and every arm must repeat within the floor this probe
reaches on these directions, 0.1 to 1.7 % across the three brackets of
2026-08-15. No arm of this stage sits on `arena → heap`, whose controls
differ there by up to 26 %.

Every row of the new entry comes from the post-change binary, and the S24
rows are not to be crossed with it: adding directions moves the pool and
arena state each round starts from.

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
