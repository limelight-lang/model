# Benchmark Results

Data only. Bench code: [`benches/alloc.rs`](alloc.rs). Reproduce with
`cargo bench`.

## Environment

- Rust 1.87, `x86_64-pc-windows-msvc`
- Developer laptop, not an isolated bench rig — expect run-to-run
  variance of ±10–15%. Treat these as *ratios*, not absolute truth.

## Read this first: how to compare against a rival, and how not to

Three ways of measuring the same allocator against mimalloc on the same
larson pattern, in increasing order of trustworthiness:

| method | verdict | worth |
|---|---|---|
| in-process Rust (`benches/standard.rs`) | we are **3.4x faster** | nothing — we inline, mimalloc is behind a call boundary |
| two exes: `larson_ours` vs `larson_mimalloc` | we are **1.13–1.26x slower** | nothing at this scale — see below |
| both allocators in **one** binary, alternating | we are **1.05–1.07x slower** | this one |

**Never compare two separately-linked executables for a difference under
~10%.** Code layout, alignment and I-cache placement differ between two
binaries by more than the effect being measured. Run against
`larson_mimalloc.exe`, our number wandered over 1.12 / 1.14 / 1.18 / 1.26
across a single afternoon, and every one of those was quoted here at some
point as though it meant something. It did not.

`bench-external/larson/bisect_probe.cpp` is what the honest version looks
like: larson's exact loop, both allocators compiled into one process, run
alternately, best-of-3. It says **1.05–1.07x**, and it says the same thing
run after run.

That number was arrived at by elimination, not assumption. Bisecting
larson's loop — its `lran2` RNG, its warmup permutation, its second
`blksize` array, its two-writes-plus-a-read, its counters — moved the ratio
by nothing (all variants 1.05–1.08x). Nor did the CRT (`/MT` 1.14x vs `/MD`
1.13x), nor thread churn (removing it made the two-exe number *worse*), nor
the cross-thread path (0.2% of frees). What was left was the comparison
method itself.

Also fixed while chasing this: mimalloc-bench reached mimalloc via
`#define CUSTOM_MALLOC mi_malloc` — a direct call — while our shim wrapped
both calls in an init check that ran on every malloc *and* every free.
`ll_malloc`/`ll_c_free` now self-initialise on a cold branch, exactly as
`mi_malloc` does (`test rcx,rcx; je _mi_malloc_generic`), and the shim is a
direct `#define` like mimalloc's.

## alloc_40b_x500_write_then_reclaim

Workload per iteration: 500 allocations of 40 bytes, **each written to**
(8-byte header, the minimum a real object pays), all kept alive, then
**reclaimed** (arena/bumpalo `reset`; malloc/mimalloc per-object free).
20 KB total fits one block.

| Contender | Time / 500 (median) | Per alloc | vs arena |
|---|---|---|---|
| **arena** | ~409 ns | ~0.82 ns | 1.0× |
| **arena + reserve** | ~405 ns | ~0.81 ns | ~1.0× |
| bumpalo | ~632 ns | ~1.26 ns | ~1.5× slower |
| mimalloc (fast malloc) | ~2.22 µs | ~4.4 ns | ~5.4× slower |
| system malloc (OS default) | ~16.8 µs | ~34 ns | ~41× slower |

## Reading the numbers honestly

- **The arena is ~1.5× faster than bumpalo** and **~5.4× faster than
  mimalloc**. Not the "50×" the first (flawed) benchmark suggested —
  that number came from comparing an untouched-memory bump loop against
  the slow OS allocator. mimalloc is the honest fast-malloc rival, and
  ~5.4× is the real gap. The 41× column now belongs only to the default
  OS allocator, which nobody serious uses under load.

- **`reserve` is within noise of the plain arena here — and that is
  correct.** With 500 allocations in one block, the plain arena hits its
  slow path at most once; `reserve` saves that single branch out of 500.
  Once each allocation writes its header, the write dominates and the
  saved limit-check disappears into noise. `reserve` earns its keep in
  tight, write-light loops over many blocks — not universally. The first
  benchmark's "2× from reserve" was an artifact of a broken workload
  (reserved 1/8 of the bytes, over a 40 KB span reserve cannot cover).

- **The malloc contenders pay to track what to free.** Each pointer is
  pushed to a `Vec` so it can be freed in the batch — a real cost of the
  malloc model, since you must remember every live object. The arena
  remembers nothing per-object; `reset` frees all 500 at once. That
  asymmetry is the design win, not a benchmark trick — but the `Vec`
  push (~1–2 ns) is charged to malloc/mimalloc and inflates their number
  slightly. Even subtracting it, the gap holds.

## Server workload simulation

Code: [`examples/server_sim.rs`](../examples/server_sim.rs). Run with
`cargo run --release --example server_sim`. Emulates a long-lived server:
each request gets its own arena, allocates a randomized batch (1000–4000
objects, 16–256 bytes each, headers written), then the arena is dropped
and its blocks return to the shared pool. A persistent cache arena
accumulates 20 000 long-lived objects (never reset). Then a
multi-threaded phase runs 8 workers against the one shared pool.

**Single thread — 100 000 requests:**

| Metric | Value |
|---|---|
| objects allocated | ~250 million |
| bytes allocated | ~33 GiB (total churn) |
| wall time | ~0.48 s |
| throughput | ~210 000 req/s, ~524 M obj/s |
| per object | ~1.9 ns |
| **regions carved (OS memory)** | **1 region = 2 MiB** |
| churn ratio | ~16 500× (bytes allocated / resident) |
| plateau | 1 region at 10% done → 1 at 100% — **STABLE** |

**Multi-thread — 8 workers × 25 000 requests, one shared pool:**

| Metric | Value |
|---|---|
| objects allocated | ~500 million |
| wall time | ~0.20 s |
| throughput | ~1 027 000 req/s, ~2.6 B obj/s aggregate |
| **regions carved** | **2 regions = 4 MiB** |

### What the server sim shows

- **Memory plateaus.** 33 GiB of allocation churned through **2 MiB** of
  resident memory — a 16 500× churn ratio — and the region count is
  identical at 10% and 100% of the run. This is the core design claim
  proved: arenas borrow blocks and return them; the resident set is a
  small working set, not a function of request count. A malloc-based
  runtime that leaked even 100 bytes/request would be 10 MB heavier by
  request 100 000; here the high-water mark never moves.

- **Cross-thread block flow works.** 8 concurrent workers on one shared
  pool held their entire working set in 2 regions (4 MiB). Blocks freed
  by one worker's arena reset flow through the global stack to another
  worker — no per-worker region explosion.

### Honest caveats on these numbers

- **~1.9 ns/object here vs ~0.82 ns in the micro-benchmark.** The higher
  figure is the *realistic* one: it includes the simulation's own
  per-object RNG and size computation (a modulo + branch), variable
  request sizes, and real cache pressure over 33 GiB. The micro-bench is
  the best-case floor; this is closer to real work. Neither is "the"
  number — they bracket it.
- The RNG/sizing cost is charged into per-object, so ~1.9 ns is an
  *upper* bound on the allocator's own share.
- Phase 1 never returns regions to the OS, so "regions carved" is the
  high-water mark — exactly the number we want to show is bounded.

## Standard patterns: heap vs mimalloc vs system

Code: [`benches/standard.rs`](standard.rs). Faithful single-thread
reimplementations of the two canonical
[mimalloc-bench](https://github.com/daanx/mimalloc-bench) synthetic
patterns, every contender through the identical harness, every
allocation written to, sizes ≤ 8 KB (our heap's envelope) for all.

- **larson** (Larson & Krishnan server pattern): 5000 live slots, 20000
  rounds of free-one + alloc-one at random sizes.
- **rptest** (rpmalloc alloc-test pattern): 10000 blocks, 40 iterations
  of scattered free/realloc of ~10% each.

Time per iteration (lower is better), and per free+alloc round:

| Pattern | our heap | mimalloc | system |
|---|---|---|---|
| larson (20k rounds) | **0.535 ms** (~27 ns/round) | 1.82 ms (~91 ns) | 2.36 ms (~118 ns) |
| rptest (40k churn) | **1.11 ms** (~28 ns/round) | 3.76 ms (~94 ns) | 6.18 ms (~155 ns) |
| vs our heap | 1.0× | ~3.4× slower | ~4.4–5.6× slower |

**jemalloc omitted**: `jemalloc-sys` does not build on
windows-msvc (autotools `configure` fails). mimalloc is the primary
rival regardless.

### Three caveats that flatter us — read before believing "3.4× faster than mimalloc"

The gap is real *for what our heap currently is*, but the comparison is
not apples-to-apples, and honesty requires saying why:

1. **Inlined vs a call boundary — and this one is worth ~4x.** Our Rust
   `Heap` inlines into the benchmark loop; mimalloc is reached through a
   non-inlinable `extern "C"` boundary (linked C static lib). The same
   allocator measured through its *own* C ABI against the same rival is
   **1.25x slower**, not 3.4x faster (see "Real C ABI vs mimalloc"). That
   is a 4x swing from the harness alone. This partly mirrors our real
   design — `ll_heap_alloc` *does* inline into PHP code via bitcode LTO,
   that is the whole point — but as a comparison it is not apples to
   apples.
2. **Specialised envelope.** Our heap handles only ≤ 8 KB, 8-byte
   alignment, no large objects, one size-class scheme. mimalloc is a
   complete general allocator. Narrower job = faster.

*(A third caveat used to sit here: "our heap is not thread-safe yet, and
mimalloc pays for thread-safety we haven't built". That is obsolete — it
landed, and the multi-threaded rematch below now runs ahead of mimalloc on
both patterns.)*

Honest headline: *measured in-process, our heap beats mimalloc ~3.4x; the
same code through the real C ABI is 1.25x slower than mimalloc.* Believe the
second number. The design is sound and the direction is right; this is not a
victory lap over a production allocator.

## Multi-threaded: heap vs mimalloc vs system (the fair rematch)

Once the heap became thread-safe, the single-thread caveat (#1 above —
mimalloc paying for thread-safety we hadn't built) no longer applies, so
we can compare on multi-threaded workloads where that machinery is
actually exercised. Code: [`examples/mt_bench.rs`](../examples/mt_bench.rs),
`cargo run --release --example mt_bench`. 8 threads, Larson server
pattern, sizes 16..8192, every allocation written.

Two patterns:
- **independent** — each thread runs larson entirely on its own (the
  per-thread local path, no cross-thread traffic).
- **bleeding** — threads in a ring; every object is freed by the *next*
  thread (Larson's cross-thread "bleeding"): our `remote_free` stack vs
  mimalloc's per-page `xthread_free`.

Aggregate throughput (M ops/s), **5 samples on a dev laptop — high
variance, read as medians and ranges, not precise figures**:

| Pattern | our heap | mimalloc | system |
|---|---|---|---|
| independent | **~18.9** (17.5–19.1) | ~13.9 (13.5–14.9) | ~3.4 |
| bleeding | **~34.2** (32.7–35.8) | ~24.4 (23.6–26.1) | ~7.1 |

Previously (before `rfc/model/memory/heap-slot-allocation.md` fixes 5-6):
independent ~11.5 vs ~12.0 — a tie; bleeding ~19.6 vs ~23.1 — **mimalloc
~15% ahead**, recorded here as "the real weakness".

### Honest read

- **Local path: ~36% ahead of mimalloc** (~18.9 vs ~13.9), up from a tie.
  Some of this is still the inlining edge this harness gives us (caveat 2
  above) — the C ABI comparison is the one to trust for absolute claims.
- **The documented cross-thread deficit is gone**, and that is the
  surprise. See below.
- **Cross-thread path: ~40% ahead of mimalloc** (~34.2 vs ~24.4), where it
  used to be ~15% behind. **Nothing on that path was touched.** The
  `remote_free` design is unchanged: still one contended atomic stack per
  owner, still not sharded per page the way mimalloc does it. What changed
  is only what the *drain* does once it has the slot — pushing onto a free
  list instead of dividing to find a bitmap index, over half as many blocks.

  So the conclusion recorded here previously — "we funnel cross-thread frees
  through one stack, mimalloc shards per page, and that costs us 15%" — was
  **wrong about the cause**. The single stack was never the bottleneck; the
  per-slot work on the owner's side was. The "known, planned fix"
  (per-destination batching / per-page sharding) is therefore **not
  justified by any measurement we have**, and should not be built until
  something re-establishes a need for it.
- Both are 4–6× the system allocator on both patterns.
- A first cold-start sample (our 8.2 vs mimalloc 3.2) was discarded as
  an unwarmed-mimalloc outlier — recording it would have been dishonest.

Bottom line: ahead of mimalloc on both multi-threaded patterns in this
harness, and clearly ahead of the system allocator. Read that against
caveat 2 above — the same code through the real C ABI is 1.25x *slower*
than mimalloc, so this is not a claim that we are the faster allocator.

## Reality check: the real C ABI path vs the in-process Rust benchmarks

Every number above (`alloc.rs`, `standard.rs`, `mt_bench.rs`) calls
`Heap`/`Arena` methods directly as Rust values in-process. Nothing above
exercises `ll_malloc`/`ll_free`/`ll_heap_alloc` — the actual C ABI a real
caller (generated PHP code, host code, or any external C/C++) uses.

Linking the real, unmodified `larson.cpp` from
[mimalloc-bench](https://github.com/daanx/mimalloc-bench) against that
C ABI (`bench-external/larson/`, `larson 5 8 1000 5000 100 4141 1`)
found our heap **~6.8x slower than mimalloc and slower than system
malloc** — the opposite of the ~2x *win* over mimalloc `standard.rs`
reports. Root cause and fix: `rfc/model/memory/heap-slot-allocation.md`.
Short version — a block was unconditionally returned to the global pool
the instant its last live slot was freed, and refill rebuilt an entire
~500-slot free list eagerly; any workload where a size class's live
count touches zero (a temp-buffer loop, this benchmark) paid a full
block rebuild on *every* allocation. Fixed via lazy (bump) slot carving
and a bounded one-empty-block-per-class retention policy.

After the fix, same unmodified benchmark:

| Contender | Throughput (ops/s) | vs mimalloc |
|---|---|---|
| mimalloc | ~45–54M | 1.0x |
| ours | ~20.0M | ~2.2–2.7x slower |
| system malloc | ~11.3M | ~4–4.7x slower |

Moved from *slower than system malloc* to ~1.8x faster than it, and
closed the mimalloc gap from ~6.8x to ~2.2–2.7x.

A second fix (same RFC file, "Fix 3 — Fast TLS") replaced
compiler-emitted TLS (`thread_local!`/`__declspec(thread)`, which on
windows-msvc costs three dependent, non-pipelineable loads through a
per-module indirection table) with the same trick mimalloc uses: a
single `gs:[fixed_offset]` read via inline `asm!`, mirroring MSVC's
`__readgsqword`. Isolated fixed-size loop (20M iterations, `SIZE=64`,
`bench-external/larson/isolate_path.cpp`) went from ~7.7–9.0 ns/op to
~6.8–7.3 ns/op; mimalloc measured ~3.6–4.3 ns/op and snmalloc (0.7.4,
same harness) ~4.4–5.4 ns/op in the same runs. No regression on the
real `larson.cpp` throughput (~18.4–20.0M ops/s, within run-to-run
noise of the fixes-1-2 number) — larson's per-op cost outside the TLS
lookup dilutes the relative win there.

jemalloc could not be added to this comparison: `tikv-jemalloc-sys`'s
autotools `configure` fails to find a working C compiler when invoked
from windows-msvc (confirmed independently, matches this file's
existing note in `Cargo.toml`).

Remaining ~1.6–2x gap to mimalloc/snmalloc is not yet attributed
further — the in-process Rust numbers above remain optimistic relative
to what a real embedder sees through the actual C ABI, and should be
read with that discount in mind until this exact real-binary check is
added as a standing part of this file's methodology rather than a
one-off investigation.

**Update: attributed and roughly halved — see the next section.**

## Real C ABI vs mimalloc, by working set

The single "~2x slower than mimalloc" figure above is one point on a
curve, and it happens to be close to the curve's worst point. Measured
with `bench-external/larson/scaling_probe.cpp` (larson's exact workload
shape — random sizes 8..1000, free-a-random-victim-then-allocate — with
only the live-set size varying), ours and mimalloc alternating inside one
process:

| live objects | live bytes | ours | mimalloc | ratio |
|---|---|---|---|---|
| 50 | 24 KB | 8.28 ns | 7.59 ns | 1.09x |
| 200 | 98 KB | 6.14 ns | 6.12 ns | **1.00x** |
| 1 000 | 492 KB | 6.60 ns | 6.78 ns | **0.97x** |
| 5 000 | 2.4 MB | 11.33 ns | 10.83 ns | **1.05x** |
| 20 000 | 9.8 MB | 20.28 ns | 17.62 ns | 1.15x |

`larson.cpp`'s standard invocation holds **5000** live objects.

Throughput on real `larson 5 8 1000 5000 100 4141 1` went from ~27.0M to
~49M ops/s over this work (+80%). The *ratio* against mimalloc from that
same run is not quoted here on purpose — it is a two-exe comparison and
therefore meaningless at this scale (see the top of this file). The
one-binary measurement says **1.05–1.07x**.

Details in `rfc/model/memory/heap-slot-allocation.md` ("Fix 5" .. "Fix 7").

### What the block size costs

Fix 6 doubled `BLOCK_SIZE` to 64 KB, which buys the mid-curve rows above
and costs footprint. A size class needs at least one whole block, so the
floor is `classes_touched * BLOCK_SIZE`:

| live bytes | 32 KB resident | 64 KB resident |
|---|---|---|
| 24 KB | 640 KB | 1280 KB |
| 98 KB | 704 KB | 1280 KB |
| 492 KB | 1184 KB | 1664 KB |
| 2.4 MB | 3392 KB | 3776 KB |
| 9.8 MB | 11.7 MB | 12.1 MB |

Up to 2x on a tiny live set, ~3% at scale — the overhead is a fixed
per-class floor, so it amortises away as the working set grows. In
absolute terms the worst case is 640 KB → 1.28 MB.

Two methodology notes this investigation cost us, worth keeping:

- **Interleave, or don't compare.** This laptop drifts ~20-30% over a long
  session (mimalloc measured anywhere from 45M to 60M ops/s on the *same*
  binary depending on how warm the machine was). Ratios from two different
  sessions are not comparable; every table here alternates the contenders
  run-by-run, and the rival's own column is the check that the two runs
  were in the same state.
- **A 39 MB live-set row exists and is deliberately not quoted.** It showed
  us ahead, but mimalloc's own number moved 31% between two back-to-back
  runs there, so the row isn't measuring what it claims.

Follow-up on a *realistic* workload (varying sizes 8..1000, 5000-object
live-set churn, matching what `larson.cpp` actually does — not one
fixed size) found the gap there was wider (~2.9x, not ~2.2–2.7x) and
attributed it to the linear size-class scan and the intrusive-linked-list
free-slot tracking. The first fix — an O(1) lookup table — stands. The
second — replacing the free list with a per-block bitmap — **was later
reverted**: it was chosen on an ablation that never actually measured a
free list (see `rfc/model/memory/heap-slot-allocation.md`, fix 4's banner
and fix 5), and removing it was worth +18-20%. Fixes 5-6 took the same
workload from 2.11x slower to **1.25x slower**; the by-working-set table
above is the current picture.

## What this does and does not prove

Proves: for the allocate-many / free-together pattern that dominates a
PHP request, the request arena is materially faster than both the best
Rust bump allocator and a top-tier general allocator, *with reclamation
counted*.

Does not prove: anything about long-lived, individually-freed objects
(the GC heap's job, not built yet), or fragmentation over months. Those
need their own benchmarks.

**And a standing warning, earned the hard way.** Every "this is what's left"
claim in this file's history has been wrong, usually by 20-40%:

- "our heap beats mimalloc ~2x" — the real C ABI said 6.8x *slower*.
- "the remaining ~2.0-2.2x gap is not fully attributed", with a list of
  candidates — none of the listed candidates were it.
- "the bitmap is ~10-20% faster than a free list" — it was ~18-20% slower;
  the ablation never measured a free list.
- "cross-thread is our real weakness, mimalloc shards per page and we
  don't" — that path is now ahead, with the sharding still not built.
- "1.25x slower than mimalloc through the real C ABI" — an artifact of
  comparing two separately-linked exes. One binary says 1.05–1.07x.

The pattern is always the same: a number measured on one harness, then
generalised. Before quoting anything here, check when it was last measured
and on which harness — several of these sat in this file for months reading
as current fact.

**And the thing none of them measured: memory.** Every number above is
throughput. The single largest defect this allocator had was a leak —
1.7 GiB held against a 2.5 MiB live set on the very benchmark this file
leads with — and no benchmark here would ever have reported it, because
none of them looked. `larson.cpp` was *designed* to catch exactly that
(Larson & Krishnan's paper is about servers whose workers come and go and
whose memory must not grow); we ran it for months and read only the ops/s
line off the bottom.
