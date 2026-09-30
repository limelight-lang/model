//! A ring counts every record written into it, by kind and, for a coded kind,
//! by its code `a`, with the sum of `b` beside: the answer for a window of
//! more records than a ring holds. The count outlives the ring's thread, and
//! the registry keeps an evicted ring's count after the ring is freed.

use super::*;

/// A coded kind is counted under its code, an `a` past the last code under
/// the last; a kind that is not coded is counted whole whatever its `a`; a
/// kind past the mask's width is counted in the row its low bits name, with
/// no panic.
#[test]
fn a_coded_kind_is_counted_by_its_code_and_the_rest_whole() {
    let _quiet = kinds::disable_sites_for_test();
    let _g = crate::memory::block_pool::test_guard();
    // The record that opens this thread's ring, where no site has.
    record(ANY_KIND, 0, 0, 0, 0);
    let identity = this_thread_identity();
    let before = counts_of(&[identity]);

    record(
        kinds::KIND_BATCH_END,
        0,
        0,
        kinds::BATCH_END_DEFERRED_PAST_B,
        5,
    );
    record(
        kinds::KIND_BATCH_END,
        0,
        0,
        kinds::BATCH_END_DEFERRED_PAST_B,
        7,
    );
    record(kinds::KIND_BATCH_END, 0, 0, 40, 1);
    record(ANY_KIND, 0, 0, 9, 3);
    record(ANY_KIND + 64, 0, 0, 2, 4);

    let read = counts_of(&[identity]).since(&before);
    assert_eq!(
        read.records(kinds::KIND_BATCH_END, kinds::BATCH_END_DEFERRED_PAST_B),
        2
    );
    assert_eq!(
        read.sum_of_b(kinds::KIND_BATCH_END, kinds::BATCH_END_DEFERRED_PAST_B),
        12
    );
    assert_eq!(
        read.records(kinds::KIND_BATCH_END, CODES as u64 - 1),
        1,
        "a code past the last is counted under the last"
    );
    assert_eq!(read.records_of_kind(kinds::KIND_BATCH_END), 3);
    assert_eq!(
        read.records(ANY_KIND, 0),
        2,
        "a kind that is not coded is counted whole, and 127 in 63's row"
    );
    assert_eq!(read.sum_of_b_of_kind(ANY_KIND), 7);
}

/// A thread's count is read after it exits, from its retired ring.
#[test]
fn a_threads_count_is_read_after_its_exit() {
    let _quiet = kinds::disable_sites_for_test();
    let _g = crate::memory::block_pool::test_guard();

    let joined = a_journaling_thread(0xC0);

    let read = counts_of(&[joined]);
    assert_eq!(read.records(ANY_KIND, 0), 1);
}

/// A retired ring the quota evicts leaves its count with the registry, so
/// that the process's count still holds it after the ring is freed.
#[test]
fn an_evicted_rings_count_stays_with_the_registry() {
    let _quiet = kinds::disable_sites_for_test();
    let _g = crate::memory::block_pool::test_guard();
    let before = counts();

    let joined = std::thread::spawn(|| {
        assert!(
            crate::memory::heap::ll_thread_init(),
            "the runtime started this thread"
        );
        for b in 1..=3 {
            record(ANY_KIND, 0, 0xC1, 0, b);
        }
        let identity = this_thread_identity();
        crate::memory::heap::ll_thread_exit();
        identity
    })
    .join()
    .expect("the journaling thread panicked");
    {
        let mut registry = locked();
        let at = registry
            .retired
            .iter()
            .position(|&ring| unsafe { (*ring).thread } == joined)
            .expect("the ring is retired");
        let ring = registry.retired.remove(at);
        registry.retired.insert(0, ring);
        evict_retired(&mut registry, 1);
    }
    // A mark is a live thread's read, which frees the evicted ring.
    let _ = mark();

    assert_eq!(
        counts_of(&[joined]).records(ANY_KIND, 0),
        0,
        "the ring itself is gone"
    );
    let read = counts().since(&before);
    assert_eq!(read.records(ANY_KIND, 0), 3);
    assert_eq!(read.sum_of_b(ANY_KIND, 0), 6);
}
