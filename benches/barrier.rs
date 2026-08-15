//! What a reference store costs, and what the arena's logging costs
//! inside it.
//!
//! What the numbers answer (`dev/BENCHMARKS.md`, "the store barrier's
//! three directions, and the arena's logging inside them"): what the
//! arena's two write-side logs cost per store. Three directions of one
//! micro-op price them apart, because the direction is what decides
//! whether anything is logged:
//!
//! - **arena into arena** — the least work the barrier can do: a retain
//!   that returns early on a non-`GcHeap` category, a category
//!   comparison, and the write. Nothing is logged.
//! - **heap into arena** — one release-at-reset record appended per
//!   store, plus the retain the record owes.
//! - **arena into heap** — the escape: the first store sets `IS_ESCAPEE`
//!   and appends one escapee record, and every later store only
//!   increments the hold-count in the entity's own header.
//!
//! Two more arms split those costs further. One prices
//! `store_category_barrier` alone on the escape direction, which is the
//! bookkeeping without the slot write, so the difference from the publish
//! above is the rest of `store_box`: the retain and the write. The other
//! prices `ref_store`, the composition of `store_box` and `drop_ref` that
//! a Rust caller uses, against the bare publish on the arena→arena
//! direction.
//!
//! **The arena is reset between timed regions, and that is not optional.**
//! A log segment is carved out of the arena's own bump and is given back
//! only by `finish_reset`, so a harness that merely drains the records
//! keeps every segment: at one segment per 500 records, a criterion run
//! of a logging arm would take gigabytes and measure a condition no
//! program produces. The reset and the rebuilding of each arm's
//! arena-side objects therefore sit inside `iter_custom` and outside the
//! clock. The arena starts each region young, which is also the shape a
//! request arena has.
//!
//! **What these numbers are not.** They are not the cost of a store in
//! compiled PHP: lowering emits the micro-ops specialized to the slot's
//! kind and to a constant `owner_cat`, and inlines them, while this
//! harness calls them across a boundary the optimizer keeps. They are the
//! cost of the work inside the barrier, and the comparison between arms
//! is what they are for.
//!
//! **The two batch sizes vary the segment's amortised share**, there
//! being no arm of its own: the release-at-reset log takes one segment
//! every 500 records, so a per-store figure at 100 stores carries a
//! different share of that allocation than one at 1000. Whether the
//! difference rises above the noise floor is the reading's answer, not a
//! promise of this file.
//!
//! The numbers are in `dev/BENCHMARKS.md`, whose "Method" decides what a
//! reading has to carry; this file is the code only. Both
//! configurations:
//!
//! ```
//! cargo bench --bench barrier                          # rc-walk (default)
//! cargo bench --bench barrier --no-default-features    # rc-trace
//! ```

use std::time::{Duration, Instant};

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use ll_model::memory::arena::Arena;
use ll_model::memory::barrier::{drop_ref, ref_store, store_box, store_category_barrier};
use ll_model::memory::context::{LLContext, set_current_context};
use ll_model::object::{ll_object_new, object_constructed};
use ll_model::promote::arena_reset_full;
use ll_model::refcount::{MemoryCategory, RcHeader};
use ll_model::value::{Tag, Value};
use ll_model::{Class, ClassBuilder, Object};

/// Stores per timed region. Large enough that the clock read around the
/// region costs well under a percent of it, and a round number so the
/// per-store figure is read off without arithmetic.
const STORES: usize = 1000;

/// The second batch size. The difference in per-store cost between the
/// two is the log's segment allocation amortised (module doc).
const STORES_SMALL: usize = 100;

/// A class with one Box property at offset 16, which is the slot every
/// arm stores into.
fn holder_class(name: &str) -> *const Class {
    ClassBuilder::new(name).prop("value", true).build()
}

/// A class with no properties: the entity the arms move around.
fn leaf_class(name: &str) -> *const Class {
    ClassBuilder::new(name).build()
}

/// Create and construct one object, as generated code does in two calls.
///
/// # Safety
/// `ctx` mounts the arena this object may be allocated from.
unsafe fn object(
    ctx: *mut LLContext,
    class: *const Class,
    category: MemoryCategory,
) -> *mut Object {
    unsafe {
        let obj = ll_object_new(ctx, class, category);
        assert!(!obj.is_null(), "object creation refused: out of memory");
        assert!(object_constructed(ctx, obj), "construction refused");
        obj
    }
}

/// Time `region` over `iterations`, giving the arena back between one
/// region and the next.
///
/// `region` receives a context whose arena is empty, builds whatever it
/// needs from it, and answers two closures: the timed work, and the
/// teardown that leaves nothing of it for the reset to promote. The clock
/// covers the first and nothing else, so the reset, the rebuild and the
/// teardown are the harness's own cost, and the arena's blocks return to
/// the pool every region rather than accumulating with the logs (module
/// doc).
///
/// # Safety
/// Nothing built by `region` may outlive the reset that follows it, and
/// no class it uses may carry a destructor: a destructor is user code and
/// would resolve its own context.
unsafe fn timed_regions(
    ctx: *mut LLContext,
    arena: *mut Arena,
    iterations: u64,
    mut region: impl FnMut(*mut LLContext) -> (Box<dyn FnMut()>, Box<dyn FnOnce()>),
) -> Duration {
    let mut elapsed = Duration::ZERO;
    for _ in 0..iterations {
        let (mut work, teardown) = region(ctx);
        let start = Instant::now();
        work();
        elapsed += start.elapsed();
        drop(work);
        teardown();
        unsafe { arena_reset_full(arena) };
    }

    elapsed
}

/// The arena→arena publish: nothing logged, nothing retained — the
/// category test and the write.
fn arena_into_arena(c: &mut Criterion, ctx: *mut LLContext, arena: *mut Arena) {
    let holder = holder_class("BarrierArenaOwner");
    let leaf = leaf_class("BarrierArenaChild");

    c.bench_function("barrier/store_arena_into_arena_x1000", |b| {
        b.iter_custom(|iterations| unsafe {
            timed_regions(ctx, arena, iterations, |ctx| {
                let owner = object(ctx, holder, MemoryCategory::RequestArena);
                let child = object(ctx, leaf, MemoryCategory::RequestArena);
                let slot = Object::prop_at(owner, 16);
                let value = Value::entity(Tag::Object, child as *mut RcHeader);
                let arena = (*ctx).arena;
                let work = Box::new(move || {
                    for _ in 0..STORES {
                        assert!(
                            store_box(arena, MemoryCategory::RequestArena, slot, black_box(value)),
                            "the publish was refused"
                        );
                    }
                });
                // Nothing to undo: both entities are the arena's, and the
                // reset takes them with it.
                (
                    work as Box<dyn FnMut()>,
                    Box::new(|| {}) as Box<dyn FnOnce()>,
                )
            })
        })
    });
}

/// The same direction through the composition a Rust caller has:
/// `store_box` and then `drop_ref` of the displaced entity.
fn ref_store_arena_into_arena(c: &mut Criterion, ctx: *mut LLContext, arena: *mut Arena) {
    let holder = holder_class("BarrierRefStoreOwner");
    let leaf = leaf_class("BarrierRefStoreChild");

    c.bench_function("barrier/ref_store_arena_into_arena_x1000", |b| {
        b.iter_custom(|iterations| unsafe {
            timed_regions(ctx, arena, iterations, |ctx| {
                let owner = object(ctx, holder, MemoryCategory::RequestArena);
                let child = object(ctx, leaf, MemoryCategory::RequestArena);
                let slot = Object::prop_at(owner, 16);
                let entity = child as *mut RcHeader;
                let value = Value::entity(Tag::Object, entity);
                let arena = (*ctx).arena;
                // The slot is filled before the clock starts, so every
                // timed store displaces the same entity and the arm
                // measures the steady state rather than one empty slot.
                assert!(ref_store(
                    arena,
                    owner as *mut RcHeader,
                    slot,
                    std::ptr::null_mut(),
                    value
                ));
                let work = Box::new(move || {
                    for _ in 0..STORES {
                        assert!(
                            ref_store(
                                arena,
                                owner as *mut RcHeader,
                                slot,
                                entity,
                                black_box(value)
                            ),
                            "the store was refused"
                        );
                    }
                });
                (
                    work as Box<dyn FnMut()>,
                    Box::new(|| {}) as Box<dyn FnOnce()>,
                )
            })
        })
    });
}

/// The heap→arena publish: one release-at-reset record per store, and the
/// retain that record owes. The heap child outlives the arena, so it is
/// built once here; the reset between regions is what pays its records
/// back.
///
/// The only arm run at two batch sizes, so the stores per region and the
/// criterion `id` are the caller's (module doc).
fn heap_into_arena(
    c: &mut Criterion,
    ctx: *mut LLContext,
    arena: *mut Arena,
    stores: usize,
    id: &str,
) {
    let holder = holder_class("BarrierArenaHolder");
    let child = unsafe { object(ctx, leaf_class("BarrierHeapChild"), MemoryCategory::GcHeap) };
    let value = Value::entity(Tag::Object, child as *mut RcHeader);

    c.bench_function(id, |b| {
        b.iter_custom(|iterations| unsafe {
            timed_regions(ctx, arena, iterations, |ctx| {
                let owner = object(ctx, holder, MemoryCategory::RequestArena);
                let slot = Object::prop_at(owner, 16);
                let arena = (*ctx).arena;
                let work = Box::new(move || {
                    for _ in 0..stores {
                        assert!(
                            store_box(arena, MemoryCategory::RequestArena, slot, black_box(value)),
                            "the publish was refused"
                        );
                    }
                });
                // The records the region appended are the reset's to pay:
                // it releases the heap child once per record, back to the
                // creation reference this function holds.
                (
                    work as Box<dyn FnMut()>,
                    Box::new(|| {}) as Box<dyn FnOnce()>,
                )
            })
        })
    });
}

/// The arena→heap publish: the escape. The first store of a region
/// appends an escapee record, the rest increment the hold-count in the
/// child's header — and the reset that follows tears the escapee down
/// with the arena, since the heap owner's slot is cleared first.
fn arena_into_heap(c: &mut Criterion, ctx: *mut LLContext, arena: *mut Arena) {
    let owner = unsafe {
        object(
            ctx,
            holder_class("BarrierHeapOwner"),
            MemoryCategory::GcHeap,
        )
    };
    let leaf = leaf_class("BarrierEscapee");
    let slot = unsafe { Object::prop_at(owner, 16) };

    c.bench_function("barrier/store_arena_into_heap_x1000", |b| {
        b.iter_custom(|iterations| unsafe {
            timed_regions(ctx, arena, iterations, |ctx| {
                let child = object(ctx, leaf, MemoryCategory::RequestArena);
                let value = Value::entity(Tag::Object, child as *mut RcHeader);
                let arena = (*ctx).arena;
                let entity = child as *mut RcHeader;
                let work = Box::new(move || {
                    for _ in 0..STORES {
                        assert!(
                            store_box(arena, MemoryCategory::GcHeap, slot, black_box(value)),
                            "the publish was refused"
                        );
                    }
                });
                // Give the escape back before the reset, or the reset
                // finds a held escapee and promotes it — one heap entity
                // per region, and the next region would measure a
                // heap→heap store into the same slot.
                let teardown = Box::new(move || {
                    assert!(store_box(
                        arena,
                        MemoryCategory::GcHeap,
                        slot,
                        Value::null()
                    ));
                    for _ in 0..STORES {
                        drop_ref(MemoryCategory::GcHeap, entity);
                    }
                });
                (work as Box<dyn FnMut()>, teardown as Box<dyn FnOnce()>)
            })
        })
    });
}

/// The escape bookkeeping alone: the category comparison and the
/// hold-count, with no slot to write. What the publish costs on top of it
/// is the difference from `store_arena_into_heap`.
fn category_barrier_arena_into_heap(c: &mut Criterion, ctx: *mut LLContext, arena: *mut Arena) {
    let leaf = leaf_class("BarrierCategoryEscapee");

    c.bench_function("barrier/category_barrier_arena_into_heap_x1000", |b| {
        b.iter_custom(|iterations| unsafe {
            timed_regions(ctx, arena, iterations, |ctx| {
                let child = object(ctx, leaf, MemoryCategory::RequestArena);
                let entity = child as *mut RcHeader;
                let arena = (*ctx).arena;
                let work = Box::new(move || {
                    for _ in 0..STORES {
                        black_box(store_category_barrier(
                            arena,
                            MemoryCategory::GcHeap,
                            black_box(entity),
                        ));
                    }
                });
                // The hold-count this raised belongs to no slot, so it is
                // given back by hand; the reset then takes the entity
                // with the arena instead of promoting it.
                let teardown = Box::new(move || {
                    for _ in 0..STORES {
                        drop_ref(MemoryCategory::GcHeap, entity);
                    }
                });
                (work as Box<dyn FnMut()>, teardown as Box<dyn FnOnce()>)
            })
        })
    });
}

fn barrier(c: &mut Criterion) {
    // One arena and one context for every arm: criterion runs them
    // serially, and every arm resets the arena between its own regions,
    // so no arm inherits another's state.
    let mut arena = Arena::new();
    let arena_ptr: *mut Arena = &mut arena;
    let mut context = LLContext { arena: arena_ptr };
    let context_ptr: *mut LLContext = &mut context;
    set_current_context(context_ptr);

    arena_into_arena(c, context_ptr, arena_ptr);
    ref_store_arena_into_arena(c, context_ptr, arena_ptr);
    heap_into_arena(
        c,
        context_ptr,
        arena_ptr,
        STORES,
        "barrier/store_heap_into_arena_x1000",
    );
    heap_into_arena(
        c,
        context_ptr,
        arena_ptr,
        STORES_SMALL,
        "barrier/store_heap_into_arena_x100",
    );
    arena_into_heap(c, context_ptr, arena_ptr);
    category_barrier_arena_into_heap(c, context_ptr, arena_ptr);

    set_current_context(std::ptr::null_mut());
}

criterion_group!(benches, barrier);
criterion_main!(benches);
