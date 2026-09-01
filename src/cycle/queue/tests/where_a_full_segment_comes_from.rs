//! Growth: where the segment comes from when the live one is full, and
//! what the enrolling thread pays for it.
//!
//! Y12 clause 3 gives the path three properties and this module holds it
//! to each: the write never allocates, never locks and never copies; the
//! spare comes from a cell somebody else filled; and a growth the cells
//! cannot serve draws the critical reserve rather than dropping the
//! root.

use super::*;

use crate::test_support::allocation_probe;

/// The growth itself: the full segment stays in the chain and every
/// entry before it is still counted.
#[test]
fn an_overflow_links_a_second_segment_and_keeps_the_first() {
    let _g = test_guard();
    reset();
    assert!(refill_spares(), "the cells start full");

    let mut first = candidate(2);
    let first_entity = &raw mut first;
    assert!(unsafe { !release(first_entity) });
    assert_eq!(segment_count(), 1);

    fill_write_segment(first_entity);
    let mut second = candidate(2);
    let second_entity = &raw mut second;
    assert!(unsafe { !release(second_entity) });

    assert_eq!(segment_count(), 2, "the full segment is still in the chain");
    assert_eq!(
        candidate_count(),
        SEGMENT_CAPACITY + 1,
        "the full segment's entry count is kept, not dropped"
    );
    assert_eq!(
        write_segment_entry(0),
        second_entity,
        "the fresh segment starts with the entry the growth carried"
    );

    reset();
}

/// The clause the counter exists for. Bracketing both the ordinary write
/// and the growth, the registering thread reaches neither the global
/// allocator nor the pool — the spare was taken at a poll, and taking it
/// is a cell swap.
#[test]
fn neither_the_write_nor_the_overflow_allocates_or_asks_the_pool() {
    let _g = test_guard();
    reset();
    assert!(refill_spares(), "the cells are stocked ahead of the path");

    // The thread's first release is itself a growth, the write segment
    // being a cell: it is bracketed for what it is, and the ordinary
    // write is bracketed after it, against a segment that now exists.
    let mut opening = candidate(2);
    let opening_entity = &raw mut opening;
    let _ = allocation_probe::take_all();
    assert!(unsafe { !release(opening_entity) });
    assert_eq!(
        allocation_probe::take_all(),
        (0, 0),
        "the first registration swaps a cell in; it does not go and get a block"
    );

    let mut ordinary = candidate(2);
    let _ = allocation_probe::take_all();
    assert!(unsafe { !release(&raw mut ordinary) });
    assert_eq!(
        allocation_probe::take_all(),
        (0, 0),
        "and a registration with room is a store into the write segment"
    );
    assert_eq!(segment_count(), 1, "no segment was added for it");

    fill_write_segment(opening_entity);
    let mut on_a_full_segment = candidate(2);
    let _ = allocation_probe::take_all();
    assert!(unsafe { !release(&raw mut on_a_full_segment) });
    assert_eq!(
        allocation_probe::take_all(),
        (0, 0),
        "and neither does the growth that follows a full segment"
    );
    // Two cells, not one: the write segment is a cell too, so the opening
    // release above was itself a growth.
    assert_eq!(spare_count(), SPARE_SEGMENTS - 2);

    reset();
}

/// With both cells empty the reserve answers, and the poll is armed by
/// the draw — which is how the cells get filled again and how a
/// collection is asked for at all.
#[test]
fn a_growth_with_no_spare_draws_the_reserve_and_arms_the_poll() {
    let _g = test_guard();
    reset();
    assert!(crate::memory::critical::replenish(), "the reserve is full");
    assert_eq!(spare_count(), 0, "no cell is stocked for this one");

    let mut first = candidate(2);
    assert!(unsafe { !release(&raw mut first) });
    assert_eq!(
        segment_count(),
        1,
        "the first registration drew the reserve"
    );
    assert!(
        crate::gc::is_armed(),
        "a reserve draw is what asks for a collection"
    );

    // The poll disarms as it fires. Nothing collects yet, so what the
    // fire reports is zero either way; what the pair of assertions reads
    // is that the arming reached the poll and did not survive it.
    assert_eq!(unsafe { crate::gc::ll_gc_maybe_collect() }, 0);
    assert!(!crate::gc::is_armed(), "the poll disarmed it");
    assert_eq!(
        spare_count(),
        SPARE_SEGMENTS,
        "and refilled the cells behind it"
    );

    reset();
}

/// Every door refused: the entry lands in the escrow rather than
/// nowhere, the bit stays down where it was put, and the poll is armed.
///
/// **The enrolment's doors are the spare cells and the critical reserve,
/// and nothing here closes the pool**, deliberately.
/// `block_pool::FORCE_OOM` would refuse before the request is even
/// counted, so a growth that grew a "then ask the pool" fallback — the
/// regression that would break clause 3 — would look identical. Emptying
/// the cells and the reserve by hand leaves the pool open, and the pool
/// counter below is what sees the fallback appear.
#[test]
fn every_door_refused_puts_the_entry_in_the_escrow() {
    let _g = test_guard();
    reset();
    assert_eq!(spare_count(), 0);
    crate::memory::critical::drain_for_test();
    let _ = allocation_probe::take_all();

    let mut header = candidate(2);
    let entity = &raw mut header;
    unsafe { release(entity) };

    assert_eq!(
        overflow_len(),
        1,
        "the entry is in the overflow buffer, not lost"
    );
    assert_eq!(candidate_count(), 0, "and not in the queue, which has none");
    assert_eq!(segment_count(), 0, "nothing was swapped in");
    assert_eq!(
        allocation_probe::take_all(),
        (0, 0),
        "the overflow append is a store; it reaches past no allocation path"
    );
    assert_ne!(
        unsafe { mutator_flags(entity) } & CANDIDATE_BIT,
        0,
        "the bit stays: an entry names this entity, in the overflow buffer"
    );
    assert!(
        crate::gc::is_armed(),
        "an overflow append asks for a collection"
    );

    reset();
}

/// And the poll takes it out again, in the order that makes room first.
#[test]
fn the_poll_drains_the_escrow_into_the_queue() {
    let _g = test_guard();
    reset();
    assert_eq!(spare_count(), 0);
    crate::memory::critical::drain_for_test();

    let mut header = candidate(2);
    let entity = &raw mut header;
    unsafe { release(entity) };
    assert_eq!(overflow_len(), 1);

    assert_eq!(unsafe { crate::gc::ll_gc_maybe_collect() }, 0);

    assert_eq!(overflow_len(), 0, "the poll emptied it");
    assert_eq!(
        candidate_count(),
        1,
        "into the queue the refill made room in"
    );
    assert_eq!(
        write_segment_entry(0),
        entity,
        "and the entry still names the entity"
    );

    reset();
}

/// A drain with no room puts nothing back and loses nothing: the poll
/// that finds every door still spent leaves the entries where they are
/// for the collection it is about to run.
#[test]
fn a_drain_with_no_room_leaves_the_escrow_alone() {
    let _g = test_guard();
    reset();
    crate::memory::critical::drain_for_test();

    let mut header = candidate(2);
    unsafe { release(&raw mut header) };
    assert_eq!(overflow_len(), 1);

    crate::cycle::queue::drain_overflow();
    assert_eq!(
        overflow_len(),
        1,
        "and it stays: no cell and no write segment to move it into"
    );

    reset();
}

/// A bulk release longer than the poll stride refills its own funding
/// mid-run, so the overflow buffer it starts filling is emptied before the
/// run ends.
///
/// The loop is `ll_release_vector`'s, whose `count` is the caller's and
/// whose body the compiler never sees inside — so without a poll of its
/// own it registers without bound. Started with every cell and every
/// reserve spent, this run fills the buffer until its first backedge poll
/// and queues everything after it.
#[test]
fn a_bulk_release_polls_on_its_own_backedge() {
    let _g = test_guard();
    reset();
    crate::memory::critical::drain_for_test();
    assert_eq!(spare_count(), 0, "no spare cell to start from");

    let count = POLL_STRIDE + 1;
    let mut headers: Vec<RcHeader> = (0..count).map(|_| candidate(2)).collect();
    let entities: Vec<*mut RcHeader> = headers.iter_mut().map(|h| &raw mut *h).collect();

    unsafe { crate::object::ll_release_vector(entities.as_ptr(), count) };

    assert_eq!(
        overflow_len(),
        0,
        "the backedge poll refilled the cells and drained what had overflowed"
    );
    assert_eq!(
        candidate_count(),
        count,
        "and every candidate is in the queue"
    );

    reset();
}
