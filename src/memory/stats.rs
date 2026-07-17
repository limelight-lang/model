//! Aggregate memory telemetry — layer 1 of the two-layer design
//! (PLAN.md): cheap always-on numbers in the mimalloc/jemalloc spirit.
//!
//! **Design decision: zero hot-path tax.** No allocation path
//! increments anything per object. The counters live only on the rare
//! block operations (pool get/put), and everything else is computed at
//! query time from structures that already exist. Stats are therefore
//! block-granular; per-object precision belongs to layer 2, the opt-in
//! event log (heaptrack-style: timestamps, lifetimes, call sites via
//! `LLAllocSite`), which is a separate design pass.

use crate::memory::block_pool::{BLOCK_SIZE, BlockPool, REGION_SIZE};

/// A point-in-time snapshot of the memory manager.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemoryStats {
    /// 2 MB regions requested from the OS (never returned in phase 1).
    pub regions_carved: usize,
    /// Bytes held from the OS via regions.
    pub resident_bytes: usize,
    /// Blocks currently out of the pool, all consumers.
    pub blocks_out: usize,
    /// Bytes those blocks span — the block-granular "active" number.
    pub active_bytes: usize,
    /// Blocks sitting free in the pool (including thread caches).
    pub blocks_free: usize,
}

/// Snapshot the manager. O(1): reads a handful of relaxed counters.
pub fn memory_stats() -> MemoryStats {
    let pool = BlockPool::global();
    let regions = pool.regions_carved();
    let out = pool.blocks_out();
    let total_blocks = regions * (REGION_SIZE / BLOCK_SIZE);
    MemoryStats {
        regions_carved: regions,
        resident_bytes: regions * REGION_SIZE,
        blocks_out: out,
        active_bytes: out * BLOCK_SIZE,
        blocks_free: total_blocks.saturating_sub(out),
    }
}

/// ABI: write the snapshot to `out`.
///
/// # Safety
/// `out` must point to writable memory of `MemoryStats` size.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ll_memory_stats(out: *mut MemoryStats) {
    unsafe { out.write(memory_stats()) };
}

#[cfg(test)]
mod tests {
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
}
