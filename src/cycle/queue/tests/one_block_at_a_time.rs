//! The bounded retirement: one named block of R read, its completed deaths
//! retired, every other entry of it left as it stood, and the rest of the
//! queue not read at all. A block the sweep empties leaves the circle, a
//! hole between the front block and the tail block costing a collector a
//! read per hole (`crate::ring::tests`).

use super::*;
use crate::memory::gc_metadata::thread_stats;

/// The blocks of R, front block first.
fn ring_blocks() -> Vec<*mut BlockHeader> {
    let mut blocks = Vec::new();
    if let Some(ring) = candidate_ring() {
        ring.blocks_in_order(|block| blocks.push(block));
    }
    blocks
}

/// Register `entity` in R through the ordinary write.
///
/// # Safety
/// As [`allocated_candidate`], whose entity this is.
unsafe fn register(entity: *mut RcHeader) {
    unsafe {
        crate::refcount::update_header_flags(entity, |flags| flags | CANDIDATE_BIT);
        if spare_count() == 0 {
            assert!(refill_spares());
        }
        append_entry(mutator_state(), entity);
    }
}

#[test]
fn a_sweep_reads_its_own_block_and_no_other() {
    let _g = test_guard();
    reset();
    assert!(refill_spares());
    let _ = take_queue_work();
    let mut arena = Arena::new();
    let class = candidate_class("OneBlockAtATime");

    // Block 0: a death, then the block filled to its end with a live header
    // of the case's own — a filler the sweep reads as live and never frees,
    // so no entry of it names an allocation twice.
    let mut filler = candidate(2);
    let filler_entity = &raw mut filler;
    let dead_in_first = unsafe { allocated_candidate(&mut arena, class, 1) };
    unsafe { register(dead_in_first) };
    fill_tail_block(filler_entity);

    // Block 1: a death and a live entry.
    let dead_in_second = unsafe { allocated_candidate(&mut arena, class, 1) };
    unsafe { register(dead_in_second) };
    let live_in_second = unsafe { allocated_candidate(&mut arena, class, 1) };
    unsafe { register(live_in_second) };

    let blocks = ring_blocks();
    assert_eq!(blocks.len(), 2, "two blocks stand");
    let standing = candidate_count();
    unsafe { dismantle_candidate(dead_in_first) };
    unsafe { dismantle_candidate(dead_in_second) };

    let _ = take_queue_work();
    unsafe { sweep_one_block(blocks[1]) };

    assert_eq!(
        take_queue_work(),
        QueueWork {
            record_passes: 1,
            records_read: 2,
            records_moved: 1,
        },
        "the sweep read the second block's two records and kept its survivor"
    );
    assert_eq!(
        candidate_count(),
        standing - 1,
        "one death left the queue, and the first block's death stands"
    );

    let mut left = Vec::new();
    collect_lane_tokens(&mut left);
    assert!(
        left.contains(&dead_in_first),
        "the block the sweep never read keeps its death"
    );
    assert!(
        !left.contains(&dead_in_second),
        "and the swept one lost its"
    );
    assert!(
        left.contains(&live_in_second),
        "the survivor stays registered"
    );
    assert_eq!(segment_count(), 2, "no block left the circle");

    unsafe { dismantle_candidate(live_in_second) };
    unsafe { retire_candidates() };
    reset();
}

#[test]
fn a_block_the_sweep_empties_leaves_the_circle() {
    let _g = test_guard();
    reset();
    assert!(refill_spares());
    let mut arena = Arena::new();
    let class = candidate_class("OneBlockEmptied");

    // The first block is filled with a live header of the case's own; the
    // middle block is a block of deaths, one entity per entry, so that the
    // sweep empties it whole; the third holds one live entry.
    let mut filler = candidate(2);
    let filler_entity = &raw mut filler;
    let first = unsafe { allocated_candidate(&mut arena, class, 1) };
    unsafe { register(first) };
    fill_tail_block(filler_entity);
    let mut middle = Vec::with_capacity(BLOCK_ENTRIES);
    for _ in 0..BLOCK_ENTRIES {
        let entity = unsafe { allocated_candidate(&mut arena, class, 1) };
        unsafe { register(entity) };
        middle.push(entity);
    }
    let last = unsafe { allocated_candidate(&mut arena, class, 1) };
    unsafe { register(last) };

    let blocks = ring_blocks();
    assert_eq!(blocks.len(), 3, "three blocks stand");
    let spares_before = spare_count();
    let blocks_out_before = thread_stats().current_blocks();

    for &entity in &middle {
        unsafe { dismantle_candidate(entity) };
    }
    unsafe { sweep_one_block(blocks[1]) };

    assert_eq!(segment_count(), 2, "the emptied block left the circle");
    assert_eq!(ring_blocks(), vec![blocks[0], blocks[2]]);
    assert_eq!(
        spare_count(),
        spares_before + 1,
        "and went to a spare cell rather than to the pool"
    );
    assert_eq!(
        thread_stats().current_blocks(),
        blocks_out_before,
        "the thread holds the same blocks, one of them now spare"
    );

    unsafe { dismantle_candidate(first) };
    unsafe { dismantle_candidate(last) };
    unsafe { retire_candidates() };
    reset();
}

/// A sweep of a block the circle no longer holds is refused: the pointer a
/// bounded caller keeps outlives the block's place in the circle, and the
/// block may by then be a spare cell's, the reserve's, or another kind's.
#[test]
#[should_panic(expected = "a sweep of a block this thread's candidate ring does not hold")]
fn a_sweep_of_a_block_the_circle_no_longer_holds_is_refused() {
    let _g = test_guard();
    reset();
    assert!(refill_spares());
    let mut arena = Arena::new();
    let class = candidate_class("SweptOutOfTheCircle");
    let entity = unsafe { allocated_candidate(&mut arena, class, 1) };
    unsafe { register(entity) };
    let block = ring_blocks()[0];

    unsafe { dismantle_candidate(entity) };
    unsafe { retire_candidates() };
    // The blocks go back, so the pointer names no block of the circle.
    reset();
    assert!(ring_blocks().is_empty(), "the ring holds no block");

    unsafe { sweep_one_block(block) };
}
