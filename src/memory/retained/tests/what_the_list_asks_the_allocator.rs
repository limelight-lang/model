//! What a survivor list asks of the allocator: nothing. The list is
//! written into memory the arena already holds, so publishing it reaches
//! neither the global allocator nor the block pool, and the ledger of the
//! blocks collection owns does not move for a reset that lists.

use super::*;

/// Publishing a block's survivor list reaches neither the global
/// allocator nor the block pool. Seen red on the registry it replaced:
/// two global allocations, the `Arc<[usize]>` and a tree node.
#[test]
fn registering_a_survivor_list_reaches_no_global_allocator() {
    let _g = crate::memory::block_pool::test_guard();
    let (block, cells, live) = walkable_index(3);
    for cell in &live {
        unsafe { cell.write(1) };
    }

    let room = list_room(block, 3);
    crate::test_support::allocation_probe::take_allocations();
    let _empty = unsafe { register(block, &cells, room) };
    let (heap, pool) = crate::test_support::allocation_probe::take_allocations();
    assert_eq!(
        (heap, pool),
        (0, 0),
        "publishing a survivor list reached the allocator: {heap} global, {pool} pool"
    );

    for cell in &live {
        unsafe { cell.write(0) };
    }

    assert!(!unsafe { occupant_freed(block) });
    assert!(!unsafe { occupant_freed(block) });
    assert!(unsafe { occupant_freed(block) });
    give_back(block);
}

/// A reset that publishes a survivor list moves no figure of the ledger
/// of the blocks collection owns: the list is the arena's memory and not
/// `gc_metadata`'s, in either of its two tiers or in a fresh pool block.
#[test]
fn a_reset_that_lists_moves_no_figure_of_the_ledger() {
    use crate::memory::Arena;
    use crate::memory::context::LLContext;
    use crate::object::{ll_object_die, new_constructed};
    use crate::refcount::{MemoryCategory, RcHeader};
    let _g = crate::memory::block_pool::test_guard();

    let survivor_class = crate::class::ClassBuilder::new("LedgerListedSurvivor").build();
    let holder_class = crate::class::ClassBuilder::new("LedgerListedHolder")
        .prop("member", true)
        .build();
    let mut arena = Arena::new();
    let mut context = LLContext { arena: &mut arena };
    let holder = unsafe { new_constructed(&mut context, holder_class, MemoryCategory::GcHeap) };
    let survivor =
        unsafe { new_constructed(&mut context, survivor_class, MemoryCategory::RequestArena) };
    unsafe { crate::test_support::store_prop(&mut arena, holder, 16, survivor) };
    let block = BlockHeader::of_ptr(survivor as *const u8) as usize;

    crate::memory::gc_metadata::lower_thread_peak_to_current();
    let before = crate::memory::gc_metadata::thread_stats();
    unsafe { crate::promote::arena_reset_full(&mut arena) };
    let after = crate::memory::gc_metadata::thread_stats();
    assert_eq!(
        after, before,
        "the reset moved the ledger of the blocks collection owns"
    );
    assert!(
        unsafe { has_survivor_list(block) },
        "the reset published no list, so this test proves nothing"
    );

    unsafe {
        assert!(crate::refcount::ll_release(holder as *mut RcHeader));
        ll_object_die(holder);
    }
    assert_eq!(
        kind_of(block),
        BLOCK_KIND_FREE,
        "the survivor's death did not return its block"
    );
}
