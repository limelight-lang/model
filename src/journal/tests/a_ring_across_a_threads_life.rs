//! A thread's records matter most once it is gone, so a retired ring
//! stays readable and stays in the window, and the retired list is
//! bounded at `RETIRED_KEPT`, dropping the oldest. A record arriving
//! after the exit finds the cell closed rather than opening a second
//! ring under the same identity, while `ll_thread_init` reopens it
//! for a pool thread's next life. The eviction itself frees no ring:
//! it waits for a live thread to take one, and an investigator
//! taking a mark is such a thread.

use super::*;

/// How many live rings and how many retired ones carry this
/// identity. Tests only, and the answer a test wants where the
/// registry's totals are somebody else's: `RETIRED_KEPT` bounds the
/// retired list, so a suite whose threads journal keeps it full and
/// a count of the whole list moves for reasons the test is not
/// about.
fn rings_named(thread: u64) -> (usize, usize) {
    let registry = locked();
    let carrying = |rings: &Vec<*mut Ring>| {
        rings
            .iter()
            .filter(|&&ring| unsafe { (*ring).thread } == thread)
            .count()
    };

    (carrying(&registry.live), carrying(&registry.retired))
}

/// A thread's records matter most once it is gone — the census flake
/// this journal was designed for is a hypothesis about a *finishing*
/// thread — so a retired ring stays readable and stays in the window.
#[test]
fn a_retired_threads_ring_is_still_read_by_a_window() {
    let _quiet = kinds::disable_sites_for_test();
    const SUBJECT: u64 = 0xDEAD;
    let _g = crate::memory::block_pool::test_guard();
    let start = mark();

    let joined = a_journaling_thread(SUBJECT);

    let end = mark();
    let found = events(between(&start, &end))
        .into_iter()
        .any(|event| event.thread == joined && event.subject == SUBJECT);
    assert!(found, "the exited thread's ring left the window with it");
}

/// Thread exit is not the last thing a dying thread does: the heap
/// teardown that follows the journal's own step decommissions blocks,
/// which is a default event kind. A record from there must find the
/// thread closed rather than open a second ring under the same
/// identity — one nothing would ever retire, and one that would make
/// `RETIRED_KEPT` bound a list the leak is not on.
#[test]
fn a_thread_that_journals_after_its_exit_starts_no_second_ring() {
    let _quiet = kinds::disable_sites_for_test();
    const BEFORE_EXIT: u64 = 0xE1;
    const AFTER_EXIT: u64 = 0xE2;
    let _g = crate::memory::block_pool::test_guard();
    let (live_before, _) = registry_counts();
    let start = mark();

    let identity = std::thread::spawn(|| {
        crate::memory::heap::ll_thread_init();
        record(ANY_KIND, 0, BEFORE_EXIT, 0, 0);
        let identity = this_thread_identity();
        crate::memory::heap::ll_thread_exit();
        record(ANY_KIND, 0, AFTER_EXIT, 0, 0);
        identity
    })
    .join()
    .expect("the journaling thread panicked");

    let end = mark();
    let (live_after, _) = registry_counts();
    assert_eq!(
        live_after, live_before,
        "the exited thread left a live ring behind"
    );
    assert_eq!(
        rings_named(identity),
        (0, 1),
        "the thread ended with a number of rings other than its one, retired"
    );

    // The post-exit record is not in the ring, and it is not silent
    // either: it is counted, because a window that carried neither
    // the record nor a word about it would say the thread stopped
    // after `BEFORE_EXIT`, which is not what happened.
    let answers = between(&start, &end);
    let subjects: Vec<u64> = events(answers.clone())
        .into_iter()
        .filter(|event| event.thread == identity)
        .map(|event| event.subject)
        .collect();
    assert_eq!(subjects, vec![BEFORE_EXIT]);
    assert!(
        answers
            .iter()
            .any(|window| matches!(window, Window::Lost { records } if *records >= 1)),
        "the record raised after the exit was dropped without a word: {answers:?}"
    );
}

/// A pool thread runs `ll_thread_init` and `ll_thread_exit` once per
/// task, so one OS thread is a sequence of thread lives. The second
/// life journals into a ring of its own: without that it journals
/// nothing at all and looks exactly like a thread doing nothing.
#[test]
fn a_second_life_on_one_thread_journals_into_a_ring_of_its_own() {
    let _quiet = kinds::disable_sites_for_test();
    const FIRST: u64 = 0x11FE;
    const SECOND: u64 = 0x21FE;
    let _g = crate::memory::block_pool::test_guard();
    let start = mark();

    let (first, second) = std::thread::spawn(|| {
        crate::memory::heap::ll_thread_init();
        record(ANY_KIND, 0, FIRST, 0, 0);
        let first = this_thread_identity();
        crate::memory::heap::ll_thread_exit();

        crate::memory::heap::ll_thread_init();
        record(ANY_KIND, 0, SECOND, 0, 0);
        let second = this_thread_identity();
        crate::memory::heap::ll_thread_exit();
        (first, second)
    })
    .join()
    .expect("the journaling thread panicked");

    let end = mark();
    assert_ne!(first, second, "the second life reused the first's ring");
    assert_ne!(second, 0, "the second life journaled nothing");
    let subjects: Vec<u64> = events(between(&start, &end))
        .into_iter()
        .filter(|event| event.thread == first || event.thread == second)
        .map(|event| event.subject)
        .collect();
    assert_eq!(subjects, vec![FIRST, SECOND]);
}

/// The retired list stops growing at [`RETIRED_KEPT`], and what it
/// drops is the oldest. That bound is the only thing between a
/// program that spawns a thread per request and a ring per request
/// held for the life of the process.
///
/// Read per identity through [`rings_named`], because the list is
/// process-global and a thread exiting elsewhere in the suite retires
/// a ring into it while this test counts.
#[test]
fn the_retired_list_keeps_the_newest_and_drops_the_oldest() {
    let _quiet = kinds::disable_sites_for_test();
    let _g = crate::memory::block_pool::test_guard();

    let mut mine = Vec::new();
    let mut freed = Vec::new();
    for _ in 0..=RETIRED_KEPT {
        let ring = allocate_ring();
        assert!(!ring.is_null());
        free_rings(register_ring(ring));
        mine.push(unsafe { (*ring).thread });
        retire_ring(ring);
        let evicted = std::mem::take(&mut locked().pending_free);
        for old in evicted {
            freed.push(unsafe { (*old).thread });
            unsafe { crate::memory::stdapi::ll_free(old as *mut u8) };
        }
    }

    let (_, retired_after) = registry_counts();
    assert_eq!(
        retired_after, RETIRED_KEPT,
        "the retired list outgrew its bound"
    );
    // Per identity, never by the list's totals, and as a boundary
    // rather than a count. Another thread retiring a ring inside the
    // loop above pushes the bound down on this test's rings too, and
    // `test_guard()` does not serialise a retirement — so how many of
    // them go is not this test's to state. What eviction guarantees
    // is the order: it drains the front, so the rings that went are a
    // prefix of the order they were retired in, and the oldest is
    // always among them.
    assert!(
        freed.contains(&mine[0]),
        "the oldest of this test's rings was not dropped"
    );
    let surviving: Vec<bool> = mine.iter().map(|&t| rings_named(t).1 == 1).collect();
    assert!(
        !surviving[0],
        "the ring reported dropped is still on the list"
    );
    assert!(
        surviving.windows(2).all(|pair| pair[0] <= pair[1]),
        "the list dropped a ring while keeping an older one: {surviving:?}"
    );

    // Leave the list as it was found, so that a later test's window
    // does not carry this one's evictions.
    for (thread, alive) in mine.into_iter().zip(surviving).skip(1) {
        assert_eq!(
            evict_retired_ring(thread),
            alive,
            "the list holds a different set of this test's rings than it reported"
        );
    }
}

/// A retiring thread frees no ring, because by then it cannot: its
/// parked backlog is gone, and a ring's free parks while a collector
/// collection is in flight. The rings wait for a thread that is not on its
/// way out — an investigator taking a mark is one.
#[test]
fn an_evicted_ring_is_freed_by_a_live_thread_rather_than_a_dying_one() {
    let _quiet = kinds::disable_sites_for_test();
    const SUBJECT: u64 = 0xEB0;
    let _g = crate::memory::block_pool::test_guard();
    let identity = a_journaling_thread(SUBJECT);
    // A delta rather than a total: the quota evicts other tests'
    // rings into this same list whenever the suite journals.
    let pending_before = pending_count();

    {
        let mut registry = locked();
        evict_retired(&mut registry, 1);
    }

    assert_eq!(
        pending_count(),
        pending_before + 1,
        "the eviction freed on the spot instead of leaving the ring"
    );

    let _ = mark();
    assert_eq!(pending_count(), 0, "a mark left the eviction unfreed");
    // The oldest retired ring is whichever test ran before this one,
    // so the eviction above may have taken that rather than this
    // test's. Leave nothing of its own behind either way.
    let _ = evict_retired_ring(identity);
}
