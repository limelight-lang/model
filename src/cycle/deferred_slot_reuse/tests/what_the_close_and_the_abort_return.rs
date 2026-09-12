//! What the close gives back, and that the abort gives back the same.
//!
//! The close pops every withheld return through `ll_free`, so a slot reaches
//! its free list, a retained survivor its count word, a pooled block the pool
//! and a run the operating system — each exactly once, whoever owned the block
//! at the death, and to a live owner through the block's cross-thread stack.
//! The abort is the same pop with both allocation paths refusing.

use super::*;

/// A collection that withheld nothing reads no slot at its close.
///
/// What the probe answers is the size of the close, and this is its zero: the
/// stack is popped once per withheld death and a collection that withheld none
/// touches no slot at all. A close that walked a block it had stamped would
/// read one here.
#[test]
fn a_collection_that_withheld_nothing_reads_no_slot() {
    let _guard = test_guard();

    // Two classes, so the row and the death stand in blocks of their own: the
    // collection meets one block and the death happens in the other.
    let met = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
    let victim = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE * 2) };
    assert!(!met.is_null() && !victim.is_null());
    let met = unsafe { live_entity(met, 1) };
    let victim = unsafe { dead_entity(victim) };
    let victim_block = block_of(victim);

    let mut window = ActiveTrace::open().expect("the pool funds the trace window");
    unsafe { ensure_row(window.arena(), met, 1) };
    assert!(
        unsafe { crate::memory::heap::block_shadow(victim_block) }.is_null(),
        "the row stamped the other class's block, not this one"
    );

    unsafe { crate::memory::stdapi::ll_free(victim as *mut u8) };
    assert_eq!(
        deferred_slot_count(),
        0,
        "the death stands in a block this collection never met"
    );

    let _ = take_slots_popped();
    drop(window);
    assert_eq!(
        take_slots_popped(),
        0,
        "a collection that withheld nothing reads no slot at its close"
    );

    unsafe { crate::refcount::set_header_refcount(met, 0) };
    unsafe { crate::memory::stdapi::ll_free(met as *mut u8) };
}

/// The close returns a withheld entity slot, which is what makes the
/// withholding a deferral rather than a leak: the slot's block loses an
/// occupant and the class hands the address out again, the state word reading
/// alike on both sides of the close.
///
/// The victim stands in a size class of its own so that the block it is in
/// is the only one of that class and the allocation below is the block's own
/// free list answering. A second entity keeps that block off the pool, which
/// is what makes the readings after the close the block's own and not a
/// stranger's; the block that empties entirely is
/// `the_close_returns_a_withheld_retained_survivor`'s.
#[test]
fn the_close_returns_a_withheld_slot() {
    let _guard = test_guard();
    let held_before = gc_blocks();

    let keeper = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE * 3) };
    assert!(!keeper.is_null());
    let keeper = unsafe { live_entity(keeper, 1) };
    let victim = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE * 3) };
    assert!(!victim.is_null());
    let victim = unsafe { live_entity(victim, 1) };
    let block = block_of(victim);
    assert_eq!(
        block,
        block_of(keeper),
        "both entities are in the one block of their class"
    );

    let mut window = ActiveTrace::open().expect("the pool funds the trace window");
    let row = unsafe { ensure_row(window.arena(), victim, 1) };
    assert!(!row.is_null());

    let occupied_before = unsafe { crate::memory::heap::block_occupancy(block) };

    unsafe { crate::refcount::set_header_refcount(victim, 0) };
    unsafe { crate::memory::stdapi::ll_free(victim as *mut u8) };
    assert_eq!(
        unsafe { crate::refcount::slot_state(victim) },
        crate::refcount::SlotState::DeadInPlace,
        "the death was withheld"
    );

    let _ = take_slots_popped();
    drop(window);

    assert_eq!(
        unsafe { crate::refcount::slot_state(victim) },
        crate::refcount::SlotState::DeadInPlace,
        "the slot is neither live nor free, which is the whole of what the word \
         says on either side of a return; the return itself is the block's \
         count below"
    );
    assert_eq!(
        unsafe { crate::memory::heap::block_occupancy(block) },
        occupied_before - 1,
        "the return went through the owner's `used`, which is what retires a block"
    );
    assert_eq!(
        take_slots_popped(),
        1,
        "the close read the one slot it withheld and no other"
    );
    assert_eq!(
        gc_blocks(),
        held_before,
        "and the close that made it drew nothing"
    );

    let served = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE * 3) };
    assert_eq!(
        served, victim as *mut u8,
        "the slot is the class's again: the return reached the free list rather \
         than only clearing the bit"
    );
    unsafe { dead_entity(served) };
    unsafe { crate::memory::stdapi::ll_free(served) };
    unsafe { crate::refcount::set_header_refcount(keeper, 0) };
    unsafe { crate::memory::stdapi::ll_free(keeper as *mut u8) };
}

/// The close returns a withheld retained survivor through the count word, and
/// the block that empties by it retires to the pool.
#[test]
fn the_close_returns_a_withheld_retained_survivor() {
    let _guard = test_guard();
    let (_arena, [holder], [survivor], block) = unsafe { retained_survivors::<1>() };

    // One live entity of the holder's own class, so that the holder's block
    // is still this thread's when its slot is read back after the close. The
    // block that does empty here is the retained one, and that is what the
    // kind word below reads.
    let holder_block = block_of(holder);
    let (_, holder_stride, _) =
        unsafe { crate::memory::heap::entity_block_slot_bounds(holder_block) };
    let keeper = unsafe { crate::memory::heap::entity_alloc(holder_stride) };
    assert!(!keeper.is_null());
    let keeper = unsafe { live_entity(keeper, 1) };
    assert_eq!(
        holder_block,
        block_of(keeper),
        "the keeper stands in the holder's own block"
    );

    let held_before = gc_blocks();

    let mut window = ActiveTrace::open().expect("the pool funds the trace window");
    unsafe { ensure_row(window.arena(), survivor, 1) };
    unsafe { ensure_row(window.arena(), holder as *mut RcHeader, 1) };

    unsafe {
        assert!(crate::refcount::ll_release(holder as *mut RcHeader));
        ll_object_die(holder);
    }

    assert_eq!(
        unsafe { crate::refcount::slot_state(survivor) },
        crate::refcount::SlotState::DeadInPlace,
        "the survivor's slot is neither live nor free"
    );
    assert_eq!(
        deferred_slot_count(),
        2,
        "the stack holds the survivor and the holder, which is what says both \
         deaths were withheld: the header reads the same for a return this \
         window never made"
    );

    drop(window);

    assert_eq!(
        unsafe { crate::refcount::slot_state(holder as *const RcHeader) },
        crate::refcount::SlotState::DeadInPlace,
        "the close returned the holder's own slot, whose block it did not retire; \
         the state word reads alike on both sides of that return, `ll_free` \
         taking the slot again as it makes it"
    );
    // The survivor's own header is not read back: its block reached the pool
    // at the return, and reading a slot of a returned block is what the close
    // itself is forbidden to do. The kind word is the pool's own, written by
    // the return.
    assert_eq!(
        unsafe { crate::memory::block_pool::load_block_kind(&raw const (*block).kind) },
        BLOCK_KIND_FREE,
        "the survivor's return spent the last hold, so the block went to the pool"
    );
    assert_eq!(
        gc_blocks(),
        held_before,
        "and the close that made both returns drew nothing"
    );

    unsafe { crate::refcount::set_header_refcount(keeper, 0) };
    unsafe { crate::memory::stdapi::ll_free(keeper as *mut u8) };
}

/// A retained block holding two withheld survivors comes back once: the close
/// returns both, and the second return spends the block's last occupant
/// count, which is what hands the block to the pool.
#[test]
fn the_close_returns_two_withheld_survivors_of_one_block() {
    let _guard = test_guard();
    let (_arena, holders, survivors, block) = unsafe { retained_survivors::<2>() };
    let held_before = gc_blocks();

    let mut window = ActiveTrace::open().expect("the pool funds the trace window");
    for index in 0..2 {
        unsafe { ensure_row(window.arena(), survivors[index], 1) };
        unsafe { ensure_row(window.arena(), holders[index] as *mut RcHeader, 1) };
    }

    for holder in holders {
        unsafe {
            assert!(crate::refcount::ll_release(holder as *mut RcHeader));
            ll_object_die(holder);
        }
    }

    for survivor in survivors {
        assert_eq!(
            unsafe { crate::refcount::slot_state(survivor) },
            crate::refcount::SlotState::DeadInPlace,
            "both deaths were withheld"
        );
    }

    assert_eq!(
        unsafe { crate::memory::retained::held_occupant_count(block as usize) },
        2,
        "and neither has been counted down: the withheld return is what owes the decrement"
    );

    drop(window);

    assert_eq!(
        unsafe { crate::memory::block_pool::load_block_kind(&raw const (*block).kind) },
        BLOCK_KIND_FREE,
        "the close returned both survivors and gave the block back once"
    );
    assert_eq!(gc_blocks(), held_before, "and drew nothing to do it");
}

/// The close returns a withheld pooled large entity: its block goes back to the
/// pool at the close rather than at the next collection.
#[test]
fn the_close_returns_a_withheld_pooled_large_entity() {
    let _guard = test_guard();
    let entity = crate::memory::large_entity::alloc(crate::memory::heap::MAX_SMALL + 16);
    assert!(!entity.is_null());
    let entity = unsafe { dead_entity(entity) };
    let block = BlockHeader::of_ptr(entity as *const u8);

    let held_before = gc_blocks();
    let mut window = ActiveTrace::open().expect("the pool funds the trace window");
    unsafe { ensure_row(window.arena(), entity, 0) };

    unsafe { crate::memory::stdapi::ll_free(entity as *mut u8) };
    assert_eq!(
        unsafe { crate::refcount::slot_state(entity) },
        crate::refcount::SlotState::DeadInPlace,
        "the entity's slot is neither live nor free"
    );
    assert_eq!(
        deferred_slot_count(),
        1,
        "the stack holds it, which is what says the death was withheld: the \
         header reads the same for a return this window never made"
    );

    drop(window);

    assert_eq!(
        unsafe { crate::memory::block_pool::load_block_kind(&raw const (*block).kind) },
        BLOCK_KIND_FREE,
        "the close returned the block the withheld entity held"
    );
    assert_eq!(gc_blocks(), held_before, "and the close drew nothing");
}

/// The close returns a withheld OS-direct run: the registry loses the entry and
/// the mapping goes back to the operating system.
#[test]
fn the_close_returns_a_withheld_run() {
    let _guard = test_guard();
    let entity = crate::memory::large_entity::alloc(crate::memory::block_pool::BLOCK_PAYLOAD + 1);
    assert!(!entity.is_null());
    let entity = unsafe { dead_entity(entity) };
    let block = BlockHeader::of_ptr(entity as *const u8) as usize;
    assert!(crate::memory::large_entity::snapshot().contains(&block));

    let held_before = gc_blocks();
    let mut window = ActiveTrace::open().expect("the pool funds the trace window");
    unsafe { ensure_row(window.arena(), entity, 0) };

    unsafe { crate::memory::stdapi::ll_free(entity as *mut u8) };
    assert_eq!(
        unsafe { crate::refcount::slot_state(entity) },
        crate::refcount::SlotState::DeadInPlace,
        "the entity's slot is neither live nor free"
    );
    assert_eq!(
        deferred_slot_count(),
        1,
        "the stack holds it, which is what says the death was withheld: the \
         header reads the same for a return this window never made, and past \
         the close the run's memory is the operating system's and cannot be \
         read at all"
    );

    drop(window);

    assert!(
        !crate::memory::large_entity::snapshot().contains(&block),
        "the close unmapped the run the withheld entity held"
    );
    assert_eq!(gc_blocks(), held_before, "and the close drew nothing");
}

/// The close of a window that withheld a slot of a **live** owner's block gives
/// that slot back to the owner, through the block's own stack of cross-thread
/// frees.
///
/// The owner fills one block of its class to capacity and holds every slot,
/// so its next allocation has nowhere to go but that stack
/// (`crate::memory::heap::Heap::alloc_block_full`). A close that dropped the
/// stacked slot, or returned it to the tracing thread's own heap, leaves the
/// owner allocating out of a second block instead.
#[test]
fn the_close_returns_a_withheld_slot_to_a_live_owner() {
    use std::sync::mpsc;

    let _guard = test_guard();
    let (to_tracer, from_owner) = mpsc::channel::<usize>();
    let (to_owner, from_tracer) = mpsc::channel::<()>();

    let owner = std::thread::spawn(move || {
        assert!(
            crate::memory::heap::ll_thread_init(),
            "the pool served the second thread"
        );

        let mut held = unsafe { fill_one_block(ENTITY_SIZE) };
        let victim = held.swap_remove(held.len() / 2) as *mut RcHeader;
        to_tracer
            .send(victim as usize)
            .expect("the tracer is waiting");
        from_tracer.recv().expect("the tracer closed its window");

        let served = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
        assert_eq!(
            served, victim as *mut u8,
            "the block is full, so an allocation answering with anything but \
             the withheld slot is a slot the close lost"
        );

        unsafe { dead_entity(served) };
        unsafe { crate::memory::stdapi::ll_free(served) };
        for slot in held {
            let slot = slot as *mut RcHeader;
            unsafe { crate::refcount::set_header_refcount(slot, 0) };
            unsafe { crate::memory::stdapi::ll_free(slot as *mut u8) };
        }

        crate::memory::heap::ll_thread_exit();
    });

    let victim = from_owner.recv().expect("the owner handed out a slot") as *mut RcHeader;
    let block = block_of(victim);
    assert!(
        !unsafe { crate::memory::heap::block_is_owned_by_this_thread(block) },
        "the block belongs to the thread that is still holding it"
    );

    let mut window = ActiveTrace::open().expect("the pool funds the trace window");
    let row = unsafe { ensure_row(window.arena(), victim, 1) };
    assert!(!row.is_null());

    let occupancy_before = unsafe { crate::memory::heap::block_occupancy(block) };

    unsafe { crate::refcount::set_header_refcount(victim, 0) };
    unsafe { crate::memory::stdapi::ll_free(victim as *mut u8) };
    assert_eq!(
        unsafe { crate::refcount::slot_state(victim) },
        crate::refcount::SlotState::DeadInPlace,
        "the death in a stamped foreign block was withheld"
    );
    assert_eq!(
        unsafe { crate::memory::heap::block_occupancy(block) },
        occupancy_before,
        "and the owner cannot hand the slot out again, never having heard of \
         the death"
    );

    let _ = take_slots_popped();
    drop(window);
    assert_eq!(
        take_slots_popped(),
        1,
        "the close read the one stacked slot and no other"
    );
    assert_eq!(
        unsafe { crate::refcount::slot_state(victim) },
        crate::refcount::SlotState::DeadInPlace,
        "and made the return, which the owner's own allocation is what shows"
    );

    to_owner.send(()).expect("the owner is waiting");
    owner.join().expect("the owner thread finished");
}

/// A block another thread's when one of its slots died, and this thread's by
/// the time the next slot of it dies, returns each of the two exactly once.
///
/// `Heap::adopt` runs on the ordinary refill path, so ownership moves inside
/// an open window and one block ends up with two slots on the window's stack,
/// pushed under different owners. What the case pins is that the owner decides
/// nothing here: the close pops both through `ll_free`, which reads the owner
/// word itself and sends each return down the path that word names.
#[test]
fn a_block_adopted_after_a_slot_of_it_was_stacked_returns_each_slot_once() {
    const CLASS: usize = ENTITY_SIZE * 5;

    let _guard = test_guard();

    // Two live occupants of one abandoned block, in a class nothing else
    // fills, so this thread's first allocation of that class adopts it.
    let pair = std::thread::spawn(|| {
        assert!(
            crate::memory::heap::ll_thread_init(),
            "the pool served the second thread"
        );
        let first = unsafe { crate::memory::heap::entity_alloc(CLASS) };
        let second = unsafe { crate::memory::heap::entity_alloc(CLASS) };
        assert!(!first.is_null() && !second.is_null());
        let pair = [
            unsafe { live_entity(first, 1) } as usize,
            unsafe { live_entity(second, 1) } as usize,
        ];
        crate::memory::heap::ll_thread_exit();
        pair
    })
    .join()
    .expect("the second thread finished");

    let before_adoption = pair[0] as *mut RcHeader;
    let after_adoption = pair[1] as *mut RcHeader;
    let block = block_of(before_adoption);
    assert_eq!(
        block,
        block_of(after_adoption),
        "the two occupants share one block"
    );

    let mut window = ActiveTrace::open().expect("the pool funds the trace window");
    unsafe { ensure_row(window.arena(), before_adoption, 1) };
    assert!(
        !unsafe { crate::memory::heap::block_shadow(block) }.is_null(),
        "the row stamped the block both deaths stand in"
    );

    unsafe { crate::refcount::set_header_refcount(before_adoption, 0) };
    unsafe { crate::memory::stdapi::ll_free(before_adoption as *mut u8) };

    // The adoption: this thread holds no block of the class, and the
    // abandoned one is already carved for it
    // (`crate::memory::heap::Heap::adopt`).
    let keeper = unsafe { crate::memory::heap::entity_alloc(CLASS) };
    assert!(!keeper.is_null());
    let keeper = unsafe { live_entity(keeper, 1) };
    assert_eq!(
        block_of(keeper),
        block,
        "the refill carved a fresh block rather than adopting the abandoned one"
    );
    assert!(
        unsafe { crate::memory::heap::block_is_owned_by_this_thread(block) },
        "the adoption did not move the owner word"
    );

    unsafe { crate::refcount::set_header_refcount(after_adoption, 0) };
    unsafe { crate::memory::stdapi::ll_free(after_adoption as *mut u8) };
    assert_eq!(
        deferred_slot_count(),
        2,
        "one stack holds both slots of the block, whoever owned it at each death"
    );

    let occupancy_before = unsafe { crate::memory::heap::block_occupancy(block) };
    drop(window);

    assert_eq!(
        unsafe { crate::memory::heap::block_occupancy(block) },
        occupancy_before - 2,
        "the close did not return the two withheld slots exactly once between them"
    );
    for (slot, name) in [
        (before_adoption, "pre-adoption"),
        (after_adoption, "post-adoption"),
    ] {
        assert_eq!(
            unsafe { crate::refcount::slot_state(slot) },
            crate::refcount::SlotState::DeadInPlace,
            "the close left the {name} slot as `ll_free` holds it"
        );
    }

    // The free list of a block whose slots were returned twice hands one
    // address out twice, or closes into a cycle.
    let first = unsafe { crate::memory::heap::entity_alloc(CLASS) };
    let second = unsafe { crate::memory::heap::entity_alloc(CLASS) };
    assert!(!first.is_null() && !second.is_null());
    assert_ne!(first, second, "one slot reached the free list twice");

    for slot in [first, second] {
        unsafe { dead_entity(slot) };
        unsafe { crate::memory::stdapi::ll_free(slot) };
    }

    unsafe { crate::refcount::set_header_refcount(keeper, 0) };
    unsafe { crate::memory::stdapi::ll_free(keeper as *mut u8) };
}

/// The abort path returns what was withheld as the ordered close does, and it does so
/// with both allocation paths refusing: the close asks no allocation path,
/// which is what lets a collection that ran out of memory give its withheld
/// returns back.
///
/// The abort is staged the way a collection gives up — a batch detached and
/// never disposed of, rows standing over the block — so the drop runs its
/// whole order, the batch's merge included (`queue::merge_candidates`), rather
/// than the bare close the success case makes.
#[test]
fn an_aborted_window_returns_its_withheld_slots() {
    let _guard = test_guard();
    let held_before = gc_blocks();

    let keeper = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE * 3) };
    assert!(!keeper.is_null());
    let keeper = unsafe { live_entity(keeper, 1) };
    let victim = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE * 3) };
    assert!(!victim.is_null());
    let victim = unsafe { live_entity(victim, 1) };
    let block = block_of(victim);

    let mut window = ActiveTrace::open().expect("the pool funds the trace window");
    window.detach_candidates();
    unsafe { ensure_row(window.arena(), victim, 1) };

    let occupied_before = unsafe { crate::memory::heap::block_occupancy(block) };
    unsafe { crate::refcount::set_header_refcount(victim, 0) };
    unsafe { crate::memory::stdapi::ll_free(victim as *mut u8) };
    assert_eq!(
        unsafe { crate::refcount::slot_state(victim) },
        crate::refcount::SlotState::DeadInPlace,
        "the death was withheld"
    );

    crate::memory::critical::drain_for_test();
    let oom = force_oom();
    let _ = crate::test_support::allocation_probe::take_allocations();
    drop(window);
    let (heap, _pool) = crate::test_support::allocation_probe::take_allocations();
    drop(oom);

    assert_eq!(
        heap, 0,
        "the abort's returns stand on memory the thread already holds"
    );
    assert_eq!(
        unsafe { crate::refcount::slot_state(victim) },
        crate::refcount::SlotState::DeadInPlace,
        "the abort returned the withheld slot, which the block's count below shows"
    );
    assert_eq!(
        unsafe { crate::memory::heap::block_occupancy(block) },
        occupied_before - 1,
        "through the owner's `used`, as the ordered close does"
    );
    assert_eq!(
        gc_blocks(),
        held_before,
        "and the abort drew nothing to make the return"
    );

    unsafe { crate::refcount::set_header_refcount(keeper, 0) };
    unsafe { crate::memory::stdapi::ll_free(keeper as *mut u8) };
}

/// The abort returns a withheld retained survivor, and the block it empties
/// reaches the pool with both allocation paths refusing.
#[test]
fn an_aborted_window_returns_a_withheld_retained_survivor() {
    let _guard = test_guard();
    let (_arena, [holder], [survivor], block) = unsafe { retained_survivors::<1>() };

    let mut window = ActiveTrace::open().expect("the pool funds the trace window");
    window.detach_candidates();
    unsafe { ensure_row(window.arena(), survivor, 1) };
    unsafe { ensure_row(window.arena(), holder as *mut RcHeader, 1) };

    unsafe {
        assert!(crate::refcount::ll_release(holder as *mut RcHeader));
        ll_object_die(holder);
    }

    assert_eq!(
        unsafe { crate::refcount::slot_state(survivor) },
        crate::refcount::SlotState::DeadInPlace,
        "the survivor's slot is neither live nor free"
    );
    assert_eq!(
        deferred_slot_count(),
        2,
        "the stack holds the survivor and the holder, which is what says both \
         deaths were withheld: the header reads the same for a return this \
         window never made"
    );

    crate::memory::critical::drain_for_test();
    let oom = force_oom();
    let _ = crate::test_support::allocation_probe::take_allocations();
    drop(window);
    let (heap, _pool) = crate::test_support::allocation_probe::take_allocations();
    drop(oom);

    assert_eq!(
        heap, 0,
        "the retained return stands on memory the thread already holds"
    );
    assert_eq!(
        unsafe { crate::memory::block_pool::load_block_kind(&raw const (*block).kind) },
        BLOCK_KIND_FREE,
        "the abort spent the last hold and gave the block back"
    );
}

/// The abort returns a withheld pooled large entity, whose block goes back to
/// a pool that is refusing every draw.
#[test]
fn an_aborted_window_returns_a_withheld_large_entity() {
    let _guard = test_guard();
    let entity = crate::memory::large_entity::alloc(crate::memory::heap::MAX_SMALL + 16);
    assert!(!entity.is_null());
    let entity = unsafe { dead_entity(entity) };
    let block = BlockHeader::of_ptr(entity as *const u8);

    let mut window = ActiveTrace::open().expect("the pool funds the trace window");
    window.detach_candidates();
    unsafe { ensure_row(window.arena(), entity, 0) };

    unsafe { crate::memory::stdapi::ll_free(entity as *mut u8) };
    assert_eq!(
        unsafe { crate::refcount::slot_state(entity) },
        crate::refcount::SlotState::DeadInPlace,
        "the entity's slot is neither live nor free"
    );
    assert_eq!(
        deferred_slot_count(),
        1,
        "the stack holds it, which is what says the death was withheld: the \
         header reads the same for a return this window never made"
    );

    crate::memory::critical::drain_for_test();
    let oom = force_oom();
    let _ = crate::test_support::allocation_probe::take_allocations();
    drop(window);
    let (heap, _pool) = crate::test_support::allocation_probe::take_allocations();
    drop(oom);

    assert_eq!(
        heap, 0,
        "the large return stands on memory the thread already holds"
    );
    assert_eq!(
        unsafe { crate::memory::block_pool::load_block_kind(&raw const (*block).kind) },
        BLOCK_KIND_FREE,
        "the abort gave the block the withheld entity held back"
    );
}
