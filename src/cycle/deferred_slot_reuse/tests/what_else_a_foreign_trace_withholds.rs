//! The addresses a trace holds beside entity slots, withheld under a
//! foreign holder of the token the way a death is: a buffer chunk an array's
//! growth would free, a whole block the pool would take back — an arena's
//! at its reset, a buffer arena's when it empties — and an OS-direct run.
//!
//! Every case reads the count of what stands withheld and the poll's return
//! of it; what the reuse would have cost is the module doc's argument, and
//! a chunk's reuse in particular is not observable through the allocator,
//! whose bump precedes its free list.

use super::*;
use crate::cycle::token::testing::HeldByACollector;
use crate::cycle::token::this_thread_token;
use crate::memory::block_pool::{BLOCK_PAYLOAD, BlockPool};
use crate::memory::buffer_arena::{buffer_alloc_longlived_payload, buffer_free_longlived_payload};

/// Red without the chunk arm: the free would reach the arena's free list at
/// once.
#[test]
fn a_chunk_freed_under_a_holder_waits_for_the_release() {
    let _guard = test_guard();
    let (chunk, granted) = buffer_alloc_longlived_payload(256);
    assert!(!chunk.is_null(), "the buffer arena served a chunk");

    let mut holder = HeldByACollector::take(this_thread_token(), false);
    unsafe { buffer_free_longlived_payload(chunk, granted) };
    assert_eq!(
        foreign_withheld_chunks(),
        1,
        "the chunk waits under the holder"
    );

    holder.release();
    unsafe { crate::gc::ll_gc_maybe_collect() };
    assert_eq!(foreign_withheld_chunks(), 0, "the poll gave the chunk back");
}

/// A whole block the pool would take back waits too: an arena's blocks at
/// its reset, which a trace may hold addresses into as untracked children.
#[test]
fn an_arenas_blocks_wait_for_the_release_at_its_reset() {
    let _guard = test_guard();
    let pool = BlockPool::global();
    let mut arena = Arena::new();
    // Past one block, so the reset returns at least one.
    for _ in 0..3 {
        let p = arena.alloc(BLOCK_PAYLOAD / 2);
        assert!(!p.is_null(), "the arena served");
    }

    let out_before = pool.blocks_out();
    let mut holder = HeldByACollector::take(this_thread_token(), false);
    unsafe { crate::promote::arena_reset_full(&mut arena) };
    assert!(
        foreign_withheld_blocks() >= 1,
        "the reset's returns wait under the holder"
    );
    assert_eq!(
        pool.blocks_out(),
        out_before,
        "no block reached the pool under the holder"
    );

    holder.release();
    unsafe { crate::gc::ll_gc_maybe_collect() };
    assert_eq!(
        foreign_withheld_blocks(),
        0,
        "the poll gave the blocks back"
    );
    assert!(pool.blocks_out() < out_before, "the pool has them");
}

/// An OS-direct run — a payload past one block, which the buffer arena
/// serves as a mapping of its own — is unmapped at its free, and under a
/// holder that unmapping waits: a trace striding it would read unmapped
/// memory, which is the one hazard of the four no stale reading survives.
#[test]
fn a_run_freed_under_a_holder_waits_for_the_release() {
    let _guard = test_guard();
    let (run, granted) = buffer_alloc_longlived_payload(BLOCK_PAYLOAD + 1);
    assert!(!run.is_null(), "the system served a run");

    let mut holder = HeldByACollector::take(this_thread_token(), false);
    unsafe { buffer_free_longlived_payload(run, granted) };
    assert_eq!(
        foreign_withheld_blocks(),
        1,
        "the run waits under the holder"
    );

    holder.release();
    unsafe { crate::gc::ll_gc_maybe_collect() };
    assert_eq!(foreign_withheld_blocks(), 0, "the poll unmapped it");
}
