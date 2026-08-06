# Plan

Implementation plan for `ll-model`, re-sorted 2026-07-24 against the RFC
after the 2026-07-22 object-layout redesign landed in the `rfc` repo.

Design lives in `rfc` and is authoritative — read before coding, do not
re-derive: `model/classes.md`, `model/values.md`, `model/lowering.md`,
`model/gc/strategies.md`, `model/gc/satb.md`, `model/memory/ffi.md`,
`runtime/object-lifecycle.md`.

## Next: finish the hashtable — four pieces are left

Read this first in a fresh session. The design is in
`rfc/model/arrays-hashtable.md` (rfc `ca0d197`, `eb68707`, `9e5ae3d`,
`2a9ce2e`) and **the crate now has `src/array/`** with 37 tests, green in
both configurations and silent under Miri: the entry layout, the table
core, string keys, the flood backstop, element references and the entity
wrapper (`model` `514c526`, `80494ff`, `eaf6a03`, `42c2a6f`, `3316343`,
`3025757`).

### What is left, in order

1. **The 2 → 3 migration** from the mixed vector: walk the vector in
   order and append each element with its integer key, so insertion order
   survives by construction. Needs the mixed vector to exist first, which
   it does not — so this may become the reason to build strategy 2 next
   instead.
2. **Promotion of the storage out of the arena.** An arena survivor's
   storage is copied into the heap with the entity header fixed in place,
   the way a string payload is. Every link inside the storage is already
   an index rather than a pointer, which is what makes the copy legal, and
   a test pins that no word in the storage points into it. Still to build:
   the `Array` arm of `carry_external_memory`, the OS-direct transfer that
   moves a large storage without copying, and the refusal path that
   retains the payload's block because the reset has no caller to report
   to.
3. **Thread hand-over.** Storage should live in the per-thread buffer
   arena under its owner/abandon/adopt protocol (`dev/DECISIONS.md`,
   2026-08-04). It currently goes through the ordinary allocator instead,
   which is correct but not final — and *not* `entity_alloc`, which would
   put headerless storage in a block the collector reads as entities.
4. **The strategy tag and the `arrays.md` hole.** Two bits for the
   strategy plus the strong-mode bit live in the ArrayBox body, not the
   flags word, which has none free. And `arrays.md`'s "strategy 1 never
   transitions" cannot hold: separation copies the storage in its current
   representation, so a callee can store a pointer into a proven
   `array<int>`. The generic element write has to dispatch on the tag and
   transition 1 → 2.

### Two defects this work found, worth not repeating

The entry's `hash_or_key` holds the key's own identity — the raw integer
or the string's cached hash — while the index slot comes from a
*different* number, the salted mix or the keyed hash. Conflating them
makes insertion succeed and lookup lose every key, and it happened twice.
And table storage must never go through `entity_alloc`: the collector
reads the first eight bytes of every occupied slot in an entity block as
an `RcHeader`, and storage has no header.

### The design, so it is not re-litigated

The string stage before all this is finished: all 16 tasks below are
closed, both critic passes are done and their findings fixed, and the gate
and Miri were green in both configurations as of 2026-08-05.

The table is one
allocation of `u32` index slots plus a dense insertion-ordered array of
40-byte entries; the collision link is an explicit `next` field, because
`values.md` forbids per-slot state in the ValueBox's padding — the store
barrier writes all sixteen bytes, so Zend's trick of threading the chain
through the element would be severed by the first value store; the
ValueBox sits last, at +24, so no write it performs reaches the key or the
link. The index layer is **decided**, not deferred: chains, on measurement
at each index's design load, with the reversal test named in the
document's Open section.

**What it deliberately leaves open**, each a measurement rather than a
question of design: the string-key check with its named reversal threshold;
the compaction threshold, borrowed from Zend at ~3 % rather than measured;
and the two flood constants.

**One retraction is recorded there and repeated here**, because it cost a
day. The first set of index measurements was withdrawn after an
independent review found six defects in the harness across two passes —
among them that every table size was a power of two, so the open-addressed
index silently ran at load 0.5 rather than the 0.875 it exists for, and
that its deletion rule truncated the probe sequence of unrelated keys and
lost live entries. The numbers that stand are the second set, and they
stand for integer keys only.

What follows is the state of the string work, not the design.

**What landed 2026-08-05**, so that nobody starts it again: the rapidhash
v3 port with its generated vector table, the seed and the `hash-folding`
build option with its stamp, the critic pass on all of that, the first
string benchmark (`benches/strings.rs`), growing a long-lived payload off
the bump top, and rules 1–3 for the interpolated template in the RFC.
Commits `ec8d8f6`, `2d6be59`, `12fe843`, `98ca50c`, `c81cdf5`, `3c25db8`,
`d65ecac`.

**Also landed 2026-08-05**, after the two open pieces were written down
as a choice: adopted buffer blocks are reused rather than only held (the
Residual entry below, and `dev/DECISIONS.md` of that date). It was taken
first because it can be verified end to end today, and because array
storage lands in the same arena later.

**Task 9's runtime half also landed 2026-08-05** (`src/template.rs`),
which closes the last of the sixteen. What it deliberately does not
include is at task 9 below: the C ABI for a foreign consumer, and
flattening a float or an object.

**The collision-flooding debt the hash stage recorded as unpaid is now
placed**, though not yet built: neither arm of the seed defends against it
(`dev/DECISIONS.md`, 2026-08-04), and the table is where the defence
lives. It counts, per insert and against current state, the entries whose
full 64-bit hash equals the new key's — a size-independent constant,
unreachable by chance, and unaffected by deletion, which a running maximum
would not be. Firing escalates the table once to a keyed hash over the key
bytes; the cached string hash at +16 is untouched, being shared across
tables. Integer keys are indexed through a salted avalanche mix rather
than by value, since `0, 1024, 2048, …` otherwise share a bucket with no
knowledge of any seed.

**What today changed underneath everything else**, worth knowing before
touching any of it (all four entries are in `dev/DECISIONS.md`,
2026-08-04): a COW value leaving the arena is now **copied** by the store
barrier rather than counted as an escapee, and a publish can therefore
refuse — `store_ptr` / `store_box` / `ref_store` return whether the store
happened, and the `drop_ref` that follows an overwriting store must not
run when it refused. The buffer arena is a heap with the object heap's
ownership rules (per-block owner, MPSC stack for foreign frees, hand-over
and adoption at thread exit). The arena reset traces through
`walk::trace_entity` and settles a COW survivor's count as
`edges + delta`. Buffer chunks park during a collector epoch.

**Settled, and not to be re-derived:**

- Two layouts, one entity kind. `COW = 1` is inline
  (`RcHeader | len | hash | bytes`), `COW = 0` is dynamic
  (`RcHeader | len | capacity | hash | data`). `len` and `hash` sit at
  the same offsets in both, +8 and +16, so only byte access and teardown
  branch.
- `len` is `u32` (2026-08-04), which caps a string at 4 GiB and buys the
  dynamic layout its `capacity` inside the padding the 8-aligned `hash`
  creates anyway: that header goes from 40 bytes to 32, the inline one
  stays at 24. The cap is language-visible; every growth path checks it
  through one choke point and raises rather than truncating. Strings
  above it are a separate class later, not a third transparent form —
  that would branch every string operation and spend the last free
  `EntityKind` code.
- The COW flag is set at allocation and never flips, which is what makes
  it readable as the layout. No sub-mode bit exists; the flags word has
  no free one.
- A dynamic string never copies on write: it is the non-COW form, writes
  go in place, no sharing test. The compiler allocates one only where it
  proved a single owner.
- An inline string obeys the barrier rule in `rfc/model/values.md`, which
  reads **category, then the count** — an immortal entity's count is
  pinned at 1 by the retain/release early-outs, and the `IS_ESCAPEE` arm
  is gone (2026-08-04): a COW value is copied out of the arena at the
  store, so it never carries an escape count.
- On a COW entity the count equals the number of holders; deferred ARC
  does not apply to them at any tier.
- No freeze operation, and no runtime promotion between layouts: both
  would rewrite the body under a header `rc-walk` may be reading.
- Arena promotion becomes layout-aware — the header stays, the payload is
  reallocated into the heap, an OS-direct payload transfers ownership.

**Open, and blocking nothing yet:** the
cross-thread slot memory model that decides whether freeing a displaced
string must route through epoch-deferred reclamation.

**Task list, in dependency order** (16 items, all closed as of
2026-08-05). This list *is* the task
list — the session tool that tracks it
does not survive a cleared context, so it is rebuilt from here. The
bump-top growth above is not in this list because it is not a string
task; it belongs to the memory manager and is written up under
"Residual".

1. ~~Sweep the contract and list the holes~~ — done 2026-08-03.
2. ~~Find a home for a sub-mode bit~~ — dropped: the COW flag is the
   layout.
3. ~~Inline string in the GC heap: allocation, header, lazy hash~~ —
   done 2026-08-04, `src/string.rs`. `ll_string_new` in every category,
   the layout pinned by a test, the lazy hash with zero as "not
   computed" and the remap inside `hash_bytes`, `fits` as the single
   length gate, the `String` arm of `ll_entity_die`, and the walker
   counting a string as the leaf it is. Not here: the rapidhash port
   (its own step — it needs the reference test vectors in CI) and the
   dynamic layout.
4. ~~String teardown by layout~~ — done 2026-08-04. Inline frees its own
   block, dynamic frees the payload too, and an arena payload is left to
   the reset. Both assertions are the kind that fail when the branch they
   name is deleted: the heap payload has to reappear from the buffer
   arena's free list, and the arena payload has to still read back as its
   own content. Both collector configurations.
5. ~~Separation on write for inline strings, in the rule's order~~ —
   done 2026-08-04. `refcount::cow_separation_needed` decides,
   `string::separate` copies, and `object::ll_cow_separate` dispatches by
   kind, so whether to separate is a property of the header and how to
   copy a property of the layout. The copy's category comes from the
   holder, not from the original; it returns at +1 like every other
   factory here; its hash starts unset. The COW flag is tested before the
   category, which `values.md`'s order does not do — that order would
   copy a non-COW plain object when immortal or escaped and break
   reference identity.
6. ~~Dynamic string: buffer fields in the string's own order, growth in
   place, compiler-chosen at allocation~~ — done 2026-08-04. Payload
   through the memory manager's buffer machinery, routed by category;
   heap or request arena only. Left open behind it: an arena dynamic
   string cannot escape until 13 lands (refused at `escape_gain`), and
   buffer-arena frees do not park during an epoch — harmless for strings,
   load-bearing before arrays (task 16 in the TaskList).
7. ~~Freeze a builder into an immutable string~~ — dropped with the
   builder/frozen sub-modes: there is no freeze operation and no runtime
   promotion between layouts.
8. ~~`buffer_arena` on the thread-exit path~~ — done 2026-08-04, and it
   was not a tail but a live abort the moment `string_die` began freeing
   payloads. Converted to the no-drop-glue cell and disposed explicitly,
   fifth in `ll_thread_exit`'s order (`dev/DECISIONS.md`).
9. ~~Interpolated template as its own class — the runtime half~~ — done
   2026-08-05, `src/template.rs`. The design landed the same day as rules
   1–3 (`rfc/model/strings.md`), and Edmond amended rule 3 while this was
   being built: **one class for every site**, with the parts in a static
   per-site `TemplateShape` that is never allocated and never freed,
   rather than a generated class per site. The instance is
   `RcHeader | class | shape | Value[n]`, an ordinary entity; because the
   value count belongs to the instance, `object.rs`'s two child walkers
   branch on `CLASS_TEMPLATE` and read it from the shape.

   Built: the factory (its own references, published through the store
   barrier, refusal unwound), the walker and teardown branch, and the
   shared flattening routine — measure, allocate once through
   `string::new_uninit`, write each piece into place.

   **Left open, each waiting on something outside this crate.** The C ABI
   a foreign consumer would read the structure through is not written:
   Edmond's call, since there is no consumer until the compiler exists.
   Flattening refuses a float (the language's precision rules are
   undecided) and an object (`__toString` is user code with no call path
   in the crate); rule 3 puts that call in the measuring pass, so the
   place it goes is already fixed. Rules 1 and 2 stay the compiler's, so
   none of this can be exercised end to end yet.
10. ~~Documents move with behaviour, in the same commit; `rfc` stays in
    sync~~ — the two corrections owed to the RFC landed 2026-08-04 in
    `rfc` `1fa621c`. `values.md`'s COW rule now separates the immortal
    and long-lived arms and gives each its own reason, since "the count
    is pinned" described immortal only. `strings.md` no longer permits
    both layouts in every memory category: it names the two the dynamic
    layout is refused in, and why `ll_string_new_dynamic` refuses rather
    than redirects.
11. ~~The verification gate in `dev/WORKFLOW.md`, plus Miri in both
    configurations~~ — run 2026-08-04 on the stage as it stands. The gate
    is green: 223 tests under `rc-walk` and 207 under `rc-trace`, three
    threaded runs each at the 4-thread width, both release builds. Miri is
    silent in both configurations (218 and 204 tests). Neither is evidence
    about the four defects the critic found the same day — a mixed-width
    race is invisible to both tools, and the promotion count defect is
    masked by the retained block.
12. ~~Critic on the finished stage, then Fable on whatever the critic
    finds~~ — done 2026-08-04. Seven findings, each verified against the
    code before acting. Three mechanical ones fixed in `f1ea847`; the two
    structural ones went to Fable for the fix shape and landed as
    `49be716` (the reset's tracer and the COW count) and `b5781d9` (the
    buffer arena's ownership rules). Fable also corrected the critic on
    one point: the reset has no live stack, so the frame-slot half of its
    scenario cannot occur — the defect was real by two other routes.
13. ~~Layout-aware arena promotion: carry the payload~~ — done
    2026-08-04. One kind-dispatched call in the survivor pass, so
    promotion still knows nothing about any layout. An OS-direct payload
    transfers (the arena forgets the run, nothing is allocated, so the
    reset cannot be refused at a point with no caller left to report to);
    an in-block one is copied, bounded by a block payload. On refusal the
    payload's block joins the retained set, and a retained block is the
    one route `buffer_free_longlived_payload` leaves alone. The escape
    ban this replaced lasted a day. Arrays will need the same.

14. ~~**Port rapidhash v3**~~ — done 2026-08-04, in `src/hash/`. The
    function is transcribed from the header vendored at
    `vendor/rapidhash/` at a pinned commit, and the part that was the
    task — proving the transcription — is a table generated from that
    header, since the author publishes no vectors of his own. Seen
    failing: one bit inverted in `secret[0]` breaks it at generated
    length 113.

    **The seed's home came out differently from the sentence above.**
    That sentence read the split as JIT versus AOT; it is really folding
    versus not, and the two halves are one question, because a compiler
    that folds has to know the seed while it compiles. Edmond's call was
    to make it optional rather than settle it: the `hash-folding` cargo
    feature, off by default, selects a build-time seed from
    `LL_HASH_SEED` plus folding, and off selects a per-process seed drawn
    from the OS plus no folding. Reasoning, including why folding buys
    one load rather than the multiplies the RFC priced, and why neither
    arm defends against hash flooding: `dev/DECISIONS.md`, and the RFC
    section it amended.

    **"The build-time selection of the function" is not built, and
    deliberately.** There is one function. The second slot the RFC names
    is HighwayHash for long keys, gated on a length threshold that is a
    measurement with no number yet, and a selection mechanism with one
    occupant is a second untested path in the gate. The axis that does
    exist — `hash-folding` — is selected exactly the way the GC strategy
    is, so the pattern is in place when the second function arrives.

    **Owed by the compiler, not by this crate:** emitting the program's
    half of `hash::seed::STAMP` and calling `ll_hash_stamp_matches` at
    startup. Until that exists, a folding build has nothing checking that
    the program and the runtime hash alike.
15. ~~**Resolve `IS_ESCAPEE` against the COW count**~~ — done
    2026-08-04, Edmond's call: build the deep copy. The store barrier
    copies a request-arena COW value into the GC heap when a longer-lived
    slot takes it, so a COW entity never becomes an escapee and the two
    invariants stop describing the same field. The rule's `IS_ESCAPEE`
    arm is gone with it, and a publish now reports refusal, because the
    copy is an allocation no reserve can fund (`dev/DECISIONS.md`; rfc
    `2b94246`). What the task said before it was done:

    **The old entry.** — a design question,
    not an implementation one, and it needs Edmond. `values.md` asserts
    two invariants over the same four bytes: on a COW entity the count
    equals the number of holders, always and in every category; and while
    bit 11 is set, the field holds the arena escape hold-count. Both
    cannot hold for a COW arena entity, which is the class the separation
    rule's third line exists for. Today the contradiction is suppressed by
    an assert in `barrier::escape_gain` forbidding a COW entity to escape
    at all, so that arm of the rule is unreachable by construction.
    `arenas.md` names the intended route — a deep copy at the barrier for
    value-like data — and it is unbuilt. Three ways out: build the deep
    copy; declare the arm dead for COW and take it out of the rule; or
    give COW escapees a second field. The present state, a live test for
    an arm the barrier forbids, is the worst of the three.
16. ~~**Park buffer-arena frees during a collector epoch**~~ — done
    2026-08-04. The epoch test `ll_free` makes is made in
    `buffer_free_longlived_payload`'s buffer branch instead, since that
    free never reaches `ll_free`; the whole call parks, so the block
    cannot empty and be re-stamped; the parked record carries
    `(pointer, size)`, because `BufferArena::free` is size-carrying and a
    chunk holds no metadata. `ll_free_large` gained the
    `BLOCK_KIND_BUFFER` arm its silent default was swallowing, and
    `deferred_free`'s module doc now describes the chunk rider instead of
    claiming the door was shut in advance. Regression:
    `deferred_free::tests::a_buffer_chunk_parks_instead_of_being_written_into`,
    seen failing. Below is what the task said before it was done.

    The parking
    test lives in `stdapi::ll_free`, and a buffer-arena chunk never
    reaches it: `buffer_free_longlived_payload` branches on the block
    kind and calls `BufferArena::free` directly, which writes a
    `{ next, size }` link into the freed chunk and can return a whole
    block to the pool mid-epoch. Harmless for strings — a payload is
    bytes and the walker never reads it — and load-bearing before array
    storage arrives, which the walker will chase. Park in that branch,
    not in `ll_free`; park the whole call, not just the link write, so
    the block cannot empty and be re-stamped; widen the parked record to
    `(pointer, size)`, since the arena's free is size-carrying. Two
    adjacent repairs: `ll_free_large`'s default arm silently ignores
    `BLOCK_KIND_BUFFER`, and `deferred_free`'s module doc claims the door
    was closed in advance for arrays, which is false while this stands.

**Why strings and not something else** (decided 2026-08-03, before the
design work): `rfc/model/strings.md` is written, A2's entity-kind switch
is what was blocking it, and it is the first real subsystem rather than a
tail. It also unblocks arrays, which nothing else does.

**Arrays are unblocked as of 2026-08-06.** One array class with three
storage strategies is designed in `rfc/model/arrays.md`, and the hashtable
under strategy 3 is now designed in
`rfc/model/arrays-hashtable.md`. Two holes in `arrays.md` were found while
writing it and are answered there, so read both before coding: the depth
of the store barrier's COW copy, which `arrays.md` calls shallow and
`values.md` calls deep — they describe two different operations, and the
escape copy is the deep one; and strategy 1's claim never to transition,
which cannot hold, because separation copies the storage in its current
representation and a callee can then store a pointer into a proven
`array<int>`.

Deliberately not next, each with its reason:

- **A7, no zeroing by default** — the only Phase A item left, and small.
  It is a performance change, so it needs a measurement, and the
  expected effect is smaller than this box's 1.5–3% noise floor
  (`dev/BENCHMARKS.md`). Worth doing on a machine that can resolve it.
- **`domains`** (`rfc/model/gc/domains.md`) — rc-walk with more than one
  mutator. A proposal with holes it names itself: no per-domain block
  enumeration, the snapshot is global, and thread exit and adoption move
  a block between domains while a walk may be in flight over it. Design
  work, and large.
- **`rc-satb`** — settled 2026-08-03: designed, deliberately unbuilt,
  triggers named in `rfc/model/gc/satb.md`'s banner. Do not start it
  without one of those triggers, and not at all until the FFI-root hole
  recorded there is closed.
- **Re-derive the TLA+ battery under eager death** — `rc-walk-model.md`
  and the TLC configs still model the pre-amendment protocol (shared
  condemned byte, F5 deferral, message acquittals), so the battery
  currently proves a rule set that was retired 2026-07-27. Cheap, useful,
  and the protocol has been still for a week. Take it if the appetite is
  for correctness rather than features.
- **rc-walk escalation rung 4 and every trigger threshold** — blocked on
  measurement, which is blocked on real workloads, which are blocked on
  the vertical slice (Phase D). Do not design further on paper.

## Status snapshot (2026-07-24, HEAD `bad9bd6`)

Done, per RFC:

- **Memory manager**, end to end: arenas, the reset/promote fixpoint,
  immortal region, buffers / buffer-arena, block pool, the size-class
  heap (mimalloc model), the store-barrier log reserve, the store barrier + remembered set
  + release-at-reset list, stats layer 1. Audit closed, clean under Miri.
- **Object model**: `value` (16-byte Box), `intern`, `class` (inline
  vtable, itables, Cohen display), `object` (`ll_object_new`, three-phase
  `ll_object_die`, `ll_instanceof`).
- **GC**: `rc-trace` cycle collector (Bacon–Rajan).
- **Compact RcHeader flags** (`bad9bd6`) + the `EntityKind` enum +
  `is_object`; `VALUE_UNDEF` bit reserved.
- **A1 — new object body + slot kinds** (2026-07-25): machine-typed slots
  (`SlotKind` scalar / pointer / Box / bool), the three-run link-time
  layout with `layout_end`/`object_size` and parent tail-padding reuse,
  and `traced_runs` as two typed lists (`ptr_runs`/`box_runs`). The GC,
  teardown and promote consume them through one shared walker,
  `for_each_counted_child`. Factory now zero-fills the body.

The crate still runs the **old** teardown/barrier shape around that body:
`ll_object_new`/`ll_object_die` are generic runtime routines (A3 replaces
them), `promote::die` handles objects only, and only `EntityKind::Object`
is produced (A2). Teardown now dispatches through the descriptor's
`dispose` pointer (A3), with `ll_default_dispose` the generic stand-in
until the compiler generates specialized ones; the `factory` half of A3
still needs generation, so `ll_object_new` stays. The store barrier has
the slot-kind micro-ops (A4: `store_ptr`/`store_box`/`drop_ref`), though
the GC/promote test graphs still build through the `ref_store` Box
composition. The rest of the rewrite is the work below.

## Recommended order

A-first: rewrite the crate to the new layout (Phase A), then close the GC
tails (B), then build the new subsystems (C). The vertical slice (D) is
the true north — it validates the central bet and unblocks calibration —
but it depends on the still-unwritten execution-pipeline RFC and the
C++/LLVM front end, so it runs as a **parallel, externally-gated track**,
not in the A chain. Everything downstream (strings, arrays, rc-satb) sits
on Phase A's new body, barrier, and entity kinds, which is why A goes
first.

### Phase A — rewrite the object model to the new layout

Dependency order: **A1 → (A2, A4) → A3 → A5 → A6 → A7**.

- [x] **A1. New object body + slot kinds** (2026-07-25) — `RcHeader`(8) +
  class(8) + machine-typed slots; 16-byte Box only for `mixed`/untyped;
  `traced_runs` as two typed lists (pointer runs stride-8 skip-NULL, Box
  runs stride-16 skip-by-flag). Foundation for the rest. `rfc/model/classes.md`,
  `lowering.md`.
- [~] **A2. Entity kinds + bare-pointer teardown switch** — *the switch
  and the first non-object kind landed 2026-07-26*: `ll_entity_die`
  dispatches every bare-pointer death on the kind field (barrier
  `drop_ref`, gc un-guard, walk un-guard all go through it), and the
  **reference box** (kind 3, `RcHeader | Value`, `src/reference.rs`) is
  produced, traced, severed and collected — the `$a->r = &$a` ring test.
  Still open: string/array (their layouts are Phase C), Box (FFI),
  lazy (compiler), and the typed slot-reference variant (type system).
  WeakRef (kind 5) landed with rc-walk step 4 (`src/weak.rs`, below).
- [x] **A4. Store-barrier micro-ops** (2026-07-25) — `store_ptr` /
  `store_box` (publish) + `drop_ref` (release old, slot-kind-independent);
  `owner_cat` is a compiler parameter, not a load from owner flags, so a
  headerless static block can be a destination. `ref_store` kept as the
  `store_box`+`drop_ref` composition for existing callers; ABI
  `ll_store_ptr` / `ll_store_box` / `ll_drop`. Composition/inlining/
  specialization stays in lowering (the RFC's §1). `rfc/model/gc/strategies.md`.
- [~] **A3. Lifecycle family** — *dispose dispatch landed 2026-07-25*: the
  descriptor carries a `dispose` pointer, teardown dispatches through
  `obj->class->dispose(obj)` (child releases via A4's `drop_ref`), and
  `ll_default_dispose` is the generic stand-in a class carries until the
  compiler generates a specialized one; a test can install its own. Still
  open: **`factory` in the descriptor** (a `factory(ctx, category)` with
  no class param needs per-class generation — the generic path stays
  `ll_object_new(ctx, class, category)`), and **`clone` / `deep_clone` /
  `thread_clone` / `thread_move`** (multi-threading-future, "reserved" in
  the RFC). "Only the GC reads `traced_runs` as data" holds once generated
  disposes replace the stand-in. `rfc/runtime/object-lifecycle.md`.
- [x] **A5. `VALUE_UNDEF` semantics + `WRITING` lock bit** — *Box half
  landed 2026-07-27*: `VALUE_WRITING` pinned (bit 2, mechanism waits for
  rc-satb), `Value::undef()`/`is_undef()`, the descriptor's `undef_runs`
  (defaultless Boxes regrouped to the box run's tail) stamped by the
  factory after the zero-fill, `unset` as the undef-store + `drop_ref`
  composition — all pinned by tests (undef never traced, any store
  clears). *Raw half landed 2026-07-27 (commit 2)*: the byte block at
  the layout tail carries the init bitmap — one bit per defaultless
  `?T`-pointer/scalar/bool slot (`PropSlot::init_bit`, absolute bit
  position, declaration-ordered; a subclass appends its own block, so
  parent bits never move), `Object::init_bit_test/set/clear` as the
  beside-the-access ops generated code emits, and the factory's
  zero-fill starting every bit clear for free. Hole-filling the byte
  block into padding stays deferred (A7 / rfc backlog).
  `rfc/model/values.md`, `gc/satb.md`.
- [x] **A6. Static-block teardown at thread exit** (2026-08-03) —
  `static_block.rs`: a per-thread registry appended in first-touch
  order, drained in reverse, each reference slot severed and dropped
  through the barrier's `drop` so the three cases (arena escapee, heap
  reference, immortal) come out right without a branch. Registration is
  `ll_static_block_register(block, layout)`; a compiler-emitted
  straight-line teardown does **not** replace the registry, because
  which blocks a thread touched and in what order is a runtime fact —
  statics initialize lazily per thread, exactly as C++ function-local
  statics do. `PLAN.md` recorded this as closing audit H3; `AUDIT.md` is
  untracked and was not read here, so that is what the plan says rather
  than a claim about the audit entry itself. Forced a second change:
  thread exit runs user code for the first time, so every per-thread
  structure it can reach lost its drop glue and `ll_thread_exit` now
  fixes the disposal order (`dev/DECISIONS.md`). `rfc/model/classes.md`
  "Teardown at thread exit".
- [ ] **A7. No zeroing by default** — the factory decides which slots need
  a defined initial state (`rfc/BACKLOG.md` deferred-optimizations).

### Phase B — GC completeness (tails deferred in the old plan)

- [x] **rc-walk build step 1** (2026-07-26) — entity blocks segregated
  from raw C-ABI allocations (`BLOCK_KIND_ENTITY`, second `Heap` per
  thread), region registry with stable indices, free-list link moved to
  slot bytes 8–15, slot headers zeroed at block commissioning, factory
  publishes the header last as one 8-byte store, kind-dispatched tracer
  + heap census (`src/walk.rs`). Design machine-checked in the rfc repo
  (`model/gc/rc-walk.md` + proof docs).
- [x] **rc-walk build step 2** (2026-07-26) — `walk::collect_cycles`, the
  synchronous whole-heap collection: Phase 1 walk over entity blocks,
  computed roots (`RC − IN > 0`), BFS mark, weakly-connected garbage
  components, and the full Phase 4 drain inline — exact test, guard,
  destructors once, guard-discounted re-verify (F1), sever
  (`object::sever_counted_children`) + un-guard through ordinary
  teardown. A whole-heap leak detector needing no candidate buffer, and
  the exact test's correctness harness.
- [~] **rc-walk build step 3** — the concurrent collector, in five
  commits. *Commit 1 landed 2026-07-26*: the `rc-walk` cargo feature
  (build-time strategy selection — the collectors share header bits,
  `dev/DECISIONS.md`), epoch + condemned bytes at header bytes 6–7,
  the retain/release condemned mask, relaxed-atomic header accesses
  (asm-verified: no RMW, no call tail in release), and the
  condemned-never-dies-ordinarily rule (F5). *Commit 2 landed
  2026-07-26*: the deferred-free queue (`memory/deferred_free.rs`) —
  the GC activity bit in `ll_free`, all four freeable kinds park on a
  thread-local intrusive list through their own bytes 8–15, flush on
  the owning thread between epochs. *Commit 3 landed 2026-07-26*: the
  epoch protocol's mutator side (`src/epoch.rs`) — soft-handshake ack,
  verdict queue (confirm + acquit), non-reentrant checkpoint riding
  `entity_alloc` + `ll_gc_maybe_collect`; per-component drains in
  `walk.rs` (`drain_confirmed` with the F5 dead-member path,
  `acquit_condemned` with the duty ordering), F8 reentrancy pinned by
  test. *Commit 4 landed 2026-07-26*: the collector side
  (`src/collector.rs`) — the steppable epoch state machine, Phases 1–3
  end to end (three-way classification by epoch byte, row-lookup edge
  validation, shared Phase 2 math, condemn + handshake +
  snapshot-compare re-check, verdict posting), the threaded `run_epoch`
  driver, post-epoch flush at the checkpoint; F3 maturity latency and
  the Phase 3 filter pinned by stepped tests. Trigger stays an explicit
  call (thresholds are unmeasured — rc-walk.md open question 1).
  *Commit 5 landed 2026-07-26*: the forced-timeline DC tests against
  the sound gates — DC1's machine-found trace (walk split into
  count/field passes for the interleave; caught by the Phase 3 count
  re-read AND independently by the exact test), DC0's `0 = 0` confirm
  (exactly-once probed through the free list), DC3's premise shown
  unreachable. Kills of broken variants stay TLC's (a runtime
  use-after-free has no deterministic observable) — agreed with
  Edmond, rfc danger-cases note updated. *Commit 6 landed 2026-07-27*:
  the relaxed-atomic sweep (field stores, header flags, block kinds),
  the condemned-aware dispose un-guard (a real F5 bypass found and
  closed — DECISIONS), the byte-preserving deferred-death store, the
  cursor-free snapshot (an atomic bump measured +14% larson —
  rejected, BENCHMARKS), a quadratic re-check fixed, and the
  free-running stress test (Miri-ignored; stepped tests carry Miri).
  **Step 3 is complete.** Next rungs stay per rc-walk.md build order:
  the escalation ladder if measurement shows starvation (5); trigger
  thresholds remain measurements.
- [x] **rc-walk eager death** (2026-07-27, Edmond's redesign; rfc
  `c2f91b1`, `model/gc/rc-walk.md`) — every refcount death tears down
  at the natural point, only the memory parks. Deleted: the condemned
  byte (bits 24–31 freed), the F5 deferral + marker, `acquit_condemned`
  and the acquittal message, `Epoch::drop`'s owed acquittals.
  Condemnation is collector-private; `drain_confirmed` opens with the
  corpse rule (any `rc 0` member drops the message whole). Two
  pre-existing BLOCKERs from the adversarial review fixed in the same
  change: the death-branch checkpoint acks only (pickup rides the
  outermost dispose's exit — the commit-to-dispose window has a live
  weak cell), and parking is out-of-band (the in-slot park link
  overwrote the class word under the walker). Both pinned by
  verified-failing regressions. The rfc's TLA+ battery models the
  pre-amendment protocol until re-derived (banner notes).
- [x] **rc-walk batched-checkpoint split** (2026-07-28; rfc `3faf110`,
  "Batched releases" amendment) — the run's checkpoint splits:
  `ll_gc_checkpoint_ack` (new ABI) before the run, full
  `ll_gc_checkpoint` after it; `ll_release_vector` same; the pickup
  gate additionally refuses messages while `walk::collect_cycles`
  runs (drain-class). Four regressions, each verified failing:
  the ack-only front, the ack-before-first-death position, the
  phase-lock shape on the vector form, the walk-active gate. Cost
  within noise (`dev/BENCHMARKS.md`). The
  forced-verdict machinery and the pressure ladder stay design-only
  (build order 5, measurement-gated).
- [x] **rc-walk build step 4 — weak references** (2026-07-27,
  `src/weak.rs`; design `rfc/model/weak-references.md`). The canonical
  `WeakReference` entity (kind 5, 16 bytes, always GC-heap) doubles as
  the weak cell; a per-thread weak table (target address → cell) lets
  the dying target null it. Notification wired at all death sites:
  first act of dispose phase 2 (before child releases — the ordering a
  cascading child `__destruct` needs), pre-destructor passes in
  `walk::collect_cycles` / `drain_confirmed` / `gc::collect_cycles`
  (the PEP-442 obligation), and the arena reset weak walk (after the
  destructor fixpoint; promoted survivors keep their cells). ABI:
  `ll_weakref_create` / `ll_weakref_get`. `WeakMap` waits for maps;
  the table row widens to a subscriber list then.
- ~~Immix-shaped `GcHeap` allocator~~ — **dropped entirely 2026-07-25**
  (confirmed 2026-07-27): no line recycling, no reuse of retained-block
  holes. Segregated entity blocks solved what Immix was drafted for;
  retained blocks stay out of circulation while their survivors live
  (`arena-reset.md`, Retention). Small future mechanism: return a
  fully-emptied retained block to the pool. Sparse-block **evacuation**
  at reset remains a real open item, gated on the escapee-reference
  fixup (`arena-reset.md`, "Evacuation is now-or-never").
- [x] **Retained-block walk** (2026-08-03) — the reset keeps its survivor
  list as each retained block's object index (`memory/retained.rs`), and
  both enumerators go through it: `heap::for_each_entity_slot` for the
  synchronous walk, `heap::snapshot_entity_blocks` for the epoch, with
  the census resolving an address inside a retained block by searching
  the index after the same single binary search that serves entity
  blocks. Closes rc-walk.md's "cycles among promoted survivors" limit —
  a ring living entirely among promoted survivors used to be
  uncollectable forever. Design and the three settled obligations:
  `rfc/model/gc/retained-block-walk.md`, `dev/DECISIONS.md` 2026-08-03.
  Left open: `retained::release` has no caller until a fully emptied
  retained block can return to the pool.
- [x] Run `__destruct` of cyclically-dead objects (2026-07-25) — Zend-style
  discipline (`run_cyclic_destructors`): restore the white set's real
  counts, guard, run each `__destruct` once through the ordinary teardown,
  then re-collect so a resurrected subgraph survives. No new mechanism (no
  retain hook, no GC-window flag); reuses `drop_ref`/`ll_object_die`/
  `forget_candidate`. Tested for the plain cycle, an `unset`-in-destructor
  (double-free), and resurrection into a live holder (child survival).
- [ ] `rc-satb` as a second build-time GC strategy (needs the `WRITING`
  bit from A5). `rfc/model/gc/satb.md`.

### Phase C — new subsystems (not started; each its own RFC + code)

- [ ] **Strings — the chosen next task** (decided 2026-08-03, see "Next"
  at the top). String-as-class and the interpolated-template class
  (`rfc/model/strings.md`). A2's entity-kind switch is what unblocked
  it. Where to start: an interned name is already a valid immortal
  string entity that the future machinery is meant to read as-is
  (`dev/ARCHITECTURE.md`, invariant 13), so the layout is half-pinned
  before a line is written.
- [ ] Arrays — one `array` class, three storage strategies; the hashtable
  design (bucket layout, collision strategy) is still a future document
  (`rfc/model/arrays.md`).
- [ ] Further out, listed in `rfc/BACKLOG.md`: exceptions runtime
  (table-driven unwind + error-return channel, `runtime/exceptions.md`),
  actors (`runtime/actors.md`), closures, enums, generators/fibers,
  resources, generics, stdlib, I/O.

### Phase D — vertical slice (parallel track, externally gated)

- [ ] Minimal hello-world through the whole stack (PHP → IR → executable)
  on the simplest memory setup. Validates the central bet — that the
  compiler can prove escape / monomorphism / ARC-pairing on real PHP —
  and unblocks every calibration item. Requires the minimal
  execution-pipeline decisions (`rfc/BACKLOG.md`, "the big one") and the
  C++/LLVM front end; both live outside this crate.

## Residual / carried-over items

Memory manager, still open:

- [x] **Grow a long-lived buffer in place off the bump top** — done
  2026-08-05, `3c25db8`. `buffer_ensure_longlived` moves the bump when
  the payload is still the last chunk taken from it, ahead of hole reuse
  in every pressure mode. **The clock could not resolve it** and the run
  was void on its own terms — two runs of the same arm disagreed by 4.6%
  — so the evidence is a count instead: an append loop moves its payload
  once now and nine times before
  (`string::tests::an_append_loop_moves_its_payload_once`,
  `dev/BENCHMARKS.md` 2026-08-05). Accepted on three grounds needing no
  measurement: less work, no payload free on the growth path and so
  nothing to park during an epoch, and no chain of holes that never
  coalesce. `benches/strings.rs` arrived with it and is the harness the
  next string measurement uses.
- [x] **Reuse an adopted block, not just reclaim it** — done 2026-08-05.
  The cursor moved into the block header, so an adopted block's tail is
  resumable: rotation takes an adopted tail, then any owned tail, then
  the pool, and `critical` searches the free lists of the whole owned
  chain under one budget. `resume_owned` is the step that keeps a block
  adopted for a request its tail cannot serve from being looked at once
  and never again. The order is the reverse of `heap.rs`, and why is in
  `dev/DECISIONS.md`, 2026-08-05.
- [ ] Buffer *K* and memory-pressure mode thresholds — **blocked on D**:
  need real workloads. Do not design further on paper (`buffers.md`).
- [ ] Cross-thread free of long-lived buffers — deferred until a consumer
  needs it (`heap.rs` remote-free is the template).
- [ ] Per-block dense/sparse reset threshold calibration — **blocked on
  D** (`arena-reset.md`).

Object model, deferred by design:

- [ ] General interception Proxy — transparent method interception on an
  existing target without touching its class; prerequisite for
  proxy-mediated movability. Needs a mechanism discussion.
- [ ] Binary-level class interceptors (vtable-slot patching) — check
  whether this is the same mechanism as the deferred CHA-style optimistic
  devirtualization (`classes.md` Deferred).
- [ ] Allocation telemetry layer 2 / debug mode — full design in
  `dev/design/debug-modes.md`; build order is its section 9. Designed, not
  scheduled.

## Cross-cutting (every phase)

- Correctness tests per the project style (`test_guard`, scenario-per-test)
  and criterion benchmarks per `dev/BENCHMARKS.md` — follow the protocol,
  do not improvise. Benches do not cross the C ABI; ABI-entry work is shown
  by IR/asm.
- `dev/ARCHITECTURE.md` — the crate's knowledge map, still absent and
  agreed to be written; the obvious documentation job over ~9k lines.
