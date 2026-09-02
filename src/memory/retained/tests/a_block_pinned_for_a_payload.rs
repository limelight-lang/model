//! A block retained for a payload the reset could not carry out waits
//! for that payload's own free the way it waits for an occupant's
//! death — and the pin is a count, one block being able to hold
//! several survivors' payloads.

use super::*;

/// A block retained for a payload it could not carry out outlives
/// its occupants: their deaths say nothing about bytes they do not
/// own.
#[test]
fn a_pinned_block_outlives_its_occupants() {
    let _g = crate::memory::block_pool::test_guard();
    let (block, cells, live) = walkable_index(1);
    unsafe { pin(block) };
    let _empty = unsafe {
        live[0].write(1);
        register(block, &cells, list_room(block, 1))
    };

    assert!(
        !unsafe { occupant_freed(block) },
        "a pinned block was handed back"
    );
    assert_eq!(kind_of(block), BLOCK_KIND_RETAINED);
    assert_eq!(
        unsafe { pin_count(block) },
        1,
        "registration cleared the pin set before it"
    );
    unsafe { live[0].write(0) };
    assert!(
        unsafe { payload_freed(block) },
        "the pin was the last thing holding the block"
    );
    give_back(block);
}

/// The payload's own free is the event the block was waiting for, so
/// a block held for bytes alone goes home when they are freed. Before
/// this the pin was permanent and the block was out of circulation
/// for the life of the process; the test was seen failing on
/// `payload_freed` answering false.
#[test]
fn a_freed_payload_empties_the_block_it_pinned() {
    let _g = crate::memory::block_pool::test_guard();
    let (block, cells, live) = walkable_index(1);
    unsafe { pin(block) };
    let _empty = unsafe {
        live[0].write(1);
        register(block, &cells, list_room(block, 1))
    };

    assert!(
        !unsafe { occupant_freed(block) },
        "the payload still holds it"
    );
    assert!(
        unsafe { payload_freed(block) },
        "the last holder of the block died"
    );
    unsafe { live[0].write(0) };
    give_back(block);
    assert_eq!(
        kind_of(block),
        BLOCK_KIND_FREE,
        "the block outlived the payload it was pinned for"
    );
}

/// One block can hold the payloads of several survivors, so the pin
/// is a count and every payload has to report. Seen failing with the
/// count as a flag: the first free released a block the second
/// payload was still living in.
#[test]
fn a_block_pinned_for_two_payloads_waits_for_both() {
    let _g = crate::memory::block_pool::test_guard();
    let (block, _cells, _live) = walkable_index(1);
    unsafe { pin(block) };
    unsafe { pin(block) };
    let _empty = unsafe { register(block, &[], std::ptr::null_mut()) };

    assert!(
        !unsafe { payload_freed(block) },
        "one payload still lives there"
    );
    assert!(unsafe { payload_freed(block) }, "both are gone now");
    assert_eq!(
        unsafe { pin_count(block) },
        0,
        "a block whose payloads are gone still reports a hold"
    );
    give_back(block);
}
