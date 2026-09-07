//! The chain across a segment boundary, which is where its two readers differ
//! and where a kept segment is either reused or lost.
//!
//! Every case writes an odd number of records over segments of two, so the
//! append position is always partly filled: a drain that sized the top segment
//! by its capacity instead of by the cursor would read a record nobody wrote,
//! and over full segments alone nothing would say so.
//!
//! The regions come from the test's own memory rather than from a collection's
//! arena: the chain allocates nothing and takes a region and a capacity, so a
//! case can hold one anywhere and the boundary arithmetic is the same
//! (`crate::cycle::arena` is what production draws them from).

use super::*;

/// Records one segment of these cases holds. Two, so a fourth push crosses the
/// boundary and a drain has both a full segment and a partial one to read.
const CAPACITY: usize = 2;

/// A region of one segment, aligned for a pointer by its element type and kept
/// alive by the caller.
fn region() -> Vec<u64> {
    vec![0; (SEGMENT_HEADER_BYTES + CAPACITY * size_of::<usize>()) / size_of::<u64>()]
}

#[test]
fn the_drain_hands_every_record_back_oldest_first_across_a_boundary() {
    let mut first = region();
    let mut second = region();
    let chain: RecordChain<usize> =
        unsafe { RecordChain::over(first.as_mut_ptr() as *mut u8, CAPACITY) };
    unsafe { chain.attach(second.as_mut_ptr() as *mut u8, CAPACITY) };

    for record in 0..CAPACITY + 1 {
        if !chain.push(record) {
            assert!(chain.advance_to_kept(), "the second segment is attached");
            assert!(chain.push(record));
        }
    }

    let mut read = Vec::new();
    chain.drain(|record| read.push(record));
    assert_eq!(
        read,
        (0..CAPACITY + 1).collect::<Vec<usize>>(),
        "the append order, over a full segment and the partial one above it"
    );
    assert!(chain.is_empty(), "a drained chain holds no record");
}

#[test]
fn room_counts_the_append_position_and_every_segment_above_it() {
    let mut first = region();
    let mut second = region();
    let chain: RecordChain<usize> =
        unsafe { RecordChain::over(first.as_mut_ptr() as *mut u8, CAPACITY) };
    assert_eq!(chain.room(), CAPACITY);

    unsafe { chain.attach(second.as_mut_ptr() as *mut u8, CAPACITY) };
    assert_eq!(
        chain.room(),
        2 * CAPACITY,
        "a segment attached above the append position is room a reservation \
         may count"
    );

    assert!(chain.push(0));
    assert_eq!(
        chain.room(),
        2 * CAPACITY - 1,
        "and each record written is room the next reservation may not"
    );
}

#[test]
fn a_drained_chain_reuses_the_segments_it_kept() {
    let mut first = region();
    let mut second = region();
    let chain: RecordChain<usize> =
        unsafe { RecordChain::over(first.as_mut_ptr() as *mut u8, CAPACITY) };
    unsafe { chain.attach(second.as_mut_ptr() as *mut u8, CAPACITY) };

    for round in 0..2 {
        for record in 0..CAPACITY + 1 {
            if !chain.push(record) {
                assert!(chain.advance_to_kept());
                assert!(chain.push(record));
            }
        }

        let mut read = 0;
        chain.drain(|_| read += 1);
        assert_eq!(read, CAPACITY + 1, "round {round} wrote what it read");
        assert_eq!(
            chain.segment_count(),
            2,
            "the drain leaves the append position at the base with every \
             segment kept, so the second round draws none"
        );
        assert_eq!(chain.room(), 2 * CAPACITY);
    }
}
