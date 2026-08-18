# Plan

Implementation plan for `ll-model`, re-sorted 2026-07-24 against the RFC
after the 2026-07-22 object-layout redesign landed in the `rfc` repo.

Design lives in `rfc` and is authoritative — read before coding, do not
re-derive: `model/classes.md`, `model/values.md`, `model/lowering.md`,
`model/gc/strategies.md`, `model/gc/satb.md`, `model/memory/ffi.md`,
`runtime/object-lifecycle.md`.

Updated: 2026-08-18 · Active: S28 — the sections below it are the backlog

**Closed stages are deleted whole** (rule 23.1.3), and what outlived each
of them is in the journals rather than here: `dev/DECISIONS.md` for a
decision and its reason, `dev/POSTMORTEM.md` for a trap,
`dev/BENCHMARKS.md` for a measurement, `dev/INDEX.md` and
`dev/ARCHITECTURE.md` for the map. Deleted so far: S4 through S27. A number is never reissued, so a
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

## S28 — Epoch metadata: flat per-row words only

Goal: the collector's grouping keeps no nested per-row vectors, the
judge copies no edge list, and the walk's per-row storage version fits
one word — the weight `dev/RC_WALK_CRITICAL_REVIEW.md`, "Per-epoch
graph metadata is heavy", prices at 24 bytes of empty vector shells per
walked row before any content, plus 16 bytes per row of `Option<usize>`.
Flat n-sized word arrays stay: marking over `RC − IN` is per-row by
nature. The stage claims no per-edge saving either — the 32-byte `Edge`
layout is a recorded decision (`collector.rs`, the `shape` field's
comment), and only the judge-time copy of the list goes.
Done when: the S28.1 harness stands with its by-construction shapes;
`garbage_components` builds its adjacency as flat arrays and the
probe's per-row byte budget holds, the probe re-run by hand at stage
close; `judge` hands the recorded edges over without a copy — closed by
reading the new signature, a pair copy having no site left;
`Epoch::storage_versions` holds 8 bytes per row; both GC configurations
are green, the rc-trace run being a regression sweep for the
collector-only steps (`collector` and `epoch` compile under `rc-walk`
alone).

Out of scope, named rather than implied: `recheck_and_post`'s
`component_edges` is sized by candidates and stays; the epoch arena has
its own backlog line below ("A budgeted epoch arena for the collector's
metadata"); and nothing here touches a clock — the box's noise floor
(`dev/BENCHMARKS.md`, 1.5–3 %) is wider than any expected effect, so
every criterion is a layout, an allocation shape or a suite.

Critic 2026-08-18 round 1, three lenses (technical, plan,
  verification): the cheap `Option<NonZeroUsize>` packing collides with
  version 0 in the fail-open direction; the original S28.1/S28.2 pair
  cut through `garbage_components`' signature; the source-shape
  criteria had nothing runnable behind them. Accepted — harness-first
  re-cut, sentinel and contract named, allocation probe added.
Critic 2026-08-18 round 2, on the fixes: the 28-byte budget forgot the
  mark bitmap and the unsized root stack; the first seeded mutation was
  inexpressible before the rewrite; the `Some(0)` hand-collapse was a
  no-op over every reachable state (an edge source's version is ≥ 2);
  the arena deferral cited a backlog line that did not exist. Accepted
  — budget re-derived at 32 with exact pre-sizing, mutations
  reassigned per step, seen-red retargeted at the stale-version
  comparison, the backlog line added. Disputes ruled in-session by
  Edmond's standing instruction; no Sage round.

- [ ] S28.1 The grouping harness: a direct test of `garbage_components`
      done: a partition-equality test (members sorted, components keyed
        by their minimum member — no caller depends on order, verified
        by reading both consumers, `recheck_and_post` and `walk.rs`'s
        `collect_cycles_inner`) holds a pasted textual copy of the
        current implementation as its frozen oracle and covers, by
        construction rather than by chance: n = 0, an empty candidate
        edge set, an isolated single-row candidate, a self-edge amid
        other candidates, duplicate parallel edges, a marked/unmarked
        mix, a garland of rings, and fixed-seed random graphs on top;
        two seeded mutations of the live implementation turned it red
        and were reverted — self-edges skipped in the `in_degree` pass
        (a pure self-loop reads live and leaves the partition), and the
        reverse push dropped from the undirected adjacency (a ring
        closed high-to-low splits). The test lives in `walk`'s test
        tree, compiled under both GC features.
      tier: T1 · role: Critic
- [ ] S28.2 The rewrite: edges read in place, adjacency in flat CSR
      done: `garbage_components` takes the epoch's edges without an
        intermediate `(u32, u32)` copy, and both callers compile
        against the new shape — `walk.rs`'s `collect_cycles_inner`
        supplies its native pairs. The mark walk's forward CSR still
        covers every recorded edge; only the undirected component CSR
        is restricted to candidate (both-ends-unmarked) edges, and
        component enumeration stays the `0..n` scan over `!marked`, so
        an isolated candidate is still a singleton. The S28.1 test is
        green unchanged, and one further seeded mutation — a self-edge
        contributing 1 instead of 2 to the undirected degree pass —
        turned it red and was reverted. An ignored probe, summing
        allocation requests on a thread-local counter toggled around
        the call (a `cfg(miri)` bypass in the wrapper, run
        name-filtered), asserts the grouping's allocated bytes grow by
        at most 32 per added row at fixed candidates and edges — every
        internal vector pre-sized exactly, the root stack included —
        and was red against the pre-rewrite code first. The `Edge`
        width comment is re-worded to the new reading pattern rather
        than moved: "walked twice" stops being true.
      tier: T2 · role: Critic
- [ ] S28.3 One word per row for the walked storage version
      done: `Epoch::storage_versions` holds 8-byte elements, pinned by
        an element-size helper over the live field, red against today's
        `Option<usize>` first. The sentinel is `usize::MAX`, and the
        invariant is parity rather than one value: an array version is
        even (`StorageHead::coherent` answers even only), the
        `OutsideCells` walk contract gains the clause that a group's
        coherent version is even too, and a `debug_assert` on evenness
        guards the one recording site in `walk_edges`. The sentinel
        test in `row_still_has_its_cells` stays ahead of the kind
        dispatch, replacing today's `let Some` verbatim — a Reference
        row must keep returning early, its `+8` being a Value payload
        and not a class word. Seen red: the version comparison mutated
        to accept a stale reading (`== walked || == walked + 2`) turns
        `a_component_whose_array_moved_its_entries_is_acquitted` red,
        and was reverted. The `storage_versions` doc moves with the
        encoding.
      tier: T2 · role: Critic

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
- [ ] **A budgeted epoch arena for the collector's metadata**, reused
  across collections (`dev/RC_WALK_CRITICAL_REVIEW.md`, "Per-epoch graph
  metadata is heavy") — gated, like the escalation ladder above, on a
  production driver and a starvation measurement that do not exist yet.
  S28 flattens the metadata; it does not fund its reuse.
- [ ] **`rc-satb` as a second build-time GC strategy**
  (`rfc/model/gc/satb.md`). The `WRITING` bit it waited on is pinned and
  the barrier's hook site is reserved; nothing else of it is built.
- [ ] **The birth count and the unique-owner policy**
  (`rfc/model/gc/rc-walk.md`, "The birth count" and "Unique ownership",
  designed 2026-08-17) — gated on a Phase D measurement of the share of
  dynamic publications with compiler-provable targets; the move rule
  (copy, barrier, or a never-moved proof) is the open design question.
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
  ownership pair — including the
  fast class that can block its own memory return — is
  `dev/design/owned-slots-and-the-walk.md`.
- [ ] **Proof horizon, the borrow elision**
  (`dev/design/proof-horizon.md`, Edmond's algorithm, 2026-08-18) —
  closed, and no pre-D step can change that status: the scan is
  kill-only, the census is undated, every verification artifact
  needs the compiler. Pre-D work is instrument preparation: the
  graded corpus scan, the census channel list owed to
  `dev/DECISIONS.md` before the census is specified, the
  summary-language question. Three Critic rounds are recorded in
  the document; the granularity ruling landed 2026-08-18
  (`dev/DECISIONS.md`), and the corpus names and the
  family-borrow-analysis and summary-language rulings are Edmond's.
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
