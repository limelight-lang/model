//! Allocation benchmarks — honest edition.
//!
//! Results live in `benches/RESULTS.md`; this file is the code only.
//!
//! ## Methodology (and the mistakes this version fixes)
//!
//! Every contender runs the SAME workload: `N` allocations of 40 bytes,
//! **each written to** (the 8-byte refcount header — the minimum a real
//! object pays), kept alive together, then **reclaimed**. The arena pays
//! `reset`, bumpalo pays `reset`, malloc/mimalloc pay per-object frees.
//!
//! Three flaws corrected from the first version:
//! 1. **Memory was never written.** Bump allocators handed back untouched
//!    pages (unrealistic cache advantage) while `Box::new([0u8; N])`
//!    zeroed memory (penalised). Now every contender writes the header,
//!    so the write cost is constant across all of them.
//! 2. **`reserve` math was wrong** and the workload (40 KB) exceeded one
//!    32 KB block, which `reserve` cannot span. `N = 500` (20 KB) fits a
//!    single block, so `reserve` legitimately covers the whole loop.
//! 3. **Only system malloc.** Added mimalloc — the realistic fast-malloc
//!    rival, not the default OS allocator.

use std::alloc::{GlobalAlloc, Layout};

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use ll_model::Arena;
use mimalloc::MiMalloc;

/// 500 * 40 B = 20 KB — fits in one block, so `reserve` covers the full
/// loop.
const N: usize = 500;
const SIZE: usize = 40;

/// Write the 8-byte header a real object always writes, so allocation
/// throughput is not measured over untouched memory.
#[inline]
fn touch(p: *mut u8) {
    unsafe { (p as *mut u64).write(0x1) };
    black_box(p);
}

fn bench_allocators(c: &mut Criterion) {
    let mut group = c.benchmark_group("alloc_40b_x500_write_then_reclaim");

    group.bench_function("arena", |b| {
        let mut arena = Arena::new();
        b.iter(|| {
            for _ in 0..N {
                touch(arena.alloc(black_box(SIZE)));
            }
            arena.reset(|_| {});
        });
    });

    group.bench_function("arena+reserve", |b| {
        let mut arena = Arena::new();
        b.iter(|| {
            arena.reserve(N * SIZE); // 20 KB < one block: covers the loop
            for _ in 0..N {
                touch(arena.alloc(black_box(SIZE)));
            }
            arena.reset(|_| {});
        });
    });

    group.bench_function("bumpalo", |b| {
        let mut bump = bumpalo::Bump::new();
        let layout = Layout::from_size_align(SIZE, 8).unwrap();
        b.iter(|| {
            for _ in 0..N {
                touch(bump.alloc_layout(black_box(layout)).as_ptr());
            }
            bump.reset();
        });
    });

    group.bench_function("system_malloc", |b| {
        let layout = Layout::from_size_align(SIZE, 8).unwrap();
        let mut ptrs: Vec<*mut u8> = Vec::with_capacity(N);
        b.iter(|| {
            for _ in 0..N {
                let p = unsafe { std::alloc::System.alloc(black_box(layout)) };
                touch(p);
                ptrs.push(p);
            }
            for p in ptrs.drain(..) {
                unsafe { std::alloc::System.dealloc(p, layout) };
            }
        });
    });

    group.bench_function("mimalloc", |b| {
        let layout = Layout::from_size_align(SIZE, 8).unwrap();
        let mut ptrs: Vec<*mut u8> = Vec::with_capacity(N);
        b.iter(|| {
            for _ in 0..N {
                let p = unsafe { MiMalloc.alloc(black_box(layout)) };
                touch(p);
                ptrs.push(p);
            }
            for p in ptrs.drain(..) {
                unsafe { MiMalloc.dealloc(p, layout) };
            }
        });
    });

    group.finish();
}

/// Seeded xorshift — deterministic across contenders, same schedule for all.
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

/// Continuous spread, not a handful of round numbers: 8 B up to 8 KB —
/// the same small-object ceiling `standard.rs`/`Heap::MAX_SMALL` uses.
/// Heavily skewed toward the small end (most real objects are small),
/// via two nested draws — one draw alone is uniform and unrealistic;
/// squaring the ratio biases it toward `MIN` without excluding the tail.
const MIN: usize = 8;
const MAX: usize = 8192;

/// Fisher-Yates over a fixed seed, so every contender frees in the exact
/// same shuffled order — no LIFO/FIFO advantage for the free-list
/// allocators, and no bias against them either.
fn schedule(n: usize) -> (Vec<usize>, Vec<usize>) {
    let mut rng = Rng(0xDEAD_BEEF_CAFE_F00D);
    let sizes: Vec<usize> = (0..n)
        .map(|_| {
            let a = rng.size(MIN, MAX);
            let b = rng.size(MIN, MAX);
            MIN + (a.min(b) - MIN) // skew toward MIN, keep the occasional large tail
        })
        .collect();
    let mut order: Vec<usize> = (0..n).collect();
    for i in (1..n).rev() {
        let j = (rng.next() as usize) % (i + 1);
        order.swap(i, j);
    }
    (sizes, order)
}

/// Honest edition #2: mixed sizes (not one fixed size) and free in a
/// shuffled order (not allocation order), so the malloc contenders don't
/// get the free-list-friendly best case of the first benchmark. Arena and
/// bumpalo still reclaim via `reset` — that bulk-free-together capability
/// is the actual feature under test, not something to hide.
fn bench_mixed_random_free(c: &mut Criterion) {
    let mut group = c.benchmark_group("alloc_mixed_x500_random_free_reclaim");
    let (sizes, order) = schedule(N);

    group.bench_function("arena", |b| {
        let mut arena = Arena::new();
        b.iter(|| {
            for &s in &sizes {
                touch(arena.alloc(black_box(s)));
            }
            arena.reset(|_| {});
        });
    });

    group.bench_function("bumpalo", |b| {
        let mut bump = bumpalo::Bump::new();
        b.iter(|| {
            for &s in &sizes {
                let layout = Layout::from_size_align(s, 8).unwrap();
                touch(bump.alloc_layout(black_box(layout)).as_ptr());
            }
            bump.reset();
        });
    });

    group.bench_function("system_malloc", |b| {
        let mut ptrs: Vec<*mut u8> = vec![std::ptr::null_mut(); N];
        b.iter(|| {
            for (i, &s) in sizes.iter().enumerate() {
                let layout = Layout::from_size_align(s, 8).unwrap();
                let p = unsafe { std::alloc::System.alloc(black_box(layout)) };
                touch(p);
                ptrs[i] = p;
            }
            for &i in &order {
                let layout = Layout::from_size_align(sizes[i], 8).unwrap();
                unsafe { std::alloc::System.dealloc(ptrs[i], layout) };
            }
        });
    });

    group.bench_function("mimalloc", |b| {
        let mut ptrs: Vec<*mut u8> = vec![std::ptr::null_mut(); N];
        b.iter(|| {
            for (i, &s) in sizes.iter().enumerate() {
                let layout = Layout::from_size_align(s, 8).unwrap();
                let p = unsafe { MiMalloc.alloc(black_box(layout)) };
                touch(p);
                ptrs[i] = p;
            }
            for &i in &order {
                let layout = Layout::from_size_align(sizes[i], 8).unwrap();
                unsafe { MiMalloc.dealloc(ptrs[i], layout) };
            }
        });
    });

    group.finish();
}

criterion_group!(benches, bench_allocators, bench_mixed_random_free);
criterion_main!(benches);
