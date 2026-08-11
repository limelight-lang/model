use super::*;

/// The reserve reports its own emptiness rather than being polled for
/// it: the refill point is a caller's, and it needs the signal.
mod when_the_reserve_asks_for_a_refill {
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
}

/// The reserve exists for the case where the pool says no, so what it
/// already holds must stay drawable while the refusal lasts.
mod under_a_pool_that_refuses {
    use super::*;

    /// The reserve exists for the case where the pool says no, so it has
    /// to keep working when it does.
    #[test]
    fn a_drawn_block_is_available_while_the_pool_refuses() {
        let _g = crate::memory::block_pool::test_guard();
        use crate::memory::block_pool::FORCE_OOM;
        use std::sync::atomic::Ordering;
        drain_for_test();
        assert!(replenish());

        FORCE_OOM.store(true, Ordering::Relaxed);
        assert!(BlockPool::global().get().is_null(), "the pool is refusing");
        let block = draw();
        assert!(!block.is_null(), "the reserve still hands one out");
        assert!(!replenish(), "and cannot be refilled while it lasts");
        FORCE_OOM.store(false, Ordering::Relaxed);

        BlockPool::global().put(block);
        drain_for_test();
    }
}
