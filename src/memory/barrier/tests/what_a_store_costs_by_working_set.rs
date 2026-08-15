//! Measurement probe, not a correctness test: what one publish costs in
//! the shape lowering emits — the micro-ops inlined and `owner_cat` a
//! constant — and how much of that cost belongs to the working set rather
//! than to the barrier.
//!
//! `benches/barrier.rs` answers neither question, by construction. A bench
//! is a separate crate, so every micro-op is reached through a call the
//! optimizer keeps; and each of its arms publishes one child a thousand
//! times, so every store reads a header line the store before it has just
//! written. Compiled PHP filling a thousand slots with a thousand children
//! has neither the call nor that chain (`dev/BENCHMARKS.md`, "the store
//! barrier's three directions, and the arena's logging inside them").
//!
//! Two working sets separate them: one child, which is the harness's shape,
//! and `WIDE` children, which spreads consecutive stores over as many header
//! lines. What differs between the two is the chain; what they share is the
//! store.
//!
//! ```
//! cargo test --release --lib -- --ignored measure_store_cost --nocapture
//! cargo test --release --lib --no-default-features -- --ignored measure_store_cost --nocapture
//! ```
//!
//! The figures obey `dev/BENCHMARKS.md`'s Method and are **not** comparable
//! with the harness table in that file: a different binary out of a
//! different profile. What may be compared is one shape against another
//! inside one run.

use std::time::{Duration, Instant};

use super::*;
use crate::object::{Object, new_constructed};
use crate::promote::arena_reset_full;
use crate::value::Tag;
use crate::{Class, ClassBuilder};

/// Publishes per timed region: enough that the two clock reads around the
/// region cost well under a percent of it.
const STORES: usize = 1_000;

/// The wide working set. A power of two, so the cursor over it is one AND
/// in both shapes — the narrow shape pays the same instruction rather than
/// none, and the difference between the shapes stays the header lines.
const WIDE: usize = 64;

/// Timed rounds per shape. The first is warm-up and is discarded, the
/// median of the rest is reported (`dev/BENCHMARKS.md`, Method).
const ROUNDS: usize = 5;

/// A class with one Box property at offset 16 — the slot every shape
/// publishes into.
fn holder_class(name: &str) -> *const Class {
    ClassBuilder::new(name).prop("value", true).build()
}

/// A class with no properties: the children the shapes move around.
fn leaf_class(name: &str) -> *const Class {
    ClassBuilder::new(name).build()
}

/// `n` constructed entities of one category, as the values a slot names.
///
/// # Safety
/// `ctx` mounts the arena the arena-category entities come from.
unsafe fn children(
    ctx: *mut LLContext,
    class: *const Class,
    category: MemoryCategory,
    n: usize,
) -> Vec<Value> {
    (0..n)
        .map(|_| {
            let child = unsafe { new_constructed(ctx, class, category) };
            Value::entity(Tag::Object, child as *mut RcHeader)
        })
        .collect()
}

/// Median nanoseconds per store over `ROUNDS` timed rounds, the first
/// round discarded.
fn median_ns_per_store(mut round: impl FnMut() -> Duration) -> f64 {
    let mut taken: Vec<f64> = Vec::with_capacity(ROUNDS);
    for r in 0..=ROUNDS {
        let elapsed = round();
        if r > 0 {
            taken.push(elapsed.as_nanos() as f64 / STORES as f64);
        }
    }

    taken.sort_by(|a, b| a.partial_cmp(b).expect("a duration is never NaN"));
    taken[taken.len() / 2]
}

/// One round of the arena→arena publish: the retain returns early on the
/// category, nothing is logged, and the slot write is the work.
///
/// # Safety
/// `ctx` mounts `arena`; both classes outlive the call.
unsafe fn arena_into_arena(
    ctx: *mut LLContext,
    arena: *mut Arena,
    holder: *const Class,
    leaf: *const Class,
    mask: usize,
) -> Duration {
    unsafe {
        let owner = new_constructed(ctx, holder, MemoryCategory::RequestArena);
        let slot = Object::prop_at(owner, 16);
        let values = children(ctx, leaf, MemoryCategory::RequestArena, mask + 1);

        let start = Instant::now();
        for i in 0..STORES {
            assert!(store_box(
                arena,
                MemoryCategory::RequestArena,
                slot,
                values[i & mask]
            ));
        }

        let elapsed = start.elapsed();
        // Owner, children and the log are all the arena's, so the reset
        // takes them and hands the blocks back.
        arena_reset_full(arena);
        elapsed
    }
}

/// One round of the heap→arena publish: one release-at-reset record per
/// store, and the retain each record owes.
///
/// # Safety
/// As [`arena_into_arena`].
unsafe fn heap_into_arena(
    ctx: *mut LLContext,
    arena: *mut Arena,
    holder: *const Class,
    leaf: *const Class,
    mask: usize,
) -> Duration {
    unsafe {
        let values = children(ctx, leaf, MemoryCategory::GcHeap, mask + 1);
        let owner = new_constructed(ctx, holder, MemoryCategory::RequestArena);
        let slot = Object::prop_at(owner, 16);

        let start = Instant::now();
        for i in 0..STORES {
            assert!(store_box(
                arena,
                MemoryCategory::RequestArena,
                slot,
                values[i & mask]
            ));
        }

        let elapsed = start.elapsed();
        // The reset owns one release per record it logged, which returns
        // each child to the creation reference this round holds.
        arena_reset_full(arena);
        for value in &values {
            drop_ref(MemoryCategory::GcHeap, value.entity_ptr());
        }

        elapsed
    }
}

/// One round of the arena→heap publish: the first store of a child logs it
/// as an escapee, the rest raise its hold-count.
///
/// # Safety
/// As [`arena_into_arena`].
unsafe fn arena_into_heap(
    ctx: *mut LLContext,
    arena: *mut Arena,
    holder: *const Class,
    leaf: *const Class,
    mask: usize,
) -> Duration {
    unsafe {
        let owner = new_constructed(ctx, holder, MemoryCategory::GcHeap);
        let slot = Object::prop_at(owner, 16);
        let values = children(ctx, leaf, MemoryCategory::RequestArena, mask + 1);

        let start = Instant::now();
        for i in 0..STORES {
            assert!(store_box(
                arena,
                MemoryCategory::GcHeap,
                slot,
                values[i & mask]
            ));
        }

        let elapsed = start.elapsed();
        // Every store raised a hold-count that no slot owns once the next
        // store displaced it. Give all of them back and clear the slot, or
        // the reset finds held escapees and promotes them — and the next
        // round would then measure a heap→heap store.
        for i in 0..STORES {
            drop_ref(MemoryCategory::GcHeap, values[i & mask].entity_ptr());
        }

        assert!(store_box(
            arena,
            MemoryCategory::GcHeap,
            slot,
            Value::null()
        ));
        arena_reset_full(arena);
        drop_ref(MemoryCategory::GcHeap, owner as *mut RcHeader);
        elapsed
    }
}

#[test]
#[ignore = "measurement probe; run explicitly with --ignored (release mode)"]
fn measure_store_cost() {
    let _g = crate::memory::block_pool::test_guard();
    let holder = holder_class("StoreCostOwner");
    let leaf = leaf_class("StoreCostChild");
    let mut arena = Arena::new();
    let arena_ptr: *mut Arena = &mut arena;
    let mut context = LLContext { arena: arena_ptr };
    let context_ptr: *mut LLContext = &mut context;

    for set in [1usize, WIDE] {
        let mask = set - 1;
        let into_arena = median_ns_per_store(|| unsafe {
            arena_into_arena(context_ptr, arena_ptr, holder, leaf, mask)
        });
        let heap_in = median_ns_per_store(|| unsafe {
            heap_into_arena(context_ptr, arena_ptr, holder, leaf, mask)
        });
        let escape = median_ns_per_store(|| unsafe {
            arena_into_heap(context_ptr, arena_ptr, holder, leaf, mask)
        });
        println!(
            "store_cost working_set={set}: arena_into_arena={into_arena:.3} \
             heap_into_arena={heap_in:.3} arena_into_heap={escape:.3} ns/store"
        );
    }
}
