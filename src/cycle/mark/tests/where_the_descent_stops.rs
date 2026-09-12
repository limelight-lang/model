//! Which edge the descent refuses to take, and which it takes whatever the
//! stamp says.
//!
//! The stamps here are written by real collections rather than by hand. What
//! the prune reads is the other half of `crate::cycle::maturation`'s
//! arithmetic, and a stamp a fixture wrote would leave the two halves agreeing
//! only in this file: the threshold is three, an age is one more than the
//! component's youngest member, so the third commit is the one that puts a
//! live child at the threshold and the fourth collection is the first that can
//! stop at it (`rfc/model/gc/cycle/questions.md`, Y9).
//!
//! The pruned-edge count is read through [`take_edges_pruned`], which clears
//! as it answers, so every reading below names the collections between it and
//! the one before it.

use std::ptr;

use super::*;
use crate::cycle::epoch;
use crate::cycle::queue::release_queue_segments;
use crate::cycle::testing::{ages, stamp_of};
use crate::gc::ll_gc_collect_cycles;

/// A ring of two under a keeper, with one more live entity hanging off the
/// ring at the second property: the members, the keeper, the child.
///
/// The child is what the prune is about. It keeps its creation reference, so
/// nothing registers it and the trace meets it as the target of an edge alone;
/// the ring's own members are registered candidates and therefore roots, which
/// is the population the rule spares.
///
/// # Safety
/// As [`ring`], and `class` carries two counted Box properties.
unsafe fn a_ring_with_a_child(
    arena: &mut Arena,
    class: *const Class,
) -> ([*mut Object; 2], *mut Object, *mut Object) {
    let members = unsafe { ring(arena, [class, class]) };
    let keeper = unsafe { a_held_object(arena, class) };
    let child = unsafe { a_held_object(arena, class) };
    unsafe {
        store_prop(arena, keeper, prop_offset(1), members[0]);
        store_prop(arena, members[0], prop_offset(1), child);
    }

    (members, keeper, child)
}

/// The third commit puts the child at the threshold and the fourth collection
/// is the one that stops at it.
///
/// What says the descent stopped is a second child built after the third
/// commit: any collection that reaches the first one reaches it too and stamps
/// it, so an unstamped one is an edge that was not taken. The count of pruned
/// edges says the same thing from the other side, and it is one — the ring's
/// own two edges name roots.
#[test]
fn the_fourth_collection_stops_at_the_child_the_third_matured() {
    let _g = test_guard();
    release_queue_segments();
    let _epoch = epoch::pin(0);
    let class = node_class("PruneMature");
    let mut arena = Arena::new();

    let (members, _keeper, child) = unsafe { a_ring_with_a_child(&mut arena, class) };
    take_edges_pruned();
    for age in 1..=3 {
        assert_eq!(
            unsafe { ll_gc_collect_cycles() },
            0,
            "the keeper holds the ring and the ring holds the child"
        );
        assert_eq!(unsafe { stamp_of(child) }, (0, age));
        assert_eq!(
            take_edges_pruned(),
            0,
            "the collection that leaves the child at {age} read it below that"
        );
    }

    let grandchild = unsafe { a_held_object(&mut arena, class) };
    unsafe { store_prop(&mut arena, child, prop_offset(1), grandchild) };

    assert_eq!(unsafe { ll_gc_collect_cycles() }, 0);
    assert_eq!(
        take_edges_pruned(),
        1,
        "the one edge into the child, and neither edge of the ring"
    );
    assert_eq!(
        unsafe { stamp_of(grandchild) },
        (0, 0),
        "the descent stopped at the child, so nothing behind it was met"
    );
    // A fixture check and not a discriminator: a commit that had met the child
    // would have written the same saturated stamp back.
    assert_eq!(unsafe { stamp_of(child) }, (0, 3));
    assert_eq!(
        unsafe { ages(&members) },
        vec![3, 3],
        "the ring is met at every collection and its age saturates"
    );
}

/// A ring every member of which stands at the threshold is collected at the
/// trace that meets it: the rule is about the target of an edge, and a target
/// a queue entry names is spared whatever its stamp.
///
/// This is the case the rule exists for, and the shape it is right on: every
/// member is registered, which [`ring`] guarantees by spending each creation
/// reference through a release. Pruning an edge into a registered member
/// would leave the two rows above zero, the scan would read the ring as live,
/// and a ring that became garbage at the threshold would wait for the
/// turnover — which is what `rfc/model/gc/rc-cycle.md` refuses by name under
/// "Candidate registration and trial deletion". A ring with a mature member
/// no entry names is the other shape, and it does wait (`crate::cycle::mark`,
/// module doc).
#[test]
fn a_ring_at_the_threshold_is_collected_at_the_trace_that_meets_it() {
    let _g = test_guard();
    release_queue_segments();
    let _epoch = epoch::pin(0);
    let class = node_class("PruneRoots");
    let mut arena = Arena::new();

    let members = unsafe { ring(&mut arena, [class, class]) };
    let keeper = unsafe { a_held_object(&mut arena, class) };
    unsafe { store_prop(&mut arena, keeper, prop_offset(1), members[0]) };

    for _ in 0..3 {
        assert_eq!(
            unsafe { ll_gc_collect_cycles() },
            0,
            "the keeper holds them"
        );
    }
    assert_eq!(unsafe { ages(&members) }, vec![3, 3]);

    // The keeper lets go. The decrement registers nothing new — the bit has
    // stood since the ring spent its creation references — and what is left is
    // garbage whose every member is at the threshold.
    unsafe { store_prop(&mut arena, keeper, prop_offset(1), ptr::null_mut()) };

    take_edges_pruned();
    assert_eq!(
        unsafe { ll_gc_collect_cycles() },
        members.len(),
        "a mature ring is still collected by the trace that meets it"
    );
    assert_eq!(
        take_edges_pruned(),
        0,
        "every edge of the ring names a registered candidate"
    );
}

/// A stamp of another epoch prunes nothing: the age is read against the epoch
/// beside it, and a collection that finds them disagreeing descends as though
/// there were no stamp at all.
///
/// The stamp is not cleared on the way — nothing in the trace writes an entity
/// — so what retires it is the commit's own write, and the child comes out of
/// this collection at age 1 under the new epoch.
#[test]
fn a_stamp_of_another_epoch_prunes_no_edge() {
    let _g = test_guard();
    release_queue_segments();
    let class = node_class("PruneStale");
    let mut arena = Arena::new();

    let (_members, _keeper, child) = {
        let _epoch = epoch::pin(0);
        let built = unsafe { a_ring_with_a_child(&mut arena, class) };
        for _ in 0..3 {
            assert_eq!(
                unsafe { ll_gc_collect_cycles() },
                0,
                "the keeper holds them"
            );
        }
        built
    };
    assert_eq!(unsafe { stamp_of(child) }, (0, 3), "at the threshold");

    let _epoch = epoch::pin(1);
    take_edges_pruned();
    assert_eq!(unsafe { ll_gc_collect_cycles() }, 0);
    assert_eq!(
        take_edges_pruned(),
        0,
        "an age of another epoch reads as none at all"
    );
    assert_eq!(
        unsafe { stamp_of(child) },
        (1, 1),
        "and the commit writes over the stale stamp rather than clearing it"
    );
}

/// A pinned threshold moves the collection the descent stops at, and nothing
/// else: at `k = 1` the first commit puts the child at the threshold and the
/// second collection is the one that stops at it.
///
/// The same reading as [`the_fourth_collection_stops_at_the_child_the_third_matured`]
/// two commits earlier, which is what the pin exists for: the pruned-edge
/// count at a `k` the constant does not carry (`PLAN.md` S40.1). The pin is
/// dropped before the last collection, which reads the constant again and
/// descends into a child at age 1.
#[test]
fn a_pinned_threshold_of_one_stops_the_second_collection_at_the_child() {
    let _g = test_guard();
    release_queue_segments();
    let _epoch = epoch::pin(0);
    let class = node_class("PrunePinned");
    let mut arena = Arena::new();

    let (_members, _keeper, child) = unsafe { a_ring_with_a_child(&mut arena, class) };
    let grandchild = unsafe { a_held_object(&mut arena, class) };
    unsafe { store_prop(&mut arena, child, prop_offset(1), grandchild) };

    let pin = pin_threshold(1);
    take_edges_pruned();
    assert_eq!(unsafe { ll_gc_collect_cycles() }, 0);
    assert_eq!(unsafe { stamp_of(child) }, (0, 1));
    assert_eq!(
        take_edges_pruned(),
        0,
        "the first collection reads the child unstamped"
    );

    assert_eq!(unsafe { ll_gc_collect_cycles() }, 0);
    assert_eq!(
        take_edges_pruned(),
        1,
        "the second stops at the child, which the first commit put at the threshold"
    );
    assert_eq!(
        unsafe { stamp_of(grandchild) },
        (0, 1),
        "the grandchild keeps the first commit's stamp: nothing behind the child was met"
    );

    drop(pin);
    assert_eq!(unsafe { ll_gc_collect_cycles() }, 0);
    assert_eq!(
        take_edges_pruned(),
        0,
        "the constant is 3 again, and a child at age 1 is descended into"
    );
    assert_eq!(
        unsafe { stamp_of(grandchild) },
        (0, 2),
        "the third commit met the grandchild and aged it"
    );
}
