//! A ring keeps the last `CAPACITY` records while its cursor counts
//! past the wrap, which is what makes a window an arithmetic
//! subtraction rather than a comparison of wrapped positions. Two
//! marks handed over the wrong way round bound no interval, and an
//! empty list of answers would read as "nothing happened anywhere",
//! so a reversed pair says so in an answer of its own.

use super::*;

/// A ring wraps and keeps the last `CAPACITY` records, and the cursor
/// keeps counting past the wrap — which is what makes a window's
/// arithmetic subtraction rather than a comparison of wrapped
/// positions.
#[test]
fn a_ring_wraps_and_its_cursor_does_not() {
    let _quiet = kinds::disable_sites_for_test();
    let _g = crate::memory::block_pool::test_guard();
    let ring = allocate_ring();
    assert!(!ring.is_null());
    let ring = unsafe { &*ring };

    for i in 0..(CAPACITY as u64 + 3) {
        ring.write(ANY_KIND, 0, i, 0, 0);
    }

    assert_eq!(ring.cursor.load(Ordering::Relaxed), CAPACITY as u64 + 3);
    // The three newest are readable and name the last three subjects.
    for (offset, subject) in [
        (3, CAPACITY as u64),
        (2, CAPACITY as u64 + 1),
        (1, CAPACITY as u64 + 2),
    ] {
        let at = ring.cursor.load(Ordering::Relaxed) - offset;
        assert_eq!(
            ring.read_at(at).expect("still inside the ring").subject,
            subject
        );
    }

    // The record that was lapped is gone rather than stale.
    assert!(ring.read_at(0).is_none(), "a lapped position read as live");

    unsafe { crate::memory::stdapi::ll_free(ring as *const Ring as *mut u8) };
}

/// The window is the cursor pair, so a record written before the
/// first mark is outside it and one written after is inside — which
/// is the whole of "what happened between these two moments".
#[test]
fn a_cursor_pair_names_exactly_what_was_written_inside_it() {
    let _quiet = kinds::disable_sites_for_test();
    let _g = crate::memory::block_pool::test_guard();
    const BEFORE: u64 = 0x0B4;
    const FIRST_INSIDE: u64 = 0x1_11;
    const SECOND_INSIDE: u64 = 0x2_22;
    const AFTER: u64 = 0x0AF;

    record(ANY_KIND, 0, BEFORE, 0, 0);
    let start = mark();
    record(ANY_KIND, 0, FIRST_INSIDE, 1, 0);
    record(ANY_KIND, 0, SECOND_INSIDE, 2, 0);
    let end = mark();
    record(ANY_KIND, 0, AFTER, 0, 0);

    let mine = this_thread_identity();
    let inside: Vec<u64> = events(between(&start, &end))
        .into_iter()
        .filter(|event| event.thread == mine)
        .map(|event| event.subject)
        .collect();
    assert_eq!(inside, vec![FIRST_INSIDE, SECOND_INSIDE]);
    retire_thread_ring();
}

/// A window's two ends handed over the wrong way round bound no
/// interval, and an empty list of answers reads as "nothing happened
/// anywhere" — the one answer this module may not invent. It says so
/// in one answer of its own, in the release build too, where an
/// assertion would have been compiled out, and whether or not any
/// ring is named: a pair taken before anything journaled names none,
/// and a per-ring answer over no rings is the empty list again.
#[test]
fn two_marks_in_the_wrong_order_answer_that_they_bound_nothing() {
    let _quiet = kinds::disable_sites_for_test();
    let _g = crate::memory::block_pool::test_guard();
    record(ANY_KIND, 0, 0x0D, 0, 0);
    let start = mark();
    let end = mark();

    assert_eq!(between(&end, &start), vec![Window::Reversed]);
    retire_thread_ring();
}

/// The same, with no ring in either mark — where a per-ring answer
/// has nothing to answer over and the empty list comes back.
#[test]
fn a_reversed_pair_naming_no_ring_still_answers() {
    let _quiet = kinds::disable_sites_for_test();
    let _g = crate::memory::block_pool::test_guard();
    let start = Mark {
        positions: Vec::new(),
        evictions: 0,
        refusals: 0,
        lost: 0,
        taken: 1,
    };

    let end = Mark {
        positions: Vec::new(),
        evictions: 0,
        refusals: 0,
        lost: 0,
        taken: 2,
    };

    assert_eq!(between(&end, &start), vec![Window::Reversed]);
}
