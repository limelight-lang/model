//! A store publishes the new value and reports whether it did.
//! Giving back what it displaced is the caller's second call,
//! `drop_ref`, and only on a report of `true`; a `Value` slot and a
//! bare pointer slot compose the same way. A null store clears the
//! slot, and with an arena owner the displaced heap value's release
//! belongs to the reset log rather than to the store — exactly one
//! release either way.

use super::*;
use crate::refcount::entity_refcount;

#[test]
fn heap_to_heap_counts_and_writes_slot() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = Arena::new();
    let mut owner = Holder::new(MemoryCategory::GcHeap);
    let mut a = entity(MemoryCategory::GcHeap);
    let mut b = entity(MemoryCategory::GcHeap);
    // One pointer per entity, taken once: a second `&mut a` would
    // retag and invalidate the copy the slot is holding.
    let (pa, pb): (*mut RcHeader, *mut RcHeader) = (&mut a, &mut b);

    unsafe { owner.store(&mut arena, pa) };
    assert_eq!(owner.entity_ptr(), pa);
    assert_eq!(
        unsafe { crate::refcount::entity_refcount(pa) },
        2,
        "initial + the slot's reference"
    );

    unsafe { owner.store(&mut arena, pb) };
    assert_eq!(
        unsafe { crate::refcount::entity_refcount(pa) },
        1,
        "displaced from a heap slot: released now"
    );
    assert_eq!(unsafe { crate::refcount::entity_refcount(pb) }, 2);
}

/// The pointer-slot analog, driven by the micro-ops directly: an
/// 8-byte `*mut RcHeader` slot published by `store_ptr` (no drop on an
/// initializing store), then an overwrite as `store_ptr` + `drop_ref`.
#[test]
fn store_ptr_publishes_a_pointer_slot_then_drop_releases_the_old() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = Arena::new();
    let mut a = entity(MemoryCategory::GcHeap);
    let mut b = entity(MemoryCategory::GcHeap);
    let (pa, pb): (*mut RcHeader, *mut RcHeader) = (&mut a, &mut b);
    let mut slot: *mut RcHeader = std::ptr::null_mut();

    // Initializing store: publish only, no old to drop.
    assert!(unsafe { store_ptr(&mut arena, MemoryCategory::GcHeap, &mut slot, pa) });
    assert_eq!(slot, pa, "slot published as a bare 8-byte pointer");
    assert_eq!(
        unsafe { crate::refcount::entity_refcount(pa) },
        2,
        "initial + the slot's reference"
    );

    // Overwriting store: publish the new pointer, then drop the old.
    let old = slot;
    assert!(unsafe { store_ptr(&mut arena, MemoryCategory::GcHeap, &mut slot, pb) });
    unsafe { drop_ref(MemoryCategory::GcHeap, old) };
    assert_eq!(slot, pb);
    assert_eq!(
        unsafe { crate::refcount::entity_refcount(pa) },
        1,
        "displaced from a heap slot: released"
    );
    assert_eq!(unsafe { crate::refcount::entity_refcount(pb) }, 2);
}

#[test]
fn storing_null_clears_the_slot_without_double_release() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = Arena::new();
    let mut owner = Holder::new(MemoryCategory::RequestArena);
    let mut a = entity(MemoryCategory::GcHeap);

    unsafe { owner.store(&mut arena, &mut a) };
    unsafe { owner.store(&mut arena, std::ptr::null_mut()) };
    assert!(owner.entity_ptr().is_null());
    assert_eq!(
        unsafe { crate::refcount::entity_refcount(&raw mut a) },
        2,
        "the log still owns A's release"
    );

    arena.reset(|_| {});
    assert_eq!(
        unsafe { crate::refcount::entity_refcount(&raw mut a) },
        1,
        "exactly one release, from the log"
    );
}

/// A refused store writes nothing and gives back the reference it took.
///
/// The one refusal a publish has is the copy an arena COW value takes on its
/// way into a longer-lived holder; it is driven here the way
/// `the_owned_store`'s case drives it — a block budget of zero on a thread
/// whose GC heap has served nothing, so the copy's size class must ask the
/// pool — and the pool-request count names the allocation that was refused.
/// The count read back is what says the retain the store takes before the
/// copy is released again: the value stands at the one reference its creation
/// left it.
#[test]
fn a_refused_store_leaves_the_slot_and_the_count_alone() {
    let _g = crate::memory::block_pool::test_guard();

    let (answered, requests, slot_kept_the_holder, held_count, string_count) =
        std::thread::spawn(|| {
            assert!(
                crate::memory::heap::ll_thread_init(),
                "the pool served this thread"
            );
            let mut arena = Arena::new();
            let mut ctx = LLContext {
                arena: &raw mut arena,
            };
            let cow = unsafe {
                crate::string::ll_string_new(&raw mut ctx, MemoryCategory::RequestArena, b"name")
            } as *mut RcHeader;

            let mut held = entity(MemoryCategory::GcHeap);
            let ph: *mut RcHeader = &mut held;
            let mut slot: *mut RcHeader = ph;
            unsafe { crate::refcount::ll_retain(ph) };

            let _budgeted = crate::memory::block_pool::budget_blocks(0);
            // The class the copy will ask for, emptied with the copy's own
            // call, so that the refusal is a block draw and not the pool's
            // warmth (`fill_the_class_until_refused`).
            let filled = unsafe { fill_the_class_until_refused(&raw mut ctx, b"name") };
            let _ = crate::memory::block_pool::take_pool_requests();
            let answered =
                unsafe { store_ptr(&raw mut arena, MemoryCategory::GcHeap, &mut slot, cow) };
            let requests = crate::memory::block_pool::take_pool_requests();
            drop(_budgeted);
            for taken in filled {
                unsafe { crate::refcount::ll_release(taken) };
            }

            // Nothing is released back: `held` is a header on this frame and
            // owns no memory, and a non-final decrement of it would register a
            // candidate whose address lies in no block (`cycle::row`). The
            // string is the arena's and goes with it.
            (
                answered,
                requests,
                slot == ph,
                unsafe { entity_refcount(ph) },
                unsafe { entity_refcount(cow) },
            )
        })
        .join()
        .unwrap();

    assert!(!answered, "the copy could not be allocated");
    assert!(
        requests > 0,
        "the refusal is the pool's: the copy asked for a block"
    );
    assert!(slot_kept_the_holder, "the slot was not written");
    assert_eq!(held_count, 2, "the entity the slot holds keeps its count");
    assert_eq!(
        string_count, 1,
        "the retain the store took is given back on the refusal"
    );
}
