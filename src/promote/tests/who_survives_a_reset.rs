//! Survival comes from the escape count rather than from a
//! remembered set of holder slots: a reset with no escapes returns
//! every block, an escapee survives with the count its live holders
//! justify, and the children behind it come out with it. The counter
//! is what makes a stale entry impossible — an overwritten slot
//! leaves no survivor behind, and a holder that died before the
//! reset already dropped its count, so nothing dereferences a freed
//! slot.

use super::*;

#[test]
fn no_escapes_returns_every_block() {
    let _g = crate::memory::block_pool::test_guard();
    let cls = ClassBuilder::new("Temp").prop("x", true).build();

    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };
    let obj = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::RequestArena) };
    let block = BlockHeader::of_ptr(obj as *const u8);

    unsafe { arena_reset_full(&mut arena) };

    // The block went home: a fresh arena must get it back.
    let mut second = Arena::new();
    let p = second.alloc(8);
    assert_eq!(BlockHeader::of_ptr(p), block);
    second.reset(|_| {});
}

#[test]
fn escaped_object_survives_with_exact_count_and_retained_block() {
    let _g = crate::memory::block_pool::test_guard();
    let cls = ClassBuilder::new("Session").prop("x", true).build();
    let holder_cls = ClassBuilder::new("Cache").prop("last", true).build();

    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };
    let holder = unsafe { new_constructed(&mut ctx, holder_cls, MemoryCategory::GcHeap) };
    let obj = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::RequestArena) };

    unsafe { store_prop(&mut arena, holder, 16, obj) };
    let block = BlockHeader::of_ptr(obj as *const u8);
    assert_eq!(
        unsafe { (*block).kind.load(Ordering::Relaxed) },
        BLOCK_KIND_ARENA
    );

    unsafe { arena_reset_full(&mut arena) };

    assert_eq!(
        unsafe { crate::refcount::entity_category(obj) },
        MemoryCategory::GcHeap,
        "recategorized in place"
    );
    assert_eq!(
        unsafe { crate::refcount::entity_refcount(obj) },
        1,
        "exactly the one external reference"
    );
    assert_eq!(
        unsafe { crate::refcount::entity_flags(obj) } & ARENA_RESET_MARK,
        0,
        "transient mark cleared"
    );
    assert_eq!(
        unsafe { (*block).kind.load(Ordering::Relaxed) },
        BLOCK_KIND_RETAINED
    );

    // The survivor is an ordinary counted object now: its one
    // reference is the holder's slot, so the holder's death
    // releases it and cascades into the survivor's own teardown.
    unsafe {
        assert!(crate::refcount::ll_release(holder as *mut RcHeader));
        ll_object_die(holder);
    }
}

/// An arena referent behind a surviving reference box outlives the
/// reset, and comes out of it with exactly one holder.
///
/// **What carries it is the escape count, since the box moved to the
/// heap**: storing an arena object into a heap box is a
/// crossing, so the object is an escapee in its own right and the
/// reset promotes it from the escapee log. The test was written for a
/// different mechanism — promotion gated recursion on `is_object`, so
/// every other kind was a leaf and the arena object behind an *arena*
/// `&` was never marked, dying with the reset while a promoted box
/// still pointed at it. That configuration cannot be built any more,
/// because no box is an arena entity; the assertions below are worth
/// keeping for the survival and the count, not as a guard on the
/// recursion.
#[test]
fn a_surviving_reference_box_carries_its_referent() {
    let _g = crate::memory::block_pool::test_guard();
    let cls = ClassBuilder::new("Node").prop("x", true).build();
    let holder_cls = ClassBuilder::new("Cache").prop("last", true).build();

    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };
    let holder = unsafe { new_constructed(&mut ctx, holder_cls, MemoryCategory::GcHeap) };
    let target = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::RequestArena) };
    let r = crate::reference::ll_reference_new();

    unsafe {
        assert!(ref_store(
            &mut arena,
            r as *mut RcHeader,
            &raw mut (*r).value,
            std::ptr::null_mut(),
            Value::entity(Tag::Object, target as *mut RcHeader),
        ));
        // The heap holder takes the box, which is what keeps the box
        // — and through it the referent — reachable past the reset.
        let slot = Object::prop_at(holder, 16);
        assert!(ref_store(
            &mut arena,
            holder as *mut RcHeader,
            slot,
            std::ptr::null_mut(),
            Value::entity(Tag::Reference, r as *mut RcHeader),
        ));
    }

    unsafe { arena_reset_full(&mut arena) };

    assert_eq!(
        unsafe { crate::refcount::entity_category(target) },
        MemoryCategory::GcHeap,
        "the referent stayed behind in the dying arena"
    );
    assert_eq!(
        unsafe { crate::refcount::entity_refcount(target) },
        1,
        "the box's slot is its one holder"
    );
}

#[test]
fn internal_edges_survive_and_are_counted() {
    let _g = crate::memory::block_pool::test_guard();
    let node = ClassBuilder::new("Node").prop("next", true).build();
    let holder_cls = ClassBuilder::new("Root").prop("head", true).build();

    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };
    let holder = unsafe { new_constructed(&mut ctx, holder_cls, MemoryCategory::GcHeap) };
    let a = unsafe { new_constructed(&mut ctx, node, MemoryCategory::RequestArena) };
    let b = unsafe { new_constructed(&mut ctx, node, MemoryCategory::RequestArena) };

    unsafe {
        store_prop(&mut arena, a, 16, b); // arena→arena: no logs
        store_prop(&mut arena, holder, 16, a); // escape of a (and b transitively)
        arena_reset_full(&mut arena);
    }

    unsafe {
        assert_eq!(crate::refcount::entity_category(a), MemoryCategory::GcHeap);
        assert_eq!(crate::refcount::entity_category(b), MemoryCategory::GcHeap);
        assert_eq!(
            crate::refcount::entity_refcount(a),
            1,
            "one external reference"
        );
        assert_eq!(
            crate::refcount::entity_refcount(b),
            1,
            "one internal edge from a"
        );
    }
}

#[test]
fn overwritten_slot_is_stale_and_only_the_final_target_survives() {
    let _g = crate::memory::block_pool::test_guard();
    let cls = ClassBuilder::new("Val").build();
    let holder_cls = ClassBuilder::new("One").prop("v", true).build();

    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };
    let holder = unsafe { new_constructed(&mut ctx, holder_cls, MemoryCategory::GcHeap) };
    let a = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::RequestArena) };
    let b = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::RequestArena) };

    unsafe {
        store_prop(&mut arena, holder, 16, a); // logged
        store_prop(&mut arena, holder, 16, b); // same slot, logged again
        arena_reset_full(&mut arena);
    }

    unsafe {
        assert_eq!(crate::refcount::entity_category(b), MemoryCategory::GcHeap);
        assert_eq!(
            crate::refcount::entity_refcount(b),
            1,
            "deduplicated: one slot, one count"
        );
        // `a` is not a survivor at all: the second store spent its
        // escape count, and the fixpoint skips a log entry whose
        // `IS_ESCAPEE` is already clear. It dies with the arena.
    }
}

/// Regression for the remembered-set dangle (C2): a heap holder can die
/// before the arena resets. The old design logged holder *slots* and
/// read them back at reset, so a freed holder's slot was dereferenced
/// (and its stale contents re-counted). The escape counter never reads
/// a slot: the holder's teardown already dropped the count (`lose`), so
/// reset sees the true, live external count.
#[test]
fn holder_death_before_reset_neither_dangles_nor_miscounts() {
    let _g = crate::memory::block_pool::test_guard();
    let holder_cls = ClassBuilder::new("Box").prop("v", true).build();
    let val_cls = ClassBuilder::new("Val").build();

    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };
    let h1 = unsafe { new_constructed(&mut ctx, holder_cls, MemoryCategory::GcHeap) };
    let h2 = unsafe { new_constructed(&mut ctx, holder_cls, MemoryCategory::GcHeap) };
    let a = unsafe { new_constructed(&mut ctx, val_cls, MemoryCategory::RequestArena) };

    unsafe {
        // A escapes into two heap holders: hold-count 2.
        store_prop(&mut arena, h1, 16, a);
        store_prop(&mut arena, h2, 16, a);
        assert_eq!(crate::refcount::entity_refcount(a), 2, "two heap holders");

        // H1 dies before reset. Its teardown drops the count (lose) and
        // frees its memory — including the slot that held A. The old
        // slot-based reset would read that freed slot and re-count A to
        // 2; the counter leaves the count at exactly 1.
        assert!(crate::refcount::ll_release(h1 as *mut RcHeader));
        ll_object_die(h1);
        assert_eq!(
            crate::refcount::entity_refcount(a),
            1,
            "H1's death dropped the count"
        );

        arena_reset_full(&mut arena);

        // A survived (H2 holds it), promoted with exactly one
        // reference, and no freed slot was ever dereferenced.
        assert_eq!(
            crate::refcount::entity_category(a),
            MemoryCategory::GcHeap,
            "promoted"
        );
        assert_eq!(
            crate::refcount::entity_refcount(a),
            1,
            "exactly H2's reference, not two"
        );

        // H2 dies for real: A cascades to teardown.
        assert!(crate::refcount::ll_release(h2 as *mut RcHeader));
        ll_object_die(h2);
    }
}
