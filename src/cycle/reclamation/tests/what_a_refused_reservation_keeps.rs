//! A teardown that cannot get room for the children its sever would displace,
//! and what it leaves the component as.
//!
//! The room is taken before the first cell is emptied, so the refusal is the
//! one point in the teardown where nothing has been written yet: the component
//! keeps its edges, its counts and its candidate bits, and a later trace
//! proposes it again. What it does not keep is the destructors and the weak
//! cells the steps before this one already spent — the same state a component
//! read as externally referenced is left in.

use super::*;
use crate::memory::block_pool::{BlockPool, force_oom};
use crate::refcount::header_refcount;

#[test]
fn a_component_whose_children_have_no_room_is_left_whole() {
    let _g = test_guard();
    let node = node_class("ReclamationRefusedNode");
    let plain = ClassBuilder::new("ReclamationRefusedChild").build();

    let mut arena = Arena::new();
    let first = unsafe { object(&mut arena, node) };
    let second = unsafe { object(&mut arena, node) };
    let child = unsafe { object(&mut arena, plain) };
    unsafe {
        store_prop(&mut arena, first, prop_offset(0), second);
        store_prop(&mut arena, second, prop_offset(0), first);
        store_prop(&mut arena, first, prop_offset(1), child);
        spend_creation_references(&[first, second, child]);
        read_as_unreachable(first, &[first, second]);
    }

    // The bump has nothing left to grant, so the reservation is what asks the
    // memory manager, and both of its allocation paths are refusing.
    let mut scratch = open_arena();
    let fill = scratch.room_left();
    assert!(!scratch.alloc(fill).is_null());
    crate::memory::critical::drain_for_test();
    let oom = force_oom();
    assert!(
        BlockPool::global().get().is_null(),
        "the ordinary allocation path is refusing"
    );
    assert_eq!(
        crate::memory::critical::blocks_held(),
        0,
        "and the reserve allocation path has nothing to serve"
    );

    let mut members = headers([first, second]);
    let answer = unsafe { commit(&mut members, &mut scratch) };
    drop(oom);

    assert_eq!(answer, Reclaimed::AllocationFailed);
    assert_eq!(
        unsafe { [header_refcount(members[0]), header_refcount(members[1])] },
        [1, 1],
        "the guards came off and each member carries the ring's own edge"
    );
    for &member in &members {
        assert_eq!(
            unsafe { slot_state(member) },
            SlotState::Live,
            "nothing was freed"
        );
    }

    assert_eq!(
        unsafe { crate::test_support::entity_checked(&*Object::prop_at(first, prop_offset(1))) },
        child as *mut RcHeader,
        "the sever never started, so the edge that would have been cut stands"
    );
    assert_eq!(
        unsafe { header_refcount(child as *mut RcHeader) },
        1,
        "and the child it names still counts that edge"
    );

    // The fixture's own teardown, by hand: what the refusal left is a ring
    // nothing else holds, and the case owes it the death the collection could
    // not perform.
    unsafe {
        // Both members are retained before the first edge is cut: a ring whose
        // members hold each other alone loses one to the very first null, and
        // the loop would then walk a freed slot (the shape
        // `cycle::finalization`'s own fixture takes).
        for member in [first, second] {
            crate::refcount::ll_retain(member as *mut RcHeader);
        }

        store_prop(&mut arena, first, prop_offset(0), std::ptr::null_mut());
        store_prop(&mut arena, first, prop_offset(1), std::ptr::null_mut());
        store_prop(&mut arena, second, prop_offset(0), std::ptr::null_mut());
        for member in [first, second] {
            assert!(ll_release(member as *mut RcHeader));
            crate::object::ll_object_die(member);
        }
    }

    scratch.reset();
}
