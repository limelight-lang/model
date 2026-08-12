//! Exhaustion is reported rather than fatal, and what is already
//! carved stays usable afterwards.

use super::*;

/// The third path that wrote the block header through the null the
/// pool now returns. Class loading runs mid-request under autoload,
/// so this one had to report as well.
#[test]
fn exhaustion_reports_null_and_leaves_the_region_usable() {
    let _g = crate::memory::block_pool::test_guard();
    use crate::memory::block_pool::FORCE_OOM;
    use std::sync::atomic::Ordering;

    // Fill whatever remains of the current block, so the next call
    // has to ask the pool.
    let _ = immortal_alloc(BLOCK_PAYLOAD);

    FORCE_OOM.store(true, Ordering::Relaxed);
    let p = immortal_alloc(64);
    FORCE_OOM.store(false, Ordering::Relaxed);
    assert!(p.is_null(), "exhaustion must report, not abort");

    let q = immortal_alloc(64);
    assert!(!q.is_null(), "the region survived the refusal");
}
