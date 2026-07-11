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
- [ ] **Category / write barrier + remembered set + release-at-reset
  list** — mechanics landed (`barrier.rs`: `ll_ref_store(ctx, owner,
  slot, old, new)` with RC ops + category compare; in-arena segment
  logs for remembered set and release list; reset performs one release
  per release-log record and hands remembered-set slots to a callback;
  `blocks`/`destructors`/`larges` migrated off Rust `Vec`s). Remaining:
  the reset-time consumer — validation of remembered-set entries, the
  per-block retain/evacuate/free decision (`arena-reset.md`), category
  bit rewriting for retained blocks, and the destructor↔escape fixpoint
  (reset already loops both logs; the escaped-objects-skip-destructors
  half needs the object model). Blocked in part on object-layout
  metadata (tracing children needs `prop_layout`).
- [ ] **GC strategy contract first** — fix the 4-interface contract
  (`ll_ref_store` slot, safepoint poll, `GcHeap`-only allocator, object
  metadata/teardown hooks) as a Rust trait before writing any concrete
  strategy. MMTK must plug in as just another implementation
  (`mmtk:<plan>`), never architecturally special-cased. Then implement
  `rc-trace` (the default) as the first concrete strategy.
- [ ] **Allocation telemetry / debug mode** — two layers: (1) cheap
  aggregate stats (allocated / active / resident, mimalloc/jemalloc
  style), always-on candidate; (2) opt-in full event log (heaptrack
  style: alloc timestamp, free timestamp → lifetime, object type,
  deduplicated call site via the existing `LLAllocSite`). Design pass
  needed on hooking into arena/heap/immortal/buffer/GC paths without
  taxing non-debug builds.
- [ ] **Tests, including performance benchmarks** — correctness tests per
  existing style (`test_guard`, scenario-per-test) for all of the above;
  criterion benchmarks following the project's honest-methodology
  pattern (`benches/`, `RESULTS.md`). Synthetic benches (buffer-reclaim
  cache-line analysis) can run now; workload-shaped validation is
  blocked below.
- [ ] **BLOCKED on vertical slice: threshold calibration** — the
  per-block dense/sparse threshold (`arena-reset.md`), memory-pressure
  mode thresholds and the critical-mode search bound *K*
  (`buffers.md`). What signal drives mode transitions, hysteresis to
  avoid flapping, static vs adaptive thresholds — all of it needs real
  workloads, which need the slice. Do not design further on paper.

## Object model (classes.md, already updated in rfc)

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

## Not yet started at all

- Array hashtable design (bucket layout, collision strategy) —
  `rfc/model/arrays.md` still calls this a future document.
- Execution pipeline RFC ("the big one" per `rfc/BACKLOG.md`): parser,
  own IR vs straight to LLVM, AOT/JIT split, autoloading in a compiled
  world. Everything in `model/` assumes this compiler exists. The
  vertical slice (top of this plan) forces its minimal subset early.
- Exceptions, Closures, Enums, Generators/Fibers, Resources, Generics,
  actors, stdlib, I/O — all listed in `rfc/BACKLOG.md`, untouched.
