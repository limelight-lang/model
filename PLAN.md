# Plan

Implementation plan for `ll-model`, re-sorted 2026-07-24 against the RFC
after the 2026-07-22 object-layout redesign landed in the `rfc` repo.

Design lives in `rfc` and is authoritative — read before coding, do not
re-derive: `model/classes.md`, `model/values.md`, `model/lowering.md`,
`model/gc/strategies.md`, `model/gc/satb.md`, `model/memory/ffi.md`,
`runtime/object-lifecycle.md`.

Updated: 2026-08-08 · Active: S6

**S4 and S5 are closed and deleted by rule 23.1.3.** S4 was the teardown
drain, the pinned block's release and the plan hygiene. S5 was the opt-in
event journal, built end to end: the ring, the registry and the window,
then the record sites of §9.5 behind the `debug-journal` feature, then
the acceptance hunt of 2026-08-06 run through it with no ring written by
hand. What survives them is in `dev/DECISIONS.md` (seven entries of
2026-08-08), `dev/POSTMORTEM.md`, `dev/BENCHMARKS.md` (the journal's IR
count, and the timing arm it still owes), `dev/INDEX.md` and the code's
own doc blocks. `dev/design/debug-modes.md` §9 is the journal's design
and stays.

**The whole-suite Miri run is done, in both configurations**, and it
covers S4 and S5.1: rc-walk 384 passed, 0 failed, 7 ignored in 948 s;
rc-trace 373 passed, 0 failed, 4 ignored in 724 s (2026-08-08, HEAD
`6ab0bcf`). It is not part of a stage's criterion — `dev/WORKFLOW.md`
keeps Miri beside the commit gate rather than inside it.

**It is a quarter-hour job again, and the figure that said otherwise was
one test.** The 28 minutes for 86 tests measured earlier that day was
`a_deep_array_tears_down_without_the_machine_stack` dominating the run;
its repair is in `dev/POSTMORTEM.md` under the same date, and the whole
suite has cost 12 and 16 minutes since. So the question of whether such a
run belongs at every stage or only at a release is cheap to answer either
way, and it is still Edmond's: half an hour for both configurations buys
the one class of defect no other tool in this crate can see.

The stages below are in **work order, which is the file's order rather
than the numbers' one**: a number is never reissued once given, so a stage
added later sits where it is to be done. S9 is such a stage.

The five that were there first were ordered by the Sage on 2026-08-08 and
approved by Edmond the same day; the two at the head of that order are
done and gone, and the argument for the rest is unchanged. S6 corrects a
boundary every later customer would copy, `Map` being the element layer's second customer by the ruling
of 2026-08-07; S7 gives the strategy tag the second occupant it was
deferred for, which is also what makes two of `Map`'s design questions
answerable; S8 writes that design, since building it first would decide
by accident what Edmond has reserved. S9 was added on 2026-08-08 at
Edmond's request and placed between S6 and S7 for the reason its own
section gives. The prose sections after the stages
are the reasoning behind them and the backlog they were drawn from.

## S6 — The stage-end review's five demands

Goal: the cut the Code Reviewer asked for on 2026-08-08 — the element
layer's work leaves the table, the publication idiom exists once, and the
four contracts read without following a private link.

Done when: all five demands are closed as the review stated them, the
array module has its rows in the knowledge map, and the gate is green in
both configurations with behaviour unchanged.

- [x] S6.1 `Table::make_ref`'s boxing, category crossing and giveback
      move beside `element::set`
      done: `table.rs` allocates no reference box and calls no store
        barrier; `Table` hands the old element back and the composition
        is one layer up
      tier: T2 · role: —
      handoff: `element::box_element` is the composition, beside
        `store_into` and shaped like it — it publishes through
        `store_category_barrier` and takes the displaced element from
        `Table::insert`, so no method of `Table` had to be added for the
        giveback. `Table::make_ref`, `element_for_box` and
        `destroy_empty_box` are gone from `table.rs` along with the
        `Owned::make_ref` test wrapper, and the three tests that proved
        a box survives growth and compaction moved to `element.rs` with
        the code they exercise. No behaviour changed; the gate is green
        in all six arms.
- [ ] S6.2 `barrier::publish_child` replaces the idiom's four copies
      done: one body, and the four sites call it
      tier: T1 · role: —
- [ ] S6.3 `destroy_private_copy` and `destroy_empty_box` become one body
      done: one function, with the divergence in the assertion stated
        where it is decided rather than split across two sites
      tier: T1 · role: —
- [ ] S6.4 `set`, `append`, `unset` and `make_ref` state their ordering
      guarantee in public text
      done: `cargo doc` reports no private-link warning on the four
      tier: T0 · role: —
- [ ] S6.5 `a_reference_to_an_absent_key_creates_it_as_null` tests the
      contract instead of the representation
      done: the test asserts that reading the key yields null and a write
        through the reference is visible there
      tier: T1 · role: —
- [ ] S6.6 `src/array/` gets its rows in `dev/ARCHITECTURE.md`
      done: `table`, `entry`, `element` and `entity` each have a row —
        what it is responsible for, what it knows, what it must not know,
        what it depends on — and every upward edge the module has is
        listed there, the `array::entity` → `gc` one included
      tier: T2 · role: Critic
      Last of the stage rather than first, and Edmond agreed the step on
        2026-08-08: the map records the boundary S6.1 settles, so writing
        it before the cut would describe a layout about to move. The
        module has had no row since it was born on 2026-08-06, which is
        why the next argument about where code belongs has nothing to
        decide by.

## S9 — The crate reads without its own noise

Edmond's, 2026-08-08, both halves of it: there are too many comments to
read past, and they break rule 19 of the working rules — a comment adds a
level the code does not have, above it or below it, and one at the same
level is a retelling that should not have been written. And the tests
have outgrown the bodies they live in.

**Placed here rather than earlier or later, and the reason is the two
neighbours.** S6 moves bodies between modules and merges two of them, so
a comment pass before it would be redone; S7 writes new code, and new code
copies the density of what sits beside it, so a pass after it is a pass
over more. Between the two, each file is touched once and the next module
written inherits the leaner shape.

Goal: every comment that remains carries something the code does not, and
a test can be found by what it is about.

Done when: the comment count per file is recorded before and after, no
public declaration lost its contract, every test still runs in both
configurations, and `dev/INDEX.md` states the new arrangement.

- [ ] S9.1 The comment pass over `src/memory/`
      done: comment lines counted before and after and the drop recorded
        in this step; every comment left is one of rule 19's six kinds;
        no `///` on a public declaration removed, since 19.2 makes those
        obligatory; behaviour untouched, so the gate is green with no
        test edited
      tier: T2 · role: Code Reviewer
- [ ] S9.2 The comment pass over the entity modules
      done: the same, over `object`, `string`, `array/`, `template`,
        `reference`, `weak`, `intern`, `value`, `class` and `hash/`
      tier: T2 · role: Code Reviewer
- [ ] S9.3 The comment pass over the runtime spine
      done: the same, over `refcount`, `gc`, `walk`, `collector`,
        `epoch`, `promote`, `static_block` and `journal/` — the last of
        which is where this session's own density came from
      tier: T2 · role: Code Reviewer
- [ ] S9.4 Tests leave the module bodies and arrive grouped
      done: every `#[cfg(test)] mod tests` in `src/` is its own file,
        each grouped into named submodules that say what is being pinned
        rather than which function is called; the suite's count is
        unchanged in both configurations; `dev/INDEX.md`'s line "inline
        `#[cfg(test)]` per module, no `tests/` directory" is rewritten,
        that being a convention this step overturns
      tier: T2 · role: —
      One pass per file, not two: the tests move and are grouped in the
        same edit, because touching every file in `src/` twice costs
        twice and diffs worse.


## S7 — Storage strategy 2, the tag, and the 2 → 3 migration

Goal: the mixed vector exists, the strategy tag has the second occupant
it was deferred for, and insertion order survives the migration by
construction. Read `rfc/model/arrays.md` first; that design is
authoritative.

Done when: a fresh array is strategy 2 and migrates to strategy 3 under a
key it cannot hold, both configurations green, Miri silent.

- [ ] S7.1 The mixed vector as storage strategy 2
      done: a fresh integer-keyed array is strategy 2, traced, severed
        and torn down through S4.1's drain, in both configurations
      tier: T2 · role: Critic
- [ ] S7.2 Factories stamp the tag and the element write dispatches on it
      done: a strategy-2 write of a string key migrates through the tag,
        not through a direct table call
      tier: T2 · role: —
- [ ] S7.3 The 2 → 3 migration
      done: a test pins insertion order across the migration, and the
        flood state carries over with it
      tier: T2 · role: —
- [ ] S7.4 The RFC corrections this stage owes
      done: `arrays.md`'s "strategy 1 never transitions" and
        `memory/buffers.md`'s grouping of immortal with long-lived are
        both amended in `rfc`
      tier: T1 · role: —

The 1 → 2 transition stays out of the stage: strategy 1 has no producer
in the crate, and an arm with no producer is what this crate has refused
twice. Confirm that against `arrays.md` when the stage opens.

## S8 — The Map design, written before it is built

Goal: `Map` and `MapMixed` designed in `rfc`, with every question Edmond
reserved answered before a line of code.

Done when: the design document exists and its open section is empty or
explicitly deferred.

- [ ] S8.1 The reserved questions put to Edmond, one at a time
      done: every question in the `Map` section below is answered and
        recorded in `dev/DECISIONS.md`
      tier: T2 · role: —
- [ ] S8.2 The design document in `rfc/model`
      done: it covers the key-kind word beside the entry, the object key
        as a counted child in trace and sever, and `MapMixed`'s content
        hash on a work list rather than the machine stack
      tier: T2 · role: Critic

## Closed: the census flake was two tests killing an entity at refcount 1

`walk::tests::census_counts_objects_and_their_edges` failed at roughly 5 in 30
under load. Under the same load it now fails 0 in 60 in rc-trace and 0 in 30 in
rc-walk, against 3 in 30, 7 in 40, 6 in 40 and 9 in 40 measured on this box
before the fix. The reproducer stays worth keeping: build the test binary with
`--no-run`, pin it to two cores with `taskset -c 0,1` at `--test-threads 4`, and
run two spinners on the same cores.

**Nothing left the walk.** The two censuses yield the *same* address set; the
count fails to grow because the first census already counted the two slots the
test's own objects then land on. At the first census those slots read
`0x0000_1400_0000_0001` — an inline string at refcount 1 — and at the second
they read the fresh objects. No string died inside the window, which the
earlier hunt measured correctly and read the wrong way round: the strings had
been freed *before* the window, with their headers never driven to zero.

**What does that is a test killing an entity with `ll_entity_die` while its
refcount is still 1.** `walk::tests::an_array_is_traced_through_its_elements_
and_its_string_keys` did it three times and
`array::entity::tests::dying_through_the_kind_switch_releases_the_children_and_
the_storage` twice — the second half of item 14, which named the shape and was
never connected to this. The slot then reaches the free list carrying a
live-looking header, and that word is the occupancy test both process-global
enumerators apply. For a string it is an over-count. For an object it is worse:
the free-list link is written at bytes 8-15, where the class pointer was, so a
walk that believes such a slot follows a free-list link as a `*const Class`.

**The guard is what keeps it closed.** `stdapi::ll_free` asserts in test builds
that an entity slot arrives with a refcount-0 header, so killing at 1 fails in
the test that does it rather than in an unrelated test on another thread half an
hour later. Kept with it: the census test's drift report, which on a mismatch
names the addresses that came and went and the block state behind each
(`heap::describe_slot`). Lesson in `dev/POSTMORTEM.md`.

Both earlier hypotheses stay retired and are now explained rather than merely
unobserved: retained blocks never returned to the pool, and ThreadSanitizer was
silent because there was no race to find.

Two certain defects were found beside it and both are closed (`576ffc1`).
`retained.rs`'s tests registered fabricated addresses in the **process-global**
retained registry, which a concurrent walk dereferences; they now register
leaked zeroed cells, and the third test is the regression — it was seen
segfaulting on the fabricated version. The `ll_thread_exit` comment had the
registration order backwards: `replenish` runs before `EXIT_GUARD` is
registered, so on glibc the guard runs *first* of the three, and the safety
rests on the no-drop-glue rule plus `try_with` rather than on the order.

**One thing the new regression is worth knowing about:** it walks the process
heap the way the census test does, so if it ever fails intermittently, that is
this flake and not a new one.

## Urgent, ahead of everything: the GC's walk exists in copies

Edmond's ruling, 2026-08-06, and it outranks every item below including the COW
doors. **For the GC an object, an array and a string are almost the same
thing**, so an algorithm written once for an object has to handle every other
kind. The generalization he asks for takes `*mut RcHeader` at the entrance.

The kind *dispatch* is not the debt — `walk::trace_entity` and
`ll_entity_die` already take a bare header. The debt is the **slot walk**:
where a kind keeps its counted children was written out again in every
operation that needed it, so one layout was known in five places.

**Steps 1 and 2 have landed.** Step 1 (`348d24b`) made the object layout's
stride one place, `object::for_each_counted_cell`, keyed on `(base, class)` so
that a headerless static block can be walked too, and generic over a
`CellReader` with two zero-sized implementations: plain reads for a quiescent
walk, relaxed atomic reads for the collector, which needs them because a plain
read racing a mutator store is undefined behaviour rather than the torn read
Phases 3 and 4 repair. Step 2 (`8bd73d3`) made `trace_cells` the single tracing
dispatch and deleted `collector::trace_mature` along with the last copy of the
Box layout arithmetic; the pin is a differential test, which asserts that on a
quiescent heap the two readers yield the same child set for every walked
entity. `f5234fd` then repaired what Miri caught inside the reader: a pointer
cell is read as a pointer and a Box payload as an integer, because
`Value::entity` stores the address as a `u64` and those bytes carry no
provenance to recover.

**Step 3, the sever, has landed too.** `walk::sever_cells` is the single
sever dispatch, and it goes *through* `trace_cells` rather than striding
again: one layout, one stride, two operations over it. What made that
possible is one field — a cell now carries its shape, pointer or Box, which
is the only fact about it the address does not give and the only reason the
sever needed a stride of its own. `object::sever_counted_slots` is four
lines over the shared walker and keeps its base-and-descriptor signature
for the static-block pass, which has no header to read a class from.
`6afd220` had moved the reference sever onto the store barrier ahead of
this, because a plain store there raced the collector's relaxed loads.

Left standing on purpose: `Table::sever_entries`, reached through the
Array arm. An array's cells are not `trace_cells`' (below), and emptying
one is not a null — a cleared entry is a hole, and an integer-keyed entry
has no key cell. The table owns both facts, so the dispatch delegates
rather than absorbing it.

**Arrays stay outside `trace_cells` deliberately**, and the reason is in
`8bd73d3` rather than a step left half-done: an array's cells live in storage
that moves on growth, so a relaxed reader can observe a `used` that outran the
`storage` it read and stride past the end of a stale chunk. Parked frees keep
that chunk readable and bound nothing. The bound is item 12's, and until it
exists `trace_entity` keeps its own array arm and says so.

This is debt rather than taste, and the repository has already paid twice. The
interpolated template moved its value count from the class to the instance and
**three** walkers had to learn it, the third found late by review. The array was
wired into the child walkers and not into the sever, and a confirmed-garbage
ring of two arrays was un-freeable in both configurations until `144b318`.

The nuances that have to survive step 3 are known and are not objections to it:
the collector's reader cannot use the ordinary accessors, the store barrier
stays the only writer of a published slot, clearing an array cell means a hole
rather than a null and an integer-keyed entry has no key cell at all, and the
child walkers are `#[inline]` and generic precisely so no caller pays an
indirect call per child (`rfc/model/classes.md`, "Why tracing stays data").

**Items 11 and 12 wait on step 3**, and the reasoning is that both *add arms*
to doors this refactor may delete: written now they are written twice, and item
12's arm needs the storage bound the array is excluded for.

This supersedes item 13, whose earlier proposal — exhaustive matches, `const fn`
predicates and a `specimen` registry — the critic showed would not have caught
any of the misses, every door having had a legal-looking arm already.

## Then: the COW doors, and the publish-first repair that must ride with them

Three live defects found by the second critic pass of 2026-08-06 are
closed (`2e55036`, `144b318`, `f56a035`); the section **"What the second
critic pass of 2026-08-06 found"** below carries the rest, items 15–20,
and its two rulings. Read it before the older list: it renames the
problem. **A kind has seven doors, not six** — `walk::sever_component` was
in nobody's list and was the worst of them — and the count is a symptom
rather than the thing to fix, since every door the array missed had a
legal-looking arm for it already.

What to take next, in this order and for these reasons.

1. ~~**Item 11, the two COW doors**~~ — closed. Both arms are
   `array::entity::separate`, one body with the destination category
   supplying the depth, exactly as the ruling below settled: with an arena
   destination the copy arm cannot fire and the children are shared, with a
   longer-lived destination over an arena source every arena COW child is
   copied in turn. Each child is published through
   `barrier::store_category_barrier`, now `pub(crate)` — that is also the
   crate-internal publish-without-a-slot-write the repair in item 12 needs,
   so it is built. `separation_category` moved to `refcount.rs` beside
   `cow_separation_needed`, being the COW rule rather than a string rule,
   and item 14's escape-ledger asymmetry went with it. The work list the
   ruling calls for is built: a nested arena array is copied empty,
   published, and its filling pushed onto `WorkList`, which lives in a
   buffer-arena chunk and refuses rather than aborting when it cannot
   grow. Termination is asserted in debug builds instead of paid for.
   The other half — teardown of what the copy produces, recursive at one
   frame set per level — was closed as S4.1 on 2026-08-08 by giving
   `array_die` the same drain. An object chain is still the machine
   stack's.
2. ~~**Item 12's concurrent-walk arm together with the publish-first
   repair of the key slot.**~~ — closed. The arm is in `trace_cells`, so
   the array is inside the one tracing dispatch like every other kind, and
   `array::entity::for_each_counted_child` is an adapter over it rather
   than a second stride. The bound is a version counter bracketing growth
   and compaction plus a validated read of the three words
   (`Table::coherent_entries`); failing it skips the array for one epoch,
   which leaks rather than frees early. The publish-first contract is
   stated on `Table::insert` and worked in `array::entity::separate`.
   The test call sites this entry once owed are already publish-first:
   every `table.insert` of an entity value in the crate takes its count
   before the entry exists, checked site by site on 2026-08-08 (S4.3).
3. ~~**Item 20, the candidate gate**~~ — closed. The gate masks
   `0b101` of the kind field, so `{Object 000, Array 010}` pass in the
   one compare the object-only test already was
   (`refcount::CANDIDATE_KIND_MASK`). Forgetting a candidate moved off
   `ll_default_dispose`, which is class code an array never runs, onto
   the teardown doors: `ll_entity_die` before the kind switch, and
   `ll_object_die` after `dispose` returns, because a `__destruct` can
   buffer the object afresh. What holds the window between those two
   points is not their ordering but Edmond's ruling of the same day: a
   fire point reached from inside a teardown collects nothing
   (`gc::TEARDOWN_DEPTH`, `dev/DECISIONS.md`). `ll_free` asserts in test
   builds that an entity arrives unbuffered, beside the refcount-0
   assertion. Three regression tests in `gc.rs`, each seen failing: a
   ring of two arrays with no object in it, an array forgetting its
   candidacy as it dies, and a collection fired from a destructor
   reclaiming nothing. Miri closed the stage's verification a session
   later, 2026-08-07, both logs read whole: rc-walk 320 passed, 0
   failed, 6 ignored; rc-trace — the configuration where the gate
   predicate is live — 308 passed, 0 failed, 3 ignored.

   **The two leaks this entry named are closed with it.** A ring taking
   its last external release on a ReferenceBox (`$a['x'] = &$a`) or on a
   Lazy proxy produced no candidate while the gate was a mask, and no
   mask could admit `Reference 011` without admitting `String 001`. The
   gate became a **set of kinds** the same day —
   `refcount::CANDIDATE_KINDS` is `{Object, Array, Reference, Lazy}`,
   a kind belonging to it exactly when it holds counted slots a cycle can
   close through (`dev/DECISIONS.md`, 2026-08-07) — so nothing here waits
   on the renumbering.

Both questions that were open on 2026-08-06 are answered, neither by
Edmond. The recursion bound of the deep escape copy is an explicit work
list rather than a depth limit; the ring with no object in it is a real
obligation and the gate diverges from the RFC over it. The reasoning for
each is below, with what is still Edmond's to accept.

## Then: finish the hashtable — two pieces are left

Read this first in a fresh session. The design is in
`rfc/model/arrays-hashtable.md` (rfc `ca0d197`, `eb68707`, `9e5ae3d`,
`2a9ce2e`) and **the crate now has `src/array/`** with 41 tests, green in
both configurations and silent under Miri: the entry layout, the table
core, string keys, the flood backstop, element references, the entity
wrapper and the storage's home in the buffer arena (`model` `514c526`,
`80494ff`, `eaf6a03`, `42c2a6f`, `3316343`, `3025757`).

### What is left, in order

**First a prerequisite nobody had written down, done 2026-08-06** (task 10
in the session tool). `ll_array_new` stamps `EntityKind::Array` and the COW
flag, so the crate produces the entity — but no dispatch site handled it.
`ll_entity_die` had no arm: a `debug_assert` in debug and a silent no-op in
release, so children kept the references the array owed them and the
storage was never returned. `walk::trace_entity` skipped it while its doc
still called Array a kind "the crate does not yet produce". Both now have
their arm, sharing one walk — `array::entity::for_each_counted_child`,
which yields elements and string keys alike, a table holding a reference
to each string it keys on. Promotion could not have been built before
this: it takes its survivors from the trace, and a refused carry falls
back onto the death path.

That work also found a leak in `release_children`: it discarded what
`ll_release` returns, and that answer is an obligation — whoever gets
`true` owes the teardown. It goes through the barrier's `drop_ref` now,
which also settles the escape hold-count and leaves a heap child inside an
arena array to the release-at-reset log. Seen failing through a nested
array, whose storage is a buffer chunk that can be watched for.

**Still owed on that front:** `escape_copy` has no Array arm either, and
there it is an `unreachable!()` rather than a no-op. The design is written
(`rfc/model/arrays-hashtable.md`, "The COW copy has two depths"): the
escape copy republishes each element through the barrier with the
destination's category, so an arena COW child is copied recursively while
a heap or immortal one is merely retained. Its recursion-depth guard is
still open in that document, and nesting depth is attacker-shaped input on
a store path — which is why it was not built in the same breath.


1. **The 2 → 3 migration** from the mixed vector: walk the vector in
   order and append each element with its integer key, so insertion order
   survives by construction. Needs the mixed vector to exist first, which
   it does not — so this may become the reason to build strategy 2 next
   instead.
2. ~~**Promotion of the storage out of the arena.**~~ — done 2026-08-06.
   Both routes, the same pair a string's payload takes: an in-block
   storage is copied into a buffer-arena chunk, and one over a block
   payload is an OS-direct run the arena forgets, so it keeps its address
   and nothing can be refused. The flat copy is legal because every link
   inside the storage is a `u32` index, which a test already pinned.

   **The route into it is not the one the entry assumed.** An array is a
   COW entity, so it never escapes on its own — the barrier copies a COW
   value out of the arena instead — and it becomes a survivor only as a
   *child* of something that did escape. That edge exists only since the
   tracing arm above, which is why the two had to land in this order.

   Two things the work had to settle. The table keeps its own copy of the
   memory category, so it can be used without an entity around it, and
   promotion rewrites the header — so the carry rewrites the copy too, in
   every outcome including refusal, or every later free of that storage
   would take an arm that frees nothing. And `promote::traceable_in_full`,
   a guard the earlier work left in advance, had to learn the Array kind:
   it asserts that a survivor's children are all enumerable, because the
   COW reconciliation decides a count from the edges it finds and a
   skipped kind would erase holders rather than be ignored.

   Two tests, both seen failing: the in-block carry, on the storage's
   block kind reading arena-retained rather than buffer; and the
   OS-direct transfer, which segfaults without the `forget_large` — the
   reset frees every logged run, so a missed record is a use-after-free
   rather than a leak.
3. ~~**Thread hand-over.**~~ — done 2026-08-06. Storage in a long-lived
   category is a buffer-arena chunk, so it is under the owner/abandon/adopt
   protocol (`dev/DECISIONS.md`, 2026-08-04), which matters because a table
   dies wherever its last reference is dropped: the chunk is routinely
   freed by a thread that did not allocate it, and the buffer block carries
   the owner and the stack that free posts to. The size split is the
   string's — over a block payload the storage is an OS-direct run, since
   an arena chunk is bounded by one block. The table records the *granted*
   byte size rather than the requested one: a reused chunk may be larger
   and the arena's free is size-carrying, so freeing by the request would
   lose the difference from the block's free list. Storage still never
   goes through `entity_alloc`, which would put headerless bytes in a
   block the collector reads as entities.

   **The independent review found a live abort next door**, in the branch
   this work rewrote rather than in what it added: a request-arena table
   called `Arena::alloc` for a storage of program-visible size, and that
   asserts above a block payload, so the 1025th element of a request array
   killed the process — by abort in release, the profile not unwinding.
   Both arenas split by size and neither split belonged in the table, so
   there is now `Arena::alloc_body` beside
   `buffer_arena::buffer_alloc_longlived_payload`, and `buffer::
   buffer_ensure` — which had the same split inline for a string payload —
   goes through it too.

   **One latent defect next door was closed with it.** `string::
   grow_payload` routed every category but the request arena through a
   catch-all into the buffer arena, immortal included. Unreachable today —
   `ll_string_new_dynamic` refuses both long-lived categories, so no
   dynamic string carries either — but the wrong answer was written down
   waiting for a caller: an immortal payload in a buffer chunk holds its
   block's `live` above zero for the life of the process, and growth there
   frees the old payload, whose address an immortal reader may have cached
   forever. The match is exhaustive now and those two arms refuse. The RFC
   is where the wrong answer came from: `memory/buffers.md` groups
   immortal with long-lived in the buffer arena while `memory/arenas.md`
   and `strings.md` say the opposite — the correction is owed there and
   not yet made.

   Four tests. Seen failing first:
   `a_request_arena_storage_over_a_block_takes_the_large_run_path` on the
   assert above, and
   `heap_storage_is_a_buffer_arena_chunk_and_is_returned_to_it` on the old
   routing, with the block kind reading heap rather than buffer. The other
   two are edges rather than regressions:
   `a_storage_over_a_block_payload_is_an_os_direct_run` and
   `a_table_disposed_on_another_thread_leaves_the_owners_block_alive`.
4. **The strategy tag and the `arrays.md` hole.** Two bits for the
   strategy plus the strong-mode bit live in the ArrayBox body, not the
   flags word, which has none free. And `arrays.md`'s "strategy 1 never
   transitions" cannot hold: separation copies the storage in its current
   representation, so a callee can store a pointer into a proven
   `array<int>`. The generic element write has to dispatch on the tag and
   transition 1 → 2.

### What the critic pass of 2026-08-06 found, and none of it is closed

An independent pass over the tracing-and-death commit found four doors the
array has not gone through. Each was verified against the code before
being written down. **Read this before adding any entity kind**, because
the shape of the problem is more important than the four instances.

11. ~~**The two COW doors.**~~ — closed; see the ordered list above. What
    the entry said when it was open: `ll_array_new` stamps the COW flag, so
    both COW dispatch sites answer wrongly. `object::ll_cow_separate` has a
    `debug_assert` and returns the original entity, which in release writes
    a *shared* array in place — PHP's semantics broken with no signal. The
    shallow copy it needs already exists (`array::entity::separate`); what
    has to move with it is `string.rs`'s private `separation_category`,
    which is the COW rule and not a string rule. `object::escape_copy` has
    `unreachable!()`, and that arm is the deep, category-driven copy of
    `rfc/model/arrays-hashtable.md` — each element republished through the
    barrier with the destination's category. Its recursion-depth guard is
    open in that document, and nesting depth is attacker-shaped input on a
    store path; Edmond was asked to choose between a fixed limit that
    refuses and an iterative copy with an explicit stack, and had not
    answered when the session ended.
12. **Both collectors are blind to arrays.** The concurrent tracer takes
    no arm on kind 2, so a ring through a heap array is never collected —
    the edge in is seen, the edge out is not, the holder reads RC above IN
    and is judged a root every epoch. It no longer needs a stride of its
    own: `trace_cells` is the one dispatch and the array is excluded from
    it by decision, so the arm is an arm rather than a fourth walker. And
    rc-trace's candidate
    gate in `refcount.rs` buffers only kind 0, one masked compare on the
    hot release path, so a ring with no object in it — an array holding a
    ReferenceBox holding the array, which is `$a['x'] = &$a` — never
    becomes a candidate. Both configurations are required legs of the
    gate, so rc-trace is green today with a systematic leak.
    **The bound the arm waits on is a design question, not an
    implementation one — 2026-08-06, and the first answer was wrong within
    the hour.** A relaxed reader cannot read `storage`, `nslots` and `used`
    as unrelated words: growth moves the entries, so it could stride a
    fresh count over a stale chunk. The obvious repair — read `storage`,
    then the counts, then `storage` again, retrying while the two readings
    differ — is refuted by `Table::compact`, which slides live entries down
    **inside the same chunk** and lowers `used`. The storage pointer does
    not change, so the double read sees nothing and the reader walks
    entries that moved under it. Whatever the bound is, it has to cover an
    in-place rearrangement as well as a move.

    A version counter bumped by both operations is the obvious next shape:
    read the version, read the three words, read the version again, retry
    while it changed. It costs the mutator one relaxed store per growth and
    per compaction, both rare, and nothing on insert or lookup.

    **What a walker does when the read never stabilizes, corrected.** An
    earlier note here called giving up the dangerous direction. It is the
    opposite: an entity the walk does not enumerate becomes a **root
    source** by the derived-roots corollary — its out-edges land in `RC`
    and never in `IN`, so its children are computed roots and survive
    (`walk.rs`, `a_ring_among_promoted_survivors_is_collected`, and
    `rfc/model/gc/retained-block-walk.md`). Skipping an array the walker
    cannot read coherently therefore leaks, exactly as skipping it
    unconditionally leaks today, and frees nothing early. So the fallback
    is sound and the only question left is how much it leaks: a bounded
    retry, then skip, costs a ring through that array one more epoch.

    That also re-dates the danger in the wrong-sign direction. What must
    never happen is an *incoherent* read — `storage` from one chunk with
    `used` from another, or a key from one entry with a value from the
    entry that replaced it. That is not a stale snapshot, it is an edge
    that never existed, and no phase repairs it.

    **What must change on the mutator side either way, and it is a defect
    today rather than an omission.** `Table::insert` bumps `self.used`
    *before* it writes the entry (`array/table.rs`, the `let k = self.used;
    self.used += 1;` pair). A reader that sees the bumped count reads an
    entry nobody has written. It is latent while nothing walks an array
    concurrently and becomes live at exactly the commit that teaches the
    tracer to, which is why it has no regression test of its own. The
    repair is publication order — write the entry, then publish the count —
    and the same rule applies to `grow`: fill the new chunk, then publish
    `storage` last. Both stores the collector reads must also *be* atomic
    stores, or the mutator's plain write against a relaxed load is a data
    race rather than a torn value.

13. **The dispatch surface itself.** A kind has six doors —
    `ll_entity_die`, `walk::trace_entity`, `collector::trace_mature`, the
    candidate gate, `ll_cow_separate`, `escape_copy` — plus
    `promote::traceable_in_full` as a guard. The array went through two and
    missed four, and nothing made that visible: a miss hides in an empty
    default arm or in a masked compare. Proposal on the table: dispatch on
    `EntityKind` with exhaustive matches, so a new kind fails to compile
    until every door has an arm. Two doors are hot and `trace_mature`
    cannot use the ordinary accessors, so the shape has to survive both.
14. **Three smaller ones.** The escape ledger is asymmetric inside
    `array/entity.rs`: `separate` takes references with a bare `ll_retain`,
    a no-op on an arena entity, while `release_children` gives them back
    through `drop_ref`, which calls `escape_lose` — a copy that recorded no
    gain spends a hold-count belonging to a real holder. Unreachable today.
    Several array tests call `ll_entity_die` on an entity still at
    refcount 1, leaving a slot the process-global walks enumerate as live.
    And `ll_array_new` accepts `LongLived` while `array_die` frees only
    `GcHeap`, one commit after that category was marked out of use, with
    its `salt` parameter carrying a security policy that has no contract
    comment and no named source.

### What the second critic pass of 2026-08-06 found, and the two rulings

A pass over an architect's answers to the two open questions found three
live defects nobody had named, and refuted the architect's main
recommendation. Each was verified against the code before being acted on.
Items 15–17 are closed; 18–20 are not.

15. ~~**`walk::sever_component` had no Array arm**~~ — closed `144b318`.
    The seventh door, and the one that matters most about the count: a
    kind that falls off it does not leak in a way the next pass finds. The
    component has already been confirmed garbage, so its members are
    guarded, severed of nothing, and un-guarded back to the counts they
    started at — `collected` reads zero and the same work repeats on every
    later call. A ring of two arrays with no object in it was
    uncollectable in **both** configurations. The mixed object-array ring
    did free, but by accident, and in a way that broke the deferred-drop
    property the function's own doc exists to guarantee.
16. ~~**The white set was freed without regard to what an entity holds
    outside its own slot**~~ — closed `f56a035`, and wider than it was
    reported. An array's storage was the reported half; a dynamic string's
    payload is the other, older than arrays, and reached by any cyclic
    garbage with a string property.
17. ~~**`carry_out_of` left the category wrong on refusal**~~ — closed
    `2e55036`. The damage was on the allocation side rather than the free
    side its own doc worries about, which is why it survived review: a
    promoted heap array took its next storage from whatever request arena
    was mounted.
18. ~~**The flood ladder's second rung does not exist.**~~ — closed. A
    table carries `TABLE_RESEEDED` beside `TABLE_STRONG` in one byte of
    flags, and a second long chain escalates instead of rebuilding again,
    which is the bound the documents promise. The salt's redraw mixes the
    process seed and the storage address into the LCG step, so the orbit
    is no longer computable from the initial salt alone; under
    `hash-folding` the seed is a build constant and only the address is
    left. Regression: the ladder's two rungs in order, seen failing at the
    escalation. **Found while doing it, and owed to the RFC:** below
    `strong` a string key's slot *is* its cached hash, which no salt
    enters, so the first rung cannot separate string keys at all and the
    second cannot separate integer keys — the two rungs answer different
    key kinds rather than escalating one defence. What the entry said:

    `rfc/model/arrays-hashtable.md` both say a second firing escalates,
    and both bound the attacker at one rebuild and one escalation per
    table. There is no reseed counter: the chain trigger calls `reseed`,
    only the equal-hash trigger calls `escalate`, and `reseed` returns
    early only on `strong`. So the chain trigger fires without bound, each
    firing an O(`used`) rebuild. The new salt is a public LCG over the old
    with no entropy added, so an attacker who knows the initial salt knows
    the whole orbit offline and can make every insert reseed: O(n²),
    against a document that promises O(n) twice.
19. ~~**A COW copy silently de-escalates an attacked table.**~~ — closed.
    `separate` takes the source's flood state before its first insert
    (`Table::adopt_flood_state`), since the mode decides how a key is
    hashed and a table that adopts it afterwards has already indexed its
    entries the other way.

    **The entry overstated the damage and the test says why.** While the
    colliding set is still in the table the copy re-escalates on its own —
    the equal-hash trigger fires again on the ninth collider it
    re-inserts — so the weak window is bounded by the trigger rather than
    open for the life of the copy. What makes the loss permanent is
    `unset`: with the set thinned below the threshold nothing re-fires,
    and the copy is back to the hash the attacker already knows, ready
    for the same flood. The regression is built that way and was seen
    failing on it.
20. ~~**The candidate gate diverges from the RFC, and costs one
    constant.**~~ — closed; see the ordered list above and
    `dev/DECISIONS.md`, 2026-08-07. One correction to what the entry
    proposed: `forget_candidate` could not simply move up into
    `ll_entity_die`, because the object's own `__destruct` runs *inside*
    `dispose` and can buffer the object again, so `ll_object_die` calls it
    a second time after `dispose` returns. The duty the entry wanted gone
    — one every generated `dispose` must remember forever — is gone.

**The renumbering is rejected, and the reason is worth keeping.** The
architect proposed moving the four kinds that carry traceable slots to
codes 0–3 so the gate could admit `{Object, Lazy, Array, Reference}` in
one compare — the set that would catch `$a['x'] = &$a`, which
`{Object, Array}` does not, the last external release landing on the
ReferenceBox. The crate itself is clean: every use is symbolic, `is_object`
survives because Object stays 0, and the compiler holds no kind constant.
The RFC was not, and by 2026-08-07 it is: Edmond ruled the codes out of
it altogether, so the documents name kinds and the assignment is
normative in `EntityKind` alone (`rfc` `f170662`). Two of the three
reasons recorded here went with that ruling. **The third was wrong
arithmetic and is corrected rather than kept:** consolidating the Proxy
family buys codes, not a bit — seven kinds take three bits and five kinds
take three bits — so the family's adjacency was never a route to a free
bit, and `layouts.md` had been counting codes all along. What remains
against the renumbering is that the set-membership gate makes it
unnecessary, and that a numbering fix expires at the next kind that needs
admitting. Revisit it when `resource` needs a code, and price the Proxy
family then.

**Ruling on the escape copy's recursion, and on the two depths.** An
explicit work list, not a depth limit. A limit would have to refuse
through a channel whose only meaning is "out of memory", which is a lie
while memory is plentiful and unfixable until the exceptions runtime
exists; PHP has no nesting depth at which an assignment becomes invalid;
and the limit would not even remove recursion from the crate, since
teardown of what it permits is recursive too. The list itself belongs in
a buffer-arena chunk rather than on the machine stack or in arena bump
memory. Termination needs no visited set: recursion enters only arena COW
children, and a cycle cannot close inside a pure-COW subgraph while the
count-equals-holders invariant holds — every entity a real ring must pass
through is non-COW and is counted rather than entered. Assert that under a
debug feature rather than paying for it.

The deep and shallow copies are **one body parameterized by destination
category**, with the barrier supplying the depth: called with an arena
destination the copy arm cannot fire, called with a heap destination on an
arena source it is clause for clause the RFC's deep copy. The two depths
are two call sites, not two operations. `ll_cow_separate` never presents
the deep configuration — `store_category_barrier` copies every arena COW
entity crossing into a longer-lived slot, so no longer-lived slot ever
holds an arena array to separate. **This amends an authoritative sentence**
(`arrays-hashtable.md`, "two depths … must not be confused") and the
module comment in `array/entity.rs` that follows it, so it is Edmond's to
accept; the code shape does not depend on which wording wins.

`separation_category` is the COW rule and not a string rule — every reason
it gives has an array counterpart that was verified — so it moves out of
`string.rs` rather than being copied.

**One window, documented rather than closed by the table.** `Table::insert`
writes an entry raw and leaves the counting to the caller, so a string key
appears as an out-edge before any reference backs it, and the extra
in-edge pushes the key toward looking unrooted. It is absorbed today by
three independent things — a fresh copy is allocate-blacked and never
traced, the phantom edge supplies its own mark path from an array the
mutator provably reaches, and Phase 4 recomputes on the owning thread —
but relying on that is exactly the distant dependency item 13 exists to
make visible. The repair is a crate-internal barrier entry that publishes
an already-retained reference: `store_category_barrier` is that operation
already, minus the slot write. It must land with or before item 12's arm.

## What the stage-end review of S2 and S3 demanded, and none of it is closed

The Code Reviewer's verdict on the two stages together, 2026-08-08: the
element layer is the first subsystem in this crate a caller can learn
from five doc blocks without reading a body, and `table.rs` absorbed a
layer that is not its own. Everything below is behaviour-preserving; the
review's own style and comment fixes are already in the code.

1. **`Table::make_ref` is the element layer's work living in the table.**
   The module header promises index slots, a dense entry array and three
   operations over them, and nothing that knows what an entity is. That
   function now allocates a reference box, crosses two memory categories
   through the store barrier, runs an escape copy and gives the entry's
   reference back — the composition `element::store_into` performs one
   layer up. A reader who wants to know how `&$a['x']` settles ownership
   must hold four contracts at once. The cut: leave `Table` an
   entry-replacement primitive that hands the old element back, and move
   the boxing, the crossing and the giveback beside `element::set`.
2. **The publication idiom is written four times.** Retain, then
   `store_category_barrier`, then on null release and refuse, then on a
   different pointer release and swap the Value — at `element.rs`'s two
   sites, `Table::element_for_box` and `entity::fill_from`. It wants one
   helper (`barrier::publish_child(arena, category, Value) -> Option<Value>`),
   and until it exists a fix to the rule has to be found in four places.
   `element_for_box` is `store_into`'s value arm with `GcHeap` written in.
3. **`destroy_private_copy` and `destroy_empty_box` are the same two
   calls with contradictory assertions** — one asserts the release did
   not report death, the other that it did. Both tear down an
   unpublished entity at count one, and the divergence in the assertion
   is the knowledge; it is invisible from either site.
4. ~~**Neither `bool` return in `retained.rs` was `#[must_use]`**~~ —
   closed in the same pass. Dropping either answer leaks a 64 KiB block
   silently.
5. **Four public contracts are stated by pointing at a private
   function.** `set`, `append`, `unset` and `make_ref` link to
   `write_through`, which no caller can read, so the ordering guarantee
   they sell exists in full only inside the module. The crate has 47
   private-doc-link warnings overall, so the pattern is older than this
   stage; here it hollows out four contracts at once.
6. **`a_reference_to_an_absent_key_creates_it_as_null` pins the
   implementation.** It asserts the entry holds the box itself. The
   contract is that reading `$a[5]` yields null and a write through `$r`
   is visible there, which survives any other reference representation.

## Then: arrays as a performance problem

Opened 2026-08-07 at Edmond's request, and it gathers work the plan had
scattered rather than adding any. Everything here is measurement or
representation; nothing in it is a defect.

**The generic element write ran first**, by Edmond's ruling of
2026-08-07: the strategy tag exists for the dispatch inside that write,
so the write came before the tag rather than after it. That write is
`src/array/element.rs`, built and closed as stage S2.

**The representation, and the tag waits for its second occupant.** Two
bits of `Table::flags` name the storage strategy, which nothing sets yet.
It was drafted as the last step of the element-write stage and taken
out: with one strategy
built the dispatch has one arm, no test can tell a write that reads the
tag from one that calls the table directly, and `arrays.md` makes a fresh
dense array strategy 2 — so a test pinning "a fresh array is strategy 3"
is one the strategy-2 work would have to rewrite. The crate refused this
shape once already, leaving the hash-function selection unbuilt with one
occupant for the same reason. The cost of deferring is that the write is
edited twice, and the write is a few lines. Behind the tag: the generic
element write dispatches on it and performs the 1 → 2 transition
`arrays.md` says never happens — it must, because separation copies the
storage in its current representation and a callee can then store a
pointer into a proven `array<int>`. Then the 2 → 3 migration, walking
the vector in order and appending each element under its integer key, so
insertion order survives by construction.

**The entry is 32 bytes and the inline hash survived.** Settled
2026-08-07, by reasoning rather than by the clock, which is why it could
be settled here at all. The eight bytes came from the `next` field, not
from the hash: the collision link moved into the element Box's reserved
bytes at +28, `meta` went with it, and an entry is now `hash_or_key`,
`key` and the element. Per element of capacity that is 40 bytes against
`zend_array` 7.3+'s 40 — parity, where it had been 48.

What the move costs is not memory but a rule, and the rule is enforced
rather than remembered: the element field is private, so no caller can
assign a whole Box over the link; every write goes through
`Entry::store_element` or `Entry::store_link`, which compose tag, flags
and link into one relaxed atomic store matching the eight-byte relaxed
load the collector performs; and every read hands the Box out through
`Value::without_reserved`. Two rejected shapes are worth keeping: the
link in the *element's* reserved bytes with a narrowed store barrier
(mixed-width atomics against the collector's load, and it breaks on
objects before arrays), and the entry as two `Value`s with the link in
the key half (the key's tag has to be read by the collector, so the link
would share a word with it again). `dev/DECISIONS.md`, 2026-08-07.

**The numbers still unmeasured.** The string-key check's reversal
threshold, the compaction threshold borrowed from Zend at ~3 % rather
than measured, and the two flood constants. None of these can be settled
on this box: `dev/BENCHMARKS.md` puts its noise floor at 1.5–3 %, and
every effect above is smaller than that.

**What had to come first is done** (`a2e1318`): the `RcHeader` rule of
2026-08-07 (`dev/DECISIONS.md`) took `category` out of `Table`, so the
strategy tag goes into a struct whose fields have stopped moving.

## Then: `Map`, whose keys may be objects

**After the stage above, and Edmond ruled the order on 2026-08-07: the
array is finished first.** A map is strategy 3 with a wider key, so it
becomes the entry's second customer — and every question the stage above
still has open is a question about that entry. The 40-versus-32-byte
measurement decides whether the inline hash survives; the key-kind tag a
map needs wants the reserved `meta` word beside it; the strategy tag
decides whether a map is a strategy or a kind. Building the map first
would fix all three by accident, in whatever shape the first
implementation happened to take, and the array would then be optimized
around a customer rather than the customer around the array.

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
hand the tracer an `LLString` that is an `Object`. The kind has to move
somewhere the walker reads: `Entry::meta` is the reserved `u32` beside
`next` and is the obvious home, at the cost of one more load per walked
entry.

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
four items above. Numbered as they are in the session tool.

5. ~~**One place for the category → allocator routing.**~~ — closed.
   `memory/routing.rs` holds `entity_alloc_in` for the seven factories
   and `body_alloc` / `body_ensure` / `body_free` for the two out-of-line
   bodies. Each call site keeps only what is its own: the dynamic string
   factory's refusal of the two long-lived categories, and
   `carry_out_of`'s naming of the destination, which cannot come from
   `self.category` because that field still says `RequestArena` when the
   copy is made.

   **The free does not dispatch on the block kind, and the entry was
   wrong to say it could.** Both arenas put a body over a block payload
   in an OS-direct run, so `BLOCK_KIND_LARGE_RUN` names a run the request
   arena logged and will free at its reset exactly as readily as one the
   caller owns. Freeing by kind alone double-frees arena storage — the
   suite aborted on `corrupted size vs. prev_size`, which is how this was
   found. The category decides between the two populations; the block
   kind decides inside the long-lived one, which is what
   `buffer_free_longlived_payload` was already doing.
6. **~~RFC: rename the categories where they are defined~~** and
   7. **~~carry it through the referring documents~~**, 8. **~~rename them
   in the crate~~** — all three **deferred 2026-08-06**, reasoning in
   `dev/DECISIONS.md`. `LongLived` is named after a duration rather than
   an owner, which is why its reclamation was never decided; but `Region`
   would mark exactly the entities no region owns, since a `#[Region]`
   class owns *arenas*, and `Arena` would make `arenas.md`'s "between two
   request arenas: forbidden" false before the mechanism justifying it
   exists. Both wait on item 9. Meanwhile the category is marked out of
   use on the enum itself.
9. **The region reset, and the refusal that waits on it.** The mechanism
   that would make a long-lived category mean something: what a region
   owns, when it resets, how the owner's O(1) death reaches its entities,
   and what promotion across a region boundary is.
   `rfc/model/memory/regions.md` is the starting point. It also gates
   `ll_string_new_dynamic`'s refusal of that category — today nothing
   would reclaim such a string. Blocked on design, not scheduled.

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
  A fully emptied retained block returns to the pool since 2026-08-08:
  the last occupant's death reports through `stdapi::ll_free`'s retained
  arm (S3.1), and a block held for a payload the reset could not carry
  waits for that payload's own free (S4.2).
- [x] Run `__destruct` of cyclically-dead objects (2026-07-25) — Zend-style
  discipline (`run_cyclic_destructors`): restore the white set's real
  counts, guard, run each `__destruct` once through the ordinary teardown,
  then re-collect so a resurrected subgraph survives. No new mechanism (no
  retain hook, no GC-window flag); reuses `drop_ref`/`ll_object_die`/
  `forget_candidate`. Tested for the plain cycle, an `unset`-in-destructor
  (double-free), and resurrection into a live holder (child survival).
- [ ] `rc-satb` as a second build-time GC strategy (needs the `WRITING`
  bit from A5). `rfc/model/gc/satb.md`.

### Phase C — new subsystems (each its own RFC + code)

- [x] **Strings** — done 2026-08-05, all sixteen tasks. Both layouts of
  the string entity, the rapidhash V3 port with its seed and the
  `hash-folding` option, the interpolated template as its own class
  (`src/string.rs`, `src/hash/`, `src/template.rs`,
  `rfc/model/strings.md`).
- [~] **Arrays** — storage strategy 3 is built (`src/array/`): the
  ordered hash of `rfc/model/arrays-hashtable.md`, its entry layout, the
  flood ladder, element references, the generic element write. Strategy 2
  and the strategy tag are S7 above; strategy 1 has no producer.
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

- [ ] **Batch the cross-thread free, once a workload exists** — gated on
  measurement, and the gate comes first. Today `Heap::free` posts each
  foreign slot with its own CAS onto the owning block's `remote_free`
  stack (`heap.rs:967`), and `buffer_arena::post_remote` does the same for
  a chunk (`buffer_arena.rs:733`), so the cost is linear in items freed.
  snmalloc gathers the same work into one message queue per owning
  allocator and pays one atomic operation per batch instead
  (`dev/RESEARCH.md`, 2026-08-08).

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
  the crate is single-mutator, and the only caller is
  `heap.rs:2532`'s test plus whatever reaches the raw C ABI from another
  thread. Order: a program that frees another thread's objects in bulk,
  then a measurement, then this.

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
  `dev/design/debug-modes.md`; build order is its section 10. Designed,
  and its first item is scheduled: the event journal is stage S5 above.
  The rest of the section is unscheduled.
- [x] **The opt-in event journal, designed to completion** — design done
  2026-08-06, `dev/design/debug-modes.md` §9. One ring per thread, 32-byte
  fixed records, a window marked by a cursor snapshot across the registry,
  eviction reported as *unknown* rather than *none*. Not built: it is item 1
  of the build order, and its first customer is the census flake above.

## Cross-cutting (every phase)

- Correctness tests per the project style (`test_guard`, scenario-per-test)
  and criterion benchmarks per `dev/BENCHMARKS.md` — follow the protocol,
  do not improvise. Benches do not cross the C ABI; ABI-entry work is shown
  by IR/asm.
- `dev/ARCHITECTURE.md` — the crate's knowledge map: layers and their
  sanctioned edges, the per-module "does not know" table, the header-bit
  ledger, the five end-to-end paths. Written; it moves with behaviour
  like any other document (`dev/WORKFLOW.md`).
