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

    // The triple takes the free tail of that same line, which the fit
    // above does not say: a header grown to 256 satisfies it and leaves
    // the triple nowhere. 192 is what `BlockRemote`'s alignment
    // produces, so only a literal catches a header that grew into the
    // tail.
    assert_eq!(size_of::<HeapBlockHeader>(), 192);
    assert_eq!(
        COLLECTOR_TRIPLE_OFFSET + size_of::<BlockCollector>(),
        LINE_SIZE,
        "the triple ends where the reserved line does, so no slot moves"
    );
}

/// The commissioning writes the triple and no production path reads it
/// yet — `entity_slot_index` is reached only from
/// `cycle::row::edge_to`, whose own caller is S35.1 of `PLAN.md`. A
/// `refill` that skipped the write would leave the pool's previous
/// contents in the block's tail, and the first row lookup would multiply
/// by them.
#[test]
fn a_commissioned_entity_block_carries_its_collector_triple() {
    let _g = crate::memory::block_pool::test_guard();
    let mut heap = Heap::new_entity();
    let class_size = SIZE_CLASSES[5];
    let slot = heap.alloc(class_size);
    let triple = unsafe { crate::memory::heap::block_collector(HeapBlockHeader::of_ptr(slot)) };

    unsafe {
        assert!(
            (*triple).shadow.load(Ordering::Relaxed).is_null(),
            "no collection has reserved rows for this block"
        );
        assert_eq!(
            (*triple).reciprocal.load(Ordering::Relaxed),
            crate::memory::heap::reciprocal_for(class_size)
        );
        assert_eq!(
            SIZE_CLASSES[(*triple).size_class.load(Ordering::Relaxed) as usize],
            class_size,
            "the collector's copy names the class the block was cut for"
        );
    }

    unsafe { heap.free(slot) };
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

/// The reciprocal multiply the collector reaches a shadow row by is the
/// division, at every size class and every offset a block payload can
/// hold — including the offsets between two slot starts, which is where
/// a reciprocal that is off by one ulp shows up first.
///
/// The expectation is the division of the same offset, never the slot
/// number the offset was built from: an index compared with an address
/// rebuilt from that index agrees with any reciprocal at all, exact or
/// not.
///
/// Wrong by one here is another live entity's row rather than a fault,
/// so the whole domain is walked rather than sampled: 32 classes over
/// 65280 offsets, about 2.1 million comparisons, which is a few
/// milliseconds compiled and minutes interpreted.
#[test]
#[cfg_attr(
    miri,
    ignore = "2.1 million integer comparisons and no memory access: Miri sees \
              nothing here the checked build does not"
)]
fn the_reciprocal_multiply_is_the_division_over_a_whole_block() {
    let mut compared = 0usize;
    for &stride in SIZE_CLASSES {
        for offset in 0..BLOCK_PAYLOAD {
            assert_eq!(
                crate::memory::heap::slot_index_of_offset(offset, stride),
                (offset / stride) as u32,
                "stride {stride}, offset {offset}"
            );
            compared += 1;
        }
    }

    // Walking the whole domain is the claim, and a loop that ran over
    // nothing passes the assertions above without making it. Ten
    // milliseconds for two million comparisons is fast enough to look
    // like that.
    assert_eq!(compared, SIZE_CLASSES.len() * BLOCK_PAYLOAD);
}

/// The offsets the derivation actually meets: a slot start of a real
/// block, reached the way the collector reaches it. What this adds to
/// the exhaustive test above is the two steps around it — the reciprocal
/// read out of the block's collector triple, and `LINE_SIZE` taken off
/// the address — neither of which that test exercises.
#[test]
fn a_slot_of_a_live_block_derives_the_index_its_address_says() {
    let _g = crate::memory::block_pool::test_guard();
    let mut heap = Heap::new_entity();
    let stride = SIZE_CLASSES[3];
    let slots: Vec<*mut u8> = (0..8).map(|_| heap.alloc(stride)).collect();
    let block = (slots[0] as usize) & !BLOCK_MASK;

    for slot in &slots {
        assert_eq!(
            (*slot as usize) & !BLOCK_MASK,
            block,
            "the fixture wanted one block"
        );
        assert_eq!(
            unsafe { crate::memory::heap::entity_slot_index(*slot) },
            ((*slot as usize - block - LINE_SIZE) / stride) as u32
        );
    }

    for slot in &slots {
        unsafe { heap.free(*slot) };
    }
}
