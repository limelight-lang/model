//! The pair the scan exists for: the same ring, once with a reference
//! into its middle and once without one.
//!
//! Both graphs are traced from the same root and differ in one release,
//! so what separates them is the working count the mark left. The pair
//! is the test rather than either half of it: a scan that colours
//! everything live passes the first alone, and one that condemns every
//! zero row without raising it afterwards passes the second.

use super::*;
use crate::class::ClassBuilder;
use crate::cycle::mark::{Marked, mark};
use crate::cycle::stack::TraceStack;
use crate::cycle::testing::row_colour;
use crate::memory::arena::Arena;
use crate::memory::block_pool::test_guard;
use crate::memory::context::LLContext;
use crate::object::{Object, ll_object_die, new_constructed};
use crate::refcount::{MemoryCategory, ll_release, ll_retain};
use crate::test_support::{prop_offset, store_prop};

/// A two-member ring in the GC heap, each member naming the other, and
/// the fixture's own reference to the first member released — so the
/// ring is held from outside by whatever references to `second` the
/// caller has left standing.
///
/// # Safety
/// The caller runs on a quiescent heap and tears the ring down through
/// [`drop_ring`].
unsafe fn ring(arena: &mut Arena, name: &str) -> (*mut Object, *mut Object) {
    let node = ClassBuilder::new(name).prop("next", true).build();
    let mut context = LLContext { arena };
    let first = unsafe { new_constructed(&mut context, node, MemoryCategory::GcHeap) };
    let second = unsafe { new_constructed(&mut context, node, MemoryCategory::GcHeap) };

    unsafe {
        store_prop(arena, first, prop_offset(0), second);
        store_prop(arena, second, prop_offset(0), first);
        assert!(!ll_release(first as *mut RcHeader));
    }

    (first, second)
}

/// Break the ring and free both members, whatever the trace decided:
/// the collector frees nothing yet, so the fixture is what has to.
///
/// `held` names the members whose outside reference the test released,
/// which is the count this has to put back before the slots can go.
///
/// # Safety
/// The two objects are a ring [`ring`] built, and no other reference to
/// either is live.
unsafe fn drop_ring(arena: &mut Arena, members: (*mut Object, *mut Object), held: &[*mut Object]) {
    unsafe {
        for member in held {
            ll_retain(*member as *mut RcHeader);
        }

        store_prop(arena, members.0, prop_offset(0), std::ptr::null_mut());
        store_prop(arena, members.1, prop_offset(0), std::ptr::null_mut());
        for member in [members.0, members.1] {
            assert!(ll_release(member as *mut RcHeader));
            ll_object_die(member);
        }
    }
}

/// The ring the fixture still holds by its second member. The trace
/// reaches the first member before that reference is known — its row is
/// zero and the scan condemns it — so the verdict on it is the one the
/// live member has to overturn.
#[test]
fn a_ring_held_from_outside_scans_live_through_the_member_that_is_held() {
    let _g = test_guard();
    let mut arena = Arena::new();
    let (first, second) = unsafe { ring(&mut arena, "ScanHeldNode") };

    let mut shadow_arena = ShadowArena::new();
    let mut stack = TraceStack::new();
    assert_eq!(
        unsafe { mark(&mut shadow_arena, &mut stack, first as *mut RcHeader) },
        Marked::Complete
    );
    assert_eq!(
        unsafe { scan(&mut shadow_arena, &mut stack, first as *mut RcHeader) },
        Scanned::Complete
    );

    assert_eq!(
        unsafe { row_colour(second as *mut RcHeader) },
        Colour::Live,
        "the member the fixture holds keeps a working count above zero"
    );
    assert_eq!(
        unsafe { row_colour(first as *mut RcHeader) },
        Colour::Live,
        "and the member reachable from it is raised out of its condemnation"
    );

    shadow_arena.reset();
    unsafe { drop_ring(&mut arena, (first, second), &[first]) };
}

/// The same ring with nothing outside it, which is the case counting
/// cannot reclaim: every in-edge is internal, so both rows read zero and
/// both are condemned.
#[test]
fn a_ring_no_one_holds_is_condemned_whole() {
    let _g = test_guard();
    let mut arena = Arena::new();
    let (first, second) = unsafe { ring(&mut arena, "ScanWhiteNode") };
    // The second member's outside reference goes too, which is the whole
    // of the difference from the test above.
    assert!(!unsafe { ll_release(second as *mut RcHeader) });

    let mut shadow_arena = ShadowArena::new();
    let mut stack = TraceStack::new();
    assert_eq!(
        unsafe { mark(&mut shadow_arena, &mut stack, first as *mut RcHeader) },
        Marked::Complete
    );
    assert_eq!(
        unsafe { scan(&mut shadow_arena, &mut stack, first as *mut RcHeader) },
        Scanned::Complete
    );

    for member in [first, second] {
        assert_eq!(
            unsafe { row_colour(member as *mut RcHeader) },
            Colour::Condemned,
            "no reference into the ring stands, so no row is above zero"
        );
    }

    shadow_arena.reset();
    unsafe { drop_ring(&mut arena, (first, second), &[first, second]) };
}
