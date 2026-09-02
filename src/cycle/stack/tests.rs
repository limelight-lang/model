//! The worklist, on its own. Its entries are never dereferenced, so the
//! tests here queue headers off a slab rather than a real graph: what is
//! under test is the segment chain, and a graph deep enough to cross it
//! would be five hundred objects built to prove a link.
//!
//! The slab is real memory and not an integer cast to a pointer, which
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

/// The `index`-th header of a slab whose start is `base`. One-based, so
/// that no entry is the null a segment's untouched memory could hold by
/// accident.
///
/// # Safety
/// `base` is a slab of at least `index` headers, taken once and not
/// re-derived: a fresh `&mut` to the vector would retag it and leave
/// every pointer built here reading through a dead tag.
unsafe fn entry(base: *mut RcHeader, index: usize) -> *mut RcHeader {
    unsafe { base.add(index - 1) }
}

/// Order across a segment boundary. A depth past one segment is where
/// the chain starts existing at all, and last-in-first-out across it is
/// what the descent depends on: an entity popped out of order is
/// expanded before the entity that queued it, which is a different
/// traversal rather than a wrong one — and it reads correctly on every
/// graph but the one whose depth crosses the boundary.
#[test]
fn a_depth_past_one_segment_pops_in_the_order_it_pushed() {
    let _g = test_guard();
    let depth = SEGMENT_ENTRIES + SEGMENT_ENTRIES / 2;
    let mut headers = slab(depth);
    let base = headers.as_mut_ptr();

    let mut arena = TraceScratchArena::open().expect("the guard drew this thread's workspace");
    let mut stack = TraceStack::new();
    for i in 1..=depth {
        assert!(
            stack.push(&mut arena, unsafe { entry(base, i) }),
            "the pool served"
        );
    }

    assert_eq!(stack.segment_count(), 2, "the depth crossed one boundary");

    for i in (1..=depth).rev() {
        assert_eq!(stack.pop(), Some(unsafe { entry(base, i) }));
    }

    assert_eq!(stack.pop(), None, "and the worklist is exhausted");
    arena.reset();
}

/// An emptied segment is kept. The arena is a bump with no free, so a
/// trace whose depth oscillates across a boundary would otherwise take a
/// page of arena per crossing — and nothing else reports it: the entries
/// come back correctly either way.
#[test]
fn a_segment_the_depth_left_is_reused_at_the_next_crossing() {
    let _g = test_guard();
    let depth = SEGMENT_ENTRIES + 1;
    let mut headers = slab(depth);
    let base = headers.as_mut_ptr();

    let mut arena = TraceScratchArena::open().expect("the guard drew this thread's workspace");
    let mut stack = TraceStack::new();
    for crossing in 0..4 {
        for i in 1..=depth {
            assert!(stack.push(&mut arena, unsafe { entry(base, i) }));
        }

        assert_eq!(
            stack.segment_count(),
            2,
            "crossing {crossing} drew no segment of its own"
        );

        for i in (1..=depth).rev() {
            assert_eq!(stack.pop(), Some(unsafe { entry(base, i) }));
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
    let mut headers = slab(1);
    let base = headers.as_mut_ptr();

    let mut arena = TraceScratchArena::open().expect("the guard drew this thread's workspace");
    // Past the workspace: a segment served out of memory the thread already
    // holds meets no allocation path, and this case is about the refusal.
    assert!(
        !arena
            .alloc(crate::memory::block_pool::BLOCK_PAYLOAD)
            .is_null()
    );
    let mut stack = TraceStack::new();

    let oom = force_oom();
    assert!(
        BlockPool::global().get().is_null(),
        "the ordinary allocation path is refusing"
    );
    assert_eq!(crate::memory::critical::blocks_held(), 0);

    let refused = stack.push(&mut arena, unsafe { entry(base, 1) });
    drop(oom);

    assert!(!refused);
    assert_eq!(stack.pop(), None, "and nothing was queued");
    assert_eq!(stack.segment_count(), 0);

    arena.reset();
    crate::memory::critical::drain_for_test();
}

/// A stack that outlives its arena's reset forgets its segments, which
/// is the contract that keeps it from advancing into a block the pool has
/// handed to someone else. The retry after an aborted collection is the
/// caller this exists for.
#[test]
fn a_stack_reset_with_its_arena_holds_no_segment() {
    let _g = test_guard();
    let mut headers = slab(1);
    let base = headers.as_mut_ptr();

    let mut arena = TraceScratchArena::open().expect("the guard drew this thread's workspace");
    let mut stack = TraceStack::new();
    assert!(stack.push(&mut arena, unsafe { entry(base, 1) }));
    assert_eq!(stack.segment_count(), 1);

    arena.reset();
    stack.reset();
    assert_eq!(stack.segment_count(), 0, "no segment of the old arena");
    assert_eq!(stack.pop(), None, "and nothing queued in one");

    assert!(stack.push(&mut arena, unsafe { entry(base, 1) }));
    assert_eq!(stack.pop(), Some(unsafe { entry(base, 1) }));
    arena.reset();
}
