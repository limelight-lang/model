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

/// The overflow itself: the full segment stays in the chain and every
/// entry before it is still counted.
#[test]
fn an_overflow_links_a_second_segment_and_keeps_the_first() {
    let _g = test_guard();
    reset();
    assert!(replenish(), "the cells start full");

    let mut first = candidate(2);
    let first_entity = &raw mut first;
    assert!(unsafe { !release(first_entity) });
    assert_eq!(segment_count(), 1);

    fill_live_segment(first_entity);
    let mut second = candidate(2);
    let second_entity = &raw mut second;
    assert!(unsafe { !release(second_entity) });

    assert_eq!(segment_count(), 2, "the full segment is still in the chain");
    assert_eq!(
        enrolled_count(),
        SEGMENT_CAPACITY + 1,
        "the full segment's entries are counted, not dropped"
    );
    assert_eq!(
        live_entry(0),
        second_entity,
        "the fresh segment starts with the entry that overflowed"
    );

    reset();
}

/// The clause the counter exists for. Bracketing both the ordinary write
/// and the overflow, the enrolling thread reaches neither the global
/// allocator nor the pool — the spare was taken at a poll, and taking it
/// is a cell swap.
#[test]
fn neither_the_write_nor_the_overflow_allocates_or_asks_the_pool() {
    let _g = test_guard();
    reset();
    assert!(replenish(), "the cells are filled ahead of the path");

    // The thread's first release is itself an overflow, the live segment
    // being a cell: it is bracketed for what it is, and the ordinary
    // write is bracketed after it, against a segment that now exists.
    let mut opening = candidate(2);
    let opening_entity = &raw mut opening;
    let _ = allocation_probe::take_all();
    assert!(unsafe { !release(opening_entity) });
    assert_eq!(
        allocation_probe::take_all(),
        (0, 0),
        "the first enrolment swaps a cell in; it does not go and get a block"
    );

    let mut ordinary = candidate(2);
    let _ = allocation_probe::take_all();
    assert!(unsafe { !release(&raw mut ordinary) });
    assert_eq!(
        allocation_probe::take_all(),
        (0, 0),
        "and an enrolment with room is a store into the live segment"
    );
    assert_eq!(segment_count(), 1, "no segment was added for it");

    fill_live_segment(opening_entity);
    let mut overflowing = candidate(2);
    let _ = allocation_probe::take_all();
    assert!(unsafe { !release(&raw mut overflowing) });
    assert_eq!(
        allocation_probe::take_all(),
        (0, 0),
        "and neither does the overflow that follows a full segment"
    );
    // Two cells, not one: the live segment is a cell too, so the opening
    // release above was itself an overflow.
    assert_eq!(spares_held(), SPARE_SEGMENTS - 2);

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
    assert_eq!(spares_held(), 0, "no cell is filled for this one");

    let mut first = candidate(2);
    assert!(unsafe { !release(&raw mut first) });
    assert_eq!(segment_count(), 1, "the first enrolment drew the reserve");
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
        spares_held(),
        SPARE_SEGMENTS,
        "and refilled the cells behind it"
    );

    reset();
}

/// Both doors refused: no entry lands, the bit comes back down so the
/// entity is still enrollable, and the poll is armed.
///
/// **The enrolment's two doors are the spare cells and the critical
/// reserve, and nothing here closes the pool**, deliberately.
/// `block_pool::FORCE_OOM` would refuse before the request is even
/// counted, so a growth that grew a "then ask the pool" fallback — the
/// regression that would break clause 3 — would look identical. Emptying
/// the cells and the reserve by hand leaves the pool open, and the pool
/// counter below is what sees the fallback appear.
#[test]
fn a_refusal_at_both_doors_leaves_the_entity_enrollable() {
    let _g = test_guard();
    reset();
    assert_eq!(spares_held(), 0);
    crate::memory::critical::drain_for_test();
    let _ = take_refusals();
    let _ = allocation_probe::take_all();

    // Three holders, because this header is released twice: the refusal
    // below and the enrolment after it, and a second decrement from two
    // would be a death rather than a candidate.
    let mut header = candidate(3);
    let entity = &raw mut header;
    assert!(unsafe { !release(entity) });

    assert_eq!(
        take_refusals(),
        1,
        "the refusal was the queue's, not a door's"
    );
    assert_eq!(
        allocation_probe::take_all(),
        (0, 0),
        "and it refused rather than reaching past its own two doors"
    );
    assert_eq!(segment_count(), 0, "nothing was swapped in");
    assert!(crate::gc::is_armed(), "a refusal asks for a collection too");
    assert_eq!(
        unsafe { mutator_flags(entity) } & ENROLLED,
        0,
        "the bit came back down: no entry names this entity"
    );

    // And the entity is a candidate again, which is the whole point of
    // the undo: a bit left set would have reserved it an examination no
    // decrement could ever ask for again.
    assert!(replenish());
    assert!(unsafe { !release(entity) });
    assert_eq!(enrolled_count(), 1);

    reset();
}
