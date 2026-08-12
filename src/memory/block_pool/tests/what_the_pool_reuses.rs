//! A returned block comes back without carving a new region, the
//! thread cache keeps a bounded half, and a dying thread hands the
//! rest to the global stack rather than dropping it.

use super::*;

#[test]
fn put_then_get_reuses_without_new_region() {
    let _g = test_guard();
    let pool = BlockPool::global();
    let block = pool.get();
    let regions_before = pool.regions_carved();

    pool.put(block);
    let again = pool.get();

    assert_eq!(again, block, "thread cache must hand the block back");
    assert_eq!(pool.regions_carved(), regions_before);
    pool.put(again);
}

/// The overflow flush keeps half the cache and hands the rest to the
/// global list, and it does that by staging into a fixed array whose
/// width is [`FLUSH_MAX`]. The width is the load-bearing part: the
/// staging exists so the cache's borrow ends before the push, and a
/// `put` that overflowed by more than one block would index past the
/// array — a panic on a path this runtime aborts on.
#[test]
fn an_overflowing_cache_keeps_half_and_flushes_the_rest() {
    let _g = test_guard();
    let pool = BlockPool::global();
    drain_thread_cache();

    let mut blocks = Vec::new();
    for _ in 0..=THREAD_CACHE_CAPACITY {
        blocks.push(pool.get());
    }

    // The refill batch leaves blocks in the cache, so empty it again:
    // what this counts is what the puts below put there.
    drain_thread_cache();
    for block in &blocks {
        pool.put(*block);
    }

    assert_eq!(
        thread_cache_len(),
        THREAD_CACHE_CAPACITY / 2,
        "the flush kept a number of blocks other than half the cache"
    );
    // Nothing was dropped on the way out: the five the flush moved
    // are in the global list, so taking every block back costs no new
    // region. A staging array too narrow for the excess would lose
    // the difference here rather than report it — which is the whole
    // reason its width is a constant with an assertion behind it.
    let regions = pool.regions_carved();
    let mut back = Vec::new();
    for _ in 0..blocks.len() {
        back.push(pool.get());
    }

    assert_eq!(
        pool.regions_carved(),
        regions,
        "a block the flush moved never reached the global list"
    );
    for block in back {
        pool.put(block);
    }

    drain_thread_cache();
}

#[test]
fn dying_thread_returns_blocks_to_global_stack() {
    let _g = test_guard();
    let pool = BlockPool::global();

    let block_addr = std::thread::spawn(|| {
        let pool = BlockPool::global();
        let block = pool.get();
        pool.put(block); // lands in that thread's cache
        block as usize
        // thread dies -> cache Drop flushes to the global stack
    })
    .join()
    .unwrap();

    let regions_before = pool.regions_carved();

    // Drain until we find the flushed block: it must be reachable
    // without carving a new region.
    let mut taken = Vec::new();
    let mut found = false;
    for _ in 0..1000 {
        let b = pool.get();
        if b as usize == block_addr {
            found = true;
            taken.push(b);
            break;
        }

        taken.push(b);
    }

    assert!(found, "block cached by a dead thread was lost");
    assert_eq!(pool.regions_carved(), regions_before);

    for b in taken {
        pool.put(b);
    }
}
