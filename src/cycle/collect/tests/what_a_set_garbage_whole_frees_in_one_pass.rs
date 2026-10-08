//! A set the owner's own trace found garbage whole is freed in one pass when
//! no member's death and no child of it asks for the full chain
//! (`crate::cycle::reclamation::free_whole_before_drops`): its children
//! outside it dropped once, after the frees. A member with a destructor or a
//! weak reference sends the set down the full chain, which frees it as
//! before.

use super::*;
use crate::cycle::posted_set::testing::post_for_test;

/// The headers of `objects`, as the set lists them.
fn headers(objects: &[*mut Object]) -> Vec<*mut RcHeader> {
    objects
        .iter()
        .map(|&object| object as *mut RcHeader)
        .collect()
}

/// An object of `class` the case holds by its creation reference.
unsafe fn held(arena: &mut Arena, class: *const Class) -> *mut Object {
    let mut context = LLContext { arena };
    unsafe { new_constructed(&mut context, class, MemoryCategory::GcHeap) }
}

/// A ring of three of `class`, posted as the collector's set beside a verdict
/// proposing one of its roots.
unsafe fn posted_ring(arena: &mut Arena, class: *const Class) -> Vec<*mut Object> {
    let ring = unsafe { long_ring(arena, class, 3) };
    assert_eq!(stand_in_posts(1, Verdict::Proposed), Posted::Batch(1));
    post_for_test(&headers(&ring));
    ring
}

/// Reset the counters these cases read.
fn forget_the_counts() {
    let _ = crate::cycle::trace::take_sets_garbage_whole();
    let _ = crate::cycle::reclamation::take_freed_whole();
    let _ = crate::cycle::reclamation::take_queued_by_the_drain();
}

/// A ring of three with no destructor, two members of which hold an object
/// outside it, posted beside a verdict proposing one of its roots; and the
/// two outside objects, each held by the case's reference and the ring's.
unsafe fn posted_ring_holding_two(
    arena: &mut Arena,
    name: &str,
) -> (Vec<*mut Object>, [*mut Object; 2]) {
    let node = ClassBuilder::new(name)
        .prop("next", true)
        .prop("out", true)
        .build();
    let ring = unsafe { long_ring(arena, node, 3) };
    let outside = [
        unsafe { held(arena, keeper_class(&format!("{name}OutsideA"))) },
        unsafe { held(arena, keeper_class(&format!("{name}OutsideB"))) },
    ];
    for (member, child) in ring.iter().zip(outside) {
        unsafe { store_prop(arena, *member, prop_offset(1), child) };
    }
    assert_eq!(stand_in_posts(1, Verdict::Proposed), Posted::Batch(1));
    post_for_test(&headers(&ring));
    (ring, outside)
}

/// Each outside object stands at the case's reference alone, and goes.
unsafe fn the_drops_gave_back_the_rings_references(outside: [*mut Object; 2]) {
    for child in outside {
        assert_eq!(
            unsafe { crate::refcount::header_refcount(child as *const RcHeader) },
            1,
            "the drop gave back the ring's reference, once"
        );
        unsafe {
            assert!(ll_release(child as *mut RcHeader));
            ll_object_die(child);
        }
    }
}

/// A ring with no destructor, two members of which hold an object outside it:
/// the ring is freed in the one pass from the children the drain queued as it
/// met them, each outside object is dropped once and stands at the case's
/// reference, and no member stays live. Red with the one pass left out.
#[test]
fn a_ring_with_no_destructor_is_freed_in_one_pass_and_drops_its_outside_children() {
    let _g = test_guard();
    let mut arena = Arena::new();
    let (ring, outside) = unsafe { posted_ring_holding_two(&mut arena, "OnePassRingNode") };

    forget_the_counts();
    assert_eq!(unsafe { ll_gc_maybe_collect() }, 3, "the ring is freed");
    assert_eq!(crate::cycle::trace::take_sets_garbage_whole(), 1);
    assert_eq!(
        crate::cycle::reclamation::take_freed_whole(),
        1,
        "the set was freed in the one pass"
    );
    assert_eq!(
        crate::cycle::reclamation::take_queued_by_the_drain(),
        1,
        "from the children the drain queued"
    );
    for member in ring {
        assert_ne!(
            unsafe { slot_state(member as *const RcHeader) },
            SlotState::Live
        );
    }
    unsafe { the_drops_gave_back_the_rings_references(outside) };
}

/// The drain refused the queue's first segment: it stops queueing and goes on,
/// and the one pass reads the cells again for the same drops. Red on a drain
/// whose refusal ends the trace, or a one pass that frees from a queue the
/// drain left short.
#[test]
fn a_queue_the_drain_was_refused_is_read_again_from_the_cells() {
    let _g = test_guard();
    let mut arena = Arena::new();
    let (ring, outside) = unsafe { posted_ring_holding_two(&mut arena, "OnePassRefusedNode") };

    forget_the_counts();
    {
        let _refused = crate::cycle::arena::refuse_left_out_segment();
        assert_eq!(unsafe { ll_gc_maybe_collect() }, 3, "the ring is freed");
    }
    assert_eq!(crate::cycle::reclamation::take_freed_whole(), 1);
    assert_eq!(
        crate::cycle::reclamation::take_queued_by_the_drain(),
        0,
        "the cells were read again"
    );
    for member in ring {
        assert_ne!(
            unsafe { slot_state(member as *const RcHeader) },
            SlotState::Live
        );
    }
    unsafe { the_drops_gave_back_the_rings_references(outside) };
}

/// The arena refused the drain its queue's first segment and the one pass the
/// room to read the cells again: the set takes the full chain, which frees it
/// and drops the same children. Red on a refusal that leaks the ring or drops a
/// child twice.
#[test]
fn a_one_pass_refused_its_room_sends_the_set_down_the_full_chain() {
    let _g = test_guard();
    let mut arena = Arena::new();
    let (ring, outside) = unsafe { posted_ring_holding_two(&mut arena, "OnePassNoRoomNode") };

    forget_the_counts();
    {
        let _no_queue = crate::cycle::arena::refuse_left_out_segment();
        let _no_room = crate::cycle::arena::refuse_drop_reservation();
        assert_eq!(unsafe { ll_gc_maybe_collect() }, 3, "the ring is freed");
    }
    assert_eq!(crate::cycle::trace::take_sets_garbage_whole(), 1);
    assert_eq!(crate::cycle::reclamation::take_freed_whole(), 0);
    for member in ring {
        assert_ne!(
            unsafe { slot_state(member as *const RcHeader) },
            SlotState::Live
        );
    }
    unsafe { the_drops_gave_back_the_rings_references(outside) };
}

/// A ring whose class has a destructor takes the full chain: the destructor is
/// user code the one pass may not skip, and it runs once a member. Red on a
/// one pass that ignores it.
#[test]
fn a_member_with_a_destructor_sends_the_set_down_the_full_chain() {
    let _g = test_guard();
    DESTRUCTOR_RUNS.store(0, Ordering::Relaxed);
    let node = node_class("OnePassDestructorNode", counting_destructor as *const ());
    let mut arena = Arena::new();
    let ring = unsafe { posted_ring(&mut arena, node) };

    forget_the_counts();
    assert_eq!(unsafe { ll_gc_maybe_collect() }, 3, "the ring is freed");
    assert_eq!(crate::cycle::trace::take_sets_garbage_whole(), 1);
    assert_eq!(
        DESTRUCTOR_RUNS.load(Ordering::Relaxed),
        3,
        "each member's destructor ran"
    );
    assert_eq!(crate::cycle::reclamation::take_freed_whole(), 0);
    for member in ring {
        assert_ne!(
            unsafe { slot_state(member as *const RcHeader) },
            SlotState::Live
        );
    }
}

/// A ring one member of which is weakly referenced takes the full chain, which
/// clears the weak cell: the cell reads null afterwards. Red on a one pass that
/// frees the member and leaves the cell naming its slot.
#[test]
fn a_weakly_referenced_member_sends_the_set_down_the_full_chain() {
    let _g = test_guard();
    let node = ClassBuilder::new("OnePassWeakNode")
        .prop("next", true)
        .build();
    let mut arena = Arena::new();
    let ring = unsafe { long_ring(&mut arena, node, 3) };
    let weak = {
        let mut context = LLContext { arena: &mut arena };
        unsafe { crate::weak::ll_weakref_create(&mut context, ring[1] as *mut RcHeader) }
    };
    assert_eq!(stand_in_posts(1, Verdict::Proposed), Posted::Batch(1));
    post_for_test(&headers(&ring));

    forget_the_counts();
    assert_eq!(unsafe { ll_gc_maybe_collect() }, 3, "the ring is freed");
    assert_eq!(crate::cycle::reclamation::take_freed_whole(), 0);
    assert!(
        unsafe { crate::weak::ll_weakref_get(weak) }.is_null(),
        "the full chain cleared the weak cell"
    );
    unsafe {
        assert!(ll_release(weak as *mut RcHeader));
        crate::object::ll_entity_die(weak as *mut RcHeader);
    }
}

/// The state a one pass leaves — members dead in place with their cells
/// standing, slots withheld until the close — is one the next collection and
/// the allocator take as they take any death: after the retirement, a second
/// collection finds nothing, no free was refused, and every member's slot is
/// handed out again. Red on a one pass whose members a later trace expands or
/// whose slots never go back.
#[test]
fn a_set_freed_in_one_pass_leaves_the_heap_as_a_later_collection_expects() {
    let _g = test_guard();
    let node = ClassBuilder::new("OnePassAfterNode")
        .prop("next", true)
        .build();
    let mut arena = Arena::new();
    let ring = unsafe { posted_ring(&mut arena, node) };
    let _ = crate::memory::stdapi::take_refused_frees();

    forget_the_counts();
    assert_eq!(unsafe { ll_gc_maybe_collect() }, 3, "the ring is freed");
    assert_eq!(crate::cycle::reclamation::take_freed_whole(), 1);
    assert_eq!(
        unsafe { crate::gc::ll_gc_collect_cycles() },
        0,
        "nothing of the ring is proposed again"
    );
    assert_eq!(crate::memory::stdapi::take_refused_frees(), 0);
    for &member in &ring {
        assert_ne!(
            unsafe { slot_state(member as *const RcHeader) },
            SlotState::Live
        );
    }

    let freed: std::collections::HashSet<usize> =
        ring.iter().map(|&member| member as usize).collect();
    let mut context = LLContext { arena: &mut arena };
    let reissued: Vec<*mut Object> = (0..64)
        .map(|_| unsafe { new_constructed(&mut context, node, MemoryCategory::GcHeap) })
        .collect();
    assert_eq!(
        reissued
            .iter()
            .filter(|&&object| freed.contains(&(object as usize)))
            .count(),
        3,
        "every member's slot is handed out again"
    );
    for object in reissued {
        unsafe {
            assert!(ll_release(object as *mut RcHeader));
            ll_object_die(object);
        }
    }
}
