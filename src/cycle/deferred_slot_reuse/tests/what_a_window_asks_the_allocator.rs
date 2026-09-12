//! What a window asks of the allocator, which is nothing.
//!
//! The open stands on the workspace the thread already holds, a withheld
//! return stands in the dying entity, and the close asks neither allocation
//! path and spends no reserve — so the window opens and closes with both paths
//! refusing, charges no byte to the ledger, and the one refusal left is a
//! first collection with no workspace to draw, which does not open at all.

use super::*;

#[test]
fn a_trace_window_allocates_nothing_through_the_global_allocator() {
    let _guard = test_guard();
    let slot = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
    assert!(!slot.is_null());
    let dead = unsafe { dead_entity(slot) };

    let _ = crate::test_support::allocation_probe::take_allocations();
    let mut window = ActiveTrace::open().expect("the pool funds the trace window");
    let row = unsafe { ensure_row(window.arena(), dead, 0) };
    assert!(!row.is_null());
    unsafe { crate::memory::stdapi::ll_free(dead as *mut u8) };
    assert_eq!(deferred_slot_count(), 1);
    drop(window);

    let (heap, _pool) = crate::test_support::allocation_probe::take_allocations();
    assert_eq!(
        heap, 0,
        "the trace window reached the global allocator: open, withhold \
         and close are all manager-backed"
    );
}

#[test]
fn neither_the_window_nor_the_withheld_return_draws_a_manager_block() {
    let _guard = test_guard();
    let slot = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
    assert!(!slot.is_null());
    let dead = unsafe { dead_entity(slot) };

    let held_before = gc_blocks();
    let _ = crate::test_support::allocation_probe::take_allocations();
    let mut window = ActiveTrace::open().expect("this thread's workspace is in hand");
    let (heap, pool) = crate::test_support::allocation_probe::take_allocations();
    assert_eq!(
        (heap, pool),
        (0, 0),
        "the open stands on the workspace and asks no allocation path"
    );
    assert_eq!(gc_blocks(), held_before);

    // Stamped before the reading below opens, so what that reading covers is
    // the withheld return and not the row this collection needs to have met
    // the block at all.
    assert!(!unsafe { ensure_row(window.arena(), dead, 0) }.is_null());
    let _ = crate::test_support::allocation_probe::take_allocations();

    unsafe { crate::memory::stdapi::ll_free(dead as *mut u8) };
    let (heap, pool) = crate::test_support::allocation_probe::take_allocations();
    assert_eq!(
        (heap, pool),
        (0, 0),
        "and the withheld return is threaded through the dead entity itself"
    );
    assert_eq!(deferred_slot_count(), 1);

    drop(window);
    assert_eq!(gc_blocks(), held_before, "the close drew a block");
}

#[test]
fn an_aborted_window_returns_what_it_withheld_with_both_allocation_paths_refusing() {
    let _guard = test_guard();
    let first = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
    let second = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
    assert!(!first.is_null() && !second.is_null());
    let first = unsafe { dead_entity(first) };
    let second = unsafe { dead_entity(second) };

    let held_before = gc_blocks();
    let mut window = ActiveTrace::open().expect("this thread's workspace is in hand");
    unsafe { ensure_row(window.arena(), first, 0) };
    unsafe { crate::memory::stdapi::ll_free(first as *mut u8) };
    unsafe { crate::memory::stdapi::ll_free(second as *mut u8) };
    assert_eq!(deferred_slot_count(), 2);

    // The abort is a collection that gives up where memory ran out, so the path
    // is exercised with both allocation paths refusing: closing the window may
    // need no memory to return what it withheld.
    crate::memory::critical::drain_for_test();
    let oom = force_oom();
    let _ = crate::test_support::allocation_probe::take_allocations();
    drop(window);
    let (heap, _pool) = crate::test_support::allocation_probe::take_allocations();
    drop(oom);

    assert_eq!(heap, 0, "the abort path reached the global allocator");
    assert_eq!(
        deferred_slot_count(),
        0,
        "the window is closed, which is what a count of zero reads after a drop"
    );
    assert_eq!(
        gc_blocks(),
        held_before,
        "the abort drew nothing: both returns stood in the dying entities themselves"
    );

    let reused = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
    assert!(
        reused == first as *mut u8 || reused == second as *mut u8,
        "the abort lost a physical return"
    );
    unsafe { dead_entity(reused) };
    unsafe { crate::memory::stdapi::ll_free(reused) };
}

/// A thread that has collected once holds its workspace until it exits, and
/// the window stands in that workspace, so the ordinary allocation path
/// refusing everything takes no window down. The refusal this leaves is the
/// one a thread's *first* collection meets, which is the case below.
#[test]
fn a_window_opens_with_both_allocation_paths_refusing() {
    let _guard = test_guard();
    let slot = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
    assert!(!slot.is_null());
    // `test_guard` draws this thread's workspace before the case begins, which
    // is the state the claim is about: a thread that has collected once.
    let dead = unsafe { dead_entity(slot) };

    crate::memory::critical::drain_for_test();
    let oom = force_oom();
    let mut window = ActiveTrace::open().expect("the workspace was in hand before the refusal");
    assert!(!unsafe { ensure_row(window.arena(), dead, 0) }.is_null());
    unsafe { crate::memory::stdapi::ll_free(dead as *mut u8) };
    assert_eq!(deferred_slot_count(), 1, "and it withheld a return");
    drop(window);
    drop(oom);

    let reused = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
    assert_eq!(
        reused, dead as *mut u8,
        "the close lost the physical return"
    );
    unsafe { dead_entity(reused) };
    unsafe { crate::memory::stdapi::ll_free(reused) };
    crate::memory::critical::drain_for_test();
}

/// The one refusal left: a thread whose first collection cannot draw its
/// workspace. A shut window withholds nothing, which is what makes that
/// refusal answerable — the collection does not start and no slot is in hand.
///
/// On a thread of its own, because every other thread in this suite has
/// collected already and holds a workspace this case would find.
#[test]
fn a_first_collection_that_cannot_draw_a_workspace_does_not_open() {
    let _guard = test_guard();

    std::thread::spawn(|| {
        assert!(crate::memory::heap::ll_thread_init());
        let slot = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
        assert!(!slot.is_null());
        let dead = unsafe { dead_entity(slot) };

        // The workspace comes off the ordinary allocation path alone, so the
        // pool refusing is the whole of the refusal here.
        let oom = force_oom();
        let refused = ActiveTrace::open();
        assert!(
            refused.is_none(),
            "the window opened without a workspace the pool granted"
        );

        unsafe { crate::memory::stdapi::ll_free(dead as *mut u8) };
        assert_eq!(deferred_slot_count(), 0);
        drop(oom);

        let reused = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
        assert_eq!(reused, dead as *mut u8, "a shut window withheld a return");
        unsafe { dead_entity(reused) };
        unsafe { crate::memory::stdapi::ll_free(reused) };
    })
    .join()
    .unwrap();
}

#[test]
fn a_window_inside_the_workspace_region_charges_and_enters_no_byte() {
    let _guard = test_guard();
    let slot = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
    assert!(!slot.is_null());
    let dead = unsafe { dead_entity(slot) };

    let bytes_before = in_use_bytes();
    crate::memory::gc_metadata::lower_thread_peak_to_current();
    let window = ActiveTrace::open().expect("the pool funds the trace window");
    unsafe { crate::memory::stdapi::ll_free(dead as *mut u8) };
    assert_eq!(
        in_use_bytes(),
        bytes_before,
        "the append charged a byte; charging none is what keeps the ledger off \
         the free path"
    );

    drop(window);
    let after = crate::memory::gc_metadata::thread_stats();
    assert_eq!(after.current_bytes_in_use(), bytes_before);
    assert_eq!(
        after.peak_bytes_in_use(),
        bytes_before,
        "and enters none either: a control line that never left the workspace's \
         own region has no residue of its own to enter"
    );
}

/// The reserve exists for the pressure collection, where a refused pool is
/// what started the collection at all. A window that drew a block at its open
/// would spend the reserve on every such collection before a single return was
/// withheld; standing in the workspace, it spends none — and no death spends
/// any either, each being held in the dying entity's own memory
/// (`neither_the_window_nor_the_withheld_return_draws_a_manager_block`).
#[test]
fn a_windows_open_and_close_leave_the_critical_reserve_untouched() {
    let _guard = test_guard();
    let held_before = gc_blocks();
    let reserve_before = crate::memory::critical::blocks_held();
    assert!(
        reserve_before > 0,
        "the reserve has something to be spent here"
    );

    let oom = force_oom();
    let window = ActiveTrace::open().expect("the workspace was in hand before the refusal");
    assert_eq!(
        crate::memory::critical::blocks_held(),
        reserve_before,
        "the open asked neither allocation path"
    );
    assert_eq!(gc_blocks(), held_before);

    drop(window);
    drop(oom);
    assert_eq!(crate::memory::critical::blocks_held(), reserve_before);
    assert_eq!(gc_blocks(), held_before);
}
