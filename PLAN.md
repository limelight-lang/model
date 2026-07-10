# Plan

Working plan tracking what's next after the memory-manager design session
(2026-07-10). Design decisions from this session are recorded in the `rfc`
repo (`model/memory/buffers.md`, `model/classes.md` Lazy Objects,
`model/strings.md`, `model/arrays.md`, `BACKLOG.md`); this file tracks the
implementation/design work items themselves.

## Memory manager (ll-model)

- [ ] **Immortal region** — bump allocator, no reset, global singleton
  under a `Mutex` (locking needed because JIT class-loading can happen
  concurrently). New `BLOCK_KIND_IMMORTAL`, blocks from the shared
  `BlockPool`, `put()` never called on them. ABI: `ll_immortal_alloc(ctx,
  size)` with `ctx` accepted-but-ignored for uniformity, same as
  `ll_heap_alloc`. No explicit match-arm needed in `ll_free`/`usable_size`
  (falls to the existing no-op default, same as `BLOCK_KIND_ARENA` today);
  bump-style allocation has no per-object size tracking anyway.
- [ ] **Dedicated buffer arena (`BLOCK_KIND_BUFFER`)** — see
  `rfc/model/memory/buffers.md` for the full design (per-category growth,
  memory-pressure modes, LIFO free-list reclaim). Only applies to
  long-lived buffers; request-arena buffers stay in the plain arena.
- [ ] **Category / write barrier + in-arena remembered set** — the
  biggest remaining architectural gap. Static path (compiler-proven
  escape): allocate directly in the target category, no barrier at all.
  Dynamic path: log the slot into the arena's remembered set (a growable
  buffer allocated inside the arena's own bump memory, not a separate
  Rust `Vec`); fate decided lazily at arena death per
  `arena-reset.md`. Also migrate `Arena`'s existing `blocks`/`destructors`
  `Vec`s to the same in-arena scheme. Once escape exists, `arena.rs`'s
  `reset()` must stop running pre-destructors unconditionally — escaped
  objects must be excluded (`arena-reset.md`: "escaped objects are not
  dying and are skipped").
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
- [ ] **Threshold-calibration algorithm** (memory-pressure modes) — not
  just "measure with real workloads": what signal drives mode
  transitions, hysteresis to avoid flapping, static vs adaptive
  thresholds, how the critical-mode search bound *K* interacts with the
  chosen signal.
- [ ] **Tests, including performance benchmarks** — correctness tests per
  existing style (`test_guard`, scenario-per-test) for all of the above;
  criterion benchmarks following the project's honest-methodology
  pattern (`benches/`, `RESULTS.md`) to validate the buffer-reclaim
  cache-line analysis with real measurements, not just the theoretical
  O().

## Object model (classes.md, already updated in rfc)

- [ ] **General interception Proxy** — transparent method interception
  on an existing, already-constructed target object, without touching
  the target's class/code. Distinct from the Lazy Proxy (PHP 8.4,
  already designed in `classes.md`). User has ideas on the mechanism;
  not yet discussed in depth. Needs to address: composition with the
  dispatch decision tree, whether it reuses the fat interface-reference
  mechanism, identity semantics (`instanceof`, `spl_object_id`) vs
  Ghost/Lazy Proxy.
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
  world. Everything in `model/` assumes this compiler exists.
- Vertical slice: minimal hello-world through the whole stack.
- Exceptions, Closures, Enums, Generators/Fibers, Resources, Generics,
  actors, stdlib, I/O — all listed in `rfc/BACKLOG.md`, untouched.
