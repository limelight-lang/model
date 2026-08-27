//! The two duties the safepoint poll has towards the queue: refilling
//! the spare cells, and firing the collection an overflow asked for.
//!
//! Both are asked as counts rather than remembered as flags. The cells
//! are asked with [`is_short`], because a thread whose fill at init was
//! refused has never drawn and a "drawn" flag would leave it unasked for
//! the rest of its life (`memory::reserve`, `is_drawn`). The arming is a
//! flag, and legitimately so: what it stands for is an event and not a
//! state, and every path into that event sets it.

use super::*;

/// The cells are short after a spend, and the poll fills them.
#[test]
fn the_poll_refills_a_cell_an_overflow_spent() {
    let _g = test_guard();
    reset();
    assert!(replenish());
    assert!(!is_short(), "full cells are not short");

    let mut header = candidate(2);
    assert!(unsafe { !release(&raw mut header) });
    assert_eq!(spares_held(), SPARE_SEGMENTS - 1);
    assert!(!crate::gc::is_armed(), "a cell was there; nothing asked");
    assert!(is_short(), "a spent cell is what short means");

    assert_eq!(unsafe { crate::gc::ll_gc_maybe_collect() }, 0);
    assert_eq!(spares_held(), SPARE_SEGMENTS);
    assert!(!is_short());

    reset();
}

/// A poll on a thread nothing armed neither collects nor is expected to,
/// and it still refills. The two duties are independent: the refill is
/// not conditional on there being anything to collect.
#[test]
fn an_unarmed_poll_still_refills() {
    let _g = test_guard();
    reset();
    assert_eq!(spares_held(), 0, "nothing has filled the cells yet");

    assert_eq!(unsafe { crate::gc::ll_gc_maybe_collect() }, 0);
    assert_eq!(spares_held(), SPARE_SEGMENTS);

    reset();
}

/// Thread exit hands back every segment and every cell, which is what
/// keeps a dying thread from taking pool blocks with it.
#[test]
fn a_drain_returns_every_segment_and_every_spare() {
    let _g = test_guard();
    reset();
    crate::memory::critical::drain_for_test();
    let before = crate::memory::block_pool::BlockPool::global().blocks_out();

    assert!(replenish());
    let mut first = candidate(2);
    let first_entity = &raw mut first;
    assert!(unsafe { !release(first_entity) });
    fill_live_segment(first_entity);
    let mut second = candidate(2);
    assert!(unsafe { !release(&raw mut second) });

    // Both cells are spent by the two swaps above, so the poll fills
    // them again: the drain has to give back segments **and** spares,
    // and a fixture that reached it holding none would leave the loop
    // that returns them untested.
    assert_eq!(unsafe { crate::gc::ll_gc_maybe_collect() }, 0);
    assert_eq!(spares_held(), SPARE_SEGMENTS);
    assert_eq!(segment_count(), 2);
    assert!(
        crate::memory::block_pool::BlockPool::global().blocks_out() > before,
        "the queue is holding blocks"
    );

    drain();
    crate::memory::critical::drain_for_test();

    assert_eq!(segment_count(), 0);
    assert_eq!(spares_held(), 0);
    assert_eq!(enrolled_count(), 0);
    assert_eq!(
        crate::memory::block_pool::BlockPool::global().blocks_out(),
        before,
        "every block the queue held is back in the pool"
    );
}
