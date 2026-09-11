//! What the descent writes, and what it leaves when it is refused.
//!
//! The subject is the unit rather than the arithmetic: a stamp of the right
//! epoch and one age more than the component's youngest member is what
//! [`crate::cycle::finalization`]'s own cases already read on the other
//! producer. What only this file can show is that the component is the
//! strongly connected component and not the closure — two rings joined by one
//! one-way edge under one keeper stand in one closure and take two different
//! ages.
//!
//! **The rings are aged apart by the batch a collection reaches.** A ring
//! built before a collection and left live takes a stamp at it; a ring built
//! after that collection has none, so the next commit reads the first at age 1
//! and the second at 0, and the ages the two carry afterwards differ by one.
//! Ageing them apart is the whole of the fixture: on a first collection every
//! live component reads age 0 and takes age 1, and a case built that way
//! cannot tell the unit from the closure.

use super::{DescentCounts, Stamped, refuse_the_descent_at, take_descent_counts};
use crate::class::{Class, ClassBuilder};
use crate::cycle::arena::TraceScratchArena;
use crate::cycle::epoch;
use crate::cycle::membership::Membership;
use crate::cycle::queue::release_queue_segments;
use crate::cycle::testing::{ages, ring, stamp_of};
use crate::gc::ll_gc_collect_cycles;
use crate::memory::arena::Arena;
use crate::memory::block_pool::test_guard;
use crate::memory::context::LLContext;
use crate::object::{Object, new_constructed};
use crate::refcount::{MemoryCategory, RcHeader};
use crate::test_support::{prop_offset, store_prop};

/// A ring member: one counted Box property for the ring's own edge, and a
/// second for the edge that leaves it.
fn node_class(name: &str) -> *const Class {
    ClassBuilder::new(name)
        .prop("next", true)
        .prop("side", true)
        .build()
}

/// Two rings, the second reachable only through a one-way edge out of the
/// first, and a keeper holding the first from outside.
///
/// The keeper is what makes the whole closure live: without it the scan reads
/// both rings as unreachable and the commit tears them down instead of
/// stamping them.
///
/// # Safety
/// As [`ring`].
unsafe fn two_rings_under_one_keeper(
    arena: &mut Arena,
    class: *const Class,
) -> ([*mut Object; 2], [*mut Object; 2], *mut Object) {
    let keeper = {
        let mut context = LLContext { arena: &mut *arena };
        unsafe { new_constructed(&mut context, class, MemoryCategory::GcHeap) }
    };
    let upstream = unsafe { ring(arena, [class, class]) };
    let downstream = unsafe { ring(arena, [class, class]) };
    unsafe {
        store_prop(arena, upstream[0], prop_offset(1), downstream[0]);
        store_prop(arena, keeper, prop_offset(1), upstream[0]);
    }

    (upstream, downstream, keeper)
}

/// Two components in one closure take two ages: the commit reads each ring's
/// own youngest member and never the closure's.
///
/// The downstream ring is aged one reading ahead by a collection of its own,
/// so the second collection has a component at age 1 and a component at age 0
/// standing in one closure. A producer that took the closure as the unit would
/// write one age into all four members, and the age would be 1.
#[test]
fn two_rings_in_one_closure_take_the_age_of_their_own() {
    let _g = test_guard();
    release_queue_segments();
    let _epoch = epoch::pin(0);
    let class = node_class("MaturationRing");
    let mut arena = Arena::new();

    // The downstream ring alone, held by a keeper of its own: one collection
    // over it puts it a reading ahead of everything built afterwards.
    let (downstream, _spare, _first_keeper) =
        unsafe { two_rings_under_one_keeper(&mut arena, class) };
    take_descent_counts();
    assert_eq!(
        unsafe { ll_gc_collect_cycles() },
        0,
        "the keeper holds them"
    );
    assert_eq!(unsafe { ages(&downstream) }, vec![1, 1]);

    let upstream = unsafe { ring(&mut arena, [class, class]) };
    unsafe { store_prop(&mut arena, upstream[0], prop_offset(1), downstream[0]) };
    let keeper = {
        let mut context = LLContext { arena: &mut arena };
        unsafe { new_constructed(&mut context, class, MemoryCategory::GcHeap) }
    };
    unsafe { store_prop(&mut arena, keeper, prop_offset(1), upstream[0]) };

    take_descent_counts();
    assert_eq!(unsafe { ll_gc_collect_cycles() }, 0);
    assert_eq!(
        unsafe { ages(&upstream) },
        vec![1, 1],
        "the ring this collection met first reads age 0 and takes 1"
    );
    assert_eq!(
        unsafe { ages(&downstream) },
        vec![2, 2],
        "the ring a reading ahead takes one more, in the same closure"
    );
}

/// A member joined to a ring between two commits holds that ring at age 1: the
/// minimum is the component's youngest member and never its oldest.
#[test]
fn a_member_joined_between_two_commits_holds_its_ring_at_one() {
    let _g = test_guard();
    release_queue_segments();
    let _epoch = epoch::pin(0);
    let class = node_class("MaturationJoin");
    let mut arena = Arena::new();

    let (upstream, downstream, _keeper) = unsafe { two_rings_under_one_keeper(&mut arena, class) };
    assert_eq!(unsafe { ll_gc_collect_cycles() }, 0);
    assert_eq!(unsafe { ages(&upstream) }, vec![1, 1]);

    // A third member into the downstream ring: it is reached from the ring and
    // reaches it back, so it joins that component rather than standing beside
    // it.
    let joined = {
        let mut context = LLContext { arena: &mut arena };
        unsafe { new_constructed(&mut context, class, MemoryCategory::GcHeap) }
    };
    unsafe {
        store_prop(&mut arena, joined, prop_offset(0), downstream[0]);
        store_prop(&mut arena, downstream[1], prop_offset(1), joined);
    }

    assert_eq!(unsafe { ll_gc_collect_cycles() }, 0);
    assert_eq!(
        unsafe { ages(&upstream) },
        vec![2, 2],
        "the ring nothing joined takes one more"
    );
    assert_eq!(
        unsafe { ages(&downstream) },
        vec![1, 1],
        "and the ring the newcomer joined goes back to 1"
    );
    assert_eq!(unsafe { ages(&[joined]) }, vec![1], "the newcomer with it");
}

/// A commit of a later epoch writes age 1 with that epoch: a stamp of an
/// earlier one contributes nothing to the minimum and is retired by being read
/// against the epoch beside it.
#[test]
fn a_commit_of_the_next_epoch_writes_age_one() {
    let _g = test_guard();
    release_queue_segments();
    let class = node_class("MaturationTurnover");
    let mut arena = Arena::new();

    let (upstream, _downstream, _keeper) = unsafe { two_rings_under_one_keeper(&mut arena, class) };
    {
        let _epoch = epoch::pin(1);
        assert_eq!(unsafe { ll_gc_collect_cycles() }, 0);
        assert_eq!(unsafe { ll_gc_collect_cycles() }, 0);
        assert_eq!(unsafe { ages(&upstream) }, vec![2, 2]);
        assert_eq!(unsafe { stamp_of(upstream[0]) }, (1, 2));
    }

    let _epoch = epoch::pin(2);
    assert_eq!(unsafe { ll_gc_collect_cycles() }, 0);
    assert_eq!(
        unsafe { stamp_of(upstream[0]) },
        (2, 1),
        "the age of another epoch reads as none at all"
    );
}

/// A refused segment leaves every component either stamped whole or untouched,
/// and the commit goes on.
///
/// The refusal is injected at the second vertex, which stands inside the first
/// ring's own descent: that component is open when the refusal arrives, so it
/// takes no stamp, and neither does the ring behind it.
#[test]
fn a_refused_descent_stamps_no_component_it_had_opened() {
    let _g = test_guard();
    release_queue_segments();
    let _epoch = epoch::pin(0);
    let class = node_class("MaturationRefusal");
    let mut arena = Arena::new();

    let (upstream, downstream, _keeper) = unsafe { two_rings_under_one_keeper(&mut arena, class) };
    take_descent_counts();
    {
        let _refusal = refuse_the_descent_at(2);
        assert_eq!(
            unsafe { ll_gc_collect_cycles() },
            0,
            "a refused descent is not a refused collection"
        );
    }

    let counts = take_descent_counts();
    assert_eq!(counts.refusals, 1);
    assert_eq!(counts.components, 0, "no component was closed");
    assert_eq!(counts.vertices, 1, "the refusal fell on the second vertex");
    assert_eq!(
        unsafe { ages(&upstream) },
        vec![0, 0],
        "an open component carries the stamp it had"
    );
    assert_eq!(unsafe { ages(&downstream) }, vec![0, 0]);

    // The collection after it is ordinary, which is what says the refusal left
    // no state behind: the stacks are empty and every row is the next trace's.
    assert_eq!(unsafe { ll_gc_collect_cycles() }, 0);
    assert_eq!(unsafe { ages(&upstream) }, vec![1, 1]);
    assert_eq!(unsafe { ages(&downstream) }, vec![1, 1]);
    let counts = take_descent_counts();
    assert_eq!(counts.components, 2);
    assert_eq!(counts.vertices, 4);
    assert!(
        (2..=4).contains(&counts.high_water),
        "a whole ring stands at once, and the closure bounds the rest: {}",
        counts.high_water
    );
    assert_eq!(counts.refusals, 0);
}

/// A membership of harvested members is the pressure path's, and the descent
/// answers it without reading anything: that path has given its blocks back
/// before the commit, so it holds no row to walk.
#[test]
fn a_harvested_membership_has_no_rows_to_walk() {
    let _g = test_guard();
    release_queue_segments();
    let mut members: [*mut RcHeader; 0] = [];
    let membership = Membership::listed(&mut members);
    let mut arena = TraceScratchArena::open().expect("a workspace");

    assert_eq!(
        unsafe { super::stamp_live_components(&membership, &mut arena, 0) },
        Stamped::NoRows
    );
    assert_eq!(take_descent_counts(), DescentCounts::default());
    arena.reset();
}

/// The descent's row resolutions are counted apart from everything else the
/// collection resolves, and the commit around it is one that tears a component
/// down: the exact validation asks a membership about every out-edge of every
/// member, and the teardown walks them again, so a phase that ended at the
/// close would call all of that the descent's.
#[test]
fn the_descent_resolves_its_rows_under_a_phase_of_its_own() {
    let _g = test_guard();
    release_queue_segments();
    let _epoch = epoch::pin(0);
    let class = node_class("MaturationPhases");
    let mut arena = Arena::new();

    let (upstream, downstream, _keeper) = unsafe { two_rings_under_one_keeper(&mut arena, class) };
    // A ring nothing holds, so this collection tears one down as well as
    // stamping the live pair.
    let dead = unsafe { ring(&mut arena, [class, class]) };
    crate::cycle::row::take_edge_dispatches();
    crate::cycle::row::take_dispatches_in_mark_phase();
    crate::cycle::row::take_dispatches_in_the_descent();
    assert_eq!(unsafe { ll_gc_collect_cycles() }, dead.len());

    let in_mark = crate::cycle::row::take_dispatches_in_mark_phase();
    let before_descent = crate::cycle::row::take_dispatches_before_the_descent();
    let in_descent = crate::cycle::row::take_dispatches_in_the_descent();
    let whole = crate::cycle::row::take_edge_dispatches();
    // Six roots and seven edges, which is what the mark resolves; the scan
    // walks the same edges from the same roots.
    assert_eq!(in_mark, 13);
    assert_eq!(before_descent, 2 * in_mark, "and the scan repeats both");
    assert_eq!(
        in_descent, 5,
        "the five edges of the live pair, and none of the dead ring's rows: \
         the descent walks the live color alone"
    );
    assert!(
        whole > before_descent + in_descent,
        "the exact validation and the teardown resolve rows of their own, and \
         the descent's phase holds none of them: {whole} against {before_descent} \
         and {in_descent}"
    );
    assert_eq!(unsafe { ages(&upstream) }, vec![1, 1]);
    assert_eq!(unsafe { ages(&downstream) }, vec![1, 1]);
}
