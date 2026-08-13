# Plan

Implementation plan for `ll-model`, re-sorted 2026-07-24 against the RFC
after the 2026-07-22 object-layout redesign landed in the `rfc` repo.

Design lives in `rfc` and is authoritative — read before coding, do not
re-derive: `model/classes.md`, `model/values.md`, `model/lowering.md`,
`model/gc/strategies.md`, `model/gc/satb.md`, `model/memory/ffi.md`,
`runtime/object-lifecycle.md`.

Updated: 2026-08-13 · Active: S8, then S18

**Closed stages are deleted whole** (rule 23.1.3), and what outlived each
of them is in the journals rather than here: `dev/DECISIONS.md` for a
decision and its reason, `dev/POSTMORTEM.md` for a trap,
`dev/BENCHMARKS.md` for a measurement, `dev/INDEX.md` and
`dev/ARCHITECTURE.md` for the map. Deleted so far: S4 through S7, and S9
through S17. A number is never reissued, so the stages below sit in work
order rather than in numerical order, and the prose sections after them
are the backlog the stages were drawn from.

**`array::` still owes a Miri run in slices.** The module costs about an
hour at two threads, so it goes as `array::entry`, `array::table`,
`array::element` and `array::entity`, each a foreground run under a
`timeout`. `array::element`, `array::entity` and `array::vector` have had
theirs; `array::entry` and `array::table` have not. Invocation and thread
cap: `dev/WORKFLOW.md`, Miri.

## S8 — The Map design, written before it is built

Goal: `Map` and `MapMixed` designed in `rfc`, with every question Edmond
reserved answered before a line of code.

Done when: the design document exists and its open section is empty or
explicitly deferred.

- [x] S8.1 The reserved questions put to Edmond, one at a time
      done: every question in the `Map` section below is answered and
        recorded in `dev/DECISIONS.md`
      tier: T2 · role: —
      Sage 2026-08-13: an array key's content hash lives in the map's
        entry, in `hash_or_key`, computed once on insert through a work
        list; nothing invalidates it, because the entry's own reference
        puts the key's count at two and every write separates first. The
        lazy cache in the array entity died on the header-bit ledger,
        there being no free bit in either configuration. Final.
      handoff: five answers in `dev/DECISIONS.md`, 2026-08-13, five
        entries. Two of them reshape the stage: a map is a class of the
        `Object` kind rather than a new entity kind, which makes it the
        second customer of S18's `walk` hook and puts S18 under S8.2
        rather than after it; and `Map` takes object keys only while
        `MapMixed` takes all four, so `Map` has no key kind to dispatch
        on at all. The plan's own `Map` section below is older than these
        answers where the two disagree.
- [ ] S8.2 The design document in `rfc/model`
      done: it covers the two classes and what each admits as a key, the
        object key as a counted child through S18's `walk` hook in trace
        and sever, `MapMixed`'s content hash on a work list rather than
        the machine stack, and what the COW attribute obliges a class to
        carry
      tier: T2 · role: Critic
      2026-08-13: the five answers of S8.1 are the document's input, and
        they leave it dependent on S18: a map's entries lie outside the
        entity, so the walker reaches them only through the class
        descriptor's hook. The document may be written before S18 is
        built, naming the hook as its dependency, but it cannot be
        written around it.

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

## Then: `Map`, whose keys may be objects

**Edmond ruled the order on 2026-08-07 — the array is finished first —
and it is.** A map is strategy 3 with a wider key, so it becomes the
entry's second customer, and the three entry questions the map would
otherwise have decided by accident are settled: the entry is 32 bytes
with the inline hash kept, the reserved room beside the collision link
went into the element's atomic second word, and the strategy tag lives in
the head rather than in the table. What is left below is the map's own.

Edmond's, 2026-08-07. A `Map` is the ordered hash again — strategy 3's
structure unchanged, one chunk of `u32` index slots over a dense array of
entries in insertion order — with the key widened from "integer or
string" to "integer, string or object". The design is not written; what
follows is the list of questions it has to answer, each verified against
the code as it stands rather than guessed.

**An object key is compared by identity, and the identity already
exists.** `spl_object_id` is the JVM trick: derived from the address
while the object stays put, lazily stored in the object and carried with
it when the arena reset evacuates one (`rfc/model/memory/arena-reset.md`).
So the hash of an object key is that id, and equality is pointer
equality — no user code on the lookup path, which is what makes this a
different type from a `Map` keyed by `__equals`.

**The entry cannot tell an object key from a string key today, and that
is the first thing to build.** `Entry::key` distinguishes its three
states by value: `KEY_INT = 0`, `KEY_HOLE = 1`, anything above is a
string pointer — and that last test is what a *walker* makes on the raw
word (`array/entry.rs`). An object pointer passes it, so a Map would
hand the tracer an `LLString` that is an `Object`. The kind has to sit
somewhere the walker reads, and the `meta` field this entry used to
name for it no longer exists: the collision link took the element's
second word and the two spare bytes above the tag and the flags are what
is left there. Where the kind goes is therefore part of S8.2's design
rather than a settled answer, and the atomic-width rule decides it —
every byte the collector reads is written by one store of the width it
loads.

**An object key is a counted child**, exactly as a string key is: the
table owes it a reference, `for_each_counted_child` has to yield it, and
`trace_cells` has to see it, or a ring closing through a key leaks with
no pass finding it. The sever path owes the same — a cleared entry drops
its key.

**`MapMixed` takes any key at all** — integer, string, object, array —
and the array key is the one that changes the shape rather than widening
it. An object is compared by identity; an array is a *value*, so it is
compared by content, and a content comparison needs a content hash. Three
things follow.

The hash of an array key is O(size) and recursive over nested arrays,
which is the deep copy's problem again in a new place: depth is the
program's to choose, so the walk that hashes needs the same explicit list
`array::entity::separate` now uses, not the machine stack. Nothing exists
to cache it: an array carries no hash field, the way a string carries one
at +16, and adding one costs eight bytes on every array or a bit saying
"not computed" in a byte that has room.

**A key that is an array cannot change under the map, and the reason is
already in the crate.** The map holds a reference, so the key's count is
at least two, so any write by any holder separates first
(`refcount::cow_separation_needed`) and leaves the map's key untouched.
Value keys are therefore sound without freezing anything — but the
argument rests on count-equals-holders, the same invariant the deep
copy's termination rests on, and it should be written down where a
reader of the map finds it.

Equality between two array keys is structural and has to stop early: the
content hashes differ for almost every pair, so the byte-by-byte walk
runs only after 64 bits already agree, exactly as a string key's does
today.

**What is Edmond's to decide**, and none of it is implied by the above:
whether `Map` and `MapMixed` are one type with a key-kind set or two;
whether they are a second entity kind or the array kind with another
storage strategy — kind codes are nearly spent, `7` is reserved and
`4`–`6` are one family the RFC wants to consolidate; whether an object
key's id defends against flooding by itself, ASLR being its only
entropy, or whether it goes through the same salted mix an integer key
does; whether an array key's content hash is cached and where; and
whether a map is COW like `array` or a reference type, which decides
whether copying a map copies its keys.

### Beside the hashtable: the memory categories

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

## S18 — `walk`: an optional second behaviour pointer on the class descriptor

Raised by `limelight-lang/io` on 2026-08-12 and agreed with Edmond the
same day. A coroutine there is an ordinary object of a runtime-provided
class, and it embeds its waker: two wait halves inline, and a raw block
holding them all when a wait has more than two. The block's cells lie
**outside** the object, so `ptr_runs` and `box_runs` cannot describe
them — the runs are offsets within the entity.

Goal: a class may name counted cells the runs cannot reach, and every
consumer of an object's cells sees them.

Done when: `Class` carries an optional `walk` beside `dispose`, it is
honoured on the one path all three consumers already go through, it is
inherited the way `dispose` is, and a test proves a class that uses it is
traced, collected and torn down correctly in both configurations.

Two things the consumer must be told, and they belong in the decision
rather than in the code alone. The hook yields **cells**, not children:
the collector records a cell's address and raw word and re-reads it in
Phase 3, so a hook that yielded only the child could not serve it. And a
block whose cells the collector has recorded may not be freed while an
epoch is in flight — it goes through `deferred_free`, which exists for
exactly that.

- [ ] S18.1 The hook's signature and its single call site
      done: `dev/DECISIONS.md` records the signature, why it yields cells
        through the reader rather than children, and that
        `object::for_each_counted_cell` calls it after striding the runs
        so that rc-walk, rc-trace and teardown all get it from one place
      tier: T2 · role: Critic
- [ ] S18.2 The field, the builder, and inheritance
      done: `Class` carries it, `ClassBuilder` installs it, a descriptor
        built for a subclass copies it as it copies `dispose`, and a test
        fails if that copy is dropped
      tier: T2 · role: Critic
- [ ] S18.3 A class whose cells lie outside itself, end to end
      done: a test class with an out-of-object block is traced by
        rc-walk, collected by rc-trace and released at teardown, its
        block freed through the deferred path; the suite is green at the
        gate's width in both configurations
      tier: T2 · role: Critic

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
