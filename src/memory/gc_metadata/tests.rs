use super::*;

use crate::memory::block_pool::{
    BLOCK_KIND_GC_METADATA, BLOCK_PAYLOAD, BlockPool, force_oom, load_block_kind, test_guard,
};

fn current() -> usize {
    stats().current_blocks()
}

#[test]
fn a_shadow_arena_is_gc_owned_until_both_exit_paths_return_it() {
    let _g = test_guard();
    let before = current();
    let mut arena = crate::cycle::testing::open_arena();

    // The workspace is this thread's already, so the block this case follows
    // is the one the bump grows into past it.
    let room = arena.room_left();
    assert!(!arena.alloc(room).is_null());
    assert_eq!(
        current(),
        before,
        "the arena's first grant drew a block the guard had not already drawn"
    );

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

    // The reset rewound the bump into the workspace, so a second block takes
    // a second growth — and the drop is the other path that returns it.
    let room = arena.room_left();
    assert!(!arena.alloc(room).is_null());
    assert!(!arena.alloc(1).is_null());
    assert_eq!(current(), before + 1);
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
        // One base block and the two spare segments the init fills.
        assert!(current() >= 3);
    })
    .join()
    .unwrap();

    assert_eq!(current(), before);
    let kept = usize::from(cfg!(feature = "debug-journal"));
    assert!(BlockPool::global().blocks_out() <= blocks_before + kept);
}

#[test]
fn a_critical_reserve_block_is_charged_only_while_the_arena_holds_it() {
    let _g = test_guard();
    assert!(crate::memory::critical::replenish());
    let before = current();
    let mut arena = crate::cycle::testing::open_arena();

    // Past the workspace, so the grant below has to ask an allocation path.
    let room = arena.room_left();
    assert!(!arena.alloc(room).is_null());

    let oom = force_oom();
    let byte = arena.alloc(1);
    drop(oom);
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

/// Bytes in use inside the blocks collection owns: the half of the answer
/// that says how much of a reserved block is working memory.
fn in_use() -> usize {
    stats().current_bytes_in_use()
}

#[test]
fn a_block_held_and_empty_is_reservation_and_no_bytes_in_use() {
    let _g = test_guard();
    let before = stats();
    let block = acquire();
    assert!(!block.is_null());

    assert_eq!(stats().current_blocks(), before.current_blocks() + 1);
    assert_eq!(
        in_use(),
        before.current_bytes_in_use(),
        "reservation is the physical axis and moves no logical byte"
    );

    release(block);
    assert_eq!(stats().current_blocks(), before.current_blocks());
    assert_eq!(in_use(), before.current_bytes_in_use());
}

#[test]
fn a_reset_enters_the_bump_it_rewinds_in_the_high_water_figure() {
    let _g = test_guard();
    // The high-water figure is process-global and never falls, so an
    // exact rise is only assertable from a known baseline.
    lower_peak_to_current();
    let before = stats();
    let mut arena = crate::cycle::testing::open_arena();

    assert!(!arena.alloc(1).is_null());
    assert_eq!(
        in_use(),
        before.current_bytes_in_use(),
        "the block still under the bump is reserved rather than published"
    );

    arena.reset();
    assert_eq!(
        in_use(),
        before.current_bytes_in_use(),
        "the rewind releases the bump rather than charging it"
    );
    assert_eq!(
        stats().peak_bytes_in_use(),
        before.current_bytes_in_use() + 8,
        "one byte granted is eight bytes of bump, and the high-water keeps them"
    );
}

#[test]
fn a_block_crossing_publishes_the_bump_it_abandons() {
    let _g = test_guard();
    lower_peak_to_current();
    let before = stats();
    let mut arena = crate::cycle::testing::open_arena();

    assert!(!arena.alloc(BLOCK_PAYLOAD).is_null());
    assert_eq!(in_use(), before.current_bytes_in_use());

    // The second grant cannot fit, so the workspace leaves the bump —
    // consumed to the byte, which is the instant its figure is exact. Held
    // rather than returned, and charged all the same: the bytes stay in use
    // until the reset rewinds over them.
    assert!(!arena.alloc(8).is_null());
    assert_eq!(
        in_use(),
        before.current_bytes_in_use() + BLOCK_PAYLOAD,
        "the block the bump left is published whole"
    );

    arena.reset();
    assert_eq!(in_use(), before.current_bytes_in_use());
    assert_eq!(
        stats().peak_bytes_in_use(),
        before.current_bytes_in_use() + BLOCK_PAYLOAD + 8,
        "the crossing and the reset are both in the high-water figure"
    );
}

#[test]
fn a_second_reset_publishes_nothing_and_the_figure_cannot_underflow() {
    let _g = test_guard();
    lower_peak_to_current();
    let before = stats();
    let mut arena = crate::cycle::testing::open_arena();
    assert!(!arena.alloc(64).is_null());

    arena.reset();
    let after = stats();
    arena.reset();

    assert_eq!(after.current_bytes_in_use(), before.current_bytes_in_use());
    assert_eq!(
        after.peak_bytes_in_use(),
        before.current_bytes_in_use() + 64
    );
    assert_eq!(stats().current_bytes_in_use(), after.current_bytes_in_use());
    assert_eq!(
        stats().peak_bytes_in_use(),
        after.peak_bytes_in_use(),
        "the second reset finds a rewound bump and enters nothing over a settled ledger"
    );
}
