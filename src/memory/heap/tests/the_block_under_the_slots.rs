//! The header's halves are split by access rule, `kind` at offset 0
//! because the pool's own header shares that word. A block is kept
//! rather than returned and re-carved — with one live slot left in
//! it, which the larson run measured at ~140 ns/op against ~8, and
//! emptied of everything too, where it becomes the class's one
//! bounded empty spare. A full block is replaced by a refill.

use super::*;

#[test]
fn block_header_halves_are_laid_out_as_the_design_requires() {
    use std::mem::offset_of;

    // `kind` is the pool's tagged-union discriminant: the whole
    // overlay depends on it staying at offset 0 of the block.
    assert_eq!(offset_of!(HeapBlockHeader, kind), 0);
    assert_eq!(offset_of!(HeapBlockHeader, size_class), 4);
    assert_eq!(offset_of!(HeapBlockHeader, private), 8);

    // `owner` is read by every `free` to decide whether the slot is
    // local, so it must stay in the same line as the hot private
    // fields. Giving it a line of its own cost +10.8% on
    // `rptest_10k_blocks_40_iters` (measured), because each local
    // free then touched a second line just for the check.
    // Everything the fast paths touch — the counters, the free list,
    // the `available` links, and `owner` — must fit line 0 together.
    // Each field evicted from it costs a miss on a path that runs per
    // allocation or per full ↔ has-room transition; both evictions
    // that were tried measured slower (see `BlockShared`).
    let shared = offset_of!(HeapBlockHeader, shared);
    assert_eq!(
        (shared + size_of::<BlockShared>()).div_ceil(64),
        1,
        "the hot set (private + owner) must fit one cache line, got {} bytes",
        shared + size_of::<BlockShared>()
    );

    // The contended field, by contrast, must be alone on its line, or
    // a cross-thread push steals the line holding `used`/`free`/`bump`
    // (audit `heap.rs:212`).
    let remote = offset_of!(HeapBlockHeader, remote);
    assert_eq!(remote % 64, 0, "remote_free must begin a cache line");
    assert!(remote >= 64, "remote_free must leave the hot line");
    assert_eq!(
        offset_of!(HeapBlockHeader, links) / 64 >= 1,
        true,
        "cold links must not crowd the hot line"
    );

    // The header lives in the block's reserved first line; growing it
    // past that would eat payload and change slots-per-block.
    assert!(
        size_of::<HeapBlockHeader>() <= LINE_SIZE,
        "header must fit the reserved line: {} > {LINE_SIZE}",
        size_of::<HeapBlockHeader>()
    );
}

/// Regression test for the pathology found via a real `larson.cpp`
/// benchmark run: alloc-then-immediately-free of one object,
/// repeated, must reuse the retained block instead of handing it back
/// to `BlockPool` and taking it again on every cycle, which cost
/// ~140 ns/op instead of ~8 ns/op. The re-carve that follows from
/// that traffic is not what is watched here: carving is counted
/// process-wide, while the hand-back is the first half of the
/// pathology and belongs to this thread. See
/// `rfc/model/memory/heap-slot-allocation.md`.
#[test]
fn single_live_slot_churn_keeps_its_block() {
    // The instrument is this thread's block cache, not `blocks_out`
    // or `regions_carved`: both are process-global, and a thread
    // that takes a block for any reason of its own — a journal ring
    // among them — moves them under a test holding no lock over
    // them. A block handed back per cycle lands in this cache and
    // reads as one more than the baseline.
    let _g = crate::memory::block_pool::test_guard();
    let mut heap = Heap::new();

    // Warm up: this alloc carves the one block we expect to be
    // retained for the rest of the test.
    let warm = heap.alloc(64);
    unsafe { heap.free(warm) };
    let cached_before = crate::memory::block_pool::thread_cache_len();

    for i in 0..10_000u32 {
        let p = heap.alloc(64);
        assert!(!p.is_null());
        unsafe { p.write(i as u8) };
        unsafe { heap.free(p) };
        assert_eq!(
            crate::memory::block_pool::thread_cache_len(),
            cached_before,
            "block was returned to the pool on iteration {i}"
        );
    }
}

/// **Two blocks, because one proves nothing.** `Heap::retire_empty`
/// keeps the first emptied block of a class as that class's one
/// bounded empty spare and returns only the next one, so a test that
/// fills and empties a single block watches a block that never
/// leaves the thread. The second block is followed by address, the
/// way the buffer arena's own test follows one: the process-global
/// `blocks_out` moves under this test for reasons it is not about.
#[test]
fn empty_block_returns_to_pool() {
    let _g = crate::memory::block_pool::test_guard();
    let pool = BlockPool::global();
    let mut heap = Heap::new();

    let class = 64usize;
    let slots = BLOCK_PAYLOAD / class;
    let ptrs: Vec<_> = (0..2 * slots).map(|_| heap.alloc(class)).collect();
    let spare = ptrs[0] as usize & !BLOCK_MASK;
    let returned = ptrs[2 * slots - 1] as usize & !BLOCK_MASK;
    assert_ne!(spare, returned, "the fill has to span two blocks");

    let regions_before = pool.regions_carved();
    for p in &ptrs {
        unsafe { heap.free(*p) };
    }

    let mut drawn = Vec::new();
    let mut found = false;
    for _ in 0..16 {
        let b = pool.get();
        assert!(!b.is_null());
        drawn.push(b);
        if b as usize == returned {
            found = true;
            break;
        }
    }

    for b in drawn {
        pool.put(b);
    }

    assert!(found, "the second emptied block never reached the pool");

    // And the first is still here, so the next allocation is served
    // without carving.
    let p = heap.alloc(class);
    assert_eq!(p as usize & !BLOCK_MASK, spare, "the spare serves it");
    assert_eq!(pool.regions_carved(), regions_before);
    unsafe { heap.free(p) };
}

#[test]
fn full_block_refills_and_serves_distinct_slots() {
    let _g = crate::memory::block_pool::test_guard();
    let mut heap = Heap::new();
    let class = 128usize;
    let slots = BLOCK_PAYLOAD / class;

    let ptrs: Vec<_> = (0..slots + 10).map(|_| heap.alloc(class)).collect();
    assert!(ptrs.iter().all(|p| !p.is_null()));

    let mut sorted = ptrs.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted.len(), ptrs.len(), "no slot handed out twice");

    for p in ptrs {
        unsafe { heap.free(p) };
    }
}
