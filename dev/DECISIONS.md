# Decisions

An architecture changelog: what was decided and why, not what changed
in the code. Routine fixes and renames belong to git, not here.

A superseded decision is replaced by a **new entry**; old entries are
never edited or deleted.

---

## 2026-08-04 — the buffer arena joins the hand-freed structures, and the exit order gains a fifth step

`buffer_arena.rs` carried a note from 2026-08-03: its `THREAD_BUFFER_ARENA`
was a `RefCell<BufferArena>`, therefore had drop glue, therefore had a TLS
destructor registered after the exit guard's and running before it — safe
only while nothing on the thread-exit path reached it, and to be converted
"the moment a teardown frees a long-lived buffer, which strings and arrays
will do". The dynamic string is that moment: static-block teardown runs
user code, that releases entities, and `string_die` returns a payload
through `with_buffer_arena`. The panic would be an `AccessError` inside a
TLS destructor, which cannot unwind — an abort of the whole process on a
worker thread ending normally.

**Decided:** the same shape as the other three conversions — a
`Cell<*mut BufferArena>` with no drop glue, allocated on first use, freed
by an explicit `dispose` that `ll_thread_exit` calls. `Drop` stays on
`BufferArena` itself: stack-built arenas in tests still need it, and it is
now run by hand rather than by TLS glue.

**The order in `ll_thread_exit` becomes:** static blocks → candidate
buffer → parked list → weak table → **buffer arena** → the heaps. The
arena is fifth because every step above it can still free a buffer: the
static blocks through user code, and the parked backlog's flush through
the same payload route. It is before the heaps only because nothing after
it needs an arena; the blocks it hands back go to the process-global
pool, which outlives every thread. Disposing earlier is not detected — a
later free would build a second arena through the lazy path and leak it —
which is why the position is written down rather than inferred.

**Regression:** `static_block::tests::`
`a_thread_that_just_ends_frees_a_static_strings_payload`, a worker that
puts a dynamic string in a static and simply ends. Verified by restoring
the `RefCell` and watching the process abort.

## 2026-08-04 — promotion carries a survivor's payload, and the two routes are not the same route

A dynamic string that survives an arena reset keeps its entity — the
block holding the header is retained — but its payload is arena memory
and would go back to the pool. Promotion now asks one kind-dispatched
question, `carry_external_memory`, so the reset still knows nothing about
any layout: it holds blocks by the address of a header and does not
otherwise look inside an entity.

**An OS-direct payload transfers; an in-block payload is copied.** Not
symmetry for its own sake — the difference is what removes the failure
mode. A reset runs at the end of a request, where there is no caller left
to report a refusal to, and a transfer allocates nothing: `forget_large`
zeroes the arena's record of the run and the drain skips zeros. The
in-block route has to copy, since the block itself is going home, but it
is bounded by a block payload.

**When the copy is refused**, the payload's block joins the retained set
— the same mechanism that keeps the survivors' own blocks out of
circulation, for the same reason — and the string reads its old bytes for
the rest of its life. `buffer_free_longlived_payload` gained a
`BLOCK_KIND_RETAINED` arm that does nothing, which is that fallback's
other half. **Rejected: failing the reset**, which would introduce a
class of failure nothing above it can handle.

**Superseded:** the escape ban of the entry below, which lasted a day.

## 2026-08-04 — a dynamic string may not escape its arena, and the escape barrier says so out loud

`escape_gain` asserts the escaping entity is **not** COW, an assert
written to keep COW values out of the hold-count path. The dynamic string
is the crate's first arena-allocatable non-COW entity, so that assert
admits it by construction.

It must not be admitted. Promotion keeps the survivor's header and
rewrites its category but knows nothing about the payload, which is arena
memory: retention keys on the block holding the *header*, so the
payload's block goes back to the pool at the reset and the survivor reads
recycled memory. Worse than the dangle, the block is later re-stamped
`BLOCK_KIND_BUFFER`, and `buffer_free_longlived_payload` routes by
reading that kind — so the eventual free decrements a live count and
writes a free-list link into a chunk the buffer arena never granted.

**Decided:** a real `assert!`, not a `debug_assert`, refusing an escaping
entity of kind String with `COW` clear. In release the alternative is
silent corruption several allocations later, and the compiler allocates a
dynamic string only where it proved a single owner — so reaching this is
already a broken contract, and an abort naming the cause beats a heap
whose free lists disagree with it.

**Rejected: banning `RequestArena` for dynamic strings.** That taxes the
common and perfectly safe case — an accumulator that dies with the reset —
to prevent the rare one.

**Lifted when** promotion carries the payload (`rfc/model/strings.md`,
"An arena string that survives the reset takes its payload with it";
`PLAN.md` task 13).

## 2026-08-04 — a COW copy is sized by the holder's lifetime, and comes back at +1

`values.md` gives the separation rule and says the holder stores what the
barrier returns, but leaves two things to the implementation, and both
are the kind that corrupt quietly when guessed wrong.

**Where the copy lives.** `ll_cow_separate` takes `owner_cat` — the
**holder's** category, supplied by the compiler as it is to every other
store-side barrier — and the copy goes to the request arena when the
holder is arena-category, to the GC heap otherwise. The original's
category does not enter into it. A copy is a fresh entity nothing has
registered anywhere, so the only way it goes wrong is a holder outliving
it; an arena holder cannot, and every other holder needs something that
survives the reset. This also keeps a copy out of the two categories that
cannot own a written string at all — immortal is shared process-wide, and
`string_die` frees only `GcHeap`, so a long-lived copy could never be
reclaimed.

**Rejected: deriving the category from the original**, which is what the
first version did. Arena original plus longer-lived holder then produces
an arena copy under a heap slot — a cross-lifetime reference the barrier
was called to prevent — and interned original plus arena holder produces
a heap allocation and a release-at-reset record for a value the reset
would have reclaimed for free.

**The reference the copy comes back with is +1, owned by the caller**,
like every other factory here, and the composition is written out in
`string::separate`: separate, store (which retains), drop the displaced
original, release the creation reference. The last step is the one worth
naming — skipping it leaves the copy at two for one holder, and that is
worse than a leak: the sharing test reads `2 > 1` on every later write,
so the value separates forever and COW is off for the rest of its life.

**The COW flag is tested before the category**, although `values.md`
lists the category first and mentions the flag only in its third line.
For every entity this crate produces the answer is the same, since every
string is COW; the reorder buys two things. A dynamic string
(`COW = 0`, bytes out of line) that reached the barrier by mistake writes
in place as its layout demands instead of having its header read as
inline bytes. And a **non-COW plain object** that is immortal or escaped
is no longer copied — under the document's order it would be, and copying
a plain object breaks reference identity.

**Found while reviewing this, not fixed here:** `values.md` justifies the
category test with "the count is pinned", which describes immortal and
not long-lived — a COW entity takes neither early return in `ll_retain`,
so its long-lived count is fully maintained. The arm is kept for the two
reasons that do hold, recorded at `cow_separation_needed`.

## 2026-08-04 — the string header follows the design, and the design is what the test pins

`LLString` carried `hash` at +8 and `len` at +16; `rfc/model/strings.md`
specifies the opposite, and names +8 and +16 explicitly because the
dynamic layout has to put the same two fields in the same places. The
code had matched `zend_string`'s order instead, which is where it
presumably came from. Nothing was broken by it — one string kind exists,
it is immortal, and only `intern.rs` reads those fields — and nothing
would have been broken until the dynamic layout arrived and started
reading a hash as a length.

**Decided:** the fields are reordered to `len`, then `hash`, and
`intern::tests::layout_matches_the_string_design` pins all four offsets
plus the struct size. Verified by putting the old order back and
watching the test fail (`left: 16, right: 8`); the module's other tests
stay green through the swap, which is the reason the contract needs a
test of its own rather than an assumption.

**Rejected: amending `strings.md` to the code's order.** Either order
works in isolation, but the document is authoritative here and its text
leans on the specific offsets; changing the code is two lines and one
comment, changing the design is a paragraph other documents cite.

## 2026-08-04 — the runtime reaches compiled PHP code as bitcode, not across the C ABI

`Cargo.toml` declared `rlib` + `staticlib` "for the C++/LLVM layer" and
`dev/INDEX.md` repeated it, so the crate read as though its surface to
generated code were the `ll_*` C ABI. It is not: the crate is also
emitted as LLVM bitcode and merged with compiler-generated IR
(`llvm::Linker::linkModules`), and the optimizer inlines across the
boundary — `README.md`'s "LLVM IR export" records `ll_retain` fully
inlining into its caller after `opt -O2`. The design behind it is
`rfc/runtime/implementation-language.md`; the C ABI proper survives only
between this crate and the thin C++ layer that holds LLVM.

**Decided:** both places now say so and point at those two documents.
The practical consequence for new work: a new entity kind does not need
an `ll_*` entry point to be reachable from generated code, so one is
added when a caller exists, not by convention. Strings are the first
kind to be built under that reading (`PLAN.md`, task 3).

**Cost:** a hot path is only fast in the builds that do the merge; a
consumer linking the `staticlib` and calling across the real ABI gets
the call, not the inlined body.

## 2026-08-03 — thread exit owns the order its per-thread state dies in

A6 made thread exit run user code for the first time: the static-block
teardown runs `__destruct` bodies, and those cascade into the candidate
buffer, the parked-free list and the weak table. All three were
`thread_local!`s holding a `Vec` or a `HashMap` — drop glue, therefore
registered for TLS destruction. `ll_thread_exit` is itself reached from
a TLS destructor (the guard `ll_thread_init` installs), and glibc runs
destructors in reverse registration order, which puts that guard
**last** exactly because it registers first. So the structures it needs
are reliably already destroyed, `LocalKey::with` panics with
`AccessError`, and a panic inside a TLS destructor cannot unwind: the
process aborts.

**Decided:** every per-thread structure the exit path can reach is a
`Cell<*mut T>` — no drop glue, never registered for destruction,
readable for the whole life of the thread — freed by an explicit
`dispose` that `ll_thread_exit` calls in an order it chooses:
static blocks (the only user code) → candidate buffer → parked list →
weak table → the heaps. `weak.rs` had already written down that its
table must go last once static-block teardown existed; this is that day.

**Rejected: `try_with`.** For the weak table it is not a smaller fix but
an unsound one — a swallowed death notification leaves the cell's target
dangling, and the next `__destruct` calling `get()` receives a retained
pointer into freed memory. `try_with` is right only where failure means
"fall back to the global tier", which is why `block_pool` and `reserve`
already use it and keep it.

**Rejected: making the exit pass run before any TLS destructor.** There
is no such hook in Rust for a thread that simply ends, and the ordering
cannot be arranged: registration order is first-access order, the guard
is registered at `ll_thread_init`, and "last first access" is
data-dependent. Windows order is unspecified too.

**Rejected: one consolidated per-thread state block** behind a single
TLS pointer. It would put a gc candidate buffer (L4) and a weak table
(L3) inside a structure owned by `heap` (L2) — an upward-knowledge
violation and a design event by this file's own rule — for no
correctness gain over per-module cells. It stays available later,
behind the same accessors, if a measurement ever justifies collapsing
the TLS reads; on Windows it would replace three dependent loads with
the one-instruction `gs` read.

**Cost:** none on `ll_release`, which is untouched instruction for
instruction — the buffered-bit test reads flags already in a register
and `buffer_candidate` is `#[inline(never)]`. Inside the moved bodies a
`RefCell` borrow-flag load/store pair becomes one dependent load plus a
null check that is cold after first use: not a new cost class, and
**not** claimed as a speedup, because this box's noise floor is 1.5–3%
and nobody measured it.

**Found by the same review, closed here:** `ll_static_block_register`
could abort on a refused growth, where this crate refuses instead
(`try_reserve`, and a hand-rolled allocation as in `ll_thread_init`).
`buffer_arena`'s key has the same drop-glue shape and no caller on the
exit path yet; it carries a note rather than a change.

**One suggestion from that review was tried and reverted, by Miri.**
Candidates still buffered at thread death keep their buffered bit while
their blocks go to the abandoned list, so an adopted object released
later can never be re-buffered — a silent cycle leak. Clearing those
bits during disposal looked free, since the drain walks them anyway.
It is not: a candidate can already be **gone**. An arena entity dies
with its arena without individual teardown, so nothing calls
`forget_candidate` for it and its buffer entry outlives its memory;
Miri reported a write through exactly such an entry (`gc.rs`, in-bounds
pointer arithmetic on a dangling address). The disposal now frees the
buffer and touches nothing in it, and the stale-bit leak is recorded as
a known limit on `gc::dispose` — closing it needs a liveness test the
buffer cannot provide, which is a design question rather than a
cleanup. Cross-thread entity survival is reserved today, so nothing
reaches the case.

## 2026-08-03 — a static block registers its layout, not a teardown function

A6 needs each thread to release its static blocks' roots at exit, and
`rfc/model/classes.md` describes the pass as a **compiler-emitted**
per-block teardown striding the block's reference slots. No compiler
emits one, so the registration ABI had to be chosen now and survive the
generated version later.

**Decided:** `ll_static_block_register(block, layout)` — a bare pointer
to the headerless block and the descriptor whose `ptr_runs`/`box_runs`
give its reference slots. The runtime strides them generically, exactly
as `ll_default_dispose` stands in for a generated dispose (A3).

**Why not a function pointer now.** `register(block, teardown_fn)` reads
like the eventual shape, but the only function it could carry today is a
generic one that still needs the descriptor — so the descriptor would
travel anyway, as a bound argument, and the ABI would carry two things
where one suffices. When the compiler does emit straight-line teardowns,
`ll_static_block_register_fn(block, fn)` is **additive**: a second entry
point, no change to this one, and blocks registered either way tear down
in one LIFO order because the registry holds the two forms in one list.

**Order is LIFO**, as C++ tears down function-local statics: a block
initialized later may have been initialized against an earlier one, so
the earlier one has to outlive it.

**Drops go through the barrier's `drop_ref` with `owner_cat =
LongLived`**, not a bespoke release. That is what makes the three cases
come out right without a branch here: a request-arena escapee loses its
escape hold-count (thread exit is the only decrement point besides an
overwrite mid-request), a heap reference releases and cascades into
teardown, an immortal one is a no-op. The block being headerless does
not matter, because `drop_ref` acts on the displaced entity and
`owner_cat` is a parameter rather than a load from owner flags — which
is why the barrier was built that way (A4).

**Cost:** a thread that never registers a static block pays one null
check at exit. The slot stride now has its third occurrence, so it is
abstracted rather than copied: `sever_counted_slots` takes a base and a
descriptor, and object teardown, the drain's sever and this pass all
call it.

## 2026-08-03 — retained blocks are walked through a per-block object index

Retained former-arena blocks were unwalkable because an arena's bump
allocator leaves mixed sizes and no stride to divide by, so their
occupants were root sources and a ring living entirely among promoted
survivors was never collected (`rfc/model/gc/retained-block-walk.md`).
The reset already builds the inventory — `promote`'s fixpoint collects
every survivor into a vector and then drops it. It is now kept, split
per block and sorted by address, as that block's object index. Three
properties make it cheap: the set is frozen because nothing allocates
into a dead arena, a dead entry validates itself by reading refcount 0
(the walk's own occupancy test), and a retained slot is never reissued
because the block leaves the pool only when all its survivors are gone.

**Granularity: one index per retained block, not one per retained set.**
Both enumerators reach a block first — one by the 64 KiB alignment mask,
the other by scanning the region registry — and need the index from a
block address. A per-set index would need a second block→set map on the
lookup path that the per-block one does not.

**Totality: the census keeps one lookup.** rc-walk.md requires row
omission and edge omission to be one decision taken at one test, and a
census with two sources could have become two lookups with two answers.
Instead the single sorted payload list carries both kinds of block, and
only the slot derivation branches after the match: a strided block
divides, an indexed block binary-searches its own index. One search,
one answer, and an address in neither kind still finds nothing.

**Lifetime: the index is owned by a registry keyed by block address**
(`memory/retained.rs`), released by one call. Nothing calls it yet —
retained blocks never return to the pool today — so the index lives
exactly as long as the retention it describes, which is the correct
lifetime and not a leak. The call exists so the hook is one line when
"return a fully emptied retained block" lands.

**Rejected: copying survivors into entity blocks at reset** so the
existing strided walk covers them. That is the evacuation the RFC
defers behind the escapee-reference fixup, it costs a copy per survivor
where the index costs one pointer, and it would have made a placement
choice out of a collectability requirement.

**Cost:** one pointer per survivor for the life of the retention, and
`EntityBlockSnapshot` stops being plain numbers — it now carries the
index for a retained block. The collector still computes slot addresses
itself and still touches slots only through the relaxed-atomic helpers;
what it gained is data it reads, not slots it reaches into.

## 2026-07-28 — the epoch walk's child test is a dense census, not a map

The collector's walked-row lookup is a per-slot `u32` array laid out
from the block snapshot, indexed by (block via 64 KiB alignment mask +
binary search over sorted payloads, slot via division with a remainder
validity test). **Why:** the per-epoch `HashMap<address, row>` hashed
every child and cost a build pass over the walked set — measured 2–3×
of the whole walk step (`BENCHMARKS.md` 2026-07-28). Rejected: hashing
with a cheaper function (still a probe per child, still a build pass);
a region-indexed table (more machinery, only pays if the binary search
ever shows up in profiles). Cost: the snapshot allocates and fills
4 bytes per snapshotted slot, virgin tails included — collector-side,
transient, two orders below the win.

## 2026-07-28 — the batched checkpoint splits: ack before the run, pickup after it

The lowering contract for a release run changes (rfc `3faf110`,
`model/gc/rc-walk.md` "Batched releases"): one `ll_gc_checkpoint_ack`
(new ABI) fronts the run — the activity bit is observed before any
free, as on the death branch — and the full `ll_gc_checkpoint` trails
it. `ll_release_vector` carries the same split. **Why:** a pre-run
pickup judges posted components while the run's transients are still
counted — a scope-exit loop then presents every pickup with the same
held borrow, the phase-lock that would defeat the forced verdict.
Rejected: keeping the single pre-run checkpoint (the phase-lock),
pickup on both sides (a second full test buys nothing after eager
death — any death in the run already picks up at its dispose's exit).
The synchronous collection also joins the pickup gate (`walk_active`
beside mid-drain and teardown depth): it is drain-class and holds
guards a message may name. Cost: none measured (`BENCHMARKS.md`
2026-07-28); four regressions pin the shape — the ack-only front, the
ack-before-first-death position, the vector phase-lock, the walk-active
gate — each verified failing.

Edmond's redesign (rfc `c2f91b1`, `model/gc/rc-walk.md`). A release
reaching zero mid-epoch now runs full teardown at the natural point —
`__destruct` on the owner thread, weak notify, sever, free — with only
the memory parked. Deleted wholesale: the F5 deferral branch, the
deferred-death marker, the shared condemned byte (bits 24–31 return to
the free pool), `collector_condemn`, `acquit_condemned` and the
acquittal message kind, `Epoch::drop`'s owed acquittals. Condemnation
is collector-private (the candidate list); acquittals are dropped in
private; only confirmations post. `drain_confirmed` opens with the
**corpse rule**: any member reading `rc 0` drops the message whole
before a field is traced or a guard written — DC0 closed by refusing
the message instead of preventing the corpse.

Two pre-existing BLOCKERs surfaced by the adversarial review of the
amendment, fixed in the same change (causally dependent — the eager
path makes both universal):

- **The death-branch checkpoint acks only** (`epoch::checkpoint_ack`);
  message pickup and the parked flush ride the outermost dispose's
  exit (`teardown_enter/exit`, bracketed in `ll_object_die` /
  `ll_entity_die`) and are refused mid-teardown by the full
  checkpoint. Between the committing zero store and dispose the dying
  entity's weak cell is live, and a drain destructor's
  `WeakRef::get()` returned a strong reference to the corpse
  (regression `the_drain_never_sees_an_entity_between_commit_and_
  dispose`, verified failing on the old order).
- **Parking is out-of-band** (`deferred_free` keeps a thread-local
  vector; a parked slot is never written until the flush). The old
  in-slot park link overwrote the class word at bytes 8–15 under the
  walker, which dereferences `+8` one pass after reading the header
  (regression `parking_leaves_the_corpse_bytes_intact`, verified
  failing on the in-slot write). Cost: the park path may allocate —
  cold, epoch-only. Flush frees in reverse park order to keep the
  LIFO free-list behaviour.
- `Epoch::drop` now **waits for posted confirmations** before
  releasing the deferral window (two epochs' verdicts must never be
  in flight).

**Why:** the deferral traded destructor timeliness — the one
userland-visible semantic, and design principle 1 — for drain
simplicity; the parked slot already guarantees corpse identity, so
refusing the message is as safe as preventing the corpse, and the
mutator's death path drops its last collector test. **Cost:** a
component that partially dies between posting and drain waits an
epoch for its survivors; the rfc's TLA+ battery models the
pre-amendment protocol until re-derived (banner notes in the rfc).

## 2026-07-27 — the drain goes through the relaxed header helpers; the exclusivity window is proven separately

**Decided:** every header access in the verdict drains
(`drain_confirmed` / `acquit_condemned` / `exact_test`) uses the
relaxed helpers (`update_header_flags`, `mutator_guard_retain`, the new
`header_refcount` twin), although the drain window is provably free of
collector interference. The proof — three links: post follows the last
read (queue mutex), no return after post, ack follows the drain with
Release/Acquire on the close gate — lives in `rfc/model/gc/`
`drain-window.md` with a dedicated TLC spec (`DrainWindow.tla`: sound
run exhausts in 23 states; three kill variants, one per link, each
violates the invariant). **Why:** the plain accesses were flagged by an
independent review as violating the crate's own rule; the soundness
argument was real but written nowhere, which is a trap for every future
reader. **Rejected:** keeping plain accesses and documenting the
exception — the relaxed forms are the same x86 instructions, so an
absolute rule costs nothing. **Cost:** none (cold path, identical
codegen).

## 2026-07-27 — undef stamping is the factory's, from descriptor `undef_runs` (A5)

**Decided:** the class descriptor carries `undef_runs` — the Box slots
declared without a default, as `(offset, count)` runs, and the generic
factory stamps `VALUE_UNDEF` over them after the zero-fill (a
compiler-known `new` site emits the same stores straight-line). The
layout regroups a class's defaultless Boxes behind its defaulted ones,
so the undef run is one contiguous tail of the box trace run.
**Why:** an all-zero Box is `null`, not undefined, so somebody must
stamp; the out-of-line factory is the dynamic-class path and can only
read the descriptor. **Rejected:** a per-`PropSlot` flag — the factory
would scan every property to find the few defaultless ones; runs make
the stamp a stride, the crate's existing trace-map idiom.
**Cost:** physical order diverges further from declaration order
(already true of the run grouping; `declaration_index` carries the
observable order). Construction is the field's only consumer.

## 2026-07-27 — bulk object operations: vector release and cell reservation (rfc bulk-operations.md)

**Decided:** two compiler-facing ABI groups. `ll_release_vector` — one
call per release batch, checkpoint served once at entry, destructors
in vector order (physical recycling order stays the manager's).
`ll_entity_reserve` / `ll_entity_cells_return` + `ll_object_new_in` —
best-effort-contiguous runs of GcHeap entity cells, keyed by size
class, consumed by a construct-into-cell factory. The parameters are a
request; the manager decides — it may refuse or serve short, and v1
policy is tame: blocks on hand first (virgin bump tail, then free
list), at most one pool draw, never region growth. **Why (Edmond):**
no per-object ABI call on statically-known batches, and deliberate
co-location of object graphs. **Rejected:** an arena reservation ABI —
one bump allocation of `count x size` already is the contiguous run
there. **Cost:** reservation flips the tail-first order (ordinary
alloc pops the free list first) — reserved runs prefer virgin memory;
contiguity is best effort in every category and never spans blocks.

## 2026-07-27 — the narrow mutator: hot paths store only the counter half; the condemned byte is three-valued

**Decided:** rc-walk's `ll_retain` and non-final `ll_release` store
only the 4-byte refcount half — no flags store, no condemned-byte
clear (rfc, "The narrow mutator"). The F5 branch mints a
deferred-death marker (byte value 2); the acquittal duties and the
exact test discriminate by it. `Epoch::drop` posts acquittals for
condemned-but-unposted components. **Why:** measurement showed the
clear-on-touch filter forced a whole-word RMW chain onto every counted
operation (+0.5–0.6 ns per retain/release pair); the filter was never
the safety gate — the Phase 4 exact test is. Adversarial review
(Fable, fresh context): SOUND WITH CONDITIONS; both conditions are in
this change, one of them fixing a pre-existing defect — the acquittal
duties keyed on `rc == 0` alone and could tear a corpse that died
ordinarily before its condemnation landed (parked slot, class word
already a park link; regression test seen failing at `object.rs:486`).
**Rejected:** collector-side byte clears (the zombie-minting race,
already rejected in the rfc); keeping the hot-path clear for its one
epoch of earlier acquittal on transient borrows. **Cost:** stale
verdicts survive to Phase 4 and are dropped there (cold);
transiently-borrowed garbage rings die one epoch earlier (observable
in destructor timing); mixed-size atomics remain Miri-invisible, as
before.

## 2026-07-27 — the epoch checkpoint rides the death branch of ll_release, not the entity factory; batched releases pay it once

**Decided:** `epoch::checkpoint()` moves from the end of
`heap::entity_alloc` to the `1 → 0` branch of `ll_release`, entered
after the release's own header store and before any teardown. New ABI
pair for lowering: `ll_gc_checkpoint` (serve the protocol now) +
`ll_release_batch` (`ll_release` minus the test), so a compiler-emitted
run of releases pays one checkpoint test instead of one per death.
**Why (Edmond):** the factory taxed every object creation with a test
that has nothing to do with creation; death is the one event the
protocol cares about — only deaths recycle memory — and the death
branch is already cold and expensive, so the test drowns there. Every
fast path (alloc, free, factory, non-final release) now carries
nothing. **Rejected:** ack riding `ll_free`'s existing parking branch —
drains and flushes cannot live inside free (free-in-free reentrancy),
and splitting the three duties across two sites bought nothing over
one cold home. **Cost:** epoch liveness is now tied to deaths — a
workload with no entity deaths starves the epoch until the poll
(finding F2 reshaped, recorded in the rfc); `ll_release`'s death
branch grew the test, unmeasured like its predecessor.

## 2026-07-27 — rc-walk is the default build; rc-trace moves behind --no-default-features

**Decided:** `default = ["rc-walk"]` in `Cargo.toml`. rc-walk is the
primary GC strategy (Edmond, 2026-07-27); the default build must be
the one that ships. **Why:** the opt-in feature made rc-trace look
primary and every plain `cargo` command exercised the secondary
collector. **Rejected:** keeping rc-walk opt-in until its collector-side
economics are measured — measurement gates step 5, not the default.
**Cost:** plain `cargo bench` now measures the rc-walk configuration —
existing baselines in `benches/RESULTS.md` were taken on rc-trace-default
builds and comparisons must name the configuration; Cargo feature
unification now switches any workspace consumer to rc-walk unless it
opts out.

## 2026-07-27 — weak references: the cell is the canonical WeakRef entity; one table row, not a subscriber list yet (rc-walk step 4)

**Decided:** `src/weak.rs` implements `rfc/model/weak-references.md`
as designed — no separate side entry, per-thread table, notification
at every death site — with one narrowing: the table row is a single
canonical-cell pointer, not the design's tagged subscriber list. The
list exists for `WeakMap`, which the crate cannot represent yet;
building the enum for one live variant is abstraction ahead of its
third case. The row widens when maps land; nothing else changes.

**Also decided here:** the weak walk hangs off arena reset in *both*
reset paths — `Arena::reset_with` (bare mechanics) and
`promote::arena_reset_full` — because a skipped walk is not a missing
feature but a dangling cell; the memory layer calling `weak` is the
same peer-service shape as its existing `ll_release` call. And
`promote::die` now dispatches through `ll_entity_die` (it read the
class pointer of any entity — wrong for kinds 3/5, which have none;
a released reference box in the release log leaked its Value).

**Rejected:** a `Subscriber` enum now (one variant, no consumer);
notification from the collector thread (the table would need locks —
every site already runs on the owning thread).

**Cost:** none measured; the hot paths gain one masked test on an
already-loaded flags word, teardown only.

**Decided:** under `rc-walk`, every mutator store a free-running
collector can race compiles as a relaxed atomic: object-field stores
(`barrier::write_ptr_slot` / `write_value_slot`, used by the store
micro-ops, sever and the reference box), header flag writes and reads
on GcHeap teardown paths (`mutator_load_header` /
`mutator_update_flags` / the guard pair in `refcount.rs`), and block
`kind` stores (`block_pool::store_block_kind`, release — pairing with
the snapshot's acquire, so a block reading "entity" has its class and
zeroed slots visible; kind is now published last at commissioning and
in the large-allocation headers). Field *reads* stay plain: the
collector writes nothing but the two header bytes.

**The one exception is the bump cursor — resolved by not reading it.**
An atomic `bump += 1` measured **+14% on larson** (isolated by
reverting exactly that line; `dev/BENCHMARKS.md`). Instead the
snapshot takes no cursor: commissioning zeroes every entity-slot
header, so the walker scans whole blocks and virgin slots skip on the
occupancy test. Collector-side work for mutator-side zero — the
design's own trade, and the rfc carries the amendment.

**A soundness hole found and closed while sweeping:** the dispose
guard's transient `rc 0 → 1 → 0` bypassed the F5 rule with plain
arithmetic — an entity condemned *while its own destructor ran* would
finish teardown under the verdict, and the drain would later tear a
freed slot. The un-guard is now condemned-aware
(`mutator_unguard_release`): reaching zero under the byte defers the
rest of teardown to the drain (fields intact, `DESTRUCTOR_RAN` set —
torn exactly once there). With that, the deferred-death store keeps
the byte SET as the drain's marker, so the acquittal duties can tell
a deferred death (tear it) from a slot that died ordinarily after a
touch and was freed (leave it) — `acquit_condemned` snapshots the
deferred set before clearing bytes.

**Known limits, accepted and on record:** collector byte stores
against mutator word stores are mixed-size atomics — sound on x86-64
and AArch64, unrepresentable to Miri, so the free-running stress test
is Miri-ignored while every stepped test keeps Miri coverage; a
mutator thread must not exit while an epoch is in flight (entity-block
retirement is between-epochs only — actors revisit).

---

## 2026-07-26 — the collector is a steppable state machine; edge validation is one map lookup (rc-walk step 3, commit 4)

**Decided:** the collector side (`src/collector.rs`) exposes each epoch
phase as a separately callable step (open → snapshot → walk → judge →
condemn → recheck_and_post → close), with `run_epoch` chaining them
behind spin-yield waits for the threaded shape. Stepping is what the
danger-case forcing harness needs (`rc-walk-review.md` layer 3: "a
collector single-stepped between walk, condemn and check") — and it
makes the deterministic tests single-threaded, so no data race executes
and Miri's verdict is meaningful. Phase 2 (`garbage_components`) was
extracted from `collect_cycles` and is shared array math.

**Child validation collapsed to a row lookup:** the design's occupancy /
slot-boundary / epoch-byte checks exist so the walker neither
dereferences a racy child pointer nor records an edge into a skipped
row. Recording edges as *indices into the walked-row map* achieves both
in one lookup — a child that maps to no row (immature, reused,
non-GcHeap, or garbage bytes) is dropped with a counter, and no child
is ever dereferenced at all. The A8 clause holds by construction.

**Mature-only class chase:** the walker reads a class pointer at `+8`
only for entities stamped in an *older* epoch. The factory's
header-last publish is relaxed, so a fresh entity's class store has no
ordering guarantee — but a mature entity's publish is separated from
this epoch's reads by at least one handshake, which is the fence. The
new/current classification is therefore also the memory-ordering guard.

**On record as owed (commit 5):** a free-running mutator still issues
*plain* stores — object fields (barrier micro-ops, sever), header flag
bits (`DESTRUCTOR_RAN`, drain guards), block cursors — that formally
race the collector's atomic reads. The stepped tests never execute
those races; the concurrent stress tests of commit 5 require the
relaxed-atomic sweep of those mutator sites first, with the hot-path
codegen re-measured (`bump += 1` and the barrier stores are
benchmarked paths).

---

## 2026-07-26 — checkpoints ride the factory allocation and the poll; the drain is one thread-local bit (rc-walk step 3, commit 3)

**Decided:** the rc-walk checkpoint (handshake ack + verdict drain,
`src/epoch.rs`) is tested at the **end of `entity_alloc`** and in
**`ll_gc_maybe_collect`** — not in raw `ll_malloc`. Two relaxed loads
and a predicted branch, taken only when the collector wants attention.
The verdict queue is a mutex (cold, per-epoch trickle — the 2026-07-20
cold-lock rule). Drain non-reentrancy is one thread-local bit: a
nested allocator entry from a draining destructor serves memory and
acks a handshake, but never picks up a message (finding F8).

**Why the placement:** the design puts checkpoints inside the memory
manager, not compiler polls — but inside `Heap::alloc` a `&mut Heap`
is live, and a draining destructor that allocates would re-enter the
heap through a second `&mut`: undefined behaviour, not just a design
hazard. At the end of the free function `entity_alloc` the borrow is
dead and reentrancy is plain recursion. Raw `ll_malloc` stays
checkpoint-free: it is the benchmarked C-ABI hot path, and a
buffer-only workload delays the epoch no worse than the accepted
no-allocation limit (F2).

**Per-message drain vs the batch:** `walk::drain_confirmed` processes
one component alone; that is sound only because a destructor's release
into a *different* condemned component stops at that component's
condemned byte. The synchronous `collect_cycles` keeps its batch-guard
umbrella — there no bytes are set, and a sibling component would be
unprotected. Two flows, each documented with its own invariant.

**Acquittal duties order:** clear every member's condemned byte
*first*, then tear the deferred deaths from a pre-snapshotted set —
a teardown can release another member to zero (byte must already be
clear, or the death defers to a drain that never comes), and it can
free a live member (so counts are never re-read mid-loop). The
confirmed path clears bytes before its guards for the same reason:
its own un-guards must reach real deaths.

---

## 2026-07-26 — deferred free parks through the dead memory itself (rc-walk step 3, commit 2)

**Decided:** the GC activity bit is one global `AtomicBool` tested with
a relaxed load in `ll_free` after the kind dispatch; a parked
allocation threads a **thread-local intrusive list through its own
bytes 8–15**, and the owning thread flushes through the real free
between epochs (`src/memory/deferred_free.rs`). All four freeable
kinds park — entity slots, raw heap buffers, pooled large, OS-direct
runs — not just the two the walker chases today.

**Why bytes 8–15:** parking must not allocate (a free path with a
growth failure mode is a new defect class, and a `Vec` push is exactly
that), the memory is dead and ≥ 16 bytes in every freeable kind, and
bytes 0–7 of an entity slot must keep the final refcount-0 header —
the walker's occupancy test. Same choice the in-slot free list already
made, same cache line.

**Why all four kinds:** the walker chases array storage once Phase C
lands, and storage can be any size; parking large/huge now costs
nothing and removes a future correctness edge. Not parked: no-op kinds
(arena, retained — nothing recycles, identity holds) and cross-thread
`free_foreign` (single-mutator crate; actors reopen it — on record).

**Owed to commit 4:** the collector must publish the bit through a
handshake **before** snapshotting (a mutator that has not observed the
bit can still recycle a snapshotted slot), and the flush runs on the
owning thread after the epoch closes.

---

## 2026-07-26 — GC strategy is a build-time cargo feature; rc-walk claims the flags top half (rc-walk step 3, commit 1)

**Decided:** the `rc-walk` cargo feature selects the rc-walk collector
in place of rc-trace. Under it: flags bits 16–23 are the **epoch byte**
(header byte 6) and 24–31 the **condemned byte** (header byte 7);
every `ll_retain`/`ll_release` clears the condemned byte with one mask
on the header word it already loads and stores; a release reaching zero
on a condemned entity **skips teardown** and leaves the death to the
Phase 4 drain (the F5 rule); `ll_release` loses the candidate-buffering
tail entirely. All header accesses on this path compile as relaxed
atomics on the whole 8-byte word, and `RcHeader` is now `align(8)` —
the factory always published it as one 8-byte store, the attribute
makes the requirement explicit (the pinned layout test moved 4 → 8).
Header publication is centralized in `refcount::publish_header`.

**Why:** the two strategies claim the same bits — rc-trace's candidate
index is bits 15–31 — and rc-walk's mask on every retain/release would
corrupt a live index, so coexistence in one binary is impossible; the
rfc makes strategy selection build-time for exactly this kind of reason
(`rfc/model/gc/strategies.md`). Runtime switching was considered and
rejected: a per-operation branch in retain/release is the cost the
rc-walk design exists to avoid, and a shared layout would cost rc-trace
its O(1) candidate forget.

**Evidence (release asm, x86-64):** rc-walk `ll_retain` is one 8-byte
load + mask/inc + one 8-byte store, no lock prefix, no RMW; `ll_release`
likewise, with the condemned test two `sete` on the already-loaded word
and **no call tail** (rc-trace's buffering call is gone — the design's
advertised net reduction). The default configuration's code is
unchanged. Verification now runs both configurations
(`dev/WORKFLOW.md`); rc-trace's tests are gated to the default
configuration, where they keep running.

---

## 2026-07-26 — entity blocks are a second heap population (rc-walk step 1)

**Decided:** GC entities (`GcHeap`/`LongLived` factory allocations) come
from their own block population — `BLOCK_KIND_ENTITY`, served by a second
`Heap` instance per thread — while raw C-ABI allocations keep
`BLOCK_KIND_HEAP`. Four rules make the population walkable
(`rfc/model/gc/rc-walk.md`): the in-slot free-list link moves to slot
bytes 8–15 (bytes 0–7 keep the dead entity's final refcount-0 header —
the occupancy test), commissioning zeroes every slot's first 8 bytes,
the factory publishes the header last as one 8-byte store, and the block
pool records every region base in an append-only registry so blocks can
be enumerated. Abandoned-block lists are segregated per population.

**Why:** a walker cannot tell a live 40-byte object from a live 40-byte
C buffer in a shared block, and reading the buffer's first 8 bytes as a
header is a wild read. Design and proofs live in the rfc repo (rc-walk
spec, model, 22-run TLC battery).

**Considered and rejected:** an in-header FREE stamp (dies with the
first free once the link lived in bytes 0–7 — retired in the spec);
moving the link only for entity blocks (two code paths for no gain: the
new offset is the same cache line, and every class is ≥ 16 bytes).

**Cost:** one 8-byte zeroing store per slot per entity-block
commissioning (cold, once per block); a second `Heap` per thread
(~1.2 KB); huge entities (> 8 KB) stay outside the walk, conservatively.

---

## 2026-07-25 — a generated lifecycle body unrolls small, loops large

**Decided:** the counted-field strides of a compiler-generated lifecycle
operation — `dispose`'s releases, the retain strides of `clone` /
`deep_clone` / `thread_*`, and `factory`'s non-zero-default stamps — are
emitted unrolled for a class with few counted fields, and as a loop over
`traced_runs` once the field count crosses a threshold. The RFC's
"straight-line, release slot 1, release slot 2, …" is the small-class
shape, not an absolute (`rfc/model/classes.md`, "Generated body shape";
`lowering.md`, Allocation).

**Why:** these bodies run once per object, so unrolled path length wins
for the common handful-of-fields class — no loop counter, no map load, and
the releases cancel against matching retains under the ARC optimizer. But
code size is paid per class *per operation*; a class with dozens of
counted fields would emit those dozens in `dispose`, again in `clone`,
again in each `thread_*`, bloating the icache for no gain. Past the
threshold the loop reads the same `traced_runs` the GC already strides, so
a large class needs no second teardown map — the trace map serves both the
inline data consumer (GC) and the looped code consumer (`dispose`).

**Not fixed by the model:** the crossover field count is a codegen tuning
parameter, calibrated against real workloads like every other size
threshold here (order of a few tens of fields). The model fixes the two
shapes and the rule that picks between them, not the number.

**Bearing on this crate:** `dispose` is generated code, so the threshold
lives in the compiler, not in `ll-model`. Here it only means A1's generic
runtime `ll_object_die` is a temporary stand-in: A3 replaces it with the
descriptor's `dispose` pointer, and the crate's tests supply hand-written
`dispose` functions that model the generated small-class (unrolled) form.
The GC trace keeps striding `traced_runs` as data regardless.

---

## 2026-07-21 — the store barrier is funded, not checked

**Decided:** two blocks per thread are held back for arena log growth
(`memory::reserve`). `grow_log` draws on them when the pool refuses; the
draw sets a flag that `ll_gc_maybe_collect` — the compiler's safepoint
poll — refills on. A reserve block is linked into the arena's block list
so reset returns it, but it **never becomes the arena's bump block**.

**Why:** `ll_ref_store` has no channel and must not grow one — a check
after every reference store is the Zend shape this runtime rejects. But
it can fail, because recording an escape grows a log. The reserve does
not make failure impossible, it *moves* it: the barrier keeps working
from the reserve, and the next poll, which runs in a frame that can
raise, turns the shortage into an ordinary memory-exhausted exception —
thousands of records before the reserve would run dry.

**Considered and rejected:** intrusive log links in the entities
themselves, which would need no reserve at all. It fails on
release-at-reset (one heap entity gets one record per store, in several
arenas' logs) and would cost 8 bytes in every arena object's header —
paying permanently, per entity, to remove a reserve that costs two
blocks per thread.

**Cost:** the arithmetic depends on a compiler contract that does not
exist yet — a bounded number of barrier operations between two polls.
Until a compiler emits polls, the refill only happens where something
calls `ll_gc_maybe_collect`, so in this crate the reserve is a
mechanism with its trigger stubbed by tests.

---

## 2026-07-21 — the barrier owns the whole slot, and publishes it first

**Decided:** `ref_store` takes `slot: *mut Value` and `new: Value`, and
writes the whole 16-byte value **before** releasing the displaced one.
`ll_ref_store` changes shape with it.

**Why:** two defects that turn out to be one. Releasing first lets a
`__destruct` collect while the slot still points at the value being torn
down (audit C1); publishing first removes that edge. But publishing only
the payload word — which is all the barrier used to write, leaving every
call site to stamp the tag — makes the slot readable while torn, and
"tag says object, pointer is null" is a crash rather than a
miscount. One slot has one writer.

**Considered and rejected:** keeping the split write and having the
collector tolerate a torn slot. That spreads the invariant to every
future reader instead of removing it, and the reader that forgets is a
crash under memory pressure.

**Cost:** an ABI change, taken now because no generated code exists yet.
The `Value` travels by value into an `extern "C"` function, which on
Windows x64 means a pointer to a caller copy; unmeasured, and recorded
here rather than in `BENCHMARKS.md` for that reason.

---

## 2026-07-21 — a destructor is owed by the constructor, not by the factory

**Decided:** creation is two steps. `ll_object_new` is the factory: it
allocates and stamps the header, nothing more. `ll_object_constructed`
runs after the user constructor returns successfully — it sets
`DESTRUCTOR_PENDING` on the header (named `HAS_DESTRUCTOR` at the time;
renamed in the 2026-07-22 flags compaction) and registers the arena log
record.
Teardown dispatches on that header flag, never on the class.

**Why:** a constructor that throws must not get its `__destruct`
(`rfc/runtime/object-lifecycle.md`). Registering in the factory leaves a
record demanding exactly the forbidden call, for exactly the objects
forbidden to have it. Dispatching on the class does the same on the
refcount path.

**Considered and rejected:** a separate "constructed" bit. The header
flag already meant "this object owes a destructor" everywhere it was
read; making it mean that literally costs nothing and removes a bit.

**Cost:** generated code must emit the second call. A class with no
destructor needs no call at all, so the cost lands only where the
guarantee exists.

---

## 2026-07-21 — a refused destructor record fails the creation

**Decided:** `Arena::track_destructor` returns false instead of
aborting; the creation that asked for it raises memory-exhausted. The
other three arena logs keep the abort, moved from `grow_log` to each
caller so the reason can be stated where it applies.

**Why:** a lost escapee or release record dangles or leaks — there is
nothing safe to continue into. A lost destructor record only skips a
side effect, and there is a better answer available: fail the creation,
which lands on the already-specified path for a constructor that threw.
Nothing is silently skipped, because an object whose registration failed
does not survive its own creation.

**Cost:** the aborts are still there, now three instead of one. They are
placeholders for the reserve (`rfc/runtime/exceptions.md`, "The log
reserve protocol"), which is not built.

---

## 2026-07-20 — the arena handle is a raw pointer, not `&mut Arena`

**Decided:** `resolve`, `resolve_arena`, `ref_store`, `escape_gain` and
`arena_reset_full` all take `*mut Arena`. A `&mut` is materialized for
one leaf operation at a time, and the rule is: **no borrow may be live
across a call that can run user code.**

**Why:** destructors are documented to run reentrantly on the reset
path. `arena_reset_full` held a `&mut Arena` across its settling loop
while a `__destruct` it invoked resolved the same arena — two live
`&mut` to one object. That is UB regardless of the machine code being
correct, and the reset path is exactly where the optimizer is most free
to assume otherwise.

**Considered and rejected:** keeping `&mut Arena` and relying on
discipline about when it is held. It had already been violated in the
one place it mattered, and nothing would catch the next violation.

**Cost:** the borrow checker no longer helps on the arena API. Misuse
is now caught by Miri, or not at all. The reset's release drain also
had to become collect-then-release, because it used to run `die` inside
the drain closure while the drain held the arena.

---

## 2026-07-20 — trailing inline data is reached through raw pointers

**Decided:** `Object::prop_at`, `Class::vtbl` and `LLString::bytes`
take a raw pointer spanning the whole allocation instead of `&self`.
Entity pointers keep whole-allocation provenance rather than being
narrowed through `&mut RcHeader` (`ll_release` now buffers its original
argument, not the reborrow).

**Why:** all three types put inline data past a fixed header. A
reference carries provenance over the header only, so every access to
the trailing data was outside the reference it was derived from.

**Considered and rejected:** leaving it, on the grounds that it works.
It is also what stops Miri before it reaches anything else, so leaving
it meant giving up the only tool that can see this class of defect.

**Cost:** these accessors are no longer methods, so call sites are
slightly noisier.

---

## 2026-07-20 — the block header is split by access rule, not by topic

**Decided:** `HeapBlockHeader` is four `repr(C)` structs laid back to
back. Line 0 holds `BlockPrivate` (counters, free list, `available`
links) together with `BlockShared { owner }`; line 1 holds
`BlockRemote { remote_free }` alone; line 2 holds the cold
`BlockLinks`. The owner borrows `&mut (*block).private` and cannot name
the shared half.

**Why:** `&mut HeapBlockHeader` claimed exclusivity over two atomics
that every thread reaches by design, so the owner's borrow raced every
non-owner's legitimate read. The audit filed this as a non-owner
problem; Miri showed the owner is equally implicated, so no amount of
care on the non-owner path could fix it. Making it a type rule was the
only option that cannot be violated again — it had already been
violated twice, in `adopt` and in `free`.

**Considered and rejected:** grouping both atomics into one isolated
"shared" struct. `owner` and `remote_free` have opposite access
profiles — `owner` is read on every `free` and written twice in a
block's life, `remote_free` is CASed by other threads — so isolating
them together evicted a hot field and measured slower. Two further
layouts were measured and rejected; see `BENCHMARKS.md`.

**Cost:** none measured — faster on both benchmarks (larson −3.03%,
rptest −1.54%), and the header still fits the block's reserved 256-byte
line, so slots-per-block is unchanged. The price is conceptual: the
layout is now load-bearing, and the pinning test exists to say so.

---

## 2026-07-20 — cold concurrent structures take a lock, not a CAS loop

**Decided:** the block pool's free chain moved from a lock-free Treiber
stack to a `Mutex`, matching the abandoned-block list. The standing
rule: a structure whose users are all cold does not get a lock-free
implementation here.

**Why:** `pop_global` read a node's `next` non-atomically while another
thread could already own that block and be writing the same bytes
through a different view of the tagged-union header. Making `next`
atomic would not have fixed it — the racing write is the owner's, on
the allocation hot path, and making *that* atomic to serve a cold path
is the wrong trade. The race had to become impossible rather than
atomic.

**Considered and rejected:** giving the pool a dedicated atomic link
field at an offset no owner view touches, keeping the stack lock-free.
It works, but adds another layout invariant to a union five modules
share, and keeps the ABA tag.

**Cost:** none measured (see `BENCHMARKS.md`). A per-thread cache
already fronts the chain and refills in batches, so the lock is taken
rarely. The ABA tag is gone, which also retires the audit's concern
about its width.

---

## 2026-07-20 — Miri runs against a UNIX target

**Decided:** the Miri suite runs with
`--target x86_64-unknown-linux-gnu`, and the bench-only `mimalloc`
dev-dependency is gated behind `cfg(not(miri))`.

**Why:** the Windows TLS fast path is inline `asm!`, which Miri cannot
execute at all. The crate already has a portable `thread_local!` path
for non-Windows targets, so pointing Miri at a UNIX target selects it
with no source change.

**Considered and rejected:** adding a `cfg(miri)` branch to the TLS
module. It would have put a Miri-shaped concern into the hot path for
no gain over choosing the target.

**Cost:** Miri never exercises the Windows TLS path, which is the one
that actually ships here. `-Zmiri-ignore-leaks` is also required, so
Miri is blind to the leak-shaped findings.
