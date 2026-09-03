//! What the trace leaves in the rows, which is the whole of its output:
//! the scan reads these counts and nothing else. Each test here asserts
//! a count per entity rather than a shape of the graph, because a mark
//! that subtracted twice, or subtracted the root's own queue entry,
//! terminates just as cleanly and reads a component the counts hold as
//! unreachable.

use super::*;

/// The arithmetic, on a graph whose three entities end at three
/// different counts. Two of the edges point at the same entity, so a
/// trace that met a child once and subtracted once — the natural shape
/// of a mark keyed on the meeting rather than on the edge — reads one
/// too many for it.
#[test]
fn every_internal_edge_comes_off_the_row_it_points_at() {
    let _g = test_guard();
    let forked = ClassBuilder::new("MarkForked")
        .prop("left", true)
        .prop("right", true)
        .build();
    let single = ClassBuilder::new("MarkSingle").prop("next", true).build();

    let mut arena = Arena::new();
    let mut context = LLContext { arena: &mut arena };
    let root = unsafe { new_constructed(&mut context, forked, MemoryCategory::GcHeap) };
    let middle = unsafe { new_constructed(&mut context, single, MemoryCategory::GcHeap) };
    let shared = unsafe { new_constructed(&mut context, single, MemoryCategory::GcHeap) };

    unsafe {
        store_prop(&mut arena, root, prop_offset(0), middle);
        store_prop(&mut arena, root, prop_offset(1), shared);
        store_prop(&mut arena, middle, prop_offset(0), shared);
    }

    // The middle's own reference goes, so its row starts at the one edge
    // the trace is about to find and lands on zero. The shared entity
    // keeps the fixture's, which is the external reference that has to
    // survive two subtractions.
    assert!(!unsafe { ll_release(middle as *mut RcHeader) });

    let mut shadow_arena = crate::cycle::testing::open_arena();
    assert_eq!(
        unsafe { mark(&mut shadow_arena, root as *mut RcHeader) },
        MarkResult::Complete
    );

    assert_eq!(
        unsafe { working_count(root) },
        1,
        "the root keeps its own count: the queue entry that named it is not an edge"
    );
    assert_eq!(
        unsafe { working_count(middle) },
        0,
        "one in-edge, and nothing outside the component holds it"
    );
    assert_eq!(
        unsafe { working_count(shared) },
        1,
        "two in-edges off a count of three leaves the fixture's own reference"
    );

    shadow_arena.reset();
    unsafe {
        ll_retain(middle as *mut RcHeader);
        store_prop(&mut arena, root, prop_offset(0), std::ptr::null_mut());
        store_prop(&mut arena, root, prop_offset(1), std::ptr::null_mut());
        store_prop(&mut arena, middle, prop_offset(0), std::ptr::null_mut());
        for entity in [root, middle, shared] {
            assert!(ll_release(entity as *mut RcHeader));
            ll_object_die(entity);
        }
    }
}

/// A ring is what the descent has to survive, and the meeting is what
/// makes it survivable: an edge into an entity already expanded takes
/// the decrement and stops. The assertion that the trace terminated is
/// the test running at all; what the counts add is that termination did
/// not cost an edge.
#[test]
fn a_ring_no_one_holds_reads_internally_balanced() {
    let _g = test_guard();
    let node = ClassBuilder::new("MarkRingNode").prop("next", true).build();

    let mut arena = Arena::new();
    let mut context = LLContext { arena: &mut arena };
    let first = unsafe { new_constructed(&mut context, node, MemoryCategory::GcHeap) };
    let second = unsafe { new_constructed(&mut context, node, MemoryCategory::GcHeap) };

    unsafe {
        store_prop(&mut arena, first, prop_offset(0), second);
        store_prop(&mut arena, second, prop_offset(0), first);
        // The ring is garbage from here: each member is held by the other
        // and by nothing else, which is the case no counting can reclaim.
        assert!(!ll_release(first as *mut RcHeader));
        assert!(!ll_release(second as *mut RcHeader));
    }

    let mut shadow_arena = crate::cycle::testing::open_arena();
    assert_eq!(
        unsafe { mark(&mut shadow_arena, first as *mut RcHeader) },
        MarkResult::Complete
    );

    for member in [first, second] {
        assert_eq!(
            unsafe { working_count(member) },
            0,
            "a ring's every in-edge is internal, so nothing is left"
        );
    }

    shadow_arena.reset();
    unsafe {
        ll_retain(first as *mut RcHeader);
        ll_retain(second as *mut RcHeader);
        store_prop(&mut arena, first, prop_offset(0), std::ptr::null_mut());
        store_prop(&mut arena, second, prop_offset(0), std::ptr::null_mut());
        for member in [first, second] {
            assert!(ll_release(member as *mut RcHeader));
            ll_object_die(member);
        }
    }
}

/// The same ring with one reference into it from outside, which is the case the
/// collector must not read as unreachable. It is here rather than in the scan's
/// tests because the difference between the two rings is made entirely in the
/// mark: the scan reads a count it did not compute.
#[test]
fn a_ring_held_from_outside_keeps_the_holder_s_count() {
    let _g = test_guard();
    let node = ClassBuilder::new("MarkHeldNode").prop("next", true).build();

    let mut arena = Arena::new();
    let mut context = LLContext { arena: &mut arena };
    let first = unsafe { new_constructed(&mut context, node, MemoryCategory::GcHeap) };
    let second = unsafe { new_constructed(&mut context, node, MemoryCategory::GcHeap) };

    unsafe {
        store_prop(&mut arena, first, prop_offset(0), second);
        store_prop(&mut arena, second, prop_offset(0), first);
        // Only the first member's outside reference goes: the fixture
        // itself is what still holds the second.
        assert!(!ll_release(first as *mut RcHeader));
    }

    let mut shadow_arena = crate::cycle::testing::open_arena();
    assert_eq!(
        unsafe { mark(&mut shadow_arena, first as *mut RcHeader) },
        MarkResult::Complete
    );

    assert_eq!(unsafe { working_count(first) }, 0);
    assert_eq!(
        unsafe { working_count(second) },
        1,
        "the reference held outside the ring is what the row still reads"
    );

    shadow_arena.reset();
    unsafe {
        ll_retain(first as *mut RcHeader);
        store_prop(&mut arena, first, prop_offset(0), std::ptr::null_mut());
        store_prop(&mut arena, second, prop_offset(0), std::ptr::null_mut());
        for member in [first, second] {
            assert!(ll_release(member as *mut RcHeader));
            ll_object_die(member);
        }
    }
}

/// A second root inside the first one's closure. The arena and the
/// worklist belong to the collection, so the second mark meets rows that
/// already say met, expands nothing again, and — this is the clause that
/// matters — does not re-initialise a row from a refcount it has already
/// subtracted from.
#[test]
fn a_second_root_already_met_leaves_every_count_where_it_was() {
    let _g = test_guard();
    let node = ClassBuilder::new("MarkTwiceNode")
        .prop("next", true)
        .build();

    let mut arena = Arena::new();
    let mut context = LLContext { arena: &mut arena };
    let root = unsafe { new_constructed(&mut context, node, MemoryCategory::GcHeap) };
    let child = unsafe { new_constructed(&mut context, node, MemoryCategory::GcHeap) };

    unsafe {
        store_prop(&mut arena, root, prop_offset(0), child);
        assert!(!ll_release(child as *mut RcHeader));
    }

    let mut shadow_arena = crate::cycle::testing::open_arena();
    assert_eq!(
        unsafe { mark(&mut shadow_arena, root as *mut RcHeader) },
        MarkResult::Complete
    );
    let after_first = (unsafe { working_count(root) }, unsafe {
        working_count(child)
    });
    assert_eq!(after_first, (1, 0));

    assert_eq!(
        unsafe { mark(&mut shadow_arena, child as *mut RcHeader) },
        MarkResult::Complete
    );
    assert_eq!(
        (unsafe { working_count(root) }, unsafe {
            working_count(child)
        }),
        after_first,
        "the second root was met already, so it neither restored a count nor spent one"
    );

    shadow_arena.reset();
    unsafe {
        ll_retain(child as *mut RcHeader);
        store_prop(&mut arena, root, prop_offset(0), std::ptr::null_mut());
        for entity in [root, child] {
            assert!(ll_release(entity as *mut RcHeader));
            ll_object_die(entity);
        }
    }
}
