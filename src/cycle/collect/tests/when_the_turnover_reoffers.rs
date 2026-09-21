//! What becomes of a component the exact validation reads as live once its
//! last external reference goes, and when a collection meets it again.
//!
//! A reading of `ExternallyReferenced` defers the trace's records instead of
//! clearing their candidate bits, so the entities stay registered and no lane
//! offers them to a trace. The turnover is what offers them again, and these
//! cases are about the interval between: a ring that loses its keeper inside
//! that interval is garbage no collection finds, and it dies at the re-offer.
//!
//! **The reading is staged by an injected store**, except in the cases of the
//! mature member no lane names, whose live reading is the prune's own, and of
//! the withheld dead slot, whose keeper simply holds it. One thread's trace and its
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
//! **The commit counter is passed rather than driven.** A turnover is 64
//! commits, and driving them is 64 collections to reach a reading the argument
//! states. The mutator poll takes the count as its argument for that reason,
//! so a case reads the mirror the deferral recorded and answers from it: one
//! commit past that mirror is not a turnover, and one turnover past it is.

use super::*;
use crate::cycle::collect::InjectedVerdictRace;
use crate::cycle::collect::collect_under_pressure;
use crate::cycle::epoch;
use crate::cycle::mark::{TRAVERSAL_AGE_THRESHOLD, take_edges_pruned};
use crate::cycle::queue::verdicts::{Verdict, discard_standing_verdicts};
use crate::cycle::queue::{
    candidate_count, deferred_count, deferred_turnover_mirror, refill_spares,
    release_queue_segments, reoffer_deferred_if_epoch_moved,
};
use crate::cycle::testing::{move_prop, ring_with_a_spare_property, stamp_of};
use crate::refcount::{
    entity_refcount, is_registered_candidate, mutator_flags, read_maturation_stamp,
};
use crate::test_support::block_kind_and_used;

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
    // refused spare's fallback and a different case's subject
    // (`dev/DECISIONS.md`, "the deferred lane is a side exit of the compaction pass, and a
    // refused spare sends the root back to the active lane").
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

/// A thread whose active lane is empty re-offers its deferred lane at the next
/// poll, without waiting for a turnover. The clock is the collecting thread's
/// own and it moves only at a commit of that thread's own collection; a
/// collection needs a root in the active lane, so a thread that deferred its
/// last root would hold every slot of that lane until its exit — the whole
/// recall the deferral buys, taken for ever rather than for an epoch
/// (`crate::cycle::queue::reoffer_deferred_when_nothing_else_stands`).
///
/// The same fixture as the case above, driven by the production poll instead
/// of by a reading handed to the re-offer.
#[test]
fn an_idle_thread_reoffers_its_deferred_lane_at_the_next_poll() {
    let _g = test_guard();
    release_queue_segments();
    let _epoch = epoch::pin(1);
    DESTRUCTOR_RUNS.store(0, Ordering::Relaxed);

    let node = node_class("IdleReofferedNode", counting_destructor as *const ());
    let mut arena = Arena::new();
    let members = unsafe { ring(&mut arena, [node, node]) };
    let keeper = {
        let mut context = LLContext { arena: &mut arena };
        unsafe {
            new_constructed(
                &mut context,
                keeper_class("IdleReofferedKeeper"),
                MemoryCategory::GcHeap,
            )
        }
    };
    unsafe { store_prop(&mut arena, keeper, prop_offset(0), members[0]) };
    assert!(crate::cycle::queue::refill_spares());
    assert_eq!(
        unsafe { collect_with_a_reference_taken_mid_trace(&mut arena, keeper, members[0]) },
        0,
        "the reference the store took holds the whole ring"
    );
    assert_eq!(deferred_count(), 2);
    assert_eq!(candidate_count(), 0, "nothing stands in the active lane");

    unsafe {
        assert!(ll_release(keeper as *mut RcHeader));
        ll_object_die(keeper);
    }
    assert_eq!(deferred_count(), 2, "the ring is garbage no lane offers");

    let mirror = deferred_turnover_mirror();
    assert_eq!(
        epoch::turnovers_of(crate::cycle::epoch::commits()),
        epoch::turnovers_of(mirror),
        "no turnover stands between the deferral and the poll"
    );
    assert_eq!(
        unsafe { crate::gc::ll_gc_maybe_collect() },
        2,
        "the poll re-offered the lane and the collection it armed freed the ring"
    );
    assert_eq!(deferred_count(), 0);
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
/// The member is matured under a keeper by `TRAVERSAL_AGE_THRESHOLD`
/// collections with the spare cells empty, so each close's deferral falls
/// back to the active lane and the same root is offered again — the refused
/// spare's fallback
/// (`dev/DECISIONS.md`, "the deferred lane is a side exit of the compaction
/// pass, and a refused spare sends the root back to the active lane"),
/// standing in for the fresh root a real population would meet the component
/// through; the stamp is the same, because the unit stamped is the component
/// (`crate::cycle::maturation`). The cells are refilled before the collection
/// after that, which stops at the member and defers the root for real, and
/// the reading the case is about is then made from the shape it is ordinary
/// in: a garbage ring outside whose member points into this one, the ring's
/// own root standing in the deferred lane where the deferring reading left it.
/// The outside ring dies at that reading and this one does not, which is the
/// recall the prune costs.
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
    for age in 1..=TRAVERSAL_AGE_THRESHOLD {
        assert_eq!(
            unsafe { ll_gc_collect_cycles() },
            0,
            "the keeper holds the ring"
        );
        assert_eq!(unsafe { stamp_of(member) }, (0, age));
        assert_eq!(
            take_edges_pruned(),
            0,
            "below the threshold the member is descended into"
        );
        assert_eq!(
            candidate_count(),
            1,
            "with no spare cell the close keeps the root in the active lane"
        );
        assert_eq!(deferred_count(), 0);
    }

    assert!(refill_spares());
    assert_eq!(
        unsafe { ll_gc_collect_cycles() },
        0,
        "the keeper still holds the ring"
    );
    assert_eq!(
        take_edges_pruned(),
        1,
        "at the threshold the member is not descended into"
    );
    assert_eq!(candidate_count(), 0);
    assert_eq!(
        deferred_count(),
        1,
        "the reading that stopped at the member deferred the root"
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
        "the ring's root stands where the deferring reading left it"
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

/// A registered object read live through its keeper, deferred, and then
/// killed by the keeper's death: a completed death whose only record stands
/// in the deferred lane. Answers the object and its block's occupancy as the
/// death left it.
///
/// The keeper is of another size class than the object, so that its own
/// return at its death moves no figure of the object's block.
///
/// # Safety
/// As `new_constructed`: `arena` is this thread's, under the pool's guard.
unsafe fn a_dead_object_whose_record_is_deferred(
    arena: &mut Arena,
    name: &str,
) -> (*mut Object, u32) {
    let node = node_class(&format!("{name}Node"), counting_destructor as *const ());
    let keeper_of_another_class = ClassBuilder::new(&format!("{name}Keeper"))
        .prop("held", true)
        .prop("second", true)
        .prop("third", true)
        .prop("fourth", true)
        .build();
    assert_ne!(
        crate::memory::heap::size_class_index(unsafe { (*node).object_size } as usize),
        crate::memory::heap::size_class_index(
            unsafe { (*keeper_of_another_class).object_size } as usize
        ),
        "the keeper's return must not move the object's block"
    );
    let (held, keeper) = {
        let mut context = LLContext { arena: &mut *arena };
        unsafe {
            (
                new_constructed(&mut context, node, MemoryCategory::GcHeap),
                new_constructed(
                    &mut context,
                    keeper_of_another_class,
                    MemoryCategory::GcHeap,
                ),
            )
        }
    };
    unsafe {
        store_prop(arena, keeper, prop_offset(0), held);
        assert!(!ll_release(held as *mut RcHeader), "the keeper holds it");
    }
    assert_eq!(
        candidate_count(),
        1,
        "the release registered the held object"
    );

    assert!(refill_spares());
    assert_eq!(unsafe { ll_gc_collect_cycles() }, 0, "the keeper holds it");
    assert_eq!(candidate_count(), 0);
    assert_eq!(deferred_count(), 1, "read live, the record is deferred");

    let (_, used_before_the_death) = block_kind_and_used(held as usize);
    unsafe {
        assert!(ll_release(keeper as *mut RcHeader));
        ll_object_die(keeper);
    }
    assert_eq!(
        unsafe { slot_state(held as *mut RcHeader) },
        SlotState::DeadInPlace
    );
    let (_, used) = block_kind_and_used(held as usize);
    assert_eq!(
        used, used_before_the_death,
        "the free of a registered entity handed its slot to nobody"
    );
    assert_eq!(deferred_count(), 1);
    (held, used)
}

/// A record the close deferred withholds its entity's slot for the whole of
/// its wait: `ll_free` reads the standing candidate bit and hands nothing
/// back, the ordinary close reads the deferred lane not at all, and so does
/// the retirement pass. The slot comes back at the first close after the
/// turnover's re-offer, which is the bound `crate::cycle::queue` states for
/// the lane; the other end of the interval is the case below.
///
/// The instrument is the block's occupancy, which a return lowers and a
/// withheld free leaves alone (`test_support::block_kind_and_used`).
#[test]
fn a_deferred_record_withholds_its_dead_slot_until_the_close_after_the_reoffer() {
    let _g = test_guard();
    release_queue_segments();
    let _epoch = epoch::pin(1);

    let mut arena = Arena::new();
    let (held, used) = unsafe { a_dead_object_whose_record_is_deferred(&mut arena, "Withheld") };

    assert_eq!(unsafe { ll_gc_collect_cycles() }, 0, "nothing is offered");
    assert_eq!(
        block_kind_and_used(held as usize).1,
        used,
        "a close with nothing offered reads the deferred lane not at all"
    );
    assert_eq!(deferred_count(), 1);

    let mirror = deferred_turnover_mirror();
    assert!(reoffer_deferred_if_epoch_moved(epoch::one_turnover_past(
        mirror
    )));
    assert_eq!(
        candidate_count(),
        1,
        "the dead record is back in the active lane"
    );
    assert_eq!(
        unsafe { ll_gc_collect_cycles() },
        0,
        "a completed death is retired at the close, not collected"
    );
    assert_eq!(candidate_count(), 0);
    assert_eq!(
        block_kind_and_used(held as usize).1,
        used - 1,
        "the close after the re-offer handed the slot back"
    );
}

/// The earlier end of the interval: a pressure collection whose reading is
/// live defers its batch, and that deferral sweeps the lane before the batch
/// joins it, so a dead record already standing there gives its slot back
/// there (`crate::cycle::queue::defer_candidates`). The ordinary close never
/// reaches this arm.
///
/// The pressure path commits only a harvested list, so the live reading is
/// staged as the first cases stage theirs: a ring the trace proposes and a
/// reference taken before the counts are read. The ring and its keeper are
/// of another size class than the dead object, so that no allocation of
/// theirs lands in its block.
#[test]
fn a_pressure_collections_deferral_sweeps_a_dead_record_out_of_the_lane() {
    let _g = test_guard();
    release_queue_segments();
    let _epoch = epoch::pin(1);

    let mut arena = Arena::new();
    let (held, used) = unsafe { a_dead_object_whose_record_is_deferred(&mut arena, "Swept") };

    let wide = |name: &str| {
        ClassBuilder::new(name)
            .prop("next", true)
            .prop("second", true)
            .prop("third", true)
            .prop("fourth", true)
            .build()
    };
    let node = wide("SweptRingNode");
    let members = unsafe { ring(&mut arena, [node, node]) };
    let keeper = {
        let mut context = LLContext { arena: &mut arena };
        unsafe {
            new_constructed(
                &mut context,
                wide("SweptRingKeeper"),
                MemoryCategory::GcHeap,
            )
        }
    };
    assert_eq!(candidate_count(), 2);
    assert_eq!(block_kind_and_used(held as usize).1, used);

    assert!(refill_spares());
    let _race = InjectedVerdictRace::arm(&mut arena, keeper, members[0]);
    assert_eq!(
        unsafe { collect_under_pressure() },
        0,
        "the reference the store took holds the ring"
    );
    assert_eq!(candidate_count(), 0);
    assert_eq!(
        deferred_count(),
        2,
        "the ring's records joined the lane, and the dead one left it"
    );
    assert_eq!(
        block_kind_and_used(held as usize).1,
        used - 1,
        "the deferral's sweep handed the slot back"
    );

    // The ring goes back the way the first case's does.
    unsafe {
        assert!(ll_release(keeper as *mut RcHeader));
        ll_object_die(keeper);
    }
    let mirror = deferred_turnover_mirror();
    assert!(reoffer_deferred_if_epoch_moved(epoch::one_turnover_past(
        mirror
    )));
    assert_eq!(unsafe { ll_gc_collect_cycles() }, 2);
}

/// A pressure collection whose harvest is torn down defers the verdict
/// standing in P at the commit count its own reading saw, which is the count
/// an R-side deferral of the same collection records and one short of the
/// count its close leaves behind.
///
/// **The two sides cannot be read off one lane.** A mirror is written where
/// the deferred lane goes from empty to occupied
/// (`crate::cycle::queue::defer_candidates`), and a collection defers out of R
/// before its close disposes of P, so a case that defers one of each reads the
/// R side's count and nothing of the P side's. This case leaves R's roots to
/// the teardown and defers out of P alone.
///
/// The crossing is what tells the two readings apart: the counter stands one
/// short of the turnover, so the commit this collection closes crosses it, and
/// the later count puts the root a whole epoch out — the root would wait for
/// 64 collections of this thread's instead of being offered to the next poll.
#[test]
fn a_pressure_collection_defers_its_verdict_at_the_count_its_reading_saw() {
    let _g = test_guard();
    release_queue_segments();
    discard_standing_verdicts();

    let mut arena = Arena::new();
    // Registered first and posted out of R before the ring below is built, so
    // the stand-in's batch takes this root and the harvest reads the ring.
    let node = node_class("VerdictMirrorNode", counting_destructor as *const ());
    let keeper_class = ClassBuilder::new("VerdictMirrorKeeper")
        .prop("held", true)
        .build();
    let (root, keeper) = {
        let mut context = LLContext { arena: &mut arena };
        unsafe {
            (
                new_constructed(&mut context, node, MemoryCategory::GcHeap),
                new_constructed(&mut context, keeper_class, MemoryCategory::GcHeap),
            )
        }
    };
    unsafe {
        store_prop(&mut arena, keeper, prop_offset(0), root);
        assert!(!ll_release(root as *mut RcHeader), "the keeper holds it");
    }
    assert_eq!(candidate_count(), 1);
    assert_eq!(stand_in_posts(1, Verdict::ReadLive), Posted::Batch(1));
    assert_eq!(candidate_count(), 0, "the batch took the root out of R");

    let ring_node = node_class("VerdictMirrorRingNode", counting_destructor as *const ());
    let _ring = unsafe { ring(&mut arena, [ring_node, ring_node]) };
    assert_eq!(candidate_count(), 2, "the ring is what the harvest reads");

    assert!(refill_spares());
    epoch::close_commits_to_one_short_of_the_turnover();
    let reading = epoch::commits();

    assert_eq!(
        unsafe { collect_under_pressure() },
        2,
        "the ring nothing holds was torn down"
    );
    assert_eq!(
        epoch::commits(),
        reading + 1,
        "the teardown's commit closed one"
    );
    assert_eq!(deferred_count(), 1, "the root read live went to the lane");
    assert_eq!(
        deferred_turnover_mirror(),
        reading,
        "the mirror is the count the reading saw, not the one the close left"
    );

    assert!(
        reoffer_deferred_if_epoch_moved(epoch::commits()),
        "the turnover the reading stood in closed with that very commit"
    );
    assert_eq!(candidate_count(), 1, "the root is back in the active lane");

    unsafe {
        assert!(ll_release(keeper as *mut RcHeader));
        ll_object_die(keeper);
    }
    assert_eq!(
        unsafe { ll_gc_collect_cycles() },
        0,
        "a completed death is retired at the close, not collected"
    );
    assert_eq!(candidate_count(), 0);
}
