//! The three duties the safepoint poll has towards the queue: unlinking
//! the block a burst left empty behind R's tail block, refilling the spare
//! cells, and firing the collection a reserve draw or an overflow append
//! asked for.
//!
//! The first two are asked as counts rather than remembered as flags. The
//! surplus is read off the ring's own words, and the cells
//! are asked with [`needs_spares`], because a thread whose fill at init was
//! refused has never drawn and a "drawn" flag would leave it unasked for
//! the rest of its life (`memory::reserve`, `is_drawn`). The arming is a
//! flag, and legitimately so: what it stands for is an event and not a
//! state, and every path into that event sets it.

use super::*;

use crate::cycle::testing::Sent;
use crate::ring::Reader;
use std::sync::atomic::{AtomicBool, Ordering};

/// Grow R to two blocks by a burst of registrations over `headers`, one
/// past a block's worth, and have a reader on another thread take `take`
/// of them. Taking every one leaves the writer in the second block with the
/// first empty behind it; taking exactly a block's worth leaves the reader
/// standing in an emptied front block, since the front block moves only
/// when a read finds it empty.
fn burst_read_behind_by_another_thread(headers: &mut [RcHeader], take: usize) {
    assert_eq!(headers.len(), BLOCK_ENTRIES + 1);
    for header in headers.iter_mut() {
        assert!(unsafe { !release(&raw mut *header) });
    }
    assert_eq!(segment_count(), 2, "the burst grew the circle");

    assert_eq!(take_on_another_thread(take), take);
    assert_eq!(candidate_count(), headers.len() - take);
    assert_eq!(
        segment_count(),
        2,
        "and both blocks are still in the circle"
    );
}

/// Take `count` entries from this thread's R on another thread, as the
/// collector would, in reads no longer than the count so that the reader
/// never runs a block dry beyond the last entry asked for.
fn take_on_another_thread(count: usize) -> usize {
    let record = Sent(mutator_record::this_thread_record());
    let reader = std::thread::spawn(move || {
        let record: &'static MutatorRecord = unsafe { &*record.into_inner() };
        let reader = unsafe { Reader::new(record.candidate_ring()) };
        let mut out = [0; 64];
        let mut taken = 0;
        while taken < count {
            let ask = out.len().min(count - taken);
            let now = reader.take(&mut out[..ask]);
            taken += now;
            if now == 0 {
                std::thread::yield_now();
            }
        }
        taken
    });
    reader.join().expect("the reader finished")
}

/// A circle a burst grew shrinks at the poll after the reader has passed
/// the surplus and a cell is short: the empty block behind the tail block
/// goes into the cell and off the ledger, so the refill draws one block
/// fewer; the block the writer stands in stays, and a second poll finds
/// nothing to unlink.
#[test]
fn the_poll_unlinks_the_block_a_burst_left_empty_behind_the_tail() {
    let _g = test_guard();
    reset();
    assert!(refill_spares());
    let mut headers: Box<[RcHeader]> = (0..BLOCK_ENTRIES + 1).map(|_| candidate(2)).collect();
    burst_read_behind_by_another_thread(&mut headers, BLOCK_ENTRIES + 1);
    assert_eq!(
        spare_count(),
        0,
        "the first block and the growth spent both cells"
    );

    assert!(crate::memory::critical::replenish());
    let before = crate::memory::block_pool::BlockPool::global().blocks_out();
    let charged = crate::memory::gc_metadata::thread_stats().current_bytes_in_use();
    assert_eq!(unsafe { crate::gc::ll_gc_maybe_collect() }, 0);
    assert_eq!(segment_count(), 1, "the empty block left the circle");
    assert_eq!(spare_count(), SPARE_SEGMENTS, "and went into a spent cell");
    assert_eq!(
        crate::memory::block_pool::BlockPool::global().blocks_out(),
        before + 1,
        "one draw for two empty cells: the unlinked block took the other"
    );
    assert_eq!(
        crate::memory::gc_metadata::thread_stats().current_bytes_in_use(),
        charged - BLOCK_PAYLOAD,
        "a cell is a reservation, and carries no charge"
    );

    assert_eq!(unsafe { crate::gc::ll_gc_maybe_collect() }, 0);
    assert_eq!(segment_count(), 1, "the block the writer stands in stays");

    let mut late = candidate(2);
    assert!(unsafe { !release(&raw mut late) });
    assert_eq!(
        candidate_count(),
        1,
        "the one block still takes a registration"
    );
    assert_eq!(segment_count(), 1);

    reset();
}

/// Hand a reading's hold back on the way out, an unwind included, so that a
/// failing case leaves the harness thread's record free for the next.
struct HandBack(*mut MutatorRecord);

impl Drop for HandBack {
    fn drop(&mut self) {
        unsafe { mutator_record::hand_back_reading(self.0) };
    }
}

/// A collector's reading holds the record: the poll leaves the empty block
/// behind the tail block in the circle with a cell short, since the reading
/// may have loaded it as R's front block (`unlink_surplus_block`, `PLAN.md`
/// S65.22). Without the hold the same poll unlinks it, which
/// [`the_poll_unlinks_the_block_a_burst_left_empty_behind_the_tail`] reads.
#[test]
fn the_poll_leaves_the_circle_alone_while_a_reading_holds_the_record() {
    let _g = test_guard();
    reset();
    assert!(refill_spares());
    let mut headers: Box<[RcHeader]> = (0..BLOCK_ENTRIES + 1).map(|_| candidate(2)).collect();
    burst_read_behind_by_another_thread(&mut headers, BLOCK_ENTRIES + 1);
    assert!(needs_spares(), "a cell is short, so the poll would unlink");

    let record = mutator_record::this_thread_record();
    assert!(unsafe { mutator_record::take_for_reading(record) });
    let hand_back = HandBack(record);
    assert_eq!(unsafe { crate::gc::ll_gc_maybe_collect() }, 0);
    assert_eq!(
        segment_count(),
        2,
        "the block a reading may name stays in the circle"
    );

    drop(hand_back);
    reset();
}

/// Whether [`a_front_block_moved_under_a_reading_stays_in_the_circle`]'s act
/// took the hold; the case runs under `test_guard`, one at a time.
static TAKEN_INSIDE_THE_POLL: AtomicBool = AtomicBool::new(false);

/// The interleaving the gate's place decides: the poll begins, a reading
/// takes the hold and a batch on another thread moves R's front block past
/// the first block, and only then does the unlink read the circle. The first
/// block is empty and behind the tail block by then, and the reading may have
/// loaded it as the front block before the batch moved it; asked after the
/// unlink's decision loads, the hold keeps it in the circle.
#[test]
fn a_front_block_moved_under_a_reading_stays_in_the_circle() {
    let _g = test_guard();
    reset();
    assert!(refill_spares());
    let mut headers: Box<[RcHeader]> = (0..BLOCK_ENTRIES + 1).map(|_| candidate(2)).collect();
    burst_read_behind_by_another_thread(&mut headers, 0);
    assert!(needs_spares(), "a cell is short, so the poll would unlink");

    let record = mutator_record::this_thread_record();
    TAKEN_INSIDE_THE_POLL.store(false, Ordering::Relaxed);
    at_the_next_surplus_unlink(|| {
        let record = mutator_record::this_thread_record();
        let taken = unsafe { mutator_record::take_for_reading(record) };
        TAKEN_INSIDE_THE_POLL.store(taken, Ordering::Relaxed);
        assert_eq!(take_on_another_thread(BLOCK_ENTRIES + 1), BLOCK_ENTRIES + 1);
    });
    let _ = unsafe { crate::gc::ll_gc_maybe_collect() };
    assert!(
        TAKEN_INSIDE_THE_POLL.load(Ordering::Relaxed),
        "the act took the hold inside the poll"
    );
    let hand_back = HandBack(record);
    assert_eq!(
        candidate_count(),
        0,
        "the batch moved the front past every entry"
    );
    assert_eq!(
        segment_count(),
        2,
        "the emptied first block stays while the reading holds the record"
    );

    drop(hand_back);
    reset();
}

/// With both cells full the circle keeps its consumed block: the writer
/// reaches it again for nothing, where an unlink would send it to the pool
/// and the next growth draw it back.
#[test]
fn a_poll_with_full_cells_leaves_the_circle_alone() {
    let _g = test_guard();
    reset();
    assert!(refill_spares());
    let mut headers: Box<[RcHeader]> = (0..BLOCK_ENTRIES + 1).map(|_| candidate(2)).collect();
    burst_read_behind_by_another_thread(&mut headers, BLOCK_ENTRIES + 1);
    assert!(refill_spares(), "both cells full again before the poll");
    assert!(crate::memory::critical::replenish());
    let before = crate::memory::block_pool::BlockPool::global().blocks_out();

    assert_eq!(unsafe { crate::gc::ll_gc_maybe_collect() }, 0);
    assert_eq!(segment_count(), 2, "nothing asked for the block");
    assert_eq!(spare_count(), SPARE_SEGMENTS);
    assert_eq!(
        crate::memory::block_pool::BlockPool::global().blocks_out(),
        before,
        "and the pool saw nothing"
    );

    reset();
}

/// The front block is never unlinked, even empty: a reader that took
/// exactly the first block's worth stands in it, and the block after the
/// tail block is that front block. The poll leaves it, and the reader's
/// next take moves out of it into the block that holds the rest.
#[test]
fn the_poll_never_unlinks_the_front_block() {
    let _g = test_guard();
    reset();
    assert!(refill_spares());
    let mut headers: Box<[RcHeader]> = (0..BLOCK_ENTRIES + 1).map(|_| candidate(2)).collect();
    burst_read_behind_by_another_thread(&mut headers, BLOCK_ENTRIES);
    assert_eq!(
        spare_count(),
        0,
        "a cell is short, so the poll would unlink"
    );

    assert_eq!(unsafe { crate::gc::ll_gc_maybe_collect() }, 0);
    assert_eq!(segment_count(), 2, "the emptied front block stays");
    assert_eq!(candidate_count(), 1, "and the one entry past it stands");
    assert_eq!(take_on_another_thread(1), 1, "the reader moves into it");
    assert_eq!(candidate_count(), 0);

    reset();
}

/// The cells are short after a spend, and the poll fills them.
#[test]
fn the_poll_refills_a_cell_an_overflow_spent() {
    let _g = test_guard();
    reset();
    assert!(refill_spares());
    assert!(!needs_spares(), "full cells ask for nothing");

    let mut header = candidate(2);
    assert!(unsafe { !release(&raw mut header) });
    assert_eq!(spare_count(), SPARE_SEGMENTS - 1);
    assert!(!crate::gc::is_armed(), "a cell was there; nothing asked");
    assert!(needs_spares(), "a spent cell is what asks");

    assert_eq!(unsafe { crate::gc::ll_gc_maybe_collect() }, 0);
    assert_eq!(spare_count(), SPARE_SEGMENTS);
    assert!(!needs_spares());

    reset();
}

/// A poll on a thread nothing armed neither collects nor is expected to,
/// and it still refills. The two duties are independent: the refill is
/// not conditional on there being anything to collect.
#[test]
fn an_unarmed_poll_still_refills() {
    let _g = test_guard();
    reset();
    assert_eq!(spare_count(), 0, "nothing has stocked the cells yet");

    assert_eq!(unsafe { crate::gc::ll_gc_maybe_collect() }, 0);
    assert_eq!(spare_count(), SPARE_SEGMENTS);

    reset();
}

/// Thread exit hands back every segment and every cell, which is what
/// keeps a dying thread from taking pool blocks with it.
///
/// The base block is out of the pool on both sides of the bracket and so
/// cancels in it: it is the one block the thread holds for its life
/// rather than for its queue's contents, and `release_queue_base` rather
/// than [`release_queue_segments`] is what gives it back
/// (`the_base_block_a_thread_holds_for_its_life`).
#[test]
fn a_drain_returns_every_segment_and_every_spare() {
    let _g = test_guard();
    reset();
    crate::memory::critical::drain_for_test();
    let before = crate::memory::block_pool::BlockPool::global().blocks_out();

    assert!(refill_spares());
    let mut first = candidate(2);
    let first_entity = &raw mut first;
    assert!(unsafe { !release(first_entity) });
    fill_tail_block(first_entity);
    let mut second = candidate(2);
    assert!(unsafe { !release(&raw mut second) });

    // Both cells are spent by the two swaps above, so the poll fills
    // them again: the drain has to give back segments **and** spares,
    // and a fixture that reached it holding none would leave the loop
    // that returns them untested.
    assert_eq!(unsafe { crate::gc::ll_gc_maybe_collect() }, 0);
    assert_eq!(spare_count(), SPARE_SEGMENTS);
    assert_eq!(segment_count(), 2);
    assert!(
        crate::memory::block_pool::BlockPool::global().blocks_out() > before,
        "the queue is holding blocks"
    );

    release_queue_segments();
    crate::memory::critical::drain_for_test();

    assert_eq!(segment_count(), 0);
    assert_eq!(spare_count(), 0);
    assert_eq!(candidate_count(), 0);
    assert_eq!(
        crate::memory::block_pool::BlockPool::global().blocks_out(),
        before,
        "every block the queue took is back in the pool"
    );
}
