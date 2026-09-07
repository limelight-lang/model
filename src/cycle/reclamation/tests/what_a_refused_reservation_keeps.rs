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
    let [first, second] = unsafe { ring(&mut arena, [node, node]) };
    let child = unsafe { object(&mut arena, plain) };
    unsafe {
        // The child is attached before the trace, which has to meet it as a
        // live external rather than as a member.
        store_prop(&mut arena, first, prop_offset(1), child);
        spend_creation_references(&[child]);
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

    // The fixture's own teardown: what the refusal left is a ring nothing else
    // holds, and the case owes it the death the collection could not perform.
    // The child goes with the members — the death of the member holding it
    // releases the edge at property 1.
    unsafe { dismantle_ring(&mut arena, [first, second]) };

    scratch.reset();
}
