//! A heap counts the blocks on its owned chains net of the ones it gives
//! back (`release-on-heap-growth`, `dev/plans/S65.md`, S65.43): a fresh block
//! and an adopted one raise the count, a block past the class's
//! `empty_reserve` going to the pool lowers it, and a block kept as the
//! reserve stays counted. The poll reads the entity heap's figure alone.

use super::*;

/// An uncommon class, so that no other case's abandoned block of it is
/// adopted here.
const CLASS: usize = 3072;

/// Case g of the Sage's ruling: the count is the owned chains' length through
/// a draw, a retire into the reserve, a return past it and an adoption. Red
/// with draws counted instead of the net.
#[test]
fn the_count_is_the_blocks_owned_net_of_the_returns() {
    let _g = crate::memory::block_pool::test_guard();
    let mut heap = Heap::new_entity();
    let mut slots = vec![heap.alloc(CLASS)];
    assert_eq!(heap.blocks_owned, 1, "a fresh block");
    while heap.blocks_owned < 2 {
        slots.push(heap.alloc(CLASS));
    }

    // The second block's one slot freed: the block empties into the class's
    // reserve and stays owned.
    let second = slots.pop().unwrap();
    unsafe { heap.free(second) };
    assert_eq!(heap.blocks_owned, 2, "the reserve's block stays counted");
    // The first block emptied with the reserve taken: it goes to the pool.
    for slot in slots {
        unsafe { heap.free(slot) };
    }
    assert_eq!(heap.blocks_owned, 1, "a block past the reserve goes back");

    let mut donor = Heap::new_entity();
    let held = donor.alloc(CLASS);
    donor.abandon_all();
    let mut adopter = Heap::new_entity();
    let adopted = adopter.alloc(CLASS);
    assert_eq!(
        adopted as usize & !BLOCK_MASK,
        held as usize & !BLOCK_MASK,
        "the abandoned block is adopted"
    );
    assert_eq!(adopter.blocks_owned, 1, "an adoption");
    unsafe {
        adopter.free(adopted);
        adopter.free(held);
    }
}

/// The thread's raw heap drawing blocks leaves the entity heap's count
/// unmoved.
#[test]
fn the_raw_heaps_draws_are_not_counted() {
    let _g = crate::memory::block_pool::test_guard();
    let before = entity_blocks_owned();
    let raw: Vec<*mut u8> = (0..2 * BLOCK_PAYLOAD / CLASS)
        .map(|_| unsafe { crate::memory::stdapi::ll_malloc(CLASS) })
        .collect();
    assert_eq!(entity_blocks_owned(), before);
    for pointer in raw {
        unsafe { crate::memory::stdapi::ll_free(pointer) };
    }
}
