//! A store publishes the new value and reports whether it did.
//! Giving back what it displaced is the caller's second call,
//! `drop_ref`, and only on a report of `true`; a `Value` slot and a
//! bare pointer slot compose the same way. A null store clears the
//! slot, and with an arena owner the displaced heap value's release
//! belongs to the reset log rather than to the store — exactly one
//! release either way.

use super::*;

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
    assert_eq!(a.refcount, 2, "the log still owns A's release");

    arena.reset(|_| {});
    assert_eq!(a.refcount, 1, "exactly one release, from the log");
}
