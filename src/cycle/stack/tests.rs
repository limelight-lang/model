//! The worklist, through the arena that owns it. Neither half of an entry
//! is dereferenced here, so the tests queue headers and rows off two slabs
//! rather than a real graph: what is under test is the segment chain, and a
//! graph deep enough to cross it would be five hundred objects built to prove
//! a link.
//!
//! Both slabs are real memory and not integers cast to pointers, which
//! matters to the instrument rather than to the code: one such cast puts
//! Miri into permissive provenance for the whole run, and the crate
//! keeps them to the one place that owns block addresses
//! (`cycle::row::resolve_edge_target`).

use super::*;
use crate::memory::block_pool::{BlockPool, force_oom, test_guard};
use crate::refcount::{EntityKind, MemoryCategory};

/// Headers the tests queue, `count` of them, distinct and each carrying
/// a live pointer's provenance.
fn slab(count: usize) -> Vec<RcHeader> {
    (0..count)
        .map(|_| RcHeader::new(MemoryCategory::GcHeap, EntityKind::Object.to_flags()))
        .collect()
}

/// Shadow rows to pair the headers with, `count` of them.
fn row_slab(count: usize) -> Vec<u32> {
    vec![0; count]
}

/// The `index`-th entry over two slabs whose starts are `base` and `rows`.
/// One-based, so that neither half is the null a segment's untouched
/// memory could hold by accident.
///
/// # Safety
/// The slabs hold at least `index` elements each, and both starts are
/// taken once and not re-derived: a fresh `&mut` to either vector would
/// retag it and leave every pointer built here reading through a dead tag.
unsafe fn entry(base: *mut RcHeader, rows: *mut u32, index: usize) -> WorklistEntry {
    WorklistEntry {
        entity: unsafe { base.add(index - 1) },
        row: unsafe { rows.add(index - 1) },
    }
}

/// The region's own capacity, exactly. A trace this deep is served out of
/// memory the thread already holds, so the collection asks the memory manager
/// for nothing, which is the point of the fixed region.
#[test]
fn a_depth_at_the_regions_capacity_draws_nothing() {
    let _g = test_guard();
    let depth = WORKLIST_BASE_ENTRIES;
    let mut headers = slab(depth);
    let base = headers.as_mut_ptr();
    let mut shadow_rows = row_slab(depth);
    let rows = shadow_rows.as_mut_ptr();

    let mut arena = crate::cycle::testing::open_arena();
    let room_before = arena.room_left();
    for i in 1..=depth {
        assert!(
            arena.push_work(unsafe { entry(base, rows, i) }),
            "the region served"
        );
    }

    assert_eq!(
        arena.worklist_segment_count(),
        1,
        "the workspace's region alone"
    );
    assert_eq!(arena.blocks_held(), 0, "and no block was drawn");
    assert_eq!(arena.room_left(), room_before, "nor was the bump touched");

    for i in (1..=depth).rev() {
        assert_eq!(arena.pop_work(), Some(unsafe { entry(base, rows, i) }));
    }

    assert_eq!(arena.pop_work(), None, "and the worklist is exhausted");
    arena.reset();
}

/// One entry past the region, which is where the chain starts existing at
/// all. Last-in-first-out across the boundary is what the descent depends on:
/// an entity popped out of order is expanded before the entity that queued it,
/// which is a different traversal rather than a wrong one — and it reads
/// correctly on every graph but the one whose depth crosses the boundary.
#[test]
fn a_depth_one_past_the_region_pops_in_the_order_it_pushed() {
    let _g = test_guard();
    let depth = WORKLIST_BASE_ENTRIES + 1;
    let mut headers = slab(depth);
    let base = headers.as_mut_ptr();
    let mut shadow_rows = row_slab(depth);
    let rows = shadow_rows.as_mut_ptr();

    let mut arena = crate::cycle::testing::open_arena();
    let room_before = arena.room_left();
    for i in 1..=depth {
        assert!(
            arena.push_work(unsafe { entry(base, rows, i) }),
            "the bump served"
        );
    }

    assert_eq!(
        arena.worklist_segment_count(),
        2,
        "the depth crossed one boundary"
    );
    assert_eq!(
        room_before - arena.room_left(),
        SEGMENT_BYTES,
        "and the segment came out of the bump rather than out of a block of its own"
    );
    assert_eq!(arena.blocks_held(), 0);

    for i in (1..=depth).rev() {
        assert_eq!(arena.pop_work(), Some(unsafe { entry(base, rows, i) }));
    }

    assert_eq!(arena.pop_work(), None, "and the worklist is exhausted");
    arena.reset();
}

/// An emptied segment is kept. The arena is a bump with no free, so a
/// trace whose depth oscillates across a boundary would otherwise take a
/// page of arena per crossing — and nothing else reports it: the entries
/// come back correctly either way.
#[test]
fn a_segment_the_depth_left_is_reused_at_the_next_crossing() {
    let _g = test_guard();
    let depth = WORKLIST_BASE_ENTRIES + 1;
    let mut headers = slab(depth);
    let base = headers.as_mut_ptr();
    let mut shadow_rows = row_slab(depth);
    let rows = shadow_rows.as_mut_ptr();

    let mut arena = crate::cycle::testing::open_arena();
    for crossing in 0..4 {
        for i in 1..=depth {
            assert!(arena.push_work(unsafe { entry(base, rows, i) }));
        }

        assert_eq!(
            arena.worklist_segment_count(),
            2,
            "crossing {crossing} drew no segment of its own"
        );

        for i in (1..=depth).rev() {
            assert_eq!(arena.pop_work(), Some(unsafe { entry(base, rows, i) }));
        }
    }

    arena.reset();
}

/// A refused segment is a refused collection, and the refusal arrives as
/// a false rather than as a panic or a null the caller has to test.
#[test]
fn a_push_with_both_allocation_paths_refusing_answers_false() {
    let _g = test_guard();
    crate::memory::critical::drain_for_test();
    let depth = WORKLIST_BASE_ENTRIES + 1;
    let mut headers = slab(depth);
    let base = headers.as_mut_ptr();
    let mut shadow_rows = row_slab(depth);
    let rows = shadow_rows.as_mut_ptr();

    let mut arena = crate::cycle::testing::open_arena();
    // Past the region and past the bump: a segment served out of memory the
    // thread already holds meets no allocation path, and this case is about
    // the refusal.
    let room = arena.room_left();
    assert!(!arena.alloc(room).is_null());
    for i in 1..depth {
        assert!(arena.push_work(unsafe { entry(base, rows, i) }));
    }

    let oom = force_oom();
    assert!(
        BlockPool::global().get().is_null(),
        "the ordinary allocation path is refusing"
    );
    assert_eq!(crate::memory::critical::blocks_held(), 0);

    let refused = arena.push_work(unsafe { entry(base, rows, depth) });
    drop(oom);

    assert!(!refused);
    assert_eq!(
        arena.worklist_segment_count(),
        1,
        "and no segment was attached"
    );
    for i in (1..depth).rev() {
        assert_eq!(arena.pop_work(), Some(unsafe { entry(base, rows, i) }));
    }

    assert_eq!(arena.pop_work(), None, "nor was the refused entry queued");

    arena.reset();
    crate::memory::critical::drain_for_test();
}

/// The arena's reset empties the worklist and forgets every segment past the
/// workspace's own region, which is the contract that keeps a later push from
/// advancing into a block the pool has handed to someone else. The retry after
/// an aborted collection is the caller this exists for.
#[test]
fn the_arenas_reset_leaves_the_worklist_on_its_region_alone() {
    let _g = test_guard();
    let depth = WORKLIST_BASE_ENTRIES + 1;
    let mut headers = slab(depth);
    let base = headers.as_mut_ptr();
    let mut shadow_rows = row_slab(depth);
    let rows = shadow_rows.as_mut_ptr();

    let mut arena = crate::cycle::testing::open_arena();
    for i in 1..=depth {
        assert!(arena.push_work(unsafe { entry(base, rows, i) }));
    }

    assert_eq!(arena.worklist_segment_count(), 2);

    arena.reset();
    assert_eq!(
        arena.worklist_segment_count(),
        1,
        "no segment of the collection that ended"
    );
    assert_eq!(arena.pop_work(), None, "and nothing queued in one");

    assert!(arena.push_work(unsafe { entry(base, rows, 1) }));
    assert_eq!(arena.pop_work(), Some(unsafe { entry(base, rows, 1) }));
    arena.reset();
}
