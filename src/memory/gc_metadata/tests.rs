use super::*;

use crate::memory::block_pool::{
    BLOCK_KIND_GC_METADATA, BlockPool, FORCE_OOM, load_block_kind, test_guard,
};

fn current() -> usize {
    stats().current_blocks()
}

#[test]
fn a_shadow_arena_is_gc_owned_until_both_exit_paths_return_it() {
    let _g = test_guard();
    let before = current();
    let mut arena = crate::cycle::arena::ShadowArena::new();

    let byte = arena.alloc(1);
    assert!(!byte.is_null());
    assert_eq!(current(), before + 1);
    let block = BlockHeader::of_ptr(byte);
    assert_eq!(
        unsafe { load_block_kind(&raw const (*block).kind) },
        BLOCK_KIND_GC_METADATA
    );

    arena.reset();
    assert_eq!(current(), before);

    assert!(!arena.alloc(1).is_null());
    drop(arena);
    assert_eq!(current(), before);
}

#[test]
fn a_threads_exit_ends_every_block_it_acquired() {
    let _g = test_guard();
    let before = current();
    let blocks_before = BlockPool::global().blocks_out();

    std::thread::spawn(|| {
        assert!(crate::memory::heap::ll_thread_init());
        // One floor and the two spare segments the init fills.
        assert!(current() >= 3);
    })
    .join()
    .unwrap();

    assert_eq!(current(), before);
    let kept = usize::from(cfg!(feature = "debug-journal"));
    assert!(BlockPool::global().blocks_out() <= blocks_before + kept);
}

#[test]
fn a_critical_workspace_draw_is_charged_only_while_the_arena_holds_it() {
    let _g = test_guard();
    assert!(crate::memory::critical::replenish());
    let before = current();
    let mut arena = crate::cycle::arena::ShadowArena::new();

    FORCE_OOM.store(true, Ordering::Relaxed);
    let byte = arena.alloc(1);
    FORCE_OOM.store(false, Ordering::Relaxed);
    assert!(!byte.is_null(), "the critical reserve served the refusal");
    assert_eq!(current(), before + 1);

    arena.reset();
    assert_eq!(current(), before);
}

#[test]
fn a_block_the_collector_never_owned_is_refused_before_the_counter_moves() {
    let _g = test_guard();
    let before = current();
    let ordinary = BlockPool::global().get();
    assert!(!ordinary.is_null());

    let refused = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        release(ordinary);
    }));
    assert!(refused.is_err());
    assert_eq!(current(), before);

    BlockPool::global().put(ordinary);
}

#[test]
fn a_second_return_fails_before_the_counter_can_wrap() {
    let _g = test_guard();
    let before = current();
    let block = acquire();
    assert!(!block.is_null());
    release(block);

    let refused = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        release(block);
    }));
    assert!(refused.is_err());
    assert_eq!(current(), before);
}

/// The three refusals that keep a block from crossing the boundary
/// unaccounted. Each is the only thing standing between a shortcut and a
/// counter that drifts without anyone noticing, so each is exercised rather
/// than trusted.
#[test]
fn the_pool_refuses_a_block_collection_still_owns() {
    let _g = test_guard();
    let before = current();
    let block = acquire();
    assert!(!block.is_null());

    let refused = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        BlockPool::global().put(block);
    }));
    assert!(refused.is_err(), "the pool took a GC-stamped block");
    assert_eq!(current(), before + 1, "and the block is still charged");

    release(block);
    assert_eq!(current(), before);
}

#[test]
fn the_critical_reserve_refuses_a_block_collection_still_owns() {
    let _g = test_guard();
    let before = current();
    // Below capacity, which is the arm that keeps the block rather than
    // passing it to the pool. At capacity the pool's own refusal would
    // answer and this reserve's would go untested.
    assert!(crate::memory::critical::replenish());
    let drawn = crate::memory::critical::draw();
    assert!(!drawn.is_null());

    let block = acquire();
    assert!(!block.is_null());

    let refused = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        crate::memory::critical::give_back(block);
    }));
    assert!(refused.is_err(), "the reserve took a GC-stamped block");
    assert_eq!(current(), before + 1, "and the block is still charged");

    release(block);
    assert_eq!(current(), before);
    crate::memory::critical::give_back(drawn);
}

#[test]
fn adoption_refuses_a_source_that_is_not_the_reserve() {
    let _g = test_guard();
    let before = current();
    // Straight from the pool, so it is `FREE` where `adopt` demands the
    // `ARENA` stamp every block in the critical reserve carries.
    let ordinary = BlockPool::global().get();
    assert!(!ordinary.is_null());

    let refused = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        adopt(ordinary);
    }));
    assert!(refused.is_err(), "adoption crossed the wrong boundary");
    assert_eq!(current(), before, "and charged nothing");

    BlockPool::global().put(ordinary);
}

#[test]
fn bytes_and_high_water_are_derived_from_the_block_count() {
    let _g = test_guard();
    let before = stats();
    let block = acquire();
    assert!(!block.is_null());

    let held = stats();
    assert_eq!(held.current_blocks(), before.current_blocks() + 1);
    assert_eq!(held.current_bytes(), held.current_blocks() * BLOCK_SIZE);
    assert!(held.peak_blocks() >= held.current_blocks());
    assert_eq!(held.peak_bytes(), held.peak_blocks() * BLOCK_SIZE);

    release(block);
    assert_eq!(stats().current_blocks(), before.current_blocks());
}
