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

## What this does and does not prove

Proves: for the allocate-many / free-together pattern that dominates a
PHP request, the request arena is materially faster than both the best
Rust bump allocator and a top-tier general allocator, *with reclamation
counted*.

Does not prove: anything about long-lived, individually-freed objects
(the GC heap's job, not built yet), fragmentation over months, or
multi-threaded contention. Those need their own benchmarks.
