# Plan

Implementation plan for `ll-model`, re-sorted 2026-07-24 against the RFC
after the 2026-07-22 object-layout redesign landed in the `rfc` repo.

Design lives in `rfc` and is authoritative — read before coding, do not
re-derive: `model/classes.md`, `model/values.md`, `model/lowering.md`,
`model/gc/rc-cycle.md`, `model/gc/cycle/questions.md`, `model/memory/ffi.md`,
`runtime/object-lifecycle.md`.

The `rfc` repository carries its own plan at `dev/PLAN.md` for work that lands
in the specification rather than in this crate.

Updated: 2026-08-26 · Active: S30 — the sections after S40 are the backlog

**Closed stages are deleted whole** (rule 23.1.3), and what outlived each
of them is in the journals rather than here: `dev/DECISIONS.md` for a
decision and its reason, `dev/POSTMORTEM.md` for a trap,
`dev/BENCHMARKS.md` for a measurement, `dev/INDEX.md` and
`dev/ARCHITECTURE.md` for the map. Deleted so far: S4 through S29. A number is never reissued, so a
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

**S28 was abandoned rather than closed** on 2026-08-26: it optimised the epoch
metadata of `rc-walk`, and `rc-walk` is deleted by S30. The reason is in
`dev/DECISIONS.md` so the abandonment is not mistaken for completion.

**S29 was split by the same ruling.** S29.1 — a cyclically dead `Lazy` never
runs `__destruct` — closes with the code it lives in, deleted by S30. S29.2 —
thread exit leaves the enrolment bit set and leaks the cycle — is not a defect
of the old collector alone: the new design parks slots on the same bit, so the
defect would be reproduced. It is carried as S39.

**Verification collapses to one configuration at S30.** With both collectors
gone there is no `rc-walk` feature and no `rc-trace` default, so the matrix in
`dev/WORKFLOW.md` loses its GC axis: one `cargo test --lib`, three threaded
runs at four threads, and one release build. The `hash-folding` axis is
untouched.

---

## S30 — Delete `rc-walk`, `rc-trace` and the horizon  [active]

Goal, ruled by Edmond 2026-08-26: nothing of the two old collectors survives in
the working tree, in code or in documents, so that no reader — human or agent —
takes a superseded mechanism for the design in force. The old state stays
reachable as a branch rather than as files.

Done when: the crate builds and its suite is green with no cycle collector at
all; `rg 'rc-walk|rc-trace|gc-horizon'` over `src/` and `dev/` returns only
history references in the journals; `dev/tools/linkcheck.php` in `rfc` reports
zero broken links.

- [ ] S30.1 Tag the pre-deletion state in both repositories
      done: branch `archive/pre-rc-cycle` exists in `model` and in `rfc` at the
        commit before the first deletion, and its name is recorded in
        `dev/DECISIONS.md` so a later reader knows where the old code went
      tier: T0 · role: —
- [ ] S30.2 Delete the `rc-walk` code
      done: `src/walk.rs`, `src/walk/`, `src/collector.rs`, `src/collector/`,
        `src/epoch.rs`, `src/epoch/`, the `rc-walk` Cargo feature, the 24 files
        gated on it and the 13 carrying `not(feature = "rc-walk")` guards are
        gone — **except `src/memory/retained.rs`**, whose occupancy index
        `rc-cycle` inherits for retained blocks and which therefore moves rather
        than dies; what `rc-cycle` inherits — the deferred-free parking, the
        handshake, the exact test — has **moved** into its own module rather
        than died, and each moved item is named in the commit body
      tier: T2 · role: Code Reviewer
- [ ] S30.3 Delete the `rc-trace` code
      done: `src/gc.rs` and its candidate buffer are gone, together with
        `CANDIDATE_INDEX_*`, `CYCLE_COLLECTOR_COLOR_SHIFT` and the buffered
        bit's old meaning; every call site that reached into the buffer
        (`object.rs`, `array/entity.rs`) is closed rather than stubbed
      tier: T2 · role: Code Reviewer
- [ ] S30.4 Decide each dying test rather than sweeping them
      done: every test deleted with S30.2 and S30.3 is listed in the commit
        body under one of two headings — *encodes a contract that dies with the
        mechanism* or *encodes a contract that outlives it, and has moved*;
        `cargo test --lib -- --list` is diffed before and after, and the
        difference is exactly that list
      tier: T2 · role: Critic
      handoff: `dev/WORKFLOW.md` forbids deleting a test to go green. These are
        deleted because their subject is gone, which is a different act — and
        the difference has to be visible, not asserted.
- [ ] S30.5 Delete the documents of both collectors and the horizon
      done: in `rfc`, `model/gc/rc-walk*.md`, `model/gc/walk/`,
        `model/gc/retained-block-walk.md`, `model/gc/gc-horizon*.md`,
        `model/gc/gc-horizon-cases/`, `model/gc/gc-horizon-v2/`,
        `model/gc/strategies.md`, `dev/TASK-rc-walk-proof.md` and
        `dev/tools/rc-walk/` are gone; in `model`,
        `dev/RC_WALK_CRITICAL_REVIEW.md` and the three `dev/design/` records of
        the walk are gone; every inbound link is repointed or its paragraph goes
        with it
      tier: T2 · role: —
      handoff: `satb.md` stays — it describes something unbuilt rather than
        something superseded. `heap-design.md` stays, minus the CAS-handoff
        section, which dies with the GC-state field.

## S31 — The header's new flag layout

Goal: one collector claims the flags word, so the layout is chosen for the
paths that read it rather than for a truce between two strategies.

Done when: the layout below is in `refcount.rs` with every constant named, the
enrolment gate is one mask, and the kind field is four bits wide.

| bits | field | | bits | field |
|---|---|---|---|---|
| 0–1 | memory category | | 12 | has weak references |
| 2–5 | entity kind (4 bits) | | 13 | destructor pending |
| 6 | copy-on-write | | 14 | destructor ran |
| 7 | arena reset mark | | 15 | free |
| 8 | acyclic gate | | 16–17 | epoch |
| 9 | ownership mark | | 18–19 | maturation age |
| 10 | enrolled | | 20–23 | collector reserve |
| 11 | live escapee | | 24–31 | free |

- [ ] S31.1 Renumber the entity kinds so the predicates become masks
      done: `Object 0, Lazy 1, Array 2, Reference 3, String 4, StringDynamic 5,
        Box 6, WeakRef 7`, codes 8–15 free; the category keeps bits 0–1 because
        more surviving sites read its value than the kind's, and a mask test is
        position-free; "closes a cycle" is
        `flags & 0b110000 == 0`, "carries a class at +8" is
        `flags & 0b111000 == 0`, "is a string" is `flags & 0b111000 == 0b010000`;
        `CANDIDATE_KINDS`' bitset is replaced by the range test and the decision
        that refused renumbering is superseded in `dev/DECISIONS.md` with its
        reason
      tier: T2 · role: Critic
      handoff: the renumbering was refused once, when it would have bought one
        test at the price of churn. It rides along here because the field is
        being rebuilt anyway.
- [ ] S31.2 Fold the string's layout into the kind field
      done: `STRING_OUT_OF_LINE` is gone, `LLStringDynamic` is selected by kind
        code 5 — whose meaning is **bytes outside the body, whatever the
        reason**, not "growable" — and every "is a string" site accepts both
        codes; a red-first
        test proves an out-of-line string is still read through `data`
      tier: T2 · role: —
- [ ] S31.3 The enrolment gate is one mask
      done: the release path decides with `flags & 0x733 == 0` — category zero,
        kind below four, class not acyclic, ownership not proven, not already
        enrolled — and a `#[cfg(test)]` counter past the gate proves each of the
        five conditions rejects on its own
      tier: T2 · role: —
      handoff: a scenario test covers a pair, never one half — the counter is
        what sees a condition that never fires.
- [ ] S31.4 Narrow the mutator's header writes
      done: the mutator writes the refcount with a 32-bit store and the flags
        with a byte store, so no mutator write spans byte 2; the whole-word
        `mutator_update_flags` is gone, and a test asserts the collector's byte
        survives a concurrent flags update
      tier: T2 · role: Critic
      handoff: today's comment promises the opposite — "may bury a concurrent
        collector byte store". Either this step holds, or the lossy contract is
        inherited explicitly; it is not left implied.

## S32 — The block header's collector triple

Goal: the collector reaches a block's shadow array without touching the cache
line the owner writes.

Done when: the triple sits in the free tail of the block's 256-byte header
line, and the slot index derived from an address is proven exact.

- [ ] S32.0 Dispatch on the block's kind before any row
      done: the trace reads the block header first and branches — ordinary
        entity block by arithmetic, retained block by binary search over the
        occupancy index, large entity to a row in its own block header, and any
        other kind stops the descent with the child read as an external live
        reference; a test drives one entity of each population
      tier: T2 · role: Critic
      handoff: the arithmetic covers one population of three. A retained block
        was filled by an arena's bump — mixed sizes, no stride — and this is
        what `memory/retained.rs` was built for.
- [ ] S32.1 Prove the slot index derivation
      done: `((p & BLOCK_MASK) - LINE_SIZE) * recip >> 32` returns the slot's
        own index for every size class and every slot of a block, proven by an
        exhaustive test rather than by sampling
      tier: T1 · role: —
- [ ] S32.2 Put the triple in the header's free tail
      done: `HeapBlockHeader` occupies 192 bytes of the 256-byte line and the
        triple — shadow pointer, `recip`, the collector's own copy of the size
        class — sits past it on its own cache line; the layout test that pins
        the header's halves is extended rather than replaced
      tier: T2 · role: Code Reviewer
      handoff: the size class is duplicated on purpose — it is written once at
        commissioning, and the copy is what keeps the lookup off the owner's
        line.

## S33 — The shadow arena and the per-block rows

Goal: the collector's working state lives entirely off the heap, and an
aborted collection costs nothing.

Done when: a collection allocates rows, uses them, and returns everything in
one reset, with no write into any entity.

- [ ] S33.1 The arena
      done: a bump arena over 64 KB blocks from the pool, taken by the
        collector and returned whole at the end of a collection; a refusal to
        grow aborts the collection rather than failing the process
      tier: T2 · role: —
- [ ] S33.2 The per-block row array
      done: `slots × 4` bytes reserved at a block's first touch **without being
        zeroed**, the pointer stamped into the block's triple, the block pushed
        onto the touched list; the met flag lives in a bitmap of one bit per
        group of eight slots, only the bitmap and a touched group are
        initialised, and the row is colour 2 plus working count 30
      tier: T2 · role: —
- [ ] S33.3 Name the saturation clause
      done: a working count that would exceed the field saturates, saturation
        reads as "external references exist, conservatively live", and a test
        drives an entity past the bound
      tier: T1 · role: —
- [ ] S33.4 Hold the row at four bytes
      done: no captured count is stored — the commit stage judges again rather
        than comparing with a captured value — and a probe confirms the zeroing
        bill is the bitmap's, not the array's (1.4 ms against 41–76 ms measured
        for the 717 MiB case)
      tier: T1 · role: —
      handoff: decided 2026-08-26 by the ruling that phase 2 is a second
        judgement. Storing a captured value would have doubled the row and the
        design's memory with it.

## S34 — The root queue, enrolment and parking

Goal: candidates reach the collector without the mutator paying for a data
structure, and an entity that dies while enrolled leaves no dangling pointer.

- [ ] S34.1 The queue against Y12's contract
      done: the six clauses hold — a failed growth never drops a root, no
        allocation happens on the enrolling thread's hot path, and a second
        reader is either supported or refused by construction
      tier: T2 · role: Critic
- [ ] S34.2 The law: only the owner reduces state
      done: no dirty pass clears an enrolment bit, drops a queue entry or
        returns a slot; a reader may mark an entry a corpse and pass it on, and
        a test proves the acquittal case — ring A↔B with an external X→B that is
        released after the trace read the count — does not lose the ring
      tier: T2 · role: Critic
- [ ] S34.3 Parking a slot that dies enrolled
      done: death runs in full — weak cells cleared first, then `__destruct`,
        then children released — and the slot is withheld from the allocator
        while a queue entry names it; the drain reads the refcount, retires a
        zero-count entry, clears the bit and returns the slot without touching
        the body
      tier: T2 · role: —
- [ ] S34.4 Prove the corpse rule against arena reuse
      done: a red-first test enrols, kills, resets the arena and drains, and the
        category-zero clause is what makes it pass
      tier: T1 · role: —

## S35 — Mark and scan

Goal: trial deletion runs entirely in the shadow rows.

- [ ] S35.1 Mark
      done: the trace decrements children's working counts in their rows and
        writes nothing into any entity; an aborted mark leaves the heap
        byte-identical, proven by hashing the touched blocks before and after
      tier: T2 · role: —
- [ ] S35.2 Scan
      done: a non-zero working count marks its reachable set live, a zero one
        leaves it white, and the pair is proven on a graph with an external
        reference into the middle of a ring
      tier: T2 · role: —

## S36 — Commit

Goal: only the owning thread frees, and it frees what the judge condemned and
the exact test confirmed.

- [ ] S36.1 The exact test on the owner's thread
      done: current fields are re-read on the owning thread before any free,
        and the test's refusal path is exercised by a mutation racing the
        verdict
      tier: T2 · role: Critic
- [ ] S36.2 The epoch parking
      done: a slot freed inside a collection waits for its end, and a red-first
        test shows the defect it prevents — a reused slot inheriting the dead
        occupant's row
      tier: T2 · role: —

## S37 — Maturation and the two class gates

Goal: the trace stops following the whole heap.

- [ ] S37.1 The maturation stamp
      done: epoch and age live in the header's byte 2, written by one byte
        store, and an entity is traced only after it has stayed a candidate
        across `k` collections; the two-bit epoch's wrap is retired on contact
      tier: T2 · role: —
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

## S38 — The claim and concurrency

Goal: a collection runs either in a collector thread or in the mutator, never
both, and the losing side never deadlocks.

- [ ] S38.1 The claim
      done: one word for the process, three states, CAS from free; it covers the
        **trace** — the arena, the block triples and the touched list — while
        each owner's exact judgement runs at its own checkpoint, so a waiting
        thread delays only the components it is party to
      tier: T2 · role: Critic
- [ ] S38.2 The working wait
      done: an in-line collection needs no verdict list, no handshake and no
        second phase — it is exact because the owner sees its own stack — and a
        mutator that cannot allocate while a **collector thread** holds the claim
        waits for the trace to end rather than preempting; a test with a starved
        allocation and a running collection proves no deadlock
      tier: T2 · role: Critic
      handoff: waiting rather than preempting is Edmond's ruling of
        2026-08-26. The working wait is what keeps it from deadlocking against
        ruling 5.
- [ ] S38.3 Parking the mutator's frees during a trace
      done: while a collection is in flight over a thread's blocks, that
        thread's frees park until it ends; the cost is measured as the churn
        held across one collection
      tier: T2 · role: —

## S39 — Thread exit  (carried from S29.2)

- [ ] S39.1 Exit drains its own queue
      done: `ll_thread_exit` retires its queue before handing the heap over,
        the fate of live enrolled entities at exit is named rather than left to
        the reader, and a red-first test kills a thread between enrolment and
        collection
      tier: T2 · role: —

## S40 — Measure the trace's density and decide the row form

Goal: the one number the design still lacks.

- [ ] S40.1 Measure
      done: the share of a touched block's slots that a real collection traces
        is measured on the corpus and on a synthetic load, with the instrument
        checked against a known answer
      tier: T2 · role: Bench
- [ ] S40.2 Decide chunks or not
      done: below 29 % density the chunked form replaces the flat array and the
        measurement is quoted in the decision; above it the flat array stands
        and the alternative is recorded as refused with its number
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
- [ ] **GC horizon, the borrow elision** (`rfc/model/gc/gc-horizon.md`,
  Edmond's algorithm, 2026-08-18, named `proof-horizon` until
  2026-08-20) —
  closed, and no pre-D step can change that status: the scan is
  kill-only, the census is undated, every verification artifact
  needs the compiler. Pre-D work is instrument preparation: the
  graded corpus scan, the census channel list owed to
  `dev/DECISIONS.md` before the census is specified, the
  summary-language question. Three Critic rounds are recorded in
  the document; the granularity ruling landed 2026-08-18
  (`dev/DECISIONS.md`), and the corpus names and the
  family-borrow-analysis and summary-language rulings are Edmond's.
  The case book (`rfc/model/gc/gc-horizon-cases/`, 2026-08-20) opened
  five further questions in the algorithm — the weak cell's uncounted
  edge, promotion in the arena and immortal categories, raise sites in
  the placement rule, the COW-unique intersection, and runtime entries
  read as calls.
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
