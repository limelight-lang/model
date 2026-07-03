//! Honest allocation benchmarks.
//!
//! Methodology (docs/memory-manager.md, Test Plan): every contender
//! performs the same workload — N allocations of 40 bytes PLUS
//! reclamation. The arena pays its reset, malloc pays its frees,
//! bumpalo pays its reset. Nobody hides the cleanup.
//!
//! Contenders:
//! - `arena`          — our bump arena over the 32 KB block pool
//! - `arena+reserve`  — same, with the compiler batch hint
//! - `bumpalo`        — the canonical Rust bump allocator
//! - `system_malloc`  — the OS allocator via Box

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use ll_model::Arena;

const N: usize = 1000;
const SIZE: usize = 40;

fn bench_allocators(c: &mut Criterion) {
    let mut group = c.benchmark_group("alloc_40b_x1000_with_reclaim");

    group.bench_function("arena", |b| {
        let mut arena = Arena::new();
        b.iter(|| {
            for _ in 0..N {
                black_box(arena.alloc(black_box(SIZE)));
            }
            arena.reset(|_| {});
        });
    });

    group.bench_function("arena+reserve", |b| {
        let mut arena = Arena::new();
        b.iter(|| {
            arena.reserve(N * SIZE / 8); // batches within block capacity
            for _ in 0..N {
                black_box(arena.alloc(black_box(SIZE)));
            }
            arena.reset(|_| {});
        });
    });

    group.bench_function("bumpalo", |b| {
        let mut bump = bumpalo::Bump::new();
        let layout = std::alloc::Layout::from_size_align(SIZE, 8).unwrap();
        b.iter(|| {
            for _ in 0..N {
                black_box(bump.alloc_layout(black_box(layout)));
            }
            bump.reset();
        });
    });

    group.bench_function("system_malloc", |b| {
        b.iter(|| {
            let mut v: Vec<Box<[u8; SIZE]>> = Vec::with_capacity(N);
            for _ in 0..N {
                v.push(black_box(Box::new([0u8; SIZE])));
            }
            drop(v); // frees are part of the workload
        });
    });

    group.finish();
}

criterion_group!(benches, bench_allocators);
criterion_main!(benches);
