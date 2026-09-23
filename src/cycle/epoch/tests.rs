//! What the epoch cell answers, who moves it, and what a pinned reading does
//! to it.

use super::*;

/// The epoch is the cell's low two bits: every turnover opens the next
/// epoch, and four on the number the header carries is the one it carried
/// four turnovers ago, which is why a stale stamp is read against the epoch
/// beside it rather than trusted for its age alone.
#[test]
fn the_epoch_is_the_cells_low_two_bits() {
    assert_eq!(epoch_of(0), 0);
    assert_eq!(epoch_of(1), 1);
    assert_eq!(epoch_of(EPOCHS - 1), 3);
    assert_eq!(epoch_of(EPOCHS), 0);
    assert_eq!(epoch_of(EPOCHS * 5 + 2), 2);
}

/// The cell moves at a turn by one, with the byte the poll reads beside it,
/// and a collection of the thread's own moves neither: the mutator stores
/// nothing into its clock.
#[test]
fn a_turn_moves_the_cell_and_a_collection_moves_nothing() {
    let _g = crate::memory::block_pool::test_guard();
    let record = unsafe { &*this_thread_record() };
    let before = this_threads_turnovers();

    unsafe { crate::gc::ll_gc_collect_cycles() };
    assert_eq!(
        this_threads_turnovers(),
        before,
        "a collection of this thread's own left its cell"
    );

    turn_this_threads_cell();
    assert_eq!(
        this_threads_turnovers(),
        before + 1,
        "the turn is one turnover"
    );
    assert_eq!(
        record.turnover_byte(),
        (before + 1) as u8,
        "and the byte the poll reads follows the cell"
    );
}

/// A pin answers for this thread until it is dropped, and pins nest: a case
/// that stamps in one epoch and then in the next opens the second pin inside
/// the first and reads the first back afterwards.
#[test]
fn a_pin_holds_the_reading_and_gives_it_back() {
    let outer = pin(1);
    assert_eq!(current(), 1);

    {
        let _inner = pin(2);
        assert_eq!(current(), 2);
    }

    assert_eq!(current(), 1, "the inner pin gave the outer one back");
    drop(outer);
    assert_eq!(pinned(), None, "an unpinned thread reads the cell again");
}

/// A turn moves the cell of the record it names and no other. The rate a
/// thread's stamps age at is its own collector's reading of that thread: a
/// thread beside a busy one would otherwise find every stamp of its own stale
/// at its next collection and prune nothing.
#[test]
fn a_neighbours_turn_leaves_this_threads_epoch_alone() {
    let _g = crate::memory::block_pool::test_guard();
    let before = current();
    let neighbours = crate::cycle::testing::on_a_fresh_thread(|| {
        let mine = current();
        turn_this_threads_cell();
        (mine, current())
    });

    assert_eq!(
        current(),
        before,
        "a turn of another thread's cell moved this one's epoch"
    );
    assert_ne!(
        neighbours.0, neighbours.1,
        "the turn moved the epoch of the thread whose cell it was"
    );
}
