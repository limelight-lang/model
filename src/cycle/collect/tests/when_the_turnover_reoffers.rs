//! What becomes of a component the exact validation reads as live once its
//! last external reference goes, and when a collection meets it again.
//!
//! A reading of `ExternallyReferenced` defers the trace's records instead of
//! clearing their candidate bits, so the entities stay registered and no lane
//! offers them to a trace. The turnover is what offers them again, and these
//! cases are about the interval between: a ring that loses its keeper inside
//! that interval is garbage no collection finds, and it dies at the re-offer.
//!
//! **The reading is staged by an injected store.** One thread's trace and its
//! validation are a call apart, so the disagreement between them — the trace
//! proposes a component, the reading finds a reference the component does not
//! hold — has no other way in
//! (`crate::cycle::collect::InjectedVerdictRace`, and
//! `cycle/validation/tests/what_a_mutation_racing_the_verdict_costs.rs` for
//! the same staging at the reading itself).
//!
//! **The commit counter is passed rather than driven.** It is process-global,
//! and 64 commits closed here would move every other case's epoch under it
//! (`crate::cycle::epoch::pin`). The owner poll takes the count as its
//! argument for that reason, so a case reads the mirror the deferral recorded
//! and answers from it: one commit past that mirror is not a turnover, and one
//! turnover past it is.

use super::*;
use crate::cycle::collect::InjectedVerdictRace;
use crate::cycle::collect::collect_under_pressure;
use crate::cycle::epoch;
use crate::cycle::queue::{
    candidate_count, deferred_count, deferred_turnover_mirror, release_queue_segments,
    reoffer_deferred_if_epoch_moved,
};
use crate::refcount::read_maturation_stamp;

/// A class with one counted Box property, which a case uses to hold a ring
/// member from outside the component.
fn keeper_class(name: &str) -> *const Class {
    ClassBuilder::new(name).prop("held", true).build()
}

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
/// No descent reads the stamp yet — S37.1 is what will make it prune an edge —
/// so what stands here today is the arithmetic that produces the unequal ages
/// and a collection that answers the same with them as without.
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
