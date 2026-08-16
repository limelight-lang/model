# bench-external

Tooling used to validate `ll-model`'s allocator against real, unmodified
rival benchmarks and profile it under realistic conditions — built
because the in-crate `benches/` suite (Rust-only, in-process) turned out
to be optimistic relative to what a real caller sees through the actual
C ABI. Findings and the fixes that came out of this investigation are in
`rfc/model/memory/heap-slot-allocation.md` and `benches/RESULTS.md`.

Nothing here builds via `cargo`; these are throwaway MSVC (`cl.exe`)
command-line tools, run manually. Build `ll_model.lib` first
(`cargo build --release`, optionally `RUSTFLAGS="-C debuginfo=2"` for
symbol-resolved profiling), then compile against
`target/release/ll_model.lib`.

## canary/

Naive-but-clean C++ reference loops beside the same operation through
the real C ABI, in one binary — the external comparand of the
performance case (`dev/DECISIONS.md`, "the performance case's external
comparand is a canary, not a self-authored floor"). The larson tooling
above is MSVC-era; these build on Linux with `g++`, the command at the
head of each probe. The staticlib must be the rc-walk build: a
`--no-default-features` build overwrites the same `libll_model.a`.

- **`pair_canary.cpp`** — the retain/release pair: the shipped pair on
  a live ReferenceBox, its duplicate as the instrument's measured
  zero, a bare non-atomic inc/dec canary, a `std::shared_ptr`
  copy/drop canary, and an empty skeleton bounding the harness term.
  Figures: `dev/BENCHMARKS.md`, 2026-08-16, "the pair against its
  canaries". Acceptance is by disassembly, per arm — `accept.sh`,
  re-run after every rebuild.

## larson/

- **`larson.cpp`** — real, unmodified benchmark from
  [mimalloc-bench](https://github.com/daanx/mimalloc-bench)
  (`bench/larson/larson.cpp`). Not vendored here (third-party, not ours
  to commit) — download it yourself:
  ```
  curl -sL https://raw.githubusercontent.com/daanx/mimalloc-bench/master/bench/larson/larson.cpp -o bench-external/larson/larson.cpp
  ```
- **`ll_malloc_shim.h`** / **`mi_malloc_shim.h`** — point larson's
  `CUSTOM_MALLOC`/`CUSTOM_FREE` hook at our `ll_malloc`/`ll_c_free` (with
  a native per-thread `ll_thread_init()` call, since larson spawns raw
  OS threads our runtime doesn't know about) or at `mi_malloc`/`mi_free`.
  Standard invocation: `larson.exe 5 8 1000 5000 100 4141 <nthreads>`.
- **`isolate_path.cpp`** — isolates per-call path overhead (TLS, FFI
  boundary, algorithm) from workload shape: fixed size, immediate
  alloc-then-free, no live set. Links against `ll_model.lib`,
  `mimalloc.lib` (built via the `mimalloc` dev-dependency's build
  script), and optionally `snmalloc-rs`'s cdylib.
- **`selfprofile.cpp`** — admin-free statistical sampling profiler.
  No ETW/`wpr` available without local-admin rights, so this samples its
  own worker thread's RIP via `SuspendThread`/`GetThreadContext`/
  `ResumeThread` (legal on a thread you own) instead. Fixed-size
  workload.
- **`selfprofile2.cpp`** — same sampling technique, but a
  larson-shaped workload (random sizes 8..1000, live-set churn) on one
  stable thread. Build twice, with and without `-DPROFILE_MIMALLOC`, to
  compare. (larson.cpp itself can't be profiled this way in
  single-thread mode: `exercise_heap` respawns a new OS thread every
  ~25ms, which defeats thread-based sampling — confirmed the same
  failure independently in a real tool, Very Sleepy, with `/mbt`.)
- **`xprofile.cpp`** — cross-process variant of the same sampling
  technique (launch a target exe, sample its threads from outside).
  Kept for reference; superseded by `selfprofile2.cpp` for larson-shaped
  workloads for the reason above.
- **`bitmap_proto.cpp`** — isolated prototype comparing the current
  intrusive-linked-list free/local_free scheme against a bitmap-based
  free-slot tracker, and the current linear size-class scan against an
  O(1) lookup table — before committing to rewriting `Heap` itself.
  Measured ~10-20% (bitmap alone) and ~40-47% (bitmap + O(1) lookup
  table) faster than the current scheme on a larson-shaped workload.

## Symbol resolution for profiling

`dumpbin /disasm` needs debug info to print symbol names:
```
RUSTFLAGS="-C debuginfo=2" cargo build --release
cl /nologo /O2 /Zi ... /link /DEBUG /INCREMENTAL:NO /DYNAMICBASE:NO ...
dumpbin /nologo /disasm target.exe > disasm.txt
```
`/INCREMENTAL:NO` matters: `/DEBUG` enables incremental linking by
default, which inserts an extra indirect jump (`@ILT+N` stub) on every
cross-object call and shows up as phantom samples in a profile.
`/DYNAMICBASE:NO` keeps runtime addresses identical to the ones
`dumpbin` prints statically, so sampled RIPs can be matched directly
without computing a module-relocation offset.
