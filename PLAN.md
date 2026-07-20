# Plan

Working plan tracking what's next after the memory-manager design session
(2026-07-10). Design decisions from this session are recorded in the `rfc`
repo (`model/memory/buffers.md`, `model/classes.md` Lazy Objects,
`model/strings.md`, `model/arrays.md`, `BACKLOG.md`); this file tracks the
implementation/design work items themselves.

Revised after the design-review session (also 2026-07-10), which updated
`rfc`: per-block arena reset (`arena-reset.md`), release-at-reset list
owning all releases (`arenas.md`), `arena-promotion.md` rewritten,
buffer-arena size routing and per-block free lists (`buffers.md`),
actor/GC coordination via mailbox and the message-payload table
(`runtime/actors.md`).

## Unblocker: vertical slice

- [ ] **Vertical slice first** — minimal hello-world through the whole
  stack (PHP → IR → executable) on the simplest memory setup. Promoted
  from "not yet started" to the top: it is the *unblocker*. Every
  calibration item below needs real workloads, which need a running
  compiler; and the design's central bet — that the compiler can prove
  escape/monomorphism/ARC-pairing on real PHP — is unvalidated until
  code runs. Requires the minimal execution-pipeline decisions
  (`rfc/BACKLOG.md` "the big one"), but only the minimal ones.

## Memory manager (ll-model)

- [x] **Immortal region** — bump allocator, no reset, global singleton
  under a `Mutex` (locking needed because JIT class-loading can happen
  concurrently). New `BLOCK_KIND_IMMORTAL`, blocks from the shared
  `BlockPool`, `put()` never called on them. ABI: `ll_immortal_alloc(ctx,
  size)` with `ctx` accepted-but-ignored for uniformity, same as
  `ll_heap_alloc`. No explicit match-arm needed in `ll_free`/`usable_size`
  (falls to the existing no-op default, same as `BLOCK_KIND_ARENA` today);
  bump-style allocation has no per-object size tracking anyway.
- [x] **Dedicated buffer arena (`BLOCK_KIND_BUFFER`)** — done in two
  layers: `buffer.rs` (request-arena path: `Buffer`, `ll_buffer_ensure`,
  extend-in-place / copy growth, OS-direct routing, pressure-mode flag)
  and `buffer_arena.rs` (long-lived: per-block free list, live-chunk
  count, bounded critical-mode search, `ll_buffer_ensure_longlived`).
  Residual: *K* and mode thresholds stay in the blocked item below;
  cross-thread free of long-lived buffers deferred until a consumer
  needs it (heap.rs remote-free is the template). See
  `rfc/model/memory/buffers.md` for the full design, updated this
  session: size routing (payloads > block payload go OS-direct, the
  free-list machinery never sees them), per-block intrusive free list
  (head in the block header, chain never leaves the block), per-block
  live-chunk count returning empty blocks to the pool, no coalescing
  ever (compaction fallback is the defragmentation story). Only applies
  to long-lived buffers; request-arena buffers stay in the plain arena.
- [x] **Category / write barrier + remembered set + release-at-reset
  list** — done end to end. `barrier.rs`: `ll_ref_store(ctx, owner,
  slot, old, new)` with RC ops + category compare; in-arena segment
  logs; `blocks`/`destructors`/`larges` off Rust `Vec`s. `promote.rs`
  (the reset-time consumer, `ll_arena_reset` now routes here):
  destructor↔escape fixpoint (escaped objects skip their destructors;
  destructor-created escapes are caught), conservative subgraph mark,
  deduplicated slot validation, exact refcount initialization for
  survivors (external slots + internal edges + compensating retains
  for held heap entities), category bits rewritten in place, survivor
  blocks kept as `BLOCK_KIND_RETAINED`, release-log teardown dispatch
  via the `ENTITY_OBJECT` flag. Remaining per RFC phasing (additive):
  sparse-block evacuation; line recycling of retained blocks (needs
  the Immix-shaped GcHeap allocator, next item); flags-table extension
  (`ESCAPED`, `ENTITY_OBJECT`) to be reflected in rfc classes.md.
- [x] **GC strategy contract first** — `gc.rs`: the 4-interface
  contract as the `GcStrategy` trait (store hook, safepoint need,
  `GcHeap`-only allocator, metadata consumption), `NoGc`/`PureRc` as
  the trivial impls, `rc-trace` as the first real one: candidate-root
  buffering on non-zero decrements (dedup via the buffered bit,
  forget-on-death), Bacon–Rajan synchronous trial deletion on the
  reserved color bits, threshold-triggered (`ll_gc_collect_cycles`
  ABI). Known limits, logged: `__destruct` of cyclically-dead objects
  is not run (needs Zend-style re-scan discipline); non-object heap
  children of whites will need releasing when strings/arrays exist;
  the heap allocator is the standard path until the Immix-shaped one.
- [ ] **Tests, including performance benchmarks** — correctness tests per
  existing style (`test_guard`, scenario-per-test) for all of the above;
  criterion benchmarks following the project's honest-methodology
  pattern (`benches/`, `RESULTS.md`). Synthetic benches (buffer-reclaim
  cache-line analysis) can run now; workload-shaped validation is
  blocked below.
- [ ] **`stats.rs`'s three tests are racy under load** — they assert deltas
  on the process-global `blocks_out`, but `test_guard` only serialises the
  test bodies. A thread that exits now returns its blocks from a TLS
  destructor (`ll_thread_exit` → `abandon_all` → `BlockPool::put`), outside
  that lock. Reproduces at `--test-threads=16`
  (`arena_lifecycle_is_visible_at_block_granularity`, off by exactly one
  block); passes 12/12 at the default thread count, so it will surface as a
  rare CI flake, not a clean failure.

  **Not a product regression** — returning blocks at thread exit is the fix
  in `rfc/model/memory/heap-slot-allocation.md` (fix 7a) that took larson
  from 1.7 GiB resident to 10 MiB. These tests' isolation had been resting
  on the leak: thread exit used to do nothing at all.

  Fix by releasing a test's heap under the same lock the test holds, rather
  than from a TLS destructor — e.g. have `test_guard`'s guard call
  `ll_thread_exit` on drop. **Do not weaken the assertions to hide it**; a
  global counter that only holds still because nothing returns memory is
  exactly what these tests exist to catch.
- [ ] **BLOCKED on vertical slice: threshold calibration** — the
  per-block dense/sparse threshold (`arena-reset.md`), memory-pressure
  mode thresholds and the critical-mode search bound *K*
  (`buffers.md`). What signal drives mode transitions, hysteresis to
  avoid flapping, static vs adaptive thresholds — all of it needs real
  workloads, which need the slice. Do not design further on paper.

## Object model (classes.md, already updated in rfc)

- [x] **Core object model** — `value.rs` (16-byte Box, tags, refcounted
  bit), `intern.rs` (immortal interned names, pointer equality),
  `class.rs` (descriptor with inline trailing vtable, slot-stable
  inheritance, itables re-linked in subclasses, Cohen display,
  `prop_layout.refcounted_slots()`), `object.rs` (`ll_object_new`,
  three-phase `ll_object_die` with resurrection check,
  `ll_instanceof`). Deliberately absent, per agreed scope: inline
  caches / hooks / dispatch choice (generated-code territory),
  Ghost/Proxy shims, `__call`, dynamic properties (need arrays).

- [ ] **General interception Proxy** — transparent method interception
  on an existing, already-constructed target object, without touching
  the target's class/code. Distinct from the Lazy Proxy (PHP 8.4,
  already designed in `classes.md`). User has ideas on the mechanism;
  not yet discussed in depth. Needs to address: composition with the
  dispatch decision tree, whether it reuses the fat interface-reference
  mechanism, identity semantics (`instanceof`, `spl_object_id`) vs
  Ghost/Lazy Proxy. Now also the prerequisite for **proxy-mediated
  movability** (`rfc/BACKLOG.md`, new this session).
- [ ] **Binary-level class interceptors** — interception at the compiled
  code / ABI level (e.g. vtable-slot patching) for a whole class, not
  via a wrapper object. Check whether this is the same mechanism as the
  already-deferred "optimistic devirtualization with patching on
  subclass load" (CHA-style, `classes.md` Deferred section) or a
  different one.

- [ ] **Allocation telemetry layer 2 / debug mode** — *deliberately last:
  designed, not scheduled.* Layer 1 is done (`memory/stats.rs`):
  always-on aggregate stats with **zero hot-path tax**, block-granular
  by design, `ll_memory_stats` ABI. Layer 2 has its full design pass in
  **`dev/design/debug-modes.md`** — live object registry (what exists,
  of what class, in which memory), per-request leak reporting on the
  Zend MM model, lifetime histograms on a virtual clock, extensible
  per-allocation metadata in shadow memory, integrity checks and fault
  injection, dependency-free metrics export for Prometheus, and a
  parallel debug ABI carrying site id, stack id and arena identity.
  Build order is section 9 there. Its first items need no ABI change and
  no compiler work; the debug ABI needs both and wants its RFC opened
  before anything is built. OS-direct runs stay invisible to layer 1
  until this lands.

## Not yet started at all

- Array hashtable design (bucket layout, collision strategy) —
  `rfc/model/arrays.md` still calls this a future document.
- Execution pipeline RFC ("the big one" per `rfc/BACKLOG.md`): parser,
  own IR vs straight to LLVM, AOT/JIT split, autoloading in a compiled
  world. Everything in `model/` assumes this compiler exists. The
  vertical slice (top of this plan) forces its minimal subset early.
- Exceptions, Closures, Enums, Generators/Fibers, Resources, Generics,
  actors, stdlib, I/O — all listed in `rfc/BACKLOG.md`, untouched.
