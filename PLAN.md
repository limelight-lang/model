# Plan

Implementation plan for `ll-model`, re-sorted 2026-07-24 against the RFC
after the 2026-07-22 object-layout redesign landed in the `rfc` repo.

Design lives in `rfc` and is authoritative — read before coding, do not
re-derive: `model/classes.md`, `model/values.md`, `model/lowering.md`,
`model/gc/strategies.md`, `model/gc/satb.md`, `model/memory/ffi.md`,
`runtime/object-lifecycle.md`.

Updated: 2026-08-16 · Active: S27

**Closed stages are deleted whole** (rule 23.1.3), and what outlived each
of them is in the journals rather than here: `dev/DECISIONS.md` for a
decision and its reason, `dev/POSTMORTEM.md` for a trap,
`dev/BENCHMARKS.md` for a measurement, `dev/INDEX.md` and
`dev/ARCHITECTURE.md` for the map. Deleted so far: S4 through S26. A number is never reissued, so a
stage added later sits where it is to be done rather than where its
number falls, and the prose sections below are the backlog stages are
drawn from.

**`array::` has had its Miri run, in slices**, and the hour the module was
feared to cost was `array::entity`'s alone: `array::entry` took 4 seconds
over 7 tests and `array::table` 92 over 38, both clean, on 2026-08-13 at
`8d3728d`. A slice is still how the module is run — invocation and thread
cap in `dev/WORKFLOW.md`, Miri.

## S27 — the per-process key, the key word's tag, and the ladder's terminal rung  [in progress]

Goal: the crate's debt to the map design is paid — `rfc/model/maps.md`,
"What the crate owes before either class exists" and "The key word gains
a tag, for every owner" — so no map class waits on this crate and a spent
ladder refuses instead of degrading. `reseed` and `escalate` both return
early once `TABLE_STRONG` is set, so today a chain grows without bound
after two firings.

Done when, six items, none of them true today:

1. The drawn salt differs from `hash_bytes` of the bare storage
   address — the pre-change derivation, which distinct addresses
   already satisfied, so only this form can go red on the defect — and
   a source-reading test shows `draw_salt` derives from the key module
   rather than from the foldable seed.
2. The per-process key reads the same twice in one process, the
   underlying draw seen through the test window yields fresh bytes,
   and the module carries no `hash-folding` arm.
3. A ring closing through a string-keyed array is collected in a new
   collector test, and the two tracer tests
   (`walk/tests/the_children_a_kind_has.rs:62` and `:118`) go red if the
   child is left tagged.
4. An insert whose trigger trips with no rebuild left is refused with
   every entry unchanged, on an outcome the caller tells apart from an
   allocation refusal.
5. A copy of a table whose ladder is spent still copies, and no chain in
   a copy exceeds `CHAIN_LIMIT`.
6. The `dev/WORKFLOW.md` gate is green, plus Miri slices over
   `array::entry` and `array::table` for the new integer-to-pointer
   mask, and over the walk tests that carry the tracer's masked read —
   the ring test and the two tracer tests.

Plan reviewed by Critic twice before it was written here; the five
questions the second round left open were ruled by Sage and the rulings
sit on the steps they bind. A third round ran on 2026-08-17 against the
code at `b79b6ec`; its findings are folded into the steps and the gate,
and the one price outside the crate — the Windows build S27.1's
`compile_error!` refuses — is a backlog item for a Windows session.

- [ ] S27.6 `sever_entries` unlinks the holes it makes
      done: after a sever no chain reaches a hole, pinned by a test that
        inserts into a severed table
      tier: T1 · role: —
      Found in passing while the plan was written (rule 3). Runs first:
      a known defect precedes new work (`dev/WORKFLOW.md`, "Bugs
      first"), and the 2026-08-17 round noted that S27.5's rung three
      would otherwise make these hole-counting chains refusal-capable
      before the fix. The number stays where it was issued. `remove`
      unlinks and `sever_entries` does not, so a severed table's chains
      still run through its holes and an insert's `chain_len` counts
      them. Inert today, because nothing inserts into a severed table;
      with rung three it becomes a refusal of a legal insert the day a
      map's teardown leaves one insertable. Fixed rather than recorded:
      `dev/WORKFLOW.md` forbids a note about an unfixed defect anywhere
      in `dev/`, this repository being public.
- [ ] S27.1 The per-process key: 32 bytes from the OS, once per process,
      in every build, outside `STAMP` and exempt from `hash-folding`
      done: the underlying draw, called twice through a `#[cfg(test)]`
        window, returns different words — fresh OS bytes rather than a
        cached constant; the memoized accessor is equal across calls
        and threads; and a source-reading test in the idiom of
        `refcount::tests::who_may_read_a_header` shows the module carries
        no `hash-folding` arm and that `STAMP` names it nowhere
      tier: T2 · role: Critic
      Sage 2026-08-16: the 32 bytes come from `/dev/urandom` through safe
        `std::fs`, and `#[cfg(not(unix))]` is a `compile_error!` naming
        the missing door.
      Critic 2026-08-17, price the ruling had not named: the crate keeps
        a Windows build on purpose (`heap.rs`'s `#[cfg(windows)]` doors,
        the TLS fast path), and it refuses from this step until the
        Windows door lands. Edmond defers the door to a session on the
        Windows box — backlog, "The per-process key's Windows door". `RandomState` cannot serve — it caches one
        `(k0, k1)` per thread and afterwards increments `k0`, so any
        number of words carries 128 bits, and they would share the master
        the string seed is drawn from, which `rfc/model/strings.md`
        forbids for this key. Final.
      Shape: `src/hash/process_key.rs`, four `u64` words, drawn lazily
      behind a one-time check and forced at startup by
      `ll_hash_seed_init`, which stops being a no-op under
      `hash-folding`. Which words a consumer takes is fixed in that
      module's doc, once, for every consumer. The per-process guarantee
      is per-deployment under a pre-forking master, by citation to the
      seed's fork paragraph.
- [ ] S27.2 One encoding of the `key` word, tagged in its low three bits,
      for every owner
      done: a new collector test puts a string-keyed array in a ring and
        sees it collected, written first and seen failing against a
        tracer that masks the recorded raw word; the two tracer tests
        hold the child half; every reader `rfc/model/maps.md` ("What
        moves with it") names dispatches on the tag
      tier: T2 · role: Critic
      The invariant, stated before the work: `word < 8` is tested first
      and keeps its sentinels — `KEY_INT = 0`, `KEY_HOLE = 1`,
      `is_hole()` stays `word == 1` — and the tag test is reached only
      for `word >= 8`, the design's table giving `word == 1` to both the
      hole and the string tag. The order is imposable at every site,
      `walk.rs:502` included: it takes one relaxed load and makes its own
      test. The tag goes on in `Entry::set_string_key` and comes off in
      `Entry::string_key`, nowhere else, and `Key::Str` stays untagged at
      the API edge. Two readers see the word untranslated today and both
      are wrong the moment the tag lands: `entry_slot_hash`'s
      `string_bytes(e.key)` (`table.rs:464`) and the tracer
      (`walk.rs:501`), where the child is masked and the recorded raw
      word is not. `set_string_key` gains a `debug_assert` that a string
      key is 8-aligned — otherwise the mask hands `string_bytes` a header
      four bytes early, an out-of-bounds read rather than a crash.
      Falsified in place, so they move in the commit:
      `rfc/model/arrays-hashtable.md:31` and `entry.rs:8`, `entry.rs:35`,
      and `entry/tests/the_key_word_as_a_discriminant.rs`.
      One AND on the key accessor is a cost the design accepted in "A
      fifth word was refused"; this box cannot resolve it and no speed
      claim is made.
- [ ] S27.3 The ladder draws under the key, and its slots are salted for
      strings as well as integers
      done: the drawn salt differs from `hash_bytes` of the bare storage
        address in both hash builds; `draw_salt` derives from the storage
        address and the per-process key; a reseeded table's string slot is the salted mix of the
        cached hash in `slot_hash` and `entry_slot_hash` both;
        `strong_hash` takes the key together with the salt; both
        functions dispatch on the tag with the byte-hashing branch
        `debug_assert`ed unreachable from any other; the equal-identity
        counter tests tag equality rather than "not an integer key"
      tier: T2 · role: Critic
      Sage 2026-08-16: rung one's string half lands here rather than in
        the map stage — under `hash-folding` a cached string hash is a
        build constant, so today's rung one rebuilds an offline-built
        chain into the same chain. The object and array halves stay with
        the map stage under the no-producer rule. Final.
      Rule 4 falls due here: the string half turns
      `a_long_chain_draws_the_salt_once_and_then_escalates` red, because
      a scattered chain no longer escalates on the next key. Rewritten
      rather than weakened — it reads the drawn salt through the
      `#[cfg(test)]` window and forges the second set under the salted
      mix — and the sign-off is Edmond's, asked at the step.
      `strong_hash` keeps its construction: it is re-keyed, and the
      long-key slot it stands in for stays owed, recorded as a dated
      `dev/DECISIONS.md` entry amending 2026-08-13 and a backlog line
      below. The tag-equality change is verified by reading rather than
      by the suite — in an array "not an integer key" and "the tag equals
      the incoming string's" name the same set — and a test for it is
      owed to the map stage.
      Falsified in place: `draw_salt`'s block and `reseed`'s "the two
      rungs defend different key kinds, this one integer keys".
- [ ] S27.4 `Table::insert` answers a three-valued outcome, and knows a
      replay from an admission
      done: the outcome carries admitted, refused-for-memory and
        refused-by-the-ladder, the call takes which kind of insert it is,
        and all five non-test callers plus the two test harnesses answer
        it with the suite green and the third variant unreachable
      tier: T2 · role: Critic
      The callers: `element.rs:550`, `element.rs:732`,
      `entity::migrate_to_hash`, `entity::fill_table_from`,
      `entity::CopiesMade::record`, and the harnesses
      `array/testing.rs:73`, `array/table/tests.rs:38`. The two channels
      with no room for a fourth value keep their null and are named here
      rather than discovered later: `fill_table_from` unwinds into
      `ll_cow_separate`, whose refusal is a null `*mut RcHeader`, and
      `element::make_ref` refuses with a null `*mut LLReference` over
      four causes already. `CopiesMade::record` is genuine admission and
      not a replay, being a table keyed by entity address. Nothing
      observable changes here; the worth of the step is that S27.5 lands
      as one behaviour change, which holds only because the replay
      channel is part of the same signature pass.
- [ ] S27.5 Rung three, and the copy that must survive it
      done: a table whose ladder is spent refuses an insert that trips a
        trigger, with every entry unchanged, on S27.4's third outcome; a
        copy of that table still copies; no chain in a copy exceeds
        `CHAIN_LIMIT`, pinned by the dense-then-unset scenario; the copy
        tests are written first and seen failing against rung three
        alone, which is the order the commits land in
      tier: T2 · role: Critic
      Sage 2026-08-16, three rulings. The rung state is the two flags
        with no counter: neither flag — draw the salt and rebuild;
        `TABLE_RESEEDED` alone — escalate; `TABLE_STRONG` — refuse; and
        the equal-identity trigger escalates an unescalated table and
        refuses on an escalated one. A table escalated through that
        trigger has both rebuilds spent, `escalate` drawing the salt on
        its way, so refusing its first chain trip is the principle rather
        than a stricter reading of it, and `rfc/model/maps.md`'s
        enumeration gains the fourth case to match. The copy keeps the
        rung bits and redraws the salt from its own storage address under
        the per-process key, inside `new_empty_copy` once the presized
        storage exists: nothing in an entry depends on the salt, the
        replay re-derives every slot, and a fresh salt scatters the set
        built against the source. Rungs one and two stay armed on the
        replay and only the refusal is exempt — a key admitted once
        cannot be refused on re-admission — and `new_empty_copy` presizes
        the copy to the source's `nslots`, so no bucket merges. Blanket
        exemption was refused: it leaves a copy of a dense-then-unset
        array with one chain of 64 and no flags, permanently and
        heritably. `migrate_to_hash` is out of the rule entirely, dense
        positions on an unsalted staging table being unable to fire a
        trigger, and a `debug_assert` records the proof. Final.
      Cost named because it is a behaviour change of its own: a copy of
      an unset-shrunk source carries the source's slot array until its
      next `grow` — 1 MB of slots in the scenario above.
      Rule 4 falls due again:
      `a_copy_of_a_reseeded_table_inherits_the_drawn_salt` inverts its
      assertion, an intentional change and Edmond's to sign off.
      Falsified in place: `reseed`'s "a second firing escalates instead"
      and its "a COW copy inherits the drawn salt", `escalate`'s "once
      and one way", `adopt_flood_state`'s block, `new_empty_copy`'s doc,
      `Table::insert`'s "Returns `None` when the storage could not grow",
      `element::set`'s "Three refusals report `false`",
      `CopiesMade::record`'s doc, and the stated rationale of
      `entity/tests/the_flood_state_a_copy_inherits.rs`. The `maps.md`
      amendment goes to the `rfc` repo in the same push.
What the stage does not do: no map class and no array-key content hash;
no error raise, the crate having no error channel, so the third outcome
dead-ends inside the crate until the exceptions work; no long-key slot,
`strong_hash` being re-keyed rather than replaced; and no measurement, so
no figure in `dev/BENCHMARKS.md` moves.

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
- [ ] **The birth count and the unique-owner policy**
  (`rfc/model/gc/rc-walk.md`, "The birth count" and "Unique ownership",
  designed 2026-08-17) — gated on a Phase D measurement of the share of
  dynamic publications with compiler-provable targets; the move rule
  (copy, barrier, or a never-moved proof) is the open design question.
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
ladder's repair and the key word's tag — **is S27 above**, taken as one
stage on 2026-08-16 because the three are one dependency chain.

- [ ] **The long-key slot itself.** S27 re-keys `strong_hash`; it does
  not fill the slot `strong_hash`'s doc stands in for, which is
  HighwayHash-64 behind a length threshold `rfc/model/strings.md` says is
  unmeasured. Blocked on that measurement, and it belongs with the
  strings work rather than the table's.
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
