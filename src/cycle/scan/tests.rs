//! The pair the scan exists for: the same ring, once with a reference
//! into its middle and once without one.
//!
//! Both graphs are traced from the same root and differ in one release,
//! so what separates them is the working count the mark left. The pair
//! is the test rather than either half of it: a scan that colours
//! everything live passes the first alone, and one that colours every zero
//! row potentially unreachable without raising it afterwards passes the
//! second.

use super::*;
use crate::class::ClassBuilder;
use crate::cycle::mark::{MarkResult, mark};
use crate::cycle::row::take_edge_dispatches;
use crate::cycle::stack::TraceStack;
use crate::cycle::testing::row_color;
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
/// zero and the scan colours it potentially unreachable — so the verdict on
/// it is the one the live member has to overturn.
#[test]
fn a_ring_held_from_outside_scans_live_through_the_member_that_is_held() {
    let _g = test_guard();
    let mut arena = Arena::new();
    let (first, second) = unsafe { ring(&mut arena, "ScanHeldNode") };

    let mut shadow_arena = crate::cycle::testing::open_arena();
    let mut stack = TraceStack::new();
    assert_eq!(
        unsafe { mark(&mut shadow_arena, &mut stack, first as *mut RcHeader) },
        MarkResult::Complete
    );
    assert_eq!(
        unsafe { scan(&mut shadow_arena, &mut stack, first as *mut RcHeader) },
        ScanResult::Complete
    );

    assert_eq!(
        unsafe { row_color(second as *mut RcHeader) },
        Color::Live,
        "the member the fixture holds keeps a working count above zero"
    );
    assert_eq!(
        unsafe { row_color(first as *mut RcHeader) },
        Color::Live,
        "and the member reachable from it is raised out of its condemnation"
    );

    shadow_arena.reset();
    unsafe { drop_ring(&mut arena, (first, second), &[first]) };
}

/// The same ring with nothing outside it, which is the case counting
/// cannot reclaim: every in-edge is internal, so both rows read zero and
/// both are unreachable.
#[test]
fn a_ring_no_one_holds_is_colored_potentially_unreachable_whole() {
    let _g = test_guard();
    let mut arena = Arena::new();
    let (first, second) = unsafe { ring(&mut arena, "ScanWhiteNode") };
    // The second member's outside reference goes too, which is the whole
    // of the difference from the test above.
    assert!(!unsafe { ll_release(second as *mut RcHeader) });

    let mut shadow_arena = crate::cycle::testing::open_arena();
    let mut stack = TraceStack::new();
    assert_eq!(
        unsafe { mark(&mut shadow_arena, &mut stack, first as *mut RcHeader) },
        MarkResult::Complete
    );
    assert_eq!(
        unsafe { scan(&mut shadow_arena, &mut stack, first as *mut RcHeader) },
        ScanResult::Complete
    );

    for member in [first, second] {
        assert_eq!(
            unsafe { row_color(member as *mut RcHeader) },
            Color::PotentiallyUnreachable,
            "no reference into the ring stands, so no row is above zero"
        );
    }

    shadow_arena.reset();
    unsafe { drop_ring(&mut arena, (first, second), &[first, second]) };
}

/// What one scan of the two-member ring costs in block dispatches: the loop
/// head resolves the row of every entity it pops, and the classification
/// that queued that entity resolved the same row to colour it.
///
/// Seven, and each one is placed: the root's classification; its pop; the
/// classification of `second` through the root's edge, which colours it live
/// on a count of one; that entity's pop; the classification of the root
/// through its back edge, which raises the root and queues it a second time;
/// the root's second pop; and the classification of `second` again, which
/// stops on a colour that is final. Three of the seven are pops, and a pop
/// resolves a row the push that queued it already held.
///
/// The mark stands outside the bracket. It dispatches over the same edges
/// and needs the row it gets, the count it writes into living there, and it
/// reads no row at its pop, so the doubled work is the scan's alone.
#[test]
fn a_scan_resolves_the_row_of_every_entity_it_pops_a_second_time() {
    let _g = test_guard();
    let mut arena = Arena::new();
    let (first, second) = unsafe { ring(&mut arena, "ScanDispatchNode") };

    let mut shadow_arena = crate::cycle::testing::open_arena();
    let mut stack = TraceStack::new();
    assert_eq!(
        unsafe { mark(&mut shadow_arena, &mut stack, first as *mut RcHeader) },
        MarkResult::Complete
    );

    let _ = take_edge_dispatches();
    assert_eq!(
        unsafe { scan(&mut shadow_arena, &mut stack, first as *mut RcHeader) },
        ScanResult::Complete
    );
    assert_eq!(
        take_edge_dispatches(),
        7,
        "four classifications and three pops, the pops resolving a row the \
         classification that queued the entity had already found"
    );

    shadow_arena.reset();
    unsafe { drop_ring(&mut arena, (first, second), &[first]) };
}
