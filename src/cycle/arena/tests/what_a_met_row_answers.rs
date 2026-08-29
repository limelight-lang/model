//! The read-only twin of `meet`, and both of its refusals. What it must
//! never do is answer with a row that carries no verdict of this
//! collection's: the scan reads a colour through it and frees on what it
//! reads, so a row from an unzeroed group is the last tenant's opinion
//! about somebody else's entity.

use super::*;
use crate::cycle::shadow::{self, Colour, RowArray};
use crate::memory::block_pool::test_guard;

/// The slots of `block`, which is the index space `met_row` bounds
/// against.
fn slots_of(block: *mut u8) -> u32 {
    unsafe { crate::memory::heap::collector_block_slots(block) }
}

/// One met slot in a block, and every other slot of it answering that it
/// has no row. The group init zeroes eight rows at a time, so the slots
/// beside the met one have readable rows and it is the colour that
/// refuses them; the rest are refused by their group's bit.
#[test]
fn only_the_slot_the_trace_met_carries_a_row() {
    let _g = test_guard();
    let (_heap, _slot, block) = an_entity_block();
    let slots = slots_of(block);
    assert!(slots > 2 * shadow::GROUP, "the block holds several groups");

    let mut arena = ShadowArena::new();
    let met_index = 3;
    let row = met(unsafe { arena.meet(slot_row(block, met_index), 7) });
    assert_eq!(shadow::colour(unsafe { *row }), Colour::Met);

    let with_a_row = (0..slots)
        .filter(|&index| unsafe { met_row(slot_row(block, index)) }.is_some())
        .count();
    assert_eq!(
        with_a_row, 1,
        "one entity was met, so one slot of the block carries a row"
    );
    assert_eq!(
        unsafe { met_row(slot_row(block, met_index)) },
        Some(row),
        "and it is the slot that was met, at the row the meeting answered"
    );

    arena.reset();
}

/// A row array over memory this collection never wrote, which is what an
/// unzeroed group holds: the arena hands out recycled pool blocks, so
/// those rows carry the last tenant's colours and one of those colours
/// is a verdict.
///
/// The buffer is filled with `0xEE` the way `shadow`'s own tests fill
/// theirs — a word no init could write — and hung off a real block,
/// because a row is found through the block and not through the arena.
#[test]
fn a_row_whose_group_was_never_zeroed_is_not_a_met_row() {
    let _g = test_guard();
    let (_heap, _slot, block) = an_entity_block();
    let slots = slots_of(block);
    assert!(slots > 2 * shadow::GROUP, "the block holds several groups");

    let mut buffer =
        vec![0xEEEE_EEEE_EEEE_EEEEu64; shadow::bytes_for(slots).div_ceil(size_of::<u64>())];
    // One borrow of the buffer, reused: a second `as_mut_ptr` retags it
    // and invalidates the first pointer, which is a Miri failure of the
    // fixture rather than of the code under it.
    let array = buffer.as_mut_ptr() as *mut RowArray;
    unsafe {
        shadow::init(
            array,
            block,
            slots,
            crate::cycle::row::Population::Slotted,
            std::ptr::null_mut(),
        );
        crate::memory::heap::set_block_shadow(block, array as *mut u8);
    }

    let dirty = slot_row(block, shadow::GROUP);
    assert!(
        unsafe { met_row(dirty) }.is_none(),
        "the group's bit is clear, so the row under it is the last tenant's"
    );

    unsafe { shadow::meet_group(array, dirty.index) };
    assert!(
        unsafe { met_row(dirty) }.is_none(),
        "and once the group is zeroed the colour is what says the trace never came"
    );

    unsafe { crate::memory::heap::clear_block_shadow(block) };
}
