//! String creation, hashing and append.
//!
//! Written 2026-08-04 for two questions the crate could not answer:
//!
//! - **What an append loop costs, and where.** `ll_string_append` on a
//!   request-arena string extends its payload in place off the bump top
//!   when it can; the same call on a GC-heap string reallocates and copies
//!   every time it outgrows its capacity, because `buffer_ensure_longlived`
//!   has no such path (`PLAN.md`, Residual; `rfc/model/memory/buffers.md`).
//!
//!   **The two append arms are not a measurement of that gap**, and must
//!   not be read as one: they also differ in how the payload is reclaimed —
//!   an arena reset against a refcount death — and in which allocator
//!   serves the payload. What the pair is for is the A→B on *one* arm:
//!   `append_256x16_gcheap` before and after `buffer_ensure_longlived`
//!   grows off the bump top, with `append_256x16_arena` beside it as the
//!   control that should not move.
//! - **What hashing costs**, now that it is rapidhash rather than FNV-1a
//!   and the seed may come from a static rather than a constant
//!   (`src/hash/seed.rs`). Nothing here had a hash bench before.
//!
//! Read `dev/BENCHMARKS.md` before believing any number from this file.
//! The rules that bite hardest: measure both arms back to back in one
//! sitting, run A→B→A and void the run if the two A's disagree, discard
//! the first measurement, and treat anything under 3% as unresolvable on
//! this box.
//!
//! ```
//! cargo bench --bench strings                          # rc-walk (default)
//! cargo bench --bench strings --no-default-features    # rc-trace
//! ```

use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};
use ll_model::hash::hash_bytes;
use ll_model::memory::arena::Arena;
use ll_model::memory::context::{LLContext, ll_arena_reset};
use ll_model::object::ll_entity_die;
use ll_model::refcount::{MemoryCategory, RcHeader, ll_release};
use ll_model::string::{
    LLString, ll_string_append, ll_string_new, ll_string_new_dynamic, string_bytes,
};

/// Lengths chosen at the hash's branch boundaries rather than at
/// "realistic" sizes: 8 and 16 are the ends of the short arm, 17 is the
/// first input to reach the 16-byte cascade, 113 the first to enter the
/// bulk loop, and 1024 is far enough in for the loop to dominate. A short
/// identifier and a long body cover what a PHP program actually hashes.
const HASH_LENGTHS: [usize; 6] = [8, 16, 17, 64, 113, 1024];

/// One append, in bytes. Small enough that the loop crosses a capacity
/// boundary often, which is the case the in-place path exists for.
const APPEND_CHUNK: usize = 16;

/// Appends per iteration. At 16 bytes each this walks a payload from
/// nothing to 4 KiB, crossing every growth step on the way.
const APPEND_COUNT: usize = 256;

fn filled(len: usize) -> Vec<u8> {
    (0..len)
        .map(|i| (i as u8).wrapping_mul(167).wrapping_add(13))
        .collect()
}

/// The hash alone, over the bytes of a slice already in cache.
fn hashing(c: &mut Criterion) {
    let mut group = c.benchmark_group("strings/hash");
    for len in HASH_LENGTHS {
        let input = filled(len);
        group.throughput(Throughput::Bytes(len as u64));
        group.bench_with_input(BenchmarkId::from_parameter(len), &input, |b, input| {
            b.iter(|| hash_bytes(black_box(input)))
        });
    }
    group.finish();
}

/// Create an inline string, read its lazily-computed hash once, and tear
/// it down — the shape of a temporary used as an array key.
///
/// Creation and hashing are timed together on purpose: separating them
/// measures a sequence no program performs. The GC heap rather than the
/// arena, because an arena string is reclaimed only by a reset, and a loop
/// that creates without resetting grows the arena without bound — at
/// criterion's iteration counts that is gigabytes, and it showed up as a
/// 60% spread before this was fixed.
fn create_and_hash(c: &mut Criterion) {
    let input = filled(24);

    c.bench_function("strings/new_inline_gcheap_hash_die", |b| {
        b.iter(|| unsafe {
            let s = ll_string_new(
                std::ptr::null_mut(),
                MemoryCategory::GcHeap,
                black_box(&input),
            );
            let h = LLString::hash(s);

            if ll_release(s as *mut RcHeader) {
                ll_entity_die(s as *mut RcHeader);
            }
            black_box(h)
        })
    });
}

/// The append loop in the request arena, where `buffer_ensure` extends the
/// payload in place whenever it is the last chunk bumped and there is room
/// left in the block. This arm is the one that already has the
/// optimization.
///
/// **Reclamation is inside the timed region**, here and in the GC-heap arm
/// below, so the two measure the same span of a payload's life. Leaving it
/// out would also leak 4 KiB per iteration, which over a criterion run is
/// hundreds of megabytes and its own distortion.
fn append_in_arena(c: &mut Criterion) {
    let chunk = filled(APPEND_CHUNK);
    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };

    c.bench_function("strings/append_256x16_arena", |b| {
        b.iter(|| unsafe {
            let s = ll_string_new_dynamic(&mut ctx, MemoryCategory::RequestArena, &[], 0);
            for _ in 0..APPEND_COUNT {
                ll_string_append(&mut ctx, s, black_box(&chunk));
            }
            let len = string_bytes(s as *const LLString).len();

            ll_arena_reset(&mut ctx);
            black_box(len)
        })
    });
}

/// The same loop on a GC-heap string, whose payload lives in the buffer
/// arena. `buffer_ensure_longlived` reallocates and copies at every growth
/// step even when the payload is the last chunk bumped in the current
/// block. **This is the arm the bump-top optimization changes**, and the
/// A→B is this number against itself, not against the arena arm.
fn append_in_heap(c: &mut Criterion) {
    let chunk = filled(APPEND_CHUNK);

    c.bench_function("strings/append_256x16_gcheap", |b| {
        b.iter(|| unsafe {
            let s = ll_string_new_dynamic(std::ptr::null_mut(), MemoryCategory::GcHeap, &[], 0);
            for _ in 0..APPEND_COUNT {
                ll_string_append(std::ptr::null_mut(), s, black_box(&chunk));
            }
            let len = string_bytes(s as *const LLString).len();

            if ll_release(s as *mut RcHeader) {
                ll_entity_die(s as *mut RcHeader);
            }
            black_box(len)
        })
    });
}

criterion_group!(
    benches,
    hashing,
    create_and_hash,
    append_in_arena,
    append_in_heap
);
criterion_main!(benches);
