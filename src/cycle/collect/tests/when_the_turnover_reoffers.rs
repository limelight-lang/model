//! What becomes of a component the exact validation reads as live once its
//! last external reference goes, and when a collection meets it again.
//!
//! A reading of `ExternallyReferenced` defers the trace's records instead of
//! clearing their candidate bits, so the entities stay registered and no lane
//! offers them to a trace. The turnover is what offers them again, and these
//! cases are about the interval between: a ring that loses its keeper inside
//! that interval is garbage no collection finds, and it dies at the re-offer.
//!
//! **The reading is staged by an injected store**, except in the case of the
//! mature member no lane names, whose live reading is the prune's own. One thread's trace and its
//! validation are a call apart, so the disagreement between them — the trace
//! proposes a component, the reading finds a reference the component does not
//! hold — has no other way in
//! (`crate::cycle::collect::InjectedVerdictRace`, and
//! `cycle/validation/tests/what_a_mutation_racing_the_verdict_costs.rs` for
//! the same staging at the reading itself). The prune needs no injection: a
//! ring with a mature member no lane names is read live by the trace that
//! refuses the edge into that member, and its root waits for the turnover
//! (`crate::cycle::mark`, "The mature live core is not descended into").
//!
//! **The commit counter is passed rather than driven.** It is process-global,
//! and 64 commits closed here would move every other case's epoch under it
//! (`crate::cycle::epoch::pin`). The mutator poll takes the count as its
//! argument for that reason, so a case reads the mirror the deferral recorded
//! and answers from it: one commit past that mirror is not a turnover, and one
//! turnover past it is.

use super::*;
use crate::cycle::collect::InjectedVerdictRace;
use crate::cycle::collect::collect_under_pressure;
use crate::cycle::epoch;
use crate::cycle::mark::take_edges_pruned;
use crate::cycle::queue::{
    candidate_count, deferred_count, deferred_turnover_mirror, refill_spares,
    release_queue_segments, reoffer_deferred_if_epoch_moved,
};
use crate::cycle::testing::{move_prop, ring_with_a_spare_property, stamp_of};
use crate::refcount::{
    entity_refcount, is_registered_candidate, mutator_flags, read_maturation_stamp,
};

/// The age the stamp of `entity` carries: how many collections of one epoch
/// have read its component live.
///
/// # Safety
/// `entity` is a live entity of this thread's GC heap.
unsafe fn age(entity: *mut Object) -> u32 {
    unsafe { read_maturation_stamp(entity as *mut RcHeader) }.age
}

/// One collection whose reading finds `member` held by `keeper`, the store
/// landing where a racing mutator's would: after the trace proposed the
/// component, before the counts are read.
///
/// Answers what the collection freed, which is zero whenever the staging
/// worked — the point of the fixture is the reading, not a teardown.
///
/// # Safety
/// `arena` is this thread's, `keeper` is a live entity with a counted Box
/// property at `prop_offset(0)`, and `member` belongs to the component the
/// trace is about to propose.
unsafe fn collect_with_a_reference_taken_mid_trace(
    arena: *mut Arena,
    keeper: *mut Object,
    member: *mut Object,
) -> usize {
    let _race = InjectedVerdictRace::arm(arena, keeper, member);

    unsafe { ll_gc_collect_cycles() }
}

/// A ring whose keeper goes while the ring is mature has no root in any lane:
/// the decrement meets the standing candidate bit and registers nothing, and
/// the records the last collection deferred are the only ones naming it. The
/// collection between the two events is the case's control — it finds nothing
/// to trace, which is the recall the deferral costs and the reason the
/// turnover has to offer the records again.
#[test]
fn a_matured_ring_that_loses_its_keeper_is_collected_at_the_turnover_and_not_before() {
    let _g = test_guard();
    release_queue_segments();
    let _epoch = epoch::pin(1);
    DESTRUCTOR_RUNS.store(0, Ordering::Relaxed);

    let node = node_class("ReofferedNode", counting_destructor as *const ());
    let mut arena = Arena::new();
    let members = unsafe { ring(&mut arena, [node, node]) };
    let keeper = {
        let mut context = LLContext { arena: &mut arena };
        unsafe {
            new_constructed(
                &mut context,
                keeper_class("ReofferedKeeper"),
                MemoryCategory::GcHeap,
            )
        }
    };

    // The close's deferral takes a segment for the deferred lane's head out of
    // the spare cells, and `release_queue_segments` left them empty: without
    // one the marked records fall back to the active lane, which is the
    // fallback S37.6 owns and a different case's subject.
    assert!(crate::cycle::queue::refill_spares());
    assert_eq!(
        unsafe { collect_with_a_reference_taken_mid_trace(&mut arena, keeper, members[0]) },
        0,
        "the reference the store took holds the whole ring"
    );
    assert_eq!(candidate_count(), 0, "the reading deferred both records");
    assert_eq!(deferred_count(), 2);
    assert_eq!(
        unsafe { age(members[0]) },
        1,
        "the reading stamped the ring"
    );

    unsafe {
        assert!(ll_release(keeper as *mut RcHeader));
        ll_object_die(keeper);
    }
    assert_eq!(
        candidate_count(),
        0,
        "the decrement met the standing bit and registered nothing"
    );
    assert_eq!(deferred_count(), 2);

    assert_eq!(
        unsafe { ll_gc_collect_cycles() },
        0,
        "no lane offers the ring to this trace"
    );
    assert_eq!(DESTRUCTOR_RUNS.load(Ordering::Relaxed), 0);

    let mirror = deferred_turnover_mirror();
    assert!(
        !reoffer_deferred_if_epoch_moved(epoch::one_commit_inside_the_turnover_of(mirror)),
        "one commit is not a turnover"
    );
    assert_eq!(deferred_count(), 2);

    assert!(reoffer_deferred_if_epoch_moved(epoch::one_turnover_past(
        mirror
    )));
    assert_eq!(deferred_count(), 0);
    assert_eq!(candidate_count(), 2, "each record came back once");

    assert_eq!(
        unsafe { ll_gc_collect_cycles() },
        2,
        "the re-offered roots reach the whole ring"
    );
    assert_eq!(DESTRUCTOR_RUNS.load(Ordering::Relaxed), 2);
}

/// Mates of one ring carry unequal ages between two readings: a member that
/// joins a component earlier collections have read live is born unstamped
/// beside members of age 1. The reading that follows stamps the whole
/// component with the youngest member's age plus one
/// (`crate::cycle::finalization`, `stamp_component`), so what maturing apart
/// costs is that accumulated age rather than the collection itself — the ring
/// comes back to the lane whole at the turnover and dies there.
///
/// The prune the descent makes of a mature stamp is `crate::cycle::mark`'s and
/// is not what this case is about: no member here reaches the threshold, so
/// what stands is the arithmetic that produces the unequal ages and a
/// collection that answers the same with them as without.
#[test]
fn a_ring_whose_mates_matured_apart_is_collected_at_the_turnover() {
    let _g = test_guard();
    release_queue_segments();
    let _epoch = epoch::pin(1);
    DESTRUCTOR_RUNS.store(0, Ordering::Relaxed);

    let node = node_class("MaturedApartNode", counting_destructor as *const ());
    let mut arena = Arena::new();
    let members = unsafe { ring(&mut arena, [node, node]) };
    let [first, second] = members;
    let (early_keeper, late_keeper) = {
        let mut context = LLContext { arena: &mut arena };
        unsafe {
            (
                new_constructed(
                    &mut context,
                    keeper_class("MaturedApartEarlyKeeper"),
                    MemoryCategory::GcHeap,
                ),
                new_constructed(
                    &mut context,
                    keeper_class("MaturedApartLateKeeper"),
                    MemoryCategory::GcHeap,
                ),
            )
        }
    };

    // As the case above: the deferral takes a spare for the lane's head.
    assert!(crate::cycle::queue::refill_spares());
    assert_eq!(
        unsafe { collect_with_a_reference_taken_mid_trace(&mut arena, early_keeper, first) },
        0
    );
    assert_eq!(
        deferred_count(),
        2,
        "the first reading deferred both records"
    );
    assert_eq!(unsafe { age(first) }, 1);

    // The keeper of the first reading goes, and a third member joins: the ring
    // becomes first → late → second → first, and the creation reference is
    // spent so that `late` is the only root the next trace has.
    unsafe {
        assert!(ll_release(early_keeper as *mut RcHeader));
        ll_object_die(early_keeper);
    }
    let late = {
        let mut context = LLContext { arena: &mut arena };
        unsafe { new_constructed(&mut context, node, MemoryCategory::GcHeap) }
    };
    unsafe {
        store_prop(&mut arena, late, prop_offset(0), second);
        store_prop(&mut arena, first, prop_offset(0), late);
        assert!(!ll_release(late as *mut RcHeader), "the ring holds `late`");
    }
    assert_eq!(candidate_count(), 1, "only the new member is offered");
    assert_eq!(unsafe { age(first) }, 1);
    assert_eq!(
        unsafe { age(late) },
        0,
        "the mates of one ring stand at unequal ages"
    );

    assert_eq!(
        unsafe { collect_with_a_reference_taken_mid_trace(&mut arena, late_keeper, first) },
        0
    );
    assert_eq!(
        [unsafe { age(first) }, unsafe { age(second) }, unsafe {
            age(late)
        }],
        [1, 1, 1],
        "the reading carries the component at its youngest member's age"
    );
    assert_eq!(deferred_count(), 3);

    unsafe {
        assert!(ll_release(late_keeper as *mut RcHeader));
        ll_object_die(late_keeper);
    }
    assert_eq!(
        unsafe { ll_gc_collect_cycles() },
        0,
        "no lane offers the ring to this trace"
    );

    let mirror = deferred_turnover_mirror();
    assert!(reoffer_deferred_if_epoch_moved(epoch::one_turnover_past(
        mirror
    )));
    assert_eq!(
        candidate_count(),
        3,
        "every age comes back to the same lane"
    );

    assert_eq!(unsafe { ll_gc_collect_cycles() }, 3);
    assert_eq!(DESTRUCTOR_RUNS.load(Ordering::Relaxed), 3);
}

/// A bounded round of the pressure path reads a prefix of the lane, so the
/// records behind that prefix name components no reading answered for. They go
/// back to the active lane whatever the round's own reading was: a record in
/// the deferred lane is offered to no trace until the turnover, and this path
/// runs because an allocation was refused.
#[test]
fn a_bounded_pressure_round_defers_nothing_it_did_not_read() {
    let _g = test_guard();
    release_queue_segments();
    let _epoch = epoch::pin(1);
    DESTRUCTOR_RUNS.store(0, Ordering::Relaxed);

    let node = node_class("BoundedRoundNode", counting_destructor as *const ());
    let mut arena = Arena::new();

    // Rings of two, past what one harvest region holds, so the first trace
    // overflows and every round after it is bounded to half the roots it read
    // (`collect_under_pressure`).
    let pairs = MEMBER_CAPACITY as usize;
    let mut members = Vec::with_capacity(pairs * 2);
    for _ in 0..pairs {
        members.extend_from_slice(&unsafe { ring(&mut arena, [node, node]) });
    }
    assert!(members.len() > MEMBER_CAPACITY as usize);

    let keeper = {
        let mut context = LLContext { arena: &mut arena };
        unsafe {
            new_constructed(
                &mut context,
                keeper_class("BoundedRoundKeeper"),
                MemoryCategory::GcHeap,
            )
        }
    };

    // One reading of the run finds a reference the component does not hold,
    // and it is a reading of a bounded round: the trace that produced it read
    // half the lane at most.
    let _race = InjectedVerdictRace::arm(&mut arena, keeper, members[0]);
    let freed = unsafe { collect_under_pressure() };

    assert_eq!(
        deferred_count(),
        0,
        "a round that read a prefix of the lane defers none of it"
    );
    assert!(
        freed < members.len(),
        "the reading spared its own component"
    );
    assert!(
        candidate_count() > 0,
        "the records the reading spared are in the lane a trace is offered"
    );

    // Nothing was stranded: with the keeper gone the rest of the population
    // reaches the frees through an ordinary collection.
    unsafe {
        assert!(ll_release(keeper as *mut RcHeader));
        ll_object_die(keeper);
    }
    let rest = unsafe { ll_gc_collect_cycles() };
    assert_eq!(
        freed + rest,
        members.len(),
        "every member of the population was freed across the two collections"
    );
}

/// A ring one of whose members never observed a non-final decrement is read
/// live by the trace that meets it once that member is mature: the member is
/// not expanded, so its edge back into the root is never subtracted, the
/// root's row stays above zero, and the reading is the prune's rather than
/// any keeper's. No lane offers the root, deferred on an earlier reading,
/// until the turnover, whose epoch retires the stamp and lets the ring die
/// (`crate::cycle::mark`, "The mature live core is not descended into").
///
/// The member is matured under a keeper through three collections with the
/// spare cells empty, so each close's deferral falls back to the active lane
/// and the same root is offered to every reading — the fallback S37.6 owns,
/// standing in for the three fresh roots a real population would meet the
/// component through; the stamp is the same, because the unit stamped is the
/// component (`crate::cycle::maturation`). The cells are refilled before the third collection, whose
/// deferral is real, and the reading the case is about is then made from the
/// shape it is ordinary in: a garbage ring outside whose member points into
/// this one, the ring's own root standing in the deferred lane where the
/// third reading left it. The outside ring dies at that reading and this one
/// does not, which is the recall the prune costs.
#[test]
fn a_ring_with_a_mature_member_no_lane_names_is_read_live_and_dies_at_the_turnover() {
    let _g = test_guard();
    release_queue_segments();
    let epoch_of_the_stamp = epoch::pin(0);
    DESTRUCTOR_RUNS.store(0, Ordering::Relaxed);

    let node = node_class("PrunedRingNode", counting_destructor as *const ());
    let mut arena = Arena::new();
    let (root, member, keeper) = {
        let mut context = LLContext { arena: &mut arena };
        unsafe {
            (
                new_constructed(&mut context, node, MemoryCategory::GcHeap),
                new_constructed(&mut context, node, MemoryCategory::GcHeap),
                new_constructed(
                    &mut context,
                    keeper_class("PrunedRingKeeper"),
                    MemoryCategory::GcHeap,
                ),
            )
        }
    };
    unsafe {
        move_prop(root, prop_offset(0), member);
        store_prop(&mut arena, member, prop_offset(0), root);
        store_prop(&mut arena, keeper, prop_offset(0), root);
        assert!(
            !ll_release(root as *mut RcHeader),
            "the member and the keeper hold the root"
        );
    }
    assert_eq!(candidate_count(), 1, "the release registered the root");
    assert_eq!(
        unsafe { entity_refcount(member) },
        1,
        "the moved reference is the member's only holder, and its count never fell"
    );

    take_edges_pruned();
    for age in 1..=2 {
        assert_eq!(
            unsafe { ll_gc_collect_cycles() },
            0,
            "the keeper holds the ring"
        );
        assert_eq!(unsafe { stamp_of(member) }, (0, age));
        assert_eq!(
            candidate_count(),
            1,
            "with no spare cell the close keeps the root in the active lane"
        );
    }
    assert!(refill_spares());
    assert_eq!(unsafe { ll_gc_collect_cycles() }, 0);
    assert_eq!(unsafe { stamp_of(member) }, (0, 3), "at the threshold");
    assert_eq!(candidate_count(), 0);
    assert_eq!(deferred_count(), 1, "the third reading deferred the root");
    assert_eq!(
        take_edges_pruned(),
        0,
        "below the threshold the member is descended into"
    );
    assert!(
        !is_registered_candidate(unsafe { mutator_flags(member as *mut RcHeader) }),
        "no lane names the member, which is the premise the prune reads"
    );

    // The keeper lets go. The decrement meets the standing bit and registers
    // nothing, and the ring is garbage with one member at the threshold.
    unsafe {
        assert!(ll_release(keeper as *mut RcHeader));
        ll_object_die(keeper);
    }
    assert_eq!(candidate_count(), 0);

    // A garbage ring outside points into the mature one: its members are the
    // roots the trace enters through, and the root's row starts at two. An
    // outside root the case held would read live itself and raise the ring
    // live on the scan whatever the prune did, so the outside is garbage.
    let outside = unsafe { ring_with_a_spare_property(&mut arena, "PrunedRingOutside") };
    unsafe { store_prop(&mut arena, outside[0], prop_offset(1), root) };
    assert_eq!(
        candidate_count(),
        3,
        "the outside ring is what the trace is offered"
    );

    assert_eq!(
        unsafe { ll_gc_collect_cycles() },
        3,
        "the outside ring dies; the mature member is not expanded, so its edge into the root is never subtracted"
    );
    assert_eq!(take_edges_pruned(), 1, "the one edge into the member");
    assert_eq!(
        DESTRUCTOR_RUNS.load(Ordering::Relaxed),
        0,
        "the mature ring did not die: the outside ring has no destructor"
    );
    assert_eq!(candidate_count(), 0);
    assert_eq!(
        deferred_count(),
        1,
        "the ring's root stands where the third reading left it"
    );
    assert_eq!(
        unsafe { entity_refcount(root) },
        1,
        "the outside ring's teardown let go of the root, against its standing bit"
    );
    assert_eq!(
        unsafe { ll_gc_collect_cycles() },
        0,
        "no lane offers the ring to this trace"
    );

    // The turnover: the re-offer puts the root back, and the next epoch reads
    // the member's stamp as none at all.
    let mirror = deferred_turnover_mirror();
    drop(epoch_of_the_stamp);
    let _epoch = epoch::pin(1);
    assert!(reoffer_deferred_if_epoch_moved(epoch::one_turnover_past(
        mirror
    )));
    assert_eq!(deferred_count(), 0);
    assert_eq!(candidate_count(), 1, "the root came back once");

    assert_eq!(
        unsafe { ll_gc_collect_cycles() },
        2,
        "a stamp of another epoch prunes nothing, and the trace reaches the whole ring"
    );
    assert_eq!(take_edges_pruned(), 0);
    assert_eq!(DESTRUCTOR_RUNS.load(Ordering::Relaxed), 2);
}
