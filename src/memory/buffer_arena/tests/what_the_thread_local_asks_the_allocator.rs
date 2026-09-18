//! What a thread's own buffer arena costs on the global allocator: nothing,
//! at its first use and at its dispose alike. The arena is the thread-local
//! itself rather than a box the first use builds, so a first long-lived
//! payload reached from a destructor at thread exit meets no `Box::new` that
//! could abort (`dev/DECISIONS.md`, "the reset window's memory comes from the
//! manager, and an allocation it cannot get is a refusal").

use super::*;
use crate::test_support::allocation_probe;

/// On a thread with no warm-up — the shape a worker thread has when a
/// destructor at its exit frees the first dynamic string's payload — the
/// first `with_buffer_arena` allocates nothing through the global allocator,
/// and the `dispose` after it frees nothing through it. Each half is read on
/// its own counter, because a `dealloc` is invisible to the allocation
/// count.
#[test]
fn a_threads_first_use_and_its_dispose_ask_the_global_allocator_for_nothing() {
    let _g = crate::memory::block_pool::test_guard();
    let (first_use, disposal) = std::thread::spawn(|| {
        allocation_probe::take_heap_allocations();
        allocation_probe::take_heap_deallocations();
        with_buffer_arena(|_| ());
        let first_use = allocation_probe::take_heap_allocations();
        dispose();
        let disposal = allocation_probe::take_heap_deallocations();
        (first_use, disposal)
    })
    .join()
    .unwrap();

    assert_eq!(
        (first_use, disposal),
        (0, 0),
        "(allocations at the first use, frees at the dispose): the arena was a box"
    );
}
