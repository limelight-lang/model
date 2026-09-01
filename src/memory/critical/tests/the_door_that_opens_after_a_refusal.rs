//! The reserve is drawn only where the ordinary door has already said
//! no, so both halves have to be shown: that it holds what it says it
//! holds, and that it still serves while the pool refuses.

use super::*;

/// The capacity is a figure from the design read at block granularity,
/// and a reserve that quietly held fewer would turn an aborted
/// collection into an unreportable one.
#[test]
fn the_reserve_fills_to_its_capacity() {
    let _g = test_guard();
    drain_for_test();
    assert!(replenish(), "the pool served eight blocks");
    assert_eq!(blocks_held(), CRITICAL_BLOCKS);
    assert!(!is_drawn(), "a full reserve wants no safepoint");
    drain_for_test();
}

/// The case the reserve exists for. `FORCE_OOM` refuses the pool and
/// nothing else, so the assertion below names which door said no before
/// the draw is asked for anything.
#[test]
fn a_block_is_drawn_while_the_pool_refuses() {
    let _g = test_guard();
    drain_for_test();
    assert!(replenish());

    let oom = force_oom();
    assert!(
        BlockPool::global().get().is_null(),
        "the ordinary door is the one refusing"
    );
    let block = draw();
    assert!(!block.is_null(), "the critical door still serves");
    assert_eq!(blocks_held(), CRITICAL_BLOCKS - 1);
    assert!(is_drawn(), "and asks the next safepoint for a refill");
    assert!(
        !replenish(),
        "which cannot be given while the refusal lasts"
    );
    drop(oom);

    give_back(block);
    assert_eq!(blocks_held(), CRITICAL_BLOCKS, "the reserve took it back");
    assert!(!is_drawn());
    drain_for_test();
}

/// A block handed back to a full reserve is ordinary memory again. The
/// reserve is a fixed holding rather than a growing one: a return path
/// that kept everything would be a second block pool with no eviction.
#[test]
fn a_full_reserve_passes_a_returned_block_to_the_pool() {
    let _g = test_guard();
    drain_for_test();
    assert!(replenish());
    assert_eq!(blocks_held(), CRITICAL_BLOCKS);

    let block = BlockPool::global().get();
    assert!(!block.is_null());
    let before = BlockPool::global().blocks_out();
    give_back(block);
    assert_eq!(blocks_held(), CRITICAL_BLOCKS, "the reserve kept its size");
    assert_eq!(
        BlockPool::global().blocks_out(),
        before - 1,
        "and the block went to the pool"
    );
    drain_for_test();
}

/// A returned block is stamped on the way in rather than trusted to
/// carry the kind already. Every block the reserve hands out is assumed
/// to be an arena block, and the only thing enforcing that is that the
/// collection's arena is the one caller — an invariant in another file,
/// which a release store on a cold path replaces.
#[test]
fn a_returned_block_is_stamped_before_the_reserve_keeps_it() {
    let _g = test_guard();
    drain_for_test();

    let block = BlockPool::global().get();
    assert!(!block.is_null());
    assert_eq!(
        unsafe { crate::memory::block_pool::load_block_kind(&raw const (*block).kind) },
        crate::memory::block_pool::BLOCK_KIND_FREE,
        "the pool hands one over free"
    );

    give_back(block);
    assert_eq!(blocks_held(), 1, "the empty reserve kept it");
    assert_eq!(
        unsafe { crate::memory::block_pool::load_block_kind(&raw const (*block).kind) },
        BLOCK_KIND_ARENA,
        "and stamped it on the way in"
    );
    drain_for_test();
}
