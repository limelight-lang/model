//! Refusal is reported as null on every door. The two bands are
//! different defects: a near-`usize::MAX` size would wrap to a tiny
//! run and under-allocate, while anything past `isize::MAX` is
//! refused by `Layout` and used to abort the process.

use super::*;

#[test]
fn huge_size_overflow_returns_null_not_underallocation() {
    let _g = crate::memory::block_pool::test_guard();
    unsafe {
        // size + LINE_SIZE and the block round-up both overflow usize:
        // the request must be refused, never wrapped to a small run.
        assert!(ll_alloc(usize::MAX, 16).is_null());
        assert!(ll_alloc(usize::MAX - 100, 16).is_null());
    }
}

/// A size past `isize::MAX` is refused rather than aborting the
/// process. It sits in the band the checked arithmetic passes and
/// `Layout` refuses, so the old `unwrap` panicked — and a panic
/// crossing `extern "C"` aborts, which is a report no caller can
/// receive. `0x8000_0000_0000_0000` is a caller that lost a sign;
/// `isize::MAX` itself is refused too, the block round-up carrying it
/// over the limit.
#[test]
fn a_size_past_isize_max_returns_null_rather_than_aborting() {
    let _g = crate::memory::block_pool::test_guard();
    unsafe {
        assert!(ll_alloc(0x8000_0000_0000_0000, 16).is_null());
        assert!(ll_alloc(isize::MAX as usize, 16).is_null());
        // The ABI door as well, which is where the abort would have
        // happened: `ll_alloc` panics into its Rust caller, while
        // `ll_malloc` is `extern "C"` and cannot unwind out.
        assert!(ll_malloc(0x8000_0000_0000_0000).is_null());

        // And the growth door, which reaches the same call through
        // `ll_alloc`. A refused growth keeps the original.
        let live = ll_alloc(64, 16);
        assert!(!live.is_null());
        assert!(ll_realloc(live, 0x8000_0000_0000_0000, 16).is_null());
        std::ptr::write_bytes(live, 0xAB, 64);
        assert_eq!(*live, 0xAB, "the original survived the refusal");
        ll_free(live);
    }
}

/// The pooled LARGE path is the middle band — bigger than a heap slot,
/// smaller than a block payload — and it is the band the exhaustion
/// contract was written for: null, never a dead process. It used to
/// write the block header before looking at the pointer, so a refusal
/// there was a null dereference.
#[test]
fn pooled_large_reports_exhaustion_instead_of_writing_through_null() {
    let _g = crate::memory::block_pool::test_guard();
    use crate::memory::block_pool::FORCE_OOM;
    use std::sync::atomic::Ordering;

    FORCE_OOM.store(true, Ordering::Relaxed);
    let p = unsafe { ll_alloc(20_000, 16) };
    let aligned = unsafe { ll_alloc(40, 64) }; // align > 16 routes here too
    FORCE_OOM.store(false, Ordering::Relaxed);

    assert!(p.is_null(), "exhaustion must report, not abort");
    assert!(aligned.is_null(), "the over-aligned route reports too");

    let q = unsafe { ll_alloc(20_000, 16) };
    assert!(!q.is_null(), "the path survived the refusal");
    unsafe { ll_free(q) };
}
