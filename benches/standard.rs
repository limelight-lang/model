//! Standard allocator-benchmark patterns, faithfully reimplemented.
//!
//! The canonical suite is [mimalloc-bench](https://github.com/daanx/mimalloc-bench)
//! (by mimalloc's author). Its original programs are C and expect a
//! drop-in `malloc`/`free`; our heap is a typed Rust API (≤8 KB,
//! single-thread for now), so running the C suite unchanged isn't
//! meaningful. Instead we reimplement its two canonical SINGLE-THREAD
//! synthetic patterns and drive every contender through the identical
//! harness:
//!
//! - **larson** (Larson & Krishnan server pattern): keep a fixed live
//!   set of N slots; each round free a random slot and allocate a new
//!   random-sized object into it. Steady-state alloc/free under a
//!   retained working set — the classic server workload.
//! - **rptest** (rpmalloc alloc-test pattern): scattered churn — each
//!   iteration free ~10% of blocks at random indices and reallocate them
//!   at new random sizes.
//!
//! Contenders: our `Heap`, mimalloc (the model we followed and the
//! best-benchmarked small-object allocator), and the system allocator.
//! jemalloc is omitted — it does not build on windows-msvc (see
//! Cargo.toml). Every allocation is WRITTEN to (8-byte header), so we
//! never measure over untouched memory. Sizes are capped at 8 KB, our
//! heap's small-object envelope, applied to all contenders equally.

use std::alloc::{GlobalAlloc, Layout, System};

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use ll_model::Heap;
use mimalloc::MiMalloc;

/// Uniform allocator interface. `free` receives the size so the
/// GlobalAlloc contenders can rebuild their `Layout` — this is if
/// anything generous to them (real malloc stores the size); our heap
/// ignores it and recovers the class from the block header.
trait Alloc {
    fn a(&mut self, size: usize) -> *mut u8;
    /// # Safety: `ptr`/`size` must match a live allocation from `a`.
    unsafe fn f(&mut self, ptr: *mut u8, size: usize);
}

struct OurHeap(Heap);
impl Alloc for OurHeap {
    #[inline]
    fn a(&mut self, size: usize) -> *mut u8 {
        self.0.alloc(size)
    }
    #[inline]
    unsafe fn f(&mut self, ptr: *mut u8, _size: usize) {
        unsafe { self.0.free(ptr) }
    }
}

struct Global<G: GlobalAlloc>(G);
impl<G: GlobalAlloc> Alloc for Global<G> {
    #[inline]
    fn a(&mut self, size: usize) -> *mut u8 {
        unsafe { self.0.alloc(Layout::from_size_align_unchecked(size, 8)) }
    }
    #[inline]
    unsafe fn f(&mut self, ptr: *mut u8, size: usize) {
        unsafe { self.0.dealloc(ptr, Layout::from_size_align_unchecked(size, 8)) }
    }
}

/// Seeded xorshift — deterministic across contenders.
struct Rng(u64);
impl Rng {
    #[inline]
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    #[inline]
    fn size(&mut self, min: usize, max: usize) -> usize {
        min + (self.next() as usize) % (max - min)
    }
}

#[inline]
fn touch(p: *mut u8) {
    if !p.is_null() {
        unsafe { (p as *mut u64).write(0x1) };
    }
    black_box(p);
}

const MIN: usize = 16;
const MAX: usize = 8192; // our heap's small-object ceiling, applied to all

/// larson: fixed live set, each round free-one + alloc-one.
fn larson<A: Alloc>(al: &mut A, slots: usize, rounds: usize, rng: &mut Rng) {
    let mut live: Vec<(*mut u8, usize)> = Vec::with_capacity(slots);
    for _ in 0..slots {
        let s = rng.size(MIN, MAX);
        let p = al.a(s);
        touch(p);
        live.push((p, s));
    }
    for _ in 0..rounds {
        let i = (rng.next() as usize) % slots;
        let (p, s) = live[i];
        unsafe { al.f(p, s) };
        let ns = rng.size(MIN, MAX);
        let np = al.a(ns);
        touch(np);
        live[i] = (np, ns);
    }
    for (p, s) in live {
        unsafe { al.f(p, s) };
    }
}

/// rptest: scattered churn — free/realloc ~10% of blocks each iteration.
fn rptest<A: Alloc>(al: &mut A, blocks: usize, iters: usize, rng: &mut Rng) {
    let mut live: Vec<(*mut u8, usize)> = (0..blocks)
        .map(|_| {
            let s = rng.size(MIN, MAX);
            let p = al.a(s);
            touch(p);
            (p, s)
        })
        .collect();
    let churn = blocks / 10;
    for _ in 0..iters {
        for _ in 0..churn {
            let i = (rng.next() as usize) % blocks;
            let (p, s) = live[i];
            unsafe { al.f(p, s) };
            let ns = rng.size(MIN, MAX);
            let np = al.a(ns);
            touch(np);
            live[i] = (np, ns);
        }
    }
    for (p, s) in live {
        unsafe { al.f(p, s) };
    }
}

fn bench(c: &mut Criterion) {
    // larson: 5000 live slots, 20000 free+alloc rounds.
    let mut g = c.benchmark_group("larson_5k_slots_20k_rounds");
    g.bench_function("our_heap", |b| {
        let mut a = OurHeap(Heap::new());
        b.iter(|| larson(&mut a, 5000, 20_000, &mut Rng(0x1234_5678)));
    });
    g.bench_function("mimalloc", |b| {
        let mut a = Global(MiMalloc);
        b.iter(|| larson(&mut a, 5000, 20_000, &mut Rng(0x1234_5678)));
    });
    g.bench_function("system", |b| {
        let mut a = Global(System);
        b.iter(|| larson(&mut a, 5000, 20_000, &mut Rng(0x1234_5678)));
    });
    g.finish();

    // rptest: 10000 blocks, 40 iterations (× 1000 churn each = 40k ops).
    let mut g = c.benchmark_group("rptest_10k_blocks_40_iters");
    g.bench_function("our_heap", |b| {
        let mut a = OurHeap(Heap::new());
        b.iter(|| rptest(&mut a, 10_000, 40, &mut Rng(0x0BADF00D)));
    });
    g.bench_function("mimalloc", |b| {
        let mut a = Global(MiMalloc);
        b.iter(|| rptest(&mut a, 10_000, 40, &mut Rng(0x0BADF00D)));
    });
    g.bench_function("system", |b| {
        let mut a = Global(System);
        b.iter(|| rptest(&mut a, 10_000, 40, &mut Rng(0x0BADF00D)));
    });
    g.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
