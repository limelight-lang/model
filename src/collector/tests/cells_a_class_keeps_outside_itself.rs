//! The concurrent collector over a class whose counted cells lie outside
//! the object: the racing walk records the edge, the drain severs it
//! through the class's own hook, and the storage behind the cells goes
//! back through the deferred queue rather than straight to the free list.
//!
//! The block has to be parkable for that last part to be true at all —
//! the epoch holds the addresses of cells inside it until Phase 3 has
//! re-read them — which is why the class draws it under the instance's
//! own category (`crate::test_support::outside_block`).

use super::*;
use crate::object::ll_entity_die;
use crate::test_support::{chunk_from_the_free_list, outside_block};

/// A ring closed through the block alone. The object's whole count is the
/// self-reference in a cell no run describes, so an epoch that walked
/// only the body would find the count unaccounted for and keep the object
/// as a computed root forever.
///
/// The block is followed through the epoch: parked while the epoch holds
/// the cell addresses it recorded, and back on the buffer arena's free
/// list once the flush has run. Both readings are the same probe under
/// critical pressure, where an allocation searches the free lists rather
/// than bumping.
///
/// The second cell holds a **live** child, which is what makes the
/// sever's own contract observable: the hook empties a cell and hands
/// what it held back undropped, and the drain drops it after the members
/// are freed. A hook that released it itself would kill a live entity, a
/// hook that yielded nothing would leak the count — one assertion sees
/// both, because the child's count is the test's own reference plus the
/// cell's.
#[test]
fn a_ring_closed_through_the_block_is_collected_and_its_block_parked() {
    let _g = crate::memory::block_pool::test_guard();
    DESTRUCTS.store(0, Ordering::Relaxed);

    let cls = outside_block::class("WakerRing");
    let held_cls = ClassBuilder::new("WakerRingHeld")
        .destructor(counting_destructor as *const ())
        .build();
    let mut arena = Arena::new();
    let arena_ptr: *mut Arena = &mut arena;
    let mut ctx = LLContext { arena: arena_ptr };
    let obj = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
    unsafe { outside_block::install_block(&mut ctx, obj) };
    let (block, capacity) = unsafe { outside_block::block_and_capacity(obj) };
    let held = unsafe { new_constructed(&mut ctx, held_cls, MemoryCategory::GcHeap) };

    unsafe {
        assert!(
            outside_block::store_cell(
                arena_ptr,
                obj,
                0,
                std::ptr::null_mut(),
                Value::entity(Tag::Object, obj as *mut RcHeader),
            ),
            "the barrier refused the ring this test is built on"
        );
        // The test keeps its own reference, so this child is outside the
        // garbage component rather than a member of it.
        assert!(outside_block::store_cell(
            arena_ptr,
            obj,
            1,
            std::ptr::null_mut(),
            Value::entity(Tag::Object, held as *mut RcHeader),
        ));
        assert!(
            !ll_release(obj as *mut RcHeader),
            "the cell holds the object now"
        );
    }

    stepped_epoch(); // mature

    let mut e = Epoch::open();
    checkpoint();
    e.snapshot();
    e.walk();
    e.judge();
    assert_eq!(
        e.stats.candidates, 1,
        "the walk did not record the edge the block holds"
    );

    e.condemn();
    checkpoint(); // the Phase 3 ack
    e.recheck_and_post();
    checkpoint(); // the Phase 4 drain: the object dies here

    // The drain has run: the external child kept the test's reference and
    // lost the cell's, exactly once. Two would mean the sever yielded
    // nothing, zero would mean it dropped what it was told to hand back —
    // and the destructor says which.
    assert_eq!(
        unsafe { (*(held as *mut RcHeader)).refcount },
        1,
        "the cell's reference was neither handed back nor dropped"
    );
    assert_eq!(
        DESTRUCTS.load(Ordering::Relaxed),
        0,
        "the sever dropped the child it was told to hand back"
    );

    // Mid-epoch the block is out of circulation: the epoch still holds
    // the addresses of the cells it recorded inside it.
    assert_ne!(
        chunk_from_the_free_list(capacity),
        block,
        "the block was recycled while the epoch still addressed it"
    );

    let stats = e.close();
    checkpoint(); // the post-epoch flush
    assert_eq!(stats.confirmed, 1, "the ring was not collected");
    assert_eq!(
        chunk_from_the_free_list(capacity),
        block,
        "the collected object's block was never given back"
    );

    unsafe {
        assert!(ll_release(held as *mut RcHeader));
        ll_entity_die(held as *mut RcHeader);
    }

    assert_eq!(DESTRUCTS.load(Ordering::Relaxed), 1);
    arena.reset(|_| {});
}

/// The re-check's own case: the mutator replaces the block between the
/// walk and Phase 3, so every cell address the epoch recorded lies in
/// storage the object has left. Comparing those words would compare a
/// frozen copy of the walk's reading against itself and confirm a
/// component on it, which is why the version is asked first — and asked
/// through the class, an object's `+8` being the class word rather than
/// an array head.
#[test]
fn a_block_that_moves_after_the_walk_acquits_the_component() {
    let _g = crate::memory::block_pool::test_guard();

    let cls = outside_block::class("WakerMoved");
    let mut arena = Arena::new();
    let arena_ptr: *mut Arena = &mut arena;
    let mut ctx = LLContext { arena: arena_ptr };
    let obj = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
    unsafe { outside_block::install_block(&mut ctx, obj) };

    unsafe {
        assert!(outside_block::store_cell(
            arena_ptr,
            obj,
            0,
            std::ptr::null_mut(),
            Value::entity(Tag::Object, obj as *mut RcHeader),
        ));
        assert!(!ll_release(obj as *mut RcHeader));
    }

    stepped_epoch(); // mature

    let walked = unsafe { outside_block::version_of(obj) };
    let mut e = Epoch::open();
    checkpoint();
    e.snapshot();
    e.walk();
    e.judge();
    assert_eq!(e.stats.candidates, 1, "the ring is garbage on the reading");

    e.condemn();
    checkpoint(); // the Phase 3 ack
    unsafe { outside_block::install_block(&mut ctx, obj) };
    assert_ne!(
        unsafe { outside_block::version_of(obj) },
        walked,
        "the move left the version the walk answered with"
    );

    e.recheck_and_post();
    checkpoint();
    let stats = e.close();
    checkpoint();
    assert_eq!(
        stats.confirmed, 0,
        "a component was confirmed on cells the object no longer holds"
    );

    // The ring is still a ring, and this is what breaks it: the store
    // drops the reference the cell held, which is the object's last.
    unsafe {
        assert!(outside_block::store_cell(
            arena_ptr,
            obj,
            0,
            obj as *mut RcHeader,
            Value::null(),
        ));
    }

    arena.reset(|_| {});
}

/// The walk's own give-up, which is the reading the plain walker has no
/// way to answer. A window left open is a block the mutator is moving, so
/// the racing walk yields no cell and names no version — and an entity
/// with no recorded out-edge is a root source, its count unaccounted for,
/// which is the safe direction: one epoch of leak and nothing freed
/// early.
///
/// The window is opened by hand because the stepped harness is one thread
/// playing both actors, so no walk of it ever runs inside a real one.
#[test]
fn a_walk_that_meets_an_open_window_gives_the_entity_up() {
    let _g = crate::memory::block_pool::test_guard();

    let cls = outside_block::class("WakerMidMove");
    let mut arena = Arena::new();
    let arena_ptr: *mut Arena = &mut arena;
    let mut ctx = LLContext { arena: arena_ptr };
    let obj = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
    unsafe { outside_block::install_block(&mut ctx, obj) };

    unsafe {
        assert!(outside_block::store_cell(
            arena_ptr,
            obj,
            0,
            std::ptr::null_mut(),
            Value::entity(Tag::Object, obj as *mut RcHeader),
        ));
        assert!(!ll_release(obj as *mut RcHeader));
    }

    stepped_epoch(); // mature

    unsafe { outside_block::open_window(obj) };
    let mut e = Epoch::open();
    checkpoint();
    e.snapshot();
    e.walk();
    e.judge();
    assert_eq!(
        e.stats.candidates, 0,
        "the walk strided a block the mutator was moving"
    );

    let stats = e.close();
    checkpoint();
    assert_eq!(stats.confirmed, 0);

    // Closed before the teardown, which reads the block through the same
    // members.
    unsafe {
        outside_block::close_window(obj);
        assert!(outside_block::store_cell(
            arena_ptr,
            obj,
            0,
            obj as *mut RcHeader,
            Value::null(),
        ));
    }

    arena.reset(|_| {});
}
