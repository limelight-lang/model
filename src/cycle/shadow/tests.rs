use super::*;

/// Bytes an array of `row_count` rows needs, over memory this module can
/// write into freely. A `Vec` rather than the collection's arena: what
/// the tests below pin is the layout, and the arena's own contract is
/// `cycle::arena`'s.
///
/// The buffer is filled with `0xEE` before every use, which is what
/// makes "not zeroed" observable: a row the group init did not reach
/// reads `0xEEEEEEEE`, a value no colour and no count could produce
/// together.
fn dirty_buffer(row_count: u32) -> Vec<u64> {
    let words = bytes_for(row_count).div_ceil(size_of::<u64>());
    vec![0xEEEE_EEEE_EEEE_EEEE; words]
}

/// The array at the head of `buffer`, initialised for `row_count` rows.
///
/// The block address is the buffer's own, which nothing here
/// dereferences: the sweep is what reads it, and the sweep is tested
/// against real blocks in `cycle::arena`.
fn array_over(buffer: &mut [u64], row_count: u32) -> *mut RowArray {
    // One borrow of the buffer, reused: a second `as_mut_ptr` retags it
    // and invalidates the first pointer, which is a Miri failure of the
    // fixture rather than of the code under it.
    let head = buffer.as_mut_ptr() as *mut u8;
    let array = head as *mut RowArray;
    unsafe {
        init(
            array,
            head,
            row_count,
            Population::Slotted,
            std::ptr::null_mut(),
        )
    };
    array
}

/// The raw word of row `index`, whatever its group's state.
unsafe fn raw(array: *mut RowArray, index: u32) -> u32 {
    unsafe { *row(array, index) }
}

mod what_a_first_touch_writes;
mod what_a_row_word_holds;
mod what_the_unreachable_walk_visits;
