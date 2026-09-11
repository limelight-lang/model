//! A ring member a compiler-proven slot holds registers as no candidate, and
//! the ring is still collected whole from the members that did: the trace
//! reaches the marked member through its holder's edge, the sever cuts that
//! edge like any other, and the member is freed once, through the guard
//! release rather than through its holder's `dispose`.

use super::*;
use crate::memory::barrier::store_box_owned;
use crate::refcount::{entity_flags, is_owned};
use crate::value::{Tag, Value};

/// A ring of three, the first edge stored through the owned form: member 1
/// carries the mark, and spending its creation reference registers nothing.
unsafe fn ring_with_an_owned_edge(arena: &mut Arena, class: *const Class) -> [*mut Object; 3] {
    let mut context = LLContext { arena: &mut *arena };
    let members =
        [(); 3].map(|_| unsafe { new_constructed(&mut context, class, MemoryCategory::GcHeap) });

    unsafe {
        let owned_slot = Object::prop_at(members[0], prop_offset(0));
        assert!(store_box_owned(
            &mut *arena,
            MemoryCategory::GcHeap,
            owned_slot,
            Value::entity(Tag::Object, members[1] as *mut RcHeader),
        ));
        store_prop(arena, members[1], prop_offset(0), members[2]);
        store_prop(arena, members[2], prop_offset(0), members[0]);

        for &member in &members {
            assert!(
                !ll_release(member as *mut RcHeader),
                "an edge holds every member"
            );
        }
    }

    assert!(is_owned(unsafe { entity_flags(members[1]) }));
    members
}

#[test]
fn an_owned_member_registers_nothing_and_is_freed_once_with_its_ring() {
    let _g = test_guard();
    crate::cycle::queue::release_queue_segments();
    DESTRUCTOR_RUNS.store(0, Ordering::Relaxed);
    let mut arena = Arena::new();
    let class = node_class("OwnedEdgeRing", counting_destructor as *const ());

    let members = unsafe { ring_with_an_owned_edge(&mut arena, class) };
    assert_eq!(
        crate::cycle::queue::candidate_count(),
        2,
        "the marked member's non-final decrement passed the gate unregistered"
    );

    assert_eq!(
        unsafe { ll_gc_collect_cycles() },
        3,
        "the whole ring, the marked member in it"
    );
    assert_eq!(
        DESTRUCTOR_RUNS.load(Ordering::Relaxed),
        3,
        "each member once"
    );
    assert_eq!(crate::cycle::queue::candidate_count(), 0);
    for member in members {
        assert_eq!(
            unsafe { slot_state(member as *const RcHeader) },
            SlotState::DeadInPlace,
            "freed once, through the guard release: a second teardown would abort here"
        );
    }
}
