//! The table's own arithmetic, over fabricated addresses: neither `find` nor
//! `remove` dereferences a row's words, so the mapping can be checked without
//! a heap under it.

use super::*;

/// A sixteen-byte-aligned address no allocator issued, which is all the table
/// asks of a target or a subscriber.
fn address(index: usize) -> usize {
    0x1_0000 + index * 16
}

fn insert(index: usize) {
    assert!(
        !table::ensure_room_for_one_more().is_null(),
        "the buffer layer funds the table"
    );
    unsafe { table::insert(address(index), address(index + 0x10_0000) as *mut LLWeakRef) };
}

fn found(index: usize) -> usize {
    unsafe { table::find(table::current(), address(index)) as usize }
}

#[test]
fn every_row_survives_the_growths_that_rehash_it() {
    let _g = crate::memory::block_pool::test_guard();
    table::dispose();
    // Past 1,024 rows the capacity reaches 4,096 and the payload no longer
    // fits a block, so this run crosses the buffer arena's boundary into the
    // OS-direct route and back at the disposal.
    const ROWS: usize = 1100;

    for index in 0..ROWS {
        insert(index);
        assert_eq!(
            found(index),
            address(index + 0x10_0000),
            "the row just written is not the one found back"
        );
    }

    assert_eq!(table::len(), ROWS);
    assert_eq!(
        table::capacity(),
        4096,
        "the table holds at most half its rows, and this many of them put the \
         payload past one block"
    );
    assert!(
        table::payload_bytes() > crate::memory::block_pool::BLOCK_PAYLOAD,
        "the run meant to cross the OS-direct boundary and stayed inside it"
    );
    for index in 0..ROWS {
        assert_eq!(
            found(index),
            address(index + 0x10_0000),
            "a growth lost or moved row {index}"
        );
    }

    table::dispose();
}

#[test]
fn a_removal_leaves_every_other_row_reachable() {
    let _g = crate::memory::block_pool::test_guard();
    table::dispose();
    const ROWS: usize = 400;

    for index in 0..ROWS {
        insert(index);
    }

    // Every third row goes, which is what makes the walks that close over the
    // gaps overlap: a run of occupied slots loses several of its own.
    for index in (0..ROWS).step_by(3) {
        let removed = unsafe { table::remove(table::current(), address(index)) };
        assert_eq!(removed as usize, address(index + 0x10_0000));
    }

    for index in 0..ROWS {
        let expected = if index % 3 == 0 {
            0
        } else {
            address(index + 0x10_0000)
        };
        assert_eq!(found(index), expected, "row {index} after the removals");
    }

    assert_eq!(table::len(), ROWS - ROWS.div_ceil(3));

    // Re-inserting into the closed-over gaps must find them.
    for index in (0..ROWS).step_by(3) {
        insert(index);
    }

    for index in 0..ROWS {
        assert_eq!(
            found(index),
            address(index + 0x10_0000),
            "row {index} after the re-inserts"
        );
    }

    table::dispose();
}
