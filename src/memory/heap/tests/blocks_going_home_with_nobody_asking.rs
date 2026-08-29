//! A thread that ends without calling `ll_thread_exit` still gives
//! its blocks back through the TLS guard, and a heap that falls out
//! of scope gives them back through `Drop`; before either existed
//! the blocks were stranded, nothing else knowing about them. A
//! process with no TLS slot left reports that as a null allocation
//! rather than ending.

use super::*;

/// A thread that allocates and then exits **without** calling
/// `ll_thread_exit` must still give its blocks back: the TLS guard is
/// what makes that automatic, and it is the whole reason the guard
/// exists.
///
/// Regression for audit H9. The guard used to be `#[cfg(windows)]`, so
/// on ELF targets nothing reclaimed anything — every worker thread
/// stranded its blocks forever. This test passes natively on Windows
/// either way; the one that matters is the Miri run, which executes
/// the non-Windows path (see `dev/WORKFLOW.md`):
///
/// ```text
/// MIRIFLAGS="-Zmiri-ignore-leaks" cargo +nightly miri test \
///     --target x86_64-unknown-linux-gnu --lib h9_
/// ```
#[test]
fn h9_exiting_thread_returns_its_blocks_without_an_explicit_call() {
    // The ring a journaling thread takes is a block the registry keeps
    // after that thread is gone, and this test counts the blocks the
    // pool has out. Before the pool's guard, as `set_sites_for_test`
    // requires.
    let _quiet = crate::journal::kinds::disable_sites_for_test();
    let _g = crate::memory::block_pool::test_guard();
    let pool = BlockPool::global();
    let before = pool.blocks_out();

    for _ in 0..3 {
        std::thread::spawn(|| {
            assert!(ll_thread_init(), "the runtime started this thread");
            let p = unsafe { crate::memory::stdapi::ll_alloc(40, 16) };
            assert!(!p.is_null());
            unsafe { crate::memory::stdapi::ll_free(p) };
            // Deliberately no `ll_thread_exit()`: the guard must do it.
        })
        .join()
        .unwrap();
    }

    assert_eq!(
        pool.blocks_out(),
        before,
        "an exiting thread must not strand its blocks"
    );
}

/// A `Heap` that dies by falling out of scope must give its blocks
/// back, exactly as `ll_thread_exit` does. Before `Drop` existed they
/// were stranded: nothing else knew about them, so the pool never saw
/// them again. Revert `impl Drop for Heap` and this test fails on the
/// final assert.
#[test]
fn a_dropped_heap_returns_its_blocks_to_the_pool() {
    // Same reason as the test above, and it is a flake without this:
    // this one also counts the blocks the pool has out, and a ring
    // taken on this thread's first record is a block the registry
    // keeps for good. Which test journals first is a scheduling
    // accident, so the count came back one high about once in thirty
    // `debug-journal` runs. Before the pool's guard, as
    // `set_sites_for_test` requires.
    let _quiet = crate::journal::kinds::disable_sites_for_test();
    let _g = crate::memory::block_pool::test_guard();
    let pool = BlockPool::global();
    let before = pool.blocks_out();

    {
        let mut heap = Heap::new();
        let p = heap.alloc(40);
        assert!(!p.is_null());
        assert!(
            pool.blocks_out() > before,
            "the heap took a block from the pool"
        );
        // Free it so the block is empty: an empty block goes home to
        // the pool, which is what makes the count observable here.
        unsafe { heap.free(p) };
    }

    assert_eq!(
        pool.blocks_out(),
        before,
        "a heap dropped out of scope must not strand its blocks"
    );
}

/// A process with no TLS slot left cannot give this thread a heap.
/// That is reported the same way as any other exhaustion — the
/// allocation returns null — and not by ending the process, which is
/// what storing `TlsAlloc`'s failure value used to lead to: it equals
/// our "uninitialised" sentinel, so the slot would have looked
/// unreserved and every read would have gone to a bad TEB offset.
#[cfg(windows)]
#[test]
fn a_thread_without_a_tls_slot_reports_instead_of_dying() {
    let _g = crate::memory::block_pool::test_guard();
    use std::sync::atomic::Ordering;

    // A fresh thread: this one already has its heap installed.
    let (told, heapless, refused) = std::thread::spawn(|| {
        tls::FORCE_TLS_FAILURE.store(1, Ordering::Relaxed);
        // The refusal has to be *said*, not swallowed: a silent miss
        // leaves the caller believing the pointer was stored.
        let told = !tls::set(std::ptr::null_mut());
        assert!(ll_thread_init(), "the runtime started this thread");
        let heapless = thread_heap().is_null();
        let p = unsafe { crate::memory::stdapi::ll_alloc(40, 16) };
        tls::FORCE_TLS_FAILURE.store(0, Ordering::Relaxed);
        (told, heapless, p.is_null())
    })
    .join()
    .unwrap();

    assert!(
        told,
        "installing into a slot that does not exist must report"
    );
    assert!(heapless, "so the thread stays without a heap");
    assert!(refused, "and the allocation reports null");
}
