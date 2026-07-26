# Decisions

An architecture changelog: what was decided and why, not what changed
in the code. Routine fixes and renames belong to git, not here.

A superseded decision is replaced by a **new entry**; old entries are
never edited or deleted.

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
