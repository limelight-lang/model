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
    pin(block);
    let _empty = unsafe {
        live[0].write(1);
        register(block, cells.clone())
    };

    assert!(!occupant_freed(block), "a pinned block was handed back");
    assert!(
        snapshot().iter().any(|&(b, _)| b == block),
        "registration cleared the pin set before it"
    );
    unsafe { live[0].write(0) };
    drop_index(block);
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
    pin(block);
    let _empty = unsafe {
        live[0].write(1);
        register(block, cells.clone())
    };

    assert!(!occupant_freed(block), "the payload still holds it");
    assert!(payload_freed(block), "the last holder of the block died");
    assert!(
        !snapshot().iter().any(|&(b, _)| b == block),
        "the index outlived the block it describes"
    );
    unsafe { live[0].write(0) };
}

/// One block can hold the payloads of several survivors, so the pin
/// is a count and every payload has to report. Seen failing with the
/// count as a flag: the first free released a block the second
/// payload was still living in.
#[test]
fn a_block_pinned_for_two_payloads_waits_for_both() {
    let _g = crate::memory::block_pool::test_guard();
    let (block, cells, _live) = walkable_index(1);
    pin(block);
    pin(block);
    let _empty = unsafe { register(block, Vec::new()) };

    assert!(!payload_freed(block), "one payload still lives there");
    assert!(payload_freed(block), "both are gone now");
    assert!(!payload_freed(block), "an unpinned block reports nothing");
    // The registry is process-global and a leaked cell's block
    // address can come up again in another test, so nothing is left
    // behind even on the paths where the assertions above hold.
    drop_index(block);
    let _ = cells;
}
