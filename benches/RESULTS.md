# Benchmark Results

Data only. Bench code: [`benches/alloc.rs`](alloc.rs). Reproduce with
`cargo bench`.

## Environment

- Rust 1.87, `x86_64-pc-windows-msvc`
- Developer laptop, not an isolated bench rig — expect run-to-run
  variance of ±10–15%. Treat these as *ratios*, not absolute truth.

## alloc_40b_x500_write_then_reclaim

Workload per iteration: 500 allocations of 40 bytes, **each written to**
(8-byte header, the minimum a real object pays), all kept alive, then
**reclaimed** (arena/bumpalo `reset`; malloc/mimalloc per-object free).
20 KB total fits one 32 KB block.

| Contender | Time / 500 (median) | Per alloc | vs arena |
|---|---|---|---|
| **arena** | ~471 ns | ~0.94 ns | 1.0× |
| **arena + reserve** | ~488 ns | ~0.98 ns | ~1.0× |
| bumpalo | ~789 ns | ~1.58 ns | ~1.7× slower |
| mimalloc (fast malloc) | ~3.29 µs | ~6.6 ns | ~7× slower |
| system malloc (OS default) | ~24 µs | ~48 ns | ~50× slower |

## Reading the numbers honestly

- **The arena is ~1.7× faster than bumpalo** and **~7× faster than
  mimalloc**. Not the "50×" the first (flawed) benchmark suggested —
  that number came from comparing an untouched-memory bump loop against
  the slow OS allocator. mimalloc is the honest fast-malloc rival, and
  ~7× is the real gap. The 50× column now belongs only to the default
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
| wall time | ~0.57 s |
| throughput | ~176 000 req/s, ~440 M obj/s |
| per object | ~2.3 ns |
| **regions carved (OS memory)** | **1 region = 2 MiB** |
| churn ratio | ~16 500× (bytes allocated / resident) |
| plateau | 1 region at 10% done → 1 at 100% — **STABLE** |

**Multi-thread — 8 workers × 25 000 requests, one shared pool:**

| Metric | Value |
|---|---|
| objects allocated | ~500 million |
| wall time | ~0.31 s |
| throughput | ~640 000 req/s, ~1.6 B obj/s aggregate |
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

- **~2.3 ns/object here vs ~0.94 ns in the micro-benchmark.** The higher
  figure is the *realistic* one: it includes the simulation's own
  per-object RNG and size computation (a modulo + branch), variable
  request sizes, and real cache pressure over 33 GiB. The micro-bench is
  the best-case floor; this is closer to real work. Neither is "the"
  number — they bracket it.
- The RNG/sizing cost is charged into per-object, so ~2.3 ns is an
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
| larson (20k rounds) | **0.84 ms** (~42 ns/round) | 1.88 ms (~94 ns) | 2.50 ms (~125 ns) |
| rptest (40k churn) | **1.85 ms** (~46 ns/round) | 4.40 ms (~110 ns) | 7.69 ms (~192 ns) |
| vs our heap | 1.0× | ~2.2–2.4× slower | ~3–4× slower |

**jemalloc omitted**: `jemalloc-sys` does not build on
windows-msvc (autotools `configure` fails). mimalloc is the primary
rival regardless.

### Three caveats that flatter us — read before believing "2× faster than mimalloc"

The gap is real *for what our heap currently is*, but the comparison is
not apples-to-apples, and honesty requires saying why:

1. **Our heap is not thread-safe yet.** mimalloc pays for atomic
   operations and a thread-ownership check on *every* free even in
   single-threaded use (that is how it routes local vs cross-thread
   frees). Our phase-1 heap pays none of that. A large part of the gap
   is thread-safety we simply haven't built. When cross-thread free
   lands, expect this to narrow.
2. **Inlined vs a call boundary.** Our Rust `Heap` inlines into the
   benchmark loop; mimalloc is reached through a non-inlinable
   `extern "C"` boundary (linked C static lib). This mirrors our real
   design — `ll_heap_alloc` *does* inline into PHP code via bitcode LTO,
   that is the whole point — but in this harness it is still an
   asymmetry in our favour.
3. **Specialised envelope.** Our heap handles only ≤ 8 KB, 8-byte
   alignment, no large objects, one size-class scheme. mimalloc is a
   complete general allocator. Narrower job = faster.

Honest headline: *in its current single-threaded, small-object, inlined
form, our heap beats mimalloc ~2×* — with a meaningful share of that
coming from work we haven't done (thread safety), not pure superiority.
The design is sound and the numbers are encouraging; they are not a
victory lap over a production allocator.

## What this does and does not prove

Proves: for the allocate-many / free-together pattern that dominates a
PHP request, the request arena is materially faster than both the best
Rust bump allocator and a top-tier general allocator, *with reclamation
counted*.

Does not prove: anything about long-lived, individually-freed objects
(the GC heap's job, not built yet), fragmentation over months, or
multi-threaded contention. Those need their own benchmarks.
