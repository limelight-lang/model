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
- [ ] **A6. Static-block teardown at thread exit** — per-thread registry,
  walked LIFO, each ref slot dropped via the barrier's `drop`; counterpart
  of the static initializer. Closes audit H3. `rfc/model/classes.md`
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
