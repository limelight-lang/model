//! The reserve exists for the case where the pool says no, so what it
//! already holds must stay drawable while the refusal lasts.

use super::*;

/// The reserve exists for the case where the pool says no, so it has
/// to keep working when it does.
#[test]
fn a_drawn_block_is_available_while_the_pool_refuses() {
    let _g = crate::memory::block_pool::test_guard();
    use crate::memory::block_pool::force_oom;

    drain_for_test();
    assert!(replenish());

    let oom = force_oom();
    assert!(BlockPool::global().get().is_null(), "the pool is refusing");
    let block = draw();
    assert!(!block.is_null(), "the reserve still hands one out");
    assert!(!replenish(), "and cannot be refilled while it lasts");
    drop(oom);

    BlockPool::global().put(block);
    drain_for_test();
}
