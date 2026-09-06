//! Which rows the harvest's walk reads and which it must not: the rows of a
//! group the trace never met, and the rows the rounding adds past the last
//! slot.
//!
//! Both bounds are invisible in a walk over an array whose groups are all met
//! and whose row count is a multiple of eight, which is what an entity block of
//! the smallest size class gives. So the arrays here are built by hand: 1,020
//! rows, the count a 64-byte size class produces, and one group met at each end
//! with a dirty one between them.

use super::*;

/// A block of the 64-byte class holds 1,020 slots, and 1,020 is not a whole
/// number of groups — which is the case the `min` in the walk exists for.
const ROWS: u32 = 1_020;

/// The indexes the walk answered, in the order it answered them.
unsafe fn visited(array: *mut RowArray) -> Vec<u32> {
    let mut seen = Vec::new();
    assert!(
        unsafe {
            for_each_unreachable(array, |index| {
                seen.push(index);
                true
            })
        },
        "the walk ran to the end of the array"
    );
    seen
}

/// Colour row `index` as the scan would, whatever the state of its group: the
/// point of two of the cases below is a colour standing in memory the walk is
/// forbidden to read.
unsafe fn write_unreachable(array: *mut RowArray, index: u32) {
    unsafe { row(array, index).write(compose(Color::PotentiallyUnreachable, 0)) };
}

/// The walk answers the rows the scan left unreachable, in ascending index, and
/// nothing else: a met group's other colours are passed over.
#[test]
fn the_walk_answers_every_unreachable_row_of_a_met_group() {
    let mut buffer = dirty_buffer(ROWS);
    let array = array_over(&mut buffer, ROWS);

    unsafe {
        ensure_group_initialized(array, 0);
        write_unreachable(array, 1);
        write_unreachable(array, 5);
        row(array, 2).write(compose(Color::Live, 3));
        row(array, 3).write(compose(Color::Unclassified, 1));

        ensure_group_initialized(array, 24);
        write_unreachable(array, 27);
    }

    assert_eq!(unsafe { visited(array) }, vec![1, 5, 27]);
}

/// A group the trace never met holds whatever the block that owned this memory
/// before left in it, so a colour there is another collection's verdict. The
/// walk reads the bitmap and passes over it.
#[test]
fn a_row_of_an_unmet_group_is_never_read() {
    let mut buffer = dirty_buffer(ROWS);
    let array = array_over(&mut buffer, ROWS);

    unsafe {
        ensure_group_initialized(array, 0);
        write_unreachable(array, 3);

        // Group 1, which nothing has met: the bit is clear and these bytes are
        // the fixture's dirt, colours and all.
        assert!(!group_is_initialized(array, 8));
        write_unreachable(array, 9);
        write_unreachable(array, 15);
    }

    assert_eq!(
        unsafe { visited(array) },
        vec![3],
        "the met group's row alone"
    );
}

/// The rows past `row_count` are the rounding's, reserved so that a group init
/// can write eight of them, and they name no slot of the block. A walk that
/// answered one would hand the sweep an index the address dispatch resolves
/// against a block that has no such slot.
#[test]
fn a_row_the_rounding_added_is_never_answered() {
    let mut buffer = dirty_buffer(ROWS);
    let array = array_over(&mut buffer, ROWS);
    assert_eq!(ROWS % GROUP, 4, "the last group is a partial one");

    unsafe {
        // The last group: rows 1,016 through 1,023, of which 1,016 to 1,019
        // are slots and the rest are the rounding's.
        ensure_group_initialized(array, 1_016);
        write_unreachable(array, 1_017);
        write_unreachable(array, 1_019);
        write_unreachable(array, 1_020);
        write_unreachable(array, 1_023);
    }

    assert_eq!(unsafe { visited(array) }, vec![1_017, 1_019]);
}

/// The walk stops where the visitor does and says so, which is how the sweep
/// reports a region that refused a record: what follows is the null-only walk
/// of an ordinary close.
#[test]
fn a_refusing_visitor_stops_the_walk_and_is_reported() {
    let mut buffer = dirty_buffer(ROWS);
    let array = array_over(&mut buffer, ROWS);

    unsafe {
        ensure_group_initialized(array, 0);
        write_unreachable(array, 1);
        write_unreachable(array, 4);
        ensure_group_initialized(array, 8);
        write_unreachable(array, 11);
    }

    let mut seen = Vec::new();
    let ran_out = unsafe {
        for_each_unreachable(array, |index| {
            seen.push(index);
            seen.len() < 2
        })
    };

    assert!(!ran_out, "the walk answers that it stopped early");
    assert_eq!(seen, vec![1, 4], "and stopped at the row that refused");
}
