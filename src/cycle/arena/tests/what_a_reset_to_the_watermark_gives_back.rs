//! What a reset to the watermark leaves and what it takes: the copy below the
//! watermark stands, the rows are swept, every block drawn above it goes
//! back, the next part draws under the whole budget while the budget met
//! stands, the ledger returns to the copy's charge, and a reset the injection
//! interrupts is finished by the arena's drop.

use super::*;
use crate::memory::gc_metadata::thread_stats;

/// Bytes of the stand-in for a batch's copy of its roots.
const COPY: usize = 512;

/// The arena of a batch: a budget of `blocks`, a copy of [`COPY`] bytes
/// written with `pattern` and the watermark set above it.
fn a_batch_arena(blocks: usize, pattern: u8) -> (TraceScratchArena, *mut u8) {
    let mut arena = crate::cycle::testing::open_arena();
    arena.budget_blocks(blocks);
    let copy = arena.alloc(COPY);
    assert!(!copy.is_null(), "the copy fits the workspace");
    unsafe { copy.write_bytes(pattern, COPY) };
    arena.set_watermark();
    (arena, copy)
}

/// One part that meets a row in `block` and grows past the workspace into
/// one block.
fn a_part_that_draws_a_block(arena: &mut TraceScratchArena, block: *mut u8) {
    met(unsafe { arena.ensure_row(slot_row(block, 0), 1) });
    let room = arena.room_left();
    assert!(!arena.alloc(room).is_null());
    assert!(!arena.alloc(64).is_null(), "the pool served the growth");
    assert_eq!(arena.blocks_held(), 1);
}

#[test]
fn a_reset_to_the_watermark_gives_the_blocks_back_and_keeps_the_copy() {
    let _g = test_guard();
    crate::memory::critical::drain_for_test();
    let (mut heap, slot, block) = an_entity_block();
    let before = BlockPool::global().blocks_out();

    let (mut arena, copy) = a_batch_arena(2, 0x5a);
    a_part_that_draws_a_block(&mut arena, block);
    arena.reset_to_the_watermark();

    assert_eq!(arena.blocks_held(), 0, "the part's block went back");
    assert_eq!(BlockPool::global().blocks_out(), before, "to the pool");
    assert_eq!(arena.touched_blocks(), 0, "the part's rows were swept");
    assert!(
        unsafe { crate::memory::heap::block_shadow(block) }.is_null(),
        "and the block names no row of the part"
    );
    assert_eq!(
        arena.room_left(),
        WORKSPACE_BUMP_BYTES - COPY,
        "the bump stands at the watermark, above the copy"
    );
    assert!(
        (0..COPY).all(|offset| unsafe { *copy.add(offset) } == 0x5a),
        "and the copy below it is as the batch wrote it"
    );

    arena.reset();
    assert_eq!(BlockPool::global().blocks_out(), before);
    unsafe { heap.free(slot) };
    crate::memory::critical::drain_for_test();
}

/// The budget is each part's: under a budget of one, a part that drew its
/// block leaves the part after it the same block to draw, and the growth past
/// it is refused as the budget's; that refusal stands across the next reset
/// to the watermark, which is what tells the batch a part met its budget.
#[test]
fn every_part_draws_under_the_whole_budget() {
    let _g = test_guard();
    crate::memory::critical::drain_for_test();
    let (mut heap, slot, block) = an_entity_block();

    let (mut arena, _) = a_batch_arena(1, 0);
    a_part_that_draws_a_block(&mut arena, block);
    arena.reset_to_the_watermark();
    assert_eq!(
        arena.blocks_drawn(),
        0,
        "the part's block is counted no more"
    );

    a_part_that_draws_a_block(&mut arena, block);
    assert!(
        !arena.met_its_budget(),
        "the second part drew its block under a budget of one"
    );
    assert!(
        arena.alloc(BLOCK_PAYLOAD).is_null(),
        "and its growth past it passes the part's budget"
    );
    assert!(arena.met_its_budget(), "and it is the budget that refused");
    arena.reset_to_the_watermark();
    assert!(
        arena.met_its_budget(),
        "which the reset to the watermark leaves standing"
    );

    arena.reset();
    assert_eq!(arena.blocks_drawn(), 0);
    assert!(!arena.met_its_budget(), "and the arena's own reset clears");
    unsafe { heap.free(slot) };
    crate::memory::critical::drain_for_test();
}

/// The ledger after each part stands at the copy's charge, and after the
/// arena's reset where it stood before the open: the part's bytes are
/// discharged once, the copy's once, and none twice.
#[test]
fn the_ledger_returns_to_the_copy_after_each_part_and_to_nothing_after_the_reset() {
    let _g = test_guard();
    crate::memory::critical::drain_for_test();
    let (mut heap, slot, block) = an_entity_block();
    let before = thread_stats().current_bytes_in_use();

    let (mut arena, _) = a_batch_arena(4, 0);
    assert_eq!(
        thread_stats().current_bytes_in_use(),
        before + COPY,
        "the watermark charged the copy"
    );
    for _ in 0..2 {
        a_part_that_draws_a_block(&mut arena, block);
        assert!(thread_stats().current_bytes_in_use() > before + COPY);
        arena.reset_to_the_watermark();
        assert_eq!(
            thread_stats().current_bytes_in_use(),
            before + COPY,
            "the part's bytes were discharged and the copy's were not"
        );
    }

    arena.reset();
    assert_eq!(thread_stats().current_bytes_in_use(), before);
    unsafe { heap.free(slot) };
    crate::memory::critical::drain_for_test();
}

/// A reset to the watermark that raises where it hands the blocks back
/// leaves them the arena's, and the arena's drop gives them back and
/// discharges what stands, the copy's charge with it.
#[test]
fn a_reset_to_the_watermark_the_injection_interrupts_is_finished_by_the_drop() {
    let _g = test_guard();
    crate::memory::critical::drain_for_test();
    let (mut heap, slot, block) = an_entity_block();
    let blocks_before = BlockPool::global().blocks_out();
    let bytes_before = thread_stats().current_bytes_in_use();

    let (mut arena, _) = a_batch_arena(2, 0);
    a_part_that_draws_a_block(&mut arena, block);
    let armed = crate::cycle::arena::inject_reset_failure();
    let interrupted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        arena.reset_to_the_watermark();
    }));
    drop(armed);
    assert!(interrupted.is_err(), "the injection raised in the reset");
    assert_eq!(arena.blocks_held(), 1, "and the block is still the arena's");

    drop(arena);
    assert_eq!(BlockPool::global().blocks_out(), blocks_before);
    assert_eq!(thread_stats().current_bytes_in_use(), bytes_before);
    unsafe { heap.free(slot) };
    crate::memory::critical::drain_for_test();
}
