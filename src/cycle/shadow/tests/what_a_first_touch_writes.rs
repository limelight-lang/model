//! The zeroing is the cost the design measured — 41–76 ms to zero the
//! rows of a 717 MiB heap against 1.4 ms for the bitmap — so what these
//! tests pin is as much what is *not* written as what is. A row outside
//! the touched group must still read the dirt the bump handed over, and
//! an implementation that cleared the array at allocation passes every
//! functional test in the crate while paying that difference.

use super::*;

/// One group, and only one. The dirt is the instrument: rows of the
/// touched group read zero, and the rows of every other group still read
/// what the buffer was filled with.
#[test]
fn a_first_touch_zeroes_its_own_group_and_no_other() {
    let mut buffer = dirty_buffer(64);
    let array = array_over(&mut buffer, 64);

    unsafe { meet_group(array, 3) };
    for index in 0..GROUP {
        assert_eq!(
            unsafe { raw(array, index) },
            0,
            "row {index} of the touched group"
        );
    }

    for index in GROUP..64 {
        assert_eq!(
            unsafe { raw(array, index) },
            0xEEEE_EEEE,
            "row {index} is outside the touched group and stays dirty"
        );
    }

    unsafe { meet_group(array, 40) };
    for index in 40..48 {
        assert_eq!(unsafe { raw(array, index) }, 0, "row {index} of group five");
    }

    for index in 48..64 {
        assert_eq!(
            unsafe { raw(array, index) },
            0xEEEE_EEEE,
            "row {index} is still untouched"
        );
    }
}

/// The bit is what makes the second touch of a group harmless. Without
/// it the group would be zeroed again and every row already met in it
/// would read as untouched, which is the same defect as re-initialising
/// a row from the refcount, one group wide.
#[test]
fn a_group_met_once_is_not_zeroed_again() {
    let mut buffer = dirty_buffer(32);
    let array = array_over(&mut buffer, 32);

    unsafe { meet_group(array, 0) };
    unsafe { *row(array, 0) = compose(Colour::Met, 11) };
    unsafe { *row(array, 7) = compose(Colour::Condemned, 0) };

    for index in 0..GROUP {
        unsafe { meet_group(array, index) };
    }

    assert_eq!(unsafe { count(raw(array, 0)) }, 11);
    assert_eq!(unsafe { colour(raw(array, 7)) }, Colour::Condemned);
}

/// The bitmap sits past the rows, so the last group's rows and the first
/// bitmap byte are neighbours: an array that reserved one row too few
/// would have the group init write over the bits that guard it, and the
/// group would then be re-zeroed on its next touch. Driven at a slot
/// count that is not a whole group, which is where the rounding decides
/// the boundary.
#[test]
fn the_rows_and_the_bitmap_do_not_overlap_at_the_last_group() {
    let slots = 12;
    let mut buffer = dirty_buffer(slots);
    let array = array_over(&mut buffer, slots);

    for index in 0..slots {
        unsafe { meet_group(array, index) };
        unsafe { *row(array, index) = compose(Colour::Met, index) };
    }

    for index in 0..slots {
        unsafe { meet_group(array, index) };
        assert_eq!(
            unsafe { count(raw(array, index)) },
            index,
            "row {index} survived every later group init"
        );
    }
}

/// The group init writes eight rows whatever the last group holds, so a
/// slot count that is not a multiple of eight has to have reserved the
/// rounding. What this pins is the reservation: the write past the last
/// live row lands inside the array rather than in the bitmap or past the
/// allocation.
#[test]
fn a_partial_last_group_is_reserved_whole() {
    let slots = 9;
    assert_eq!(
        bytes_for(slots),
        size_of::<RowArray>() + 16 * size_of::<u32>() + 1,
        "nine rows reserve two whole groups and one bitmap byte"
    );

    let mut buffer = dirty_buffer(slots);
    let array = array_over(&mut buffer, slots);
    unsafe { meet_group(array, 8) };

    let last = buffer.len() * size_of::<u64>();
    let past_the_rows = size_of::<RowArray>() + 16 * size_of::<u32>();
    assert!(past_the_rows < last);
    for index in 8..16 {
        assert_eq!(unsafe { raw(array, index) }, 0, "row {index} was reserved");
    }
}
