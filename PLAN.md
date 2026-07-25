# Plan

Implementation plan for `ll-model`, re-sorted 2026-07-24 against the RFC
after the 2026-07-22 object-layout redesign landed in the `rfc` repo.

Design lives in `rfc` and is authoritative — read before coding, do not
re-derive: `model/classes.md`, `model/values.md`, `model/lowering.md`,
`model/gc/strategies.md`, `model/gc/satb.md`, `model/memory/ffi.md`,
`runtime/object-lifecycle.md`.

## Status snapshot (2026-07-24, HEAD `bad9bd6`)

Done, per RFC:

- **Memory manager**, end to end: arenas, the reset/promote fixpoint,
  immortal region, buffers / buffer-arena, block pool, the Immix-shaped
  heap, the store-barrier log reserve, the store barrier + remembered set
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
- [ ] **A2. Entity kinds + bare-pointer teardown switch** — actually
  produce non-object kinds (string / array / reference / Box / WeakRef /
  lazy) and switch `die` on the kind field; today `die` handles objects
  only. Strings and arrays are non-objects (no class pointer).
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
- [ ] **A5. `VALUE_UNDEF` semantics + `WRITING` lock bit** — uninitialized
  state per `values.md`; the `WRITING` Box-flags lock bit for rc-satb
  `store_box` (torn-16-byte-read fix, C3). `rfc/model/values.md`,
  `gc/satb.md`.
- [ ] **A6. Static-block teardown at thread exit** — per-thread registry,
  walked LIFO, each ref slot dropped via the barrier's `drop`; counterpart
  of the static initializer. Closes audit H3. `rfc/model/classes.md`
  "Teardown at thread exit".
- [ ] **A7. No zeroing by default** — the factory decides which slots need
  a defined initial state (`rfc/BACKLOG.md` deferred-optimizations).

### Phase B — GC completeness (tails deferred in the old plan)

- [ ] Immix-shaped `GcHeap` allocator → line recycling of retained blocks
  + sparse-block evacuation at reset (`arena-reset.md`).
- [ ] Run `__destruct` of cyclically-dead objects (Zend-style re-scan
  discipline); today the cycle collector skips it.
- [ ] `rc-satb` as a second build-time GC strategy (needs the `WRITING`
  bit from A5). `rfc/model/gc/satb.md`.

### Phase C — new subsystems (not started; each its own RFC + code)

- [ ] Strings — string-as-class, interpolated-template class
  (`rfc/model/strings.md`). Enabled by A2, needs real implementation.
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
