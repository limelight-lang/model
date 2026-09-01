use super::*;

use crate::memory::block_pool::{
    BLOCK_KIND_GC_METADATA, BlockPool, FORCE_OOM, load_block_kind, test_guard,
};

fn current(role: GcBlockRole) -> usize {
    stats().current_blocks(role)
}

#[test]
fn a_shadow_arena_is_workspace_until_both_exit_paths_return_it() {
    let _g = test_guard();
    let before = current(GcBlockRole::WorkspaceOverflow);
    let mut arena = crate::cycle::arena::ShadowArena::new();

    let byte = arena.alloc(1);
    assert!(!byte.is_null());
    assert_eq!(current(GcBlockRole::WorkspaceOverflow), before + 1);
    let block = BlockHeader::of_ptr(byte);
    assert_eq!(
        unsafe { load_block_kind(&raw const (*block).kind) },
        BLOCK_KIND_GC_METADATA
    );

    arena.reset();
    assert_eq!(current(GcBlockRole::WorkspaceOverflow), before);

    assert!(!arena.alloc(1).is_null());
    drop(arena);
    assert_eq!(current(GcBlockRole::WorkspaceOverflow), before);
}

#[test]
fn a_threads_exit_ends_every_queue_role_it_acquired() {
    let _g = test_guard();
    let floor_before = current(GcBlockRole::QueueFloor);
    let segment_before = current(GcBlockRole::QueueSegment);
    let blocks_before = BlockPool::global().blocks_out();

    std::thread::spawn(|| {
        assert!(crate::memory::heap::ll_thread_init());
        assert!(current(GcBlockRole::QueueFloor) >= 1);
        assert!(current(GcBlockRole::QueueSegment) >= 2);
    })
    .join()
    .unwrap();

    assert_eq!(current(GcBlockRole::QueueFloor), floor_before);
    assert_eq!(current(GcBlockRole::QueueSegment), segment_before);
    let kept = usize::from(cfg!(feature = "debug-journal"));
    assert!(BlockPool::global().blocks_out() <= blocks_before + kept);
}

#[test]
fn a_critical_workspace_draw_is_charged_only_while_the_arena_holds_it() {
    let _g = test_guard();
    assert!(crate::memory::critical::replenish());
    let before = current(GcBlockRole::WorkspaceOverflow);
    let mut arena = crate::cycle::arena::ShadowArena::new();

    FORCE_OOM.store(true, Ordering::Relaxed);
    let byte = arena.alloc(1);
    FORCE_OOM.store(false, Ordering::Relaxed);
    assert!(!byte.is_null(), "the critical reserve served the refusal");
    assert_eq!(current(GcBlockRole::WorkspaceOverflow), before + 1);

    arena.reset();
    assert_eq!(current(GcBlockRole::WorkspaceOverflow), before);
}

#[test]
fn a_wrong_role_cannot_corrupt_either_counter() {
    let _g = test_guard();
    let floors = current(GcBlockRole::QueueFloor);
    let segments = current(GcBlockRole::QueueSegment);
    let block = acquire(GcBlockRole::QueueSegment);
    assert!(!block.is_null());

    let refused = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        release(block, GcBlockRole::QueueFloor);
    }));
    assert!(refused.is_err());
    assert_eq!(current(GcBlockRole::QueueFloor), floors);
    assert_eq!(current(GcBlockRole::QueueSegment), segments + 1);

    release(block, GcBlockRole::QueueSegment);
    assert_eq!(current(GcBlockRole::QueueSegment), segments);
}

#[test]
fn a_second_return_fails_before_the_counter_can_wrap() {
    let _g = test_guard();
    let before = current(GcBlockRole::WorkspaceOverflow);
    let block = acquire(GcBlockRole::WorkspaceOverflow);
    assert!(!block.is_null());
    release(block, GcBlockRole::WorkspaceOverflow);

    let refused = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        release(block, GcBlockRole::WorkspaceOverflow);
    }));
    assert!(refused.is_err());
    assert_eq!(current(GcBlockRole::WorkspaceOverflow), before);
}

#[test]
fn bytes_and_high_water_are_derived_from_the_role_count() {
    let _g = test_guard();
    let before = stats();
    assert_eq!(before.current_blocks(GcBlockRole::WorkspaceBase), 0);
    let block = acquire(GcBlockRole::WorkspaceBase);
    assert!(!block.is_null());

    let held = stats();
    assert_eq!(held.current_blocks(GcBlockRole::WorkspaceBase), 1);
    assert_eq!(held.current_bytes(GcBlockRole::WorkspaceBase), BLOCK_SIZE);
    assert!(held.peak_blocks(GcBlockRole::WorkspaceBase) >= 1);
    assert_eq!(
        held.peak_bytes(GcBlockRole::WorkspaceBase),
        held.peak_blocks(GcBlockRole::WorkspaceBase) * BLOCK_SIZE
    );

    release(block, GcBlockRole::WorkspaceBase);
    assert_eq!(stats().current_blocks(GcBlockRole::WorkspaceBase), 0);
}
