//! A root the collector reads live under `hold-by-generation`: of the first
//! generation — its stamp carries no age, or one of the batch's own epoch —
//! the collector stamps the root alone and keeps it in the chain's ready
//! part, where the next batches read it again beside R, its core unlisted so
//! that a later batch of the same epoch can see it die; once it has outlived
//! an epoch it goes to the waiting part as the build without the feature
//! sends it (`dev/plans/S65.md`, S65.32). The mutator does nothing for
//! either.
//!
//! The collector is a thread of the case's that serves this thread's record,
//! as in `the_batch`.

use super::generation_fixtures::{
    KeptRing, MEMBERS, a_kept_ring, a_nonzero_epoch, free_the_ring, member_class,
};
use super::the_batch::served_by_a_collector;
use super::*;
use crate::cycle::chain::testing::{dismantle_this_threads, roots_of_this_threads};
use crate::cycle::queue::candidate_count;
use crate::cycle::queue::verdicts::verdict_count;
use crate::cycle::testing::{ring, stamp_of};
use crate::cycle::token::{FREE, state};
use crate::gc::ll_gc_maybe_collect;
use crate::memory::arena::Arena;
use crate::refcount::RcHeader;

fn reset() {
    dismantle_this_threads();
    reset_lanes();
    let _ = testing::take_generations();
}

fn root_of(ring: &KeptRing) -> *mut RcHeader {
    ring.root() as *mut RcHeader
}

/// Whether no member of `ring` but its root carries an age.
fn the_core_is_unstamped(ring: &KeptRing) -> bool {
    ring.members[1..]
        .iter()
        .all(|&member| unsafe { stamp_of(member) }.1 == 0)
}

/// One serve that completed its batch, and the generation figures it left.
fn a_complete_serve() -> testing::Generations {
    let _ = testing::take_generations();
    assert!(matches!(
        served_by_a_collector(),
        Served::Batch { complete: true, .. }
    ));
    testing::take_generations()
}

/// One serve, reading the batch the collector traced.
fn a_traced_serve() -> testing::TracedBatch {
    testing::read_traced_batches(true);
    let _ = served_by_a_collector();
    let traced = testing::take_traced_batches();
    testing::read_traced_batches(false);
    assert_eq!(traced.len(), 1, "one batch");
    traced[0]
}

/// A young ring the batch read live waits in the ready part, its root alone
/// stamped by the collector, and the batch leaves the mutator nothing: the
/// token goes back `FREE` and P stays empty. Red with the young root sent
/// where a root read live goes without the feature, the waiting part.
#[test]
fn a_young_root_read_live_waits_in_the_ready_part_with_its_root_alone_stamped() {
    let _g = test_guard();
    reset();
    let epoch = a_nonzero_epoch();
    let mut arena = Arena::new();
    let ring = unsafe { a_kept_ring(&mut arena, "HoldYoung") };

    let generations = a_complete_serve();
    assert_eq!(generations.posted_first, 1);
    assert_eq!(
        roots_of_this_threads(),
        (vec![root_of(&ring)], Vec::new()),
        "in the ready part, not the waiting one"
    );
    assert_eq!(state(unsafe { &*record() }.token.read()), FREE);
    assert_eq!(verdict_count(), 0, "the mutator is owed nothing");
    assert_eq!(unsafe { stamp_of(ring.root()) }, (epoch, 1));
    assert!(the_core_is_unstamped(&ring));

    unsafe { free_the_ring(&mut arena, ring) };
    reset();
}

/// A young ring let go inside its epoch is met whole by the next batch and
/// proposed, and the poll frees it, although the first batch published its
/// live list beside a garbage ring it proposed. Red with the young part's
/// core listed: the take stamps the young ring's members and the second
/// batch prunes at them.
#[test]
fn a_young_ring_let_go_inside_its_epoch_is_proposed_by_the_next_batch() {
    let _g = test_guard();
    reset();
    let _ = a_nonzero_epoch();
    let mut arena = Arena::new();
    let garbage_class = member_class("HoldGarbage");
    let _garbage = unsafe { ring(&mut arena, [garbage_class, garbage_class]) };
    let mut young = unsafe { a_kept_ring(&mut arena, "HoldYoungLetGo") };

    let _ = a_complete_serve();
    assert_eq!(
        unsafe { ll_gc_maybe_collect() },
        2,
        "the garbage ring over P, the list taken"
    );
    assert!(
        the_core_is_unstamped(&young),
        "the young core was not listed"
    );

    unsafe { young.let_the_keeper_go(&mut arena) };
    let second = a_traced_serve();
    assert_eq!(second.edges_pruned, 0, "the re-read met the whole ring");
    assert_eq!(unsafe { ll_gc_maybe_collect() }, MEMBERS);
    assert_eq!(roots_of_this_threads(), (Vec::new(), Vec::new()));
    assert_eq!(candidate_count(), 0);
    reset();
}

/// A kept ring stays in the ready part through every batch of the epoch it
/// was first read in and goes to the waiting part at the first batch after
/// the turn. Red with the generation read as the first whatever the stamp:
/// the root stays in the ready part after the turn.
#[test]
fn a_young_root_read_again_after_the_turn_goes_to_the_waiting_part() {
    let _g = test_guard();
    reset();
    let _ = a_nonzero_epoch();
    let mut arena = Arena::new();
    let ring = unsafe { a_kept_ring(&mut arena, "HoldAcrossTheTurn") };

    for _ in 0..2 {
        assert_eq!(a_complete_serve().posted_first, 1);
        assert_eq!(roots_of_this_threads(), (vec![root_of(&ring)], Vec::new()));
    }

    crate::cycle::epoch::turn_this_threads_cell();
    let generations = a_complete_serve();
    assert_eq!(
        (generations.posted_first, generations.posted_second),
        (0, 1)
    );
    assert_eq!(roots_of_this_threads(), (Vec::new(), vec![root_of(&ring)]));

    unsafe { free_the_ring(&mut arena, ring) };
    reset();
}

/// A young root that a part whose root has outlived an epoch meets goes to
/// the waiting part with the old root: the old part lists its core, the
/// young ring's rows among them, so a re-read in the epoch would only prune
/// at them. Red with the young root kept by its generation alone, which puts
/// it in the ready part.
#[test]
fn a_young_root_an_old_part_meets_goes_to_the_waiting_part() {
    let _g = test_guard();
    reset();
    let _ = a_nonzero_epoch();
    let mut arena = Arena::new();
    let old = unsafe { a_kept_ring(&mut arena, "HoldOldHolder") };
    unsafe { crate::cycle::testing::as_of_the_second_generation(root_of(&old)) };
    let young = unsafe { a_kept_ring(&mut arena, "HoldHeldYoung") };
    unsafe {
        crate::test_support::store_prop(
            &mut arena,
            old.members[1],
            crate::test_support::prop_offset(1),
            young.root(),
        );
    }

    let generations = a_complete_serve();
    assert_eq!(
        (
            generations.posted_second,
            generations.posted_in_an_old_core,
            generations.posted_first
        ),
        (1, 1, 0)
    );
    let (ready, mut waiting) = roots_of_this_threads();
    waiting.sort();
    let mut both = vec![root_of(&old), root_of(&young)];
    both.sort();
    assert_eq!((ready, waiting), (Vec::new(), both));

    unsafe {
        free_the_ring(&mut arena, young);
        free_the_ring(&mut arena, old);
    }
    reset();
}
