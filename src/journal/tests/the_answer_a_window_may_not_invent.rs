//! Turning unknown into none is what this module exists to prevent.
//! An overflowed window answers `unknown`; a ring the registry freed
//! inside the window is reported rather than dropped from the
//! answer; and a mark names rings by identity, so a freed one is
//! never read through an address the allocator has handed to
//! somebody else. An answer already given does not change under a
//! later close.

use super::*;

/// An overflowed window answers `unknown`, and that is the point of
/// the whole mechanism: the hunt it exists for turned on "no string
/// died inside the window", and a silent eviction would have made
/// that finding false.
#[test]
fn an_overflowed_window_answers_unknown_rather_than_none() {
    let _quiet = kinds::disable_sites_for_test();
    let _g = crate::memory::block_pool::test_guard();
    let start = mark();
    for i in 0..(CAPACITY as u64 * 2) {
        record(ANY_KIND, 0, i, 0, 0);
    }

    let end = mark();

    let mine = this_thread_identity();
    let mut seen = false;
    for window in between(&start, &end) {
        if let Window::Unknown { thread, written } = window
            && thread == mine
        {
            assert_eq!(written, CAPACITY as u64 * 2);
            seen = true;
        }
    }

    assert!(
        seen,
        "an overflowed ring reported records instead of unknown"
    );
    retire_thread_ring();
}

/// A ring the registry frees inside a window takes a whole thread's
/// history with it, and the window has to say so: reporting the rings
/// that are left is the conversion of *unknown* into *none* this
/// module exists to prevent.
#[test]
fn a_ring_freed_inside_the_window_is_reported_rather_than_missing() {
    let _quiet = kinds::disable_sites_for_test();
    const SUBJECT: u64 = 0x105E;
    let _g = crate::memory::block_pool::test_guard();

    let identity = a_journaling_thread(SUBJECT);
    let start = mark();
    assert!(
        evict_retired_ring(identity),
        "the exited thread's ring was not on the retired list"
    );
    let end = mark();

    assert!(
        between(&start, &end)
            .into_iter()
            .any(|window| window == Window::Evicted { rings: 1 }),
        "a ring freed inside the window left no trace in it"
    );
}

/// A mark names rings by identity, so a ring freed after it is taken
/// is reported as unknown rather than read through an address the
/// allocator has handed to somebody else.
///
/// What it pins reliably is the *repair*. On the defect it failed
/// because the freed block still held its old contents and the read
/// reported records; that is a use-after-free, so the shape of the
/// failure was the allocator's to decide and Miri is what names it as
/// one.
#[test]
fn a_ring_freed_after_the_mark_is_not_read_through_its_address() {
    let _quiet = kinds::disable_sites_for_test();
    const SUBJECT: u64 = 0x5EE;
    let _g = crate::memory::block_pool::test_guard();

    let start = mark();
    let identity = a_journaling_thread(SUBJECT);
    let end = mark();
    assert!(
        evict_retired_ring(identity),
        "the exited thread's ring was not on the retired list"
    );

    let answer = between(&start, &end)
        .into_iter()
        .find(|window| matches!(window, Window::Unknown { thread, .. } if *thread == identity));
    assert_eq!(
        answer,
        Some(Window::Unknown {
            thread: identity,
            written: 1
        }),
        "a freed ring was read rather than reported"
    );
}

/// A window that ended before a ring closed keeps its answer when the
/// thread later exits. [`Window::Lost`] says "records were raised and
/// dropped inside this window", and its count is the difference
/// between the two marks' readings of `LOST`, so a close after the
/// second mark adds nothing to it — an answer that changes under a
/// reader's feet is one that stops meaning anything.
#[test]
fn a_window_that_ended_before_the_close_is_not_reclassified_by_it() {
    let _quiet = kinds::disable_sites_for_test();
    const SUBJECT: u64 = 0xC105;
    let _g = crate::memory::block_pool::test_guard();

    let start = mark();
    let (sender, receiver) = std::sync::mpsc::channel();
    let (go, wait) = std::sync::mpsc::channel();
    let journaling = std::thread::spawn(move || {
        crate::memory::heap::ll_thread_init();
        record(ANY_KIND, 0, SUBJECT, 0, 0);
        sender
            .send(this_thread_identity())
            .expect("the test hung up");
        wait.recv().expect("the test hung up");
        crate::memory::heap::ll_thread_exit();
    });

    let identity = receiver.recv().expect("the journaling thread hung up");
    let end = mark();

    // Both marks are taken while the thread is alive, so the window
    // is complete. Only then does the thread exit.
    go.send(()).expect("the journaling thread hung up");
    journaling.join().expect("the journaling thread panicked");

    let answer = between(&start, &end)
        .into_iter()
        .find(|window| match window {
            Window::Records(events) => events.iter().any(|event| event.thread == identity),
            _ => false,
        })
        .expect("the window lost the thread it was taken around");
    assert!(
        matches!(answer, Window::Records(_)),
        "a window that was complete stopped being one: {answer:?}"
    );
}
