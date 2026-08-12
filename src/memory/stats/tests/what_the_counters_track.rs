//! Each counter answers in a unit of its own, and the unit is the
//! contract: pool traffic in blocks handed out, an arena's life in
//! whole blocks rather than bytes, residency in whole regions.

use super::*;

#[test]
fn blocks_out_follows_get_and_put() {
    let _g = crate::memory::block_pool::test_guard();
    let pool = BlockPool::global();

    let before = memory_stats();
    let a = pool.get();
    let b = pool.get();
    let during = memory_stats();
    assert_eq!(during.blocks_out, before.blocks_out + 2);
    assert_eq!(
        during.active_bytes,
        during.blocks_out * crate::memory::block_pool::BLOCK_SIZE
    );

    pool.put(a);
    pool.put(b);
    let after = memory_stats();
    assert_eq!(after.blocks_out, before.blocks_out);
}

#[test]
fn arena_lifecycle_is_visible_at_block_granularity() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = crate::memory::arena::Arena::new();

    let before = memory_stats();
    arena.alloc(64); // first block taken
    assert_eq!(memory_stats().blocks_out, before.blocks_out + 1);

    arena.reset(|_| {});
    assert_eq!(memory_stats().blocks_out, before.blocks_out);
}

#[test]
fn resident_accounts_whole_regions() {
    let _g = crate::memory::block_pool::test_guard();
    let s = memory_stats();
    assert_eq!(s.resident_bytes, s.regions_carved * REGION_SIZE);
    assert_eq!(
        s.blocks_free + s.blocks_out,
        s.regions_carved * (REGION_SIZE / BLOCK_SIZE),
        "every carved block is either out or free"
    );
}
