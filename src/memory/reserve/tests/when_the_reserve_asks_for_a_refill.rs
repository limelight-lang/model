//! The reserve reports its own emptiness rather than being polled for
//! it: the refill point is a caller's, and it needs the signal.

use super::*;

#[test]
fn filling_then_drawing_reports_that_it_wants_a_refill() {
    let _g = crate::memory::block_pool::test_guard();
    drain_for_test();

    assert!(replenish());
    assert_eq!(blocks_held(), RESERVE_BLOCKS);
    assert!(!is_drawn());

    let block = draw();
    assert!(!block.is_null());
    assert!(is_drawn(), "a draw asks the next safepoint for a refill");
    assert_eq!(blocks_held(), RESERVE_BLOCKS - 1);

    BlockPool::global().put(block);
    assert!(replenish());
    assert!(!is_drawn(), "and the refill answers it");
    drain_for_test();
}
