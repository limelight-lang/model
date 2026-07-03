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

/// 500 * 40 B = 20 KB — fits in one 32 KB block, so `reserve` covers the
/// full loop.
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

criterion_group!(benches, bench_allocators);
criterion_main!(benches);
