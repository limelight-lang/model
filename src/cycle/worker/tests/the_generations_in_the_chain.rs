//! A root the collector reads live under `hold-by-generation`: once it has
//! outlived an epoch — its entry carries the lane's mark, which the deferred
//! lane writes on the entries it hands back at a turn and the chain on every
//! entry it takes — it goes into the chain's
//! waiting part as without the feature;
//! a younger one goes on into P as a root the chain has no block for goes,
//! and the mutator's disposition defers it (`dev/plans/S65.md`, S65.32).
//!
//! The collector is a thread of the case's that serves this thread's record,
//! as in `the_batch`.

use super::generation_fixtures::{KeptRing, a_kept_ring, a_nonzero_epoch, free_the_ring};
use super::the_batch::served_by_a_collector;
use super::*;
use crate::cycle::chain::testing::{dismantle_this_threads, roots_of_this_threads};
use crate::cycle::queue::verdicts::{Verdict, standing_verdicts};
use crate::cycle::queue::{candidate_count, deferred_count};
use crate::cycle::testing::stamp_of;
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

/// One serve that completed its batch, and the generation figures it left.
fn a_complete_serve() -> testing::Generations {
    let _ = testing::take_generations();
    assert!(matches!(
        served_by_a_collector(),
        Served::Batch { complete: true, .. }
    ));
    testing::take_generations()
}

/// A young root read live goes on into P, not into the chain, and the
/// mutator's disposition defers it with its core stamped, the root itself
/// not listed. Red with the generation not asked: the root goes into the
/// chain's waiting part.
#[test]
fn a_young_root_read_live_goes_on_into_p_and_is_deferred() {
    let _g = test_guard();
    reset();
    let epoch = a_nonzero_epoch();
    let mut arena = Arena::new();
    let ring = unsafe { a_kept_ring(&mut arena, "HoldYoung") };

    assert_eq!(a_complete_serve().posted_first, 1);
    assert_eq!(roots_of_this_threads(), (Vec::new(), Vec::new()));
    assert_eq!(
        standing_verdicts(),
        vec![(root_of(&ring), Verdict::ReadLive)]
    );

    assert_eq!(unsafe { ll_gc_maybe_collect() }, 0);
    assert_eq!((candidate_count(), deferred_count()), (0, 1));
    assert!(
        ring.members[1..]
            .iter()
            .all(|&member| unsafe { stamp_of(member) } == (epoch, 1)),
        "the list stamped the core at the take"
    );
    assert_eq!(unsafe { stamp_of(ring.root()) }.1, 0, "the part's root");

    unsafe { free_the_ring(&mut arena, ring) };
    reset();
}

/// A root that has outlived an epoch goes into the chain's waiting part and
/// leaves P empty. Red with every root read live sent on into P.
#[test]
fn a_root_that_outlived_an_epoch_goes_into_the_waiting_part() {
    let _g = test_guard();
    reset();
    let _ = a_nonzero_epoch();
    let mut arena = Arena::new();
    let ring = unsafe { a_kept_ring(&mut arena, "HoldOld") };
    unsafe { crate::cycle::testing::as_of_the_second_generation(root_of(&ring)) };

    assert_eq!(a_complete_serve().posted_second, 1);
    assert_eq!(roots_of_this_threads(), (Vec::new(), vec![root_of(&ring)]));
    assert!(standing_verdicts().is_empty(), "nothing in P");

    unsafe { free_the_ring(&mut arena, ring) };
    reset();
}

/// A young root deferred by the disposition comes back into R at the turn,
/// and the batch that reads it then finds it of the second generation and
/// puts it in the waiting part. Red with the generation not asked after the
/// turn: the root goes on into P again.
#[test]
fn a_young_root_read_again_after_the_turn_goes_into_the_waiting_part() {
    let _g = test_guard();
    reset();
    let _ = a_nonzero_epoch();
    let mut arena = Arena::new();
    let ring = unsafe { a_kept_ring(&mut arena, "HoldAcrossTheTurn") };
    let _ = a_complete_serve();
    assert_eq!(unsafe { ll_gc_maybe_collect() }, 0);
    assert_eq!(deferred_count(), 1);

    crate::cycle::epoch::turn_this_threads_cell();
    crate::cycle::queue::reoffer_deferred_candidates();
    assert_eq!((candidate_count(), deferred_count()), (1, 0));
    let generations = a_complete_serve();
    assert_eq!(
        (generations.posted_first, generations.posted_second),
        (0, 1)
    );
    assert_eq!(roots_of_this_threads(), (Vec::new(), vec![root_of(&ring)]));

    unsafe { free_the_ring(&mut arena, ring) };
    reset();
}

/// Raise a recall from the collector's thread at the start of its next trace,
/// so that the pass before the parts reads it and every root is `Unwalked`.
fn recall_at_the_next_trace() {
    let token = unsafe { &raw const (*record()).token } as usize;
    testing::at_the_start_of_the_next_trace(Box::new(move || {
        unsafe { &*(token as *const crate::cycle::token::TraceToken) }.recall_for_test(true)
    }));
}

/// One serve under a recall raised at its trace's start, the recall cleared
/// after it.
fn a_recalled_serve() {
    recall_at_the_next_trace();
    assert!(matches!(served_by_a_collector(), Served::Batch { .. }));
    unsafe { &*record() }.token.recall_for_test(false);
}

/// A young root the recall left unread goes into P as without the chain, and
/// the disposition writes it back into R: it waits there, not behind the
/// ready part, and the recall does not promote it. Red with an unwalked root
/// kept in the ready part whatever its generation.
#[test]
fn a_young_root_the_recall_left_unread_goes_into_p() {
    let _g = test_guard();
    reset();
    let _ = a_nonzero_epoch();
    let mut arena = Arena::new();
    let ring = unsafe { a_kept_ring(&mut arena, "HoldUnwalkedYoung") };

    a_recalled_serve();
    assert_eq!(roots_of_this_threads(), (Vec::new(), Vec::new()));
    assert_eq!(
        standing_verdicts(),
        vec![(root_of(&ring), Verdict::Unwalked)]
    );
    assert_eq!(unsafe { ll_gc_maybe_collect() }, 0);
    assert_eq!((candidate_count(), deferred_count()), (1, 0));
    assert_eq!(a_complete_serve().posted_first, 1, "read young");

    unsafe { free_the_ring(&mut arena, ring) };
    reset();
}

/// A root of the second generation the recall left unread goes into the
/// ready part, and read live from there goes into the waiting part. Red with
/// the ready part's entry pushed without the lane's mark.
#[test]
fn an_old_root_the_recall_left_unread_keeps_its_generation() {
    let _g = test_guard();
    reset();
    let _ = a_nonzero_epoch();
    let mut arena = Arena::new();
    let ring = unsafe { a_kept_ring(&mut arena, "HoldUnwalkedOld") };
    unsafe { crate::cycle::testing::as_of_the_second_generation(root_of(&ring)) };

    a_recalled_serve();
    assert_eq!(roots_of_this_threads(), (vec![root_of(&ring)], Vec::new()));
    assert!(standing_verdicts().is_empty(), "nothing in P");
    assert_eq!(a_complete_serve().posted_second, 1);
    assert_eq!(roots_of_this_threads(), (Vec::new(), vec![root_of(&ring)]));

    unsafe { free_the_ring(&mut arena, ring) };
    reset();
}

/// A root the chain held, spliced into R as the exit and a collection under
/// pressure splice it, keeps its generation: read live again, it goes back
/// into the waiting part. Red with the chain's entries pushed bare.
#[test]
fn a_chained_root_spliced_into_r_goes_back_into_the_chain() {
    let _g = test_guard();
    reset();
    let _ = a_nonzero_epoch();
    let mut arena = Arena::new();
    let ring = unsafe { a_kept_ring(&mut arena, "HoldSpliced") };
    unsafe { crate::cycle::testing::as_of_the_second_generation(root_of(&ring)) };
    assert_eq!(a_complete_serve().posted_second, 1);

    unsafe {
        let _claim = crate::cycle::token::HeldToken::take();
        crate::cycle::chain::splice_this_threads_chain_into_r(true)
    };
    assert_eq!(roots_of_this_threads(), (Vec::new(), Vec::new()));
    assert_eq!(candidate_count(), 1);
    let generations = a_complete_serve();
    assert_eq!(
        (generations.posted_first, generations.posted_second),
        (0, 1)
    );
    assert_eq!(roots_of_this_threads(), (Vec::new(), vec![root_of(&ring)]));

    unsafe { free_the_ring(&mut arena, ring) };
    reset();
}
