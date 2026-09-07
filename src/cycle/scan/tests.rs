//! The pair the scan exists for: the same ring, once with a reference
//! into its middle and once without one.
//!
//! Both graphs are traced from the same root and differ in one retain,
//! so what separates them is the working count the mark left. The pair
//! is the test rather than either half of it: a scan that colours
//! everything live passes the first alone, and one that colours every zero
//! row potentially unreachable without raising it afterwards passes the
//! second.

use super::*;
use crate::class::ClassBuilder;
use crate::cycle::mark::{MarkResult, mark};
use crate::cycle::row::take_edge_dispatches;
use crate::cycle::testing::{dismantle_ring, ring, row_color};
use crate::memory::arena::Arena;
use crate::memory::block_pool::test_guard;
use crate::refcount::{ll_release, ll_retain};

/// The ring the fixture still holds by its second member. The trace
/// reaches the first member before that reference is known — its row is
/// zero and the scan colours it potentially unreachable — so the verdict on
/// it is the one the live member has to overturn.
#[test]
fn a_ring_held_from_outside_scans_live_through_the_member_that_is_held() {
    let _g = test_guard();
    let mut arena = Arena::new();
    let node = ClassBuilder::new("ScanHeldNode").prop("next", true).build();
    let [first, second] = unsafe { ring(&mut arena, [node, node]) };
    // The reference the fixture holds into the ring, which is what the scan
    // has to spread from.
    unsafe { ll_retain(second as *mut RcHeader) };

    let mut shadow_arena = crate::cycle::testing::open_arena();
    assert_eq!(
        unsafe { mark(&mut shadow_arena, first as *mut RcHeader) },
        MarkResult::Complete
    );
    assert_eq!(
        unsafe { scan(&mut shadow_arena, first as *mut RcHeader) },
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
    unsafe {
        assert!(
            !ll_release(second as *mut RcHeader),
            "the outside reference"
        );
        dismantle_ring(&mut arena, [first, second]);
    }
}

/// The same ring with nothing outside it, which is the case counting
/// cannot reclaim: every in-edge is internal, so both rows read zero and
/// both are unreachable.
#[test]
fn a_ring_no_one_holds_is_colored_potentially_unreachable_whole() {
    let _g = test_guard();
    let mut arena = Arena::new();
    // No reference into the ring stands, which is the whole of the difference
    // from the test above.
    let node = ClassBuilder::new("ScanWhiteNode")
        .prop("next", true)
        .build();
    let [first, second] = unsafe { ring(&mut arena, [node, node]) };

    let mut shadow_arena = crate::cycle::testing::open_arena();
    assert_eq!(
        unsafe { mark(&mut shadow_arena, first as *mut RcHeader) },
        MarkResult::Complete
    );
    assert_eq!(
        unsafe { scan(&mut shadow_arena, first as *mut RcHeader) },
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
    unsafe { dismantle_ring(&mut arena, [first, second]) };
}

/// What one scan of the two-member ring costs in block dispatches: one per
/// entity classified, and none at a pop, the worklist entry carrying the row
/// the classification found.
///
/// Four, and each one is placed: the root's classification; the
/// classification of `second` through the root's edge, which colours it live
/// on a count of one; the classification of the root through its back edge,
/// which raises the root and queues it a second time; and the classification
/// of `second` again, which stops on a colour that is final. The three pops
/// between them cost none: an entry carries the row its push resolved, and
/// the pop reads the colour through it.
///
/// The mark stands outside the bracket. It dispatches over the same edges
/// and needs the row it gets, the count it writes into living there, and it
/// reads no row at its pop, so the count here is the scan's alone.
#[test]
fn a_scan_resolves_no_row_at_a_pop() {
    let _g = test_guard();
    let mut arena = Arena::new();
    let node = ClassBuilder::new("ScanDispatchNode")
        .prop("next", true)
        .build();
    let [first, second] = unsafe { ring(&mut arena, [node, node]) };
    unsafe { ll_retain(second as *mut RcHeader) };

    let mut shadow_arena = crate::cycle::testing::open_arena();
    assert_eq!(
        unsafe { mark(&mut shadow_arena, first as *mut RcHeader) },
        MarkResult::Complete
    );

    let _ = take_edge_dispatches();
    assert_eq!(
        unsafe { scan(&mut shadow_arena, first as *mut RcHeader) },
        ScanResult::Complete
    );
    assert_eq!(
        take_edge_dispatches(),
        4,
        "one dispatch per classification, and a pop reads the row its entry carries"
    );

    shadow_arena.reset();
    unsafe {
        assert!(
            !ll_release(second as *mut RcHeader),
            "the outside reference"
        );
        dismantle_ring(&mut arena, [first, second]);
    }
}
