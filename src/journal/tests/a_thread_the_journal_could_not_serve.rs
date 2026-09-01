//! A refused allocation closes the thread instead of queueing a
//! retry, which would take two process-global mutexes and ask the OS
//! for a block on every later record — under exactly the memory
//! pressure the journal is turned on to investigate. A thread that
//! cannot arm its exit guard is refused the same way, a ring nothing
//! retires staying on the live list for the life of the process.
//! Such a thread is in no window, so it is counted: the count is
//! what keeps its silence from reading as inactivity, and its later
//! records are not counted as losses on top of it.

use super::*;

/// A refused allocation closes the thread instead of queueing a
/// retry. Retrying would take two process-global mutexes and ask the
/// OS for a block on every later record — under the memory pressure
/// the journal was turned on to investigate, which is where §9.7's
/// "no allocation, no lock" is worth the most.
#[test]
fn a_refused_ring_is_not_asked_for_a_second_time() {
    let _quiet = kinds::disable_sites_for_test();
    use crate::memory::block_pool::force_oom;
    let _g = crate::memory::block_pool::test_guard();
    let start = mark();

    let identity = std::thread::spawn(|| {
        assert!(
            crate::memory::heap::ll_thread_init(),
            "the runtime started this thread"
        );
        let oom = force_oom();
        record(ANY_KIND, 0, 1, 0, 0);
        drop(oom);
        // The pressure is gone and this thread still journals
        // nothing: the refusal was final, not a bad moment.
        record(ANY_KIND, 0, 2, 0, 0);
        let identity = this_thread_identity();
        crate::memory::heap::ll_thread_exit();
        identity
    })
    .join()
    .expect("the journaling thread panicked");

    assert_eq!(identity, 0, "a refused thread ended up with a ring");
    // The refusal count rather than the registry's totals: every
    // thread in the run moves those, and a ring retired by one of
    // them inside this window reads exactly like a ring granted here
    // (seen 1 in 300 runs at eight threads). Exactly one is the
    // stronger claim: a second ask refused again counts twice, and a
    // second ask granted shows in the identity asserted above. The
    // pool's guard holds the only tests that provoke a refusal.
    let end = mark();
    assert_eq!(
        end.refusals,
        start.refusals + 1,
        "the refusal was asked again rather than remembered"
    );
}

/// A thread that cannot arm its exit guard gets no ring: the guard is
/// what retires one, and a ring nothing retires stays on the live
/// list for the life of the process, where every later window reads
/// it as a live thread doing nothing. The state is real — a
/// destructor that allocates reaches a record site with the guard's
/// slot already destroyed — and it is counted like a refusal, being
/// the same silence from the reader's side.
#[test]
fn a_thread_that_cannot_arm_its_exit_guard_is_given_no_ring() {
    let _quiet = kinds::disable_sites_for_test();
    use crate::memory::heap::FORCE_GUARD_UNARMED;
    let _g = crate::memory::block_pool::test_guard();
    let counts_before = registry_counts();
    let start = mark();

    let identity = std::thread::spawn(|| {
        assert!(
            crate::memory::heap::ll_thread_init(),
            "the runtime started this thread"
        );
        FORCE_GUARD_UNARMED.store(true, Ordering::Relaxed);
        record(ANY_KIND, 0, 5, 0, 0);
        let identity = this_thread_identity();
        FORCE_GUARD_UNARMED.store(false, Ordering::Relaxed);
        crate::memory::heap::ll_thread_exit();
        identity
    })
    .join()
    .expect("the journaling thread panicked");

    let end = mark();
    assert_eq!(identity, 0, "a thread with no exit guard opened a ring");
    assert_eq!(
        registry_counts(),
        counts_before,
        "a ring nothing will retire was registered anyway"
    );
    let reported = between(&start, &end)
        .into_iter()
        .find_map(|window| match window {
            Window::Refused { threads } => Some(threads),
            _ => None,
        });

    assert_eq!(
        reported,
        Some(start.refusals + 1),
        "the thread left no trace in the window that covered it"
    );
}

/// A refused thread's later records are not counted as losses. Its
/// silence is already reported for the whole of its life by
/// [`Window::Refused`], and counting every record it goes on to raise
/// would mark every window it runs through as having lost something —
/// the degradation the per-window difference exists to avoid, through
/// a second door.
#[test]
fn a_refused_threads_later_records_are_not_counted_as_losses() {
    let _quiet = kinds::disable_sites_for_test();
    use crate::memory::block_pool::force_oom;
    let _g = crate::memory::block_pool::test_guard();

    let (announce, announced) = std::sync::mpsc::channel();
    let (go, wait) = std::sync::mpsc::channel();
    let refused = std::thread::spawn(move || {
        assert!(
            crate::memory::heap::ll_thread_init(),
            "the runtime started this thread"
        );
        let oom = force_oom();
        record(ANY_KIND, 0, 1, 0, 0);
        drop(oom);
        announce.send(()).expect("the test hung up");

        wait.recv().expect("the test hung up");
        // Raised while refused, and inside the window below.
        record(ANY_KIND, 0, 2, 0, 0);
        announce.send(()).expect("the test hung up");

        wait.recv().expect("the test hung up");
        crate::memory::heap::ll_thread_exit();
    });

    announced.recv().expect("the thread hung up");
    let start = mark();
    go.send(()).expect("the thread hung up");
    announced.recv().expect("the thread hung up");
    let end = mark();
    go.send(()).expect("the thread hung up");
    refused.join().expect("the refused thread panicked");
    assert!(
        !between(&start, &end)
            .iter()
            .any(|window| matches!(window, Window::Lost { .. })),
        "a refused thread's records were counted as losses"
    );
}

/// A thread whose ring the allocator refused is in no window at all,
/// so the count of such threads is the only thing standing between a
/// reader and the conclusion that they did nothing.
#[test]
fn a_thread_refused_a_ring_is_counted_since_it_is_in_no_window() {
    let _quiet = kinds::disable_sites_for_test();
    use crate::memory::block_pool::force_oom;
    let _g = crate::memory::block_pool::test_guard();
    let start = mark();

    std::thread::spawn(|| {
        assert!(
            crate::memory::heap::ll_thread_init(),
            "the runtime started this thread"
        );
        let oom = force_oom();
        record(ANY_KIND, 0, 3, 0, 0);
        drop(oom);
        crate::memory::heap::ll_thread_exit();
    })
    .join()
    .expect("the journaling thread panicked");

    let end = mark();
    let reported = between(&start, &end)
        .into_iter()
        .find_map(|window| match window {
            Window::Refused { threads } => Some(threads),
            _ => None,
        });

    assert_eq!(
        reported,
        Some(start.refusals + 1),
        "a refused thread left no trace in the window that covered it"
    );
}
