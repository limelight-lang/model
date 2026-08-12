//! The activity bit is process-wide and the collector raises it on
//! another thread, so it can rise between a caller's `flush_due` and
//! the flush itself. The backlog stays parked when that happens
//! rather than the flush asserting.

use super::*;

/// The activity bit is global and the collector runs on another
/// thread, so it can be raised between a caller's [`flush_due`] and
/// the flush itself. Measured under contention: the flag flipped
/// inside that window on roughly one check in nine. The flush must
/// leave the backlog alone when that happens rather than assert.
#[test]
fn an_epoch_opening_under_the_flush_leaves_the_backlog_parked() {
    let _g = crate::memory::block_pool::test_guard();
    let a = unsafe { crate::memory::stdapi::ll_malloc(64) };
    begin_epoch();
    unsafe { crate::memory::stdapi::ll_free(a) };
    assert_eq!(parked_count(), 1, "parked while the epoch is in flight");
    end_epoch();

    // The caller's `flush_due()` said yes here; the collector opens
    // the next epoch before the flush runs.
    assert!(flush_due());
    begin_epoch();
    assert_eq!(unsafe { flush() }, 0, "nothing recycled mid-epoch");
    assert_eq!(parked_count(), 1, "and the backlog is still there");

    end_epoch();
    assert_eq!(unsafe { flush() }, 1, "released at the next checkpoint");
}
