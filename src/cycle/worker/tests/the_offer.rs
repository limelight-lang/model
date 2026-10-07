//! The mutator's offer under `recycler-over-counts` (`crate::cycle::offer`;
//! `dev/design/recycler-over-counts.md`, §5f): when the poll offers — R at
//! the threshold, a ring standing below it for the interval, a lane merged
//! since the merges were last accounted for — and what the offer carries;
//! the returns an offer withholds, the mark that withdraws it, and the
//! offer after a withdrawal waiting for a round.
//!
//! The offers here are made by [`crate::cycle::offer::offer_at`], which
//! stands whoever stands to take them: the cases take them on threads of
//! their own, or withdraw them.

use super::*;
use crate::cycle::deferred_slot_reuse::{DEATHS_MARK, foreign_withheld_count};
use crate::cycle::offer::{note_a_withdrawal_at_a_mark, offer_at, offer_between};
use crate::cycle::token::{FREE, OFFERED, RECALL_NONE, state};
use crate::gc::ll_gc_maybe_collect;
use crate::memory::arena::Arena;
use std::time::Duration;

/// The threshold the cases offer at: a ring of two stands under it.
const THRESHOLD: usize = 4;

fn token() -> &'static crate::cycle::token::TraceToken {
    unsafe { &(*record()).token }
}

/// Register `rings` garbage rings of two members each on this thread.
fn garbage_rings(arena: &mut Arena, rings: usize, name: &str) {
    let class = node_class(name);
    for _ in 0..rings {
        let _ = unsafe { crate::cycle::testing::ring(arena, [class, class]) };
    }
}

/// This thread's record with no standing instant and every merge accounted
/// for, and a round's clock stamped, as the elder's round leaves them.
fn a_record_with_nothing_standing() -> &'static MutatorRecord {
    let mutator = unsafe { &*record() };
    mutator.note_standing_since(0);
    mutator.note_merges_seen(mutator.merges());
    testing::stamp_the_round_clock();
    mutator
}

/// Withdraw the offer standing on this thread's byte, as the thread's own
/// take does, and collect what the case left.
fn withdraw_and_collect() {
    assert_eq!(token().withdraw_the_offer(FREE), Ok(()));
    let _ = unsafe { crate::gc::ll_gc_collect_cycles() };
    reset_lanes();
}

/// R at the threshold is offered at once, with R's count as the ceiling and
/// the frame one past this thread's window; the instant is cleared, as a
/// ring the threshold reaches counts no interval.
#[test]
fn r_at_the_threshold_is_offered_with_its_count_and_the_next_frame() {
    let _g = test_guard();
    reset_lanes();
    let mut arena = Arena::new();
    let mutator = a_record_with_nothing_standing();
    garbage_rings(&mut arena, THRESHOLD / 2, "OfferAtThresholdNode");
    mutator.note_standing_since(1);
    let window = crate::refcount::the_next_window();

    assert!(unsafe { offer_at(THRESHOLD) }, "R at the threshold");
    assert_eq!(state(token().read()), OFFERED);
    assert_eq!(token().window(), window, "the next frame");
    assert_eq!(crate::refcount::this_threads_window(), window, "and opened");
    assert_eq!(token().ceiling(), THRESHOLD, "R's count");
    assert_eq!(mutator.standing_since(), 0, "the instant cleared");
    withdraw_and_collect();
}

/// R read empty offers nothing, clears the instant and accounts for every
/// merge, as the round's reading of an empty ring did.
#[test]
fn an_empty_r_offers_nothing_and_clears_the_instant() {
    let _g = test_guard();
    reset_lanes();
    let mutator = a_record_with_nothing_standing();
    mutator.note_standing_since(1);
    let window = crate::refcount::this_threads_window();

    assert!(!unsafe { offer_at(1) });
    assert_eq!(token().read(), FREE);
    assert_eq!(mutator.standing_since(), 0);
    assert_eq!(
        crate::refcount::this_threads_window(),
        window,
        "no frame spent"
    );
}

/// A ring below the threshold is not offered at the poll that first reads it
/// standing, which stamps the instant on the round's clock, and is offered at
/// the first poll an interval after it.
#[test]
fn a_ring_below_the_threshold_is_offered_an_interval_after_it_first_stood() {
    let _g = test_guard();
    reset_lanes();
    let _interval = StandingInterval::of(Duration::from_millis(1));
    let mut arena = Arena::new();
    let mutator = a_record_with_nothing_standing();
    garbage_rings(&mut arena, 1, "OfferAfterIntervalNode");

    assert!(
        !unsafe { offer_at(THRESHOLD) },
        "the first poll offers nothing"
    );
    let stood_since = mutator.standing_since();
    assert_ne!(stood_since, 0, "and stamps the instant");

    std::thread::sleep(Duration::from_millis(3));
    assert!(
        !unsafe { offer_at(THRESHOLD) },
        "the poll reads no clock: an interval on the round's clock is needed"
    );
    testing::stamp_the_round_clock();
    assert!(unsafe { offer_at(THRESHOLD) }, "offered an interval later");
    assert_eq!(mutator.standing_since(), stood_since, "the instant stands");
    withdraw_and_collect();
}

/// A lane merged since the merges were last accounted for is offered below
/// the threshold at once, with no interval to stand.
#[test]
fn a_merged_lane_is_offered_below_the_threshold() {
    let _g = test_guard();
    reset_lanes();
    let _interval = StandingInterval::of(Duration::from_secs(60));
    let mut arena = Arena::new();
    let mutator = a_record_with_nothing_standing();
    garbage_rings(&mut arena, 1, "OfferMergedNode");

    assert!(!unsafe { offer_at(THRESHOLD) }, "standing its interval");
    mutator.note_merges_seen(mutator.merges().wrapping_sub(1));
    assert!(unsafe { offer_at(THRESHOLD) }, "a merge not yet seen");
    withdraw_and_collect();
}

/// R at the threshold and below the bound is not offered at the poll that
/// first reads it there, and is offered at the first poll after it has held
/// the threshold the short interval on the thread's own clock, with no round
/// to stamp the round's clock (Edmond, 2026-10-07: the offer at the bound,
/// repaired).
#[test]
fn r_at_the_threshold_below_the_bound_is_offered_after_the_short_interval() {
    let _g = test_guard();
    reset_lanes();
    let _interval = StandingInterval::of(Duration::from_secs(60));
    let mut arena = Arena::new();
    let _ = a_record_with_nothing_standing();
    garbage_rings(&mut arena, THRESHOLD / 2, "OfferAfterShortNode");
    let short = Duration::from_millis(2);

    assert!(
        !unsafe { offer_between(THRESHOLD, 2 * THRESHOLD, short) },
        "the first poll at the threshold offers nothing"
    );
    std::thread::sleep(short * 2);
    assert!(
        unsafe { offer_between(THRESHOLD, 2 * THRESHOLD, short) },
        "offered once the short interval has passed"
    );
    assert_eq!(token().ceiling(), THRESHOLD, "R's count");
    withdraw_and_collect();
}

/// R at the bound is offered at once, with no interval to stand.
#[test]
fn r_at_the_bound_is_offered_at_once() {
    let _g = test_guard();
    reset_lanes();
    let _interval = StandingInterval::of(Duration::from_secs(60));
    let mut arena = Arena::new();
    let _ = a_record_with_nothing_standing();
    garbage_rings(&mut arena, THRESHOLD, "OfferAtBoundNode");

    assert!(unsafe { offer_between(THRESHOLD, 2 * THRESHOLD, Duration::from_secs(60)) });
    assert_eq!(token().ceiling(), 2 * THRESHOLD, "R's count");
    withdraw_and_collect();
}

/// R below the threshold waits for the standing interval, not the short one.
#[test]
fn r_below_the_threshold_does_not_take_the_short_interval() {
    let _g = test_guard();
    reset_lanes();
    let _interval = StandingInterval::of(Duration::from_secs(60));
    let mut arena = Arena::new();
    let _ = a_record_with_nothing_standing();
    garbage_rings(&mut arena, 1, "OfferBelowShortNode");
    let short = Duration::from_millis(1);

    assert!(!unsafe { offer_between(THRESHOLD, 2 * THRESHOLD, short) });
    std::thread::sleep(short * 3);
    assert!(
        !unsafe { offer_between(THRESHOLD, 2 * THRESHOLD, short) },
        "under the threshold"
    );
    assert_eq!(token().read(), FREE);
    let _ = unsafe { crate::gc::ll_gc_collect_cycles() };
    reset_lanes();
}

/// No offer over a byte that is not `FREE`: the thread's own token taken, or
/// an offer standing.
#[test]
fn no_offer_over_a_byte_that_is_not_free() {
    let _g = test_guard();
    reset_lanes();
    let mut arena = Arena::new();
    let _ = a_record_with_nothing_standing();
    garbage_rings(&mut arena, THRESHOLD / 2, "OfferOverHeldNode");

    {
        let _held = crate::cycle::token::HeldToken::take();
        assert!(
            !unsafe { offer_at(THRESHOLD) },
            "the thread holds its token"
        );
    }
    assert!(unsafe { offer_at(THRESHOLD) });
    let window = token().window();
    assert!(!unsafe { offer_at(THRESHOLD) }, "an offer stands");
    assert_eq!(token().window(), window, "and keeps its frame");
    withdraw_and_collect();
}

/// After a stack's mark withdrew an offer, the next waits for a collector's
/// round to begin: an offer no collector is free to take is not made and
/// withdrawn at every poll.
#[test]
fn the_offer_after_a_withdrawal_at_a_mark_waits_for_a_round() {
    let _g = test_guard();
    reset_lanes();
    let mut arena = Arena::new();
    let _ = a_record_with_nothing_standing();
    garbage_rings(&mut arena, THRESHOLD / 2, "OfferPacedNode");

    note_a_withdrawal_at_a_mark();
    assert!(!unsafe { offer_at(THRESHOLD) }, "no round since");
    testing::begin_a_round();
    assert!(unsafe { offer_at(THRESHOLD) }, "a round began");
    withdraw_and_collect();
}

/// `count` dead arrays of no storage, freed as a teardown frees them, each
/// return made or withheld as the byte reads at its free.
fn free_dead_slots(count: usize) -> Vec<*mut u8> {
    (0..count)
        .map(|_| {
            let slot = unsafe { crate::memory::heap::entity_alloc(64) };
            assert!(!slot.is_null(), "the heap served");
            let header = slot as *mut RcHeader;
            unsafe {
                header.write(RcHeader::new(
                    crate::refcount::MemoryCategory::GcHeap,
                    crate::refcount::EntityKind::Array.to_flags(),
                ));
                crate::refcount::set_header_refcount(header, 0);
                crate::memory::stdapi::ll_free(slot);
            }
            slot
        })
        .collect()
}

/// A death under a standing offer is withheld: a collector may take the
/// offer and read its slot at any moment, so the slot is not handed out
/// again. The offer's withdrawal gives it back at the next free.
#[test]
fn a_death_under_an_offer_is_withheld_and_its_slot_not_reused() {
    let _g = test_guard();
    reset_lanes();
    let mut arena = Arena::new();
    let _ = a_record_with_nothing_standing();
    garbage_rings(&mut arena, THRESHOLD / 2, "OfferWithholdsNode");
    assert!(unsafe { offer_at(THRESHOLD) });

    let dead = free_dead_slots(1)[0];
    assert_eq!(foreign_withheld_count(), 1, "the death waits");
    let next = unsafe { crate::memory::heap::entity_alloc(64) };
    assert_ne!(next, dead, "its slot is not handed out under the offer");
    unsafe { crate::memory::stdapi::ll_free(next) };

    assert_eq!(token().withdraw_the_offer(FREE), Ok(()));
    let _ = free_dead_slots(1);
    assert_eq!(foreign_withheld_count(), 0, "the free under FREE gave back");
    let _ = unsafe { crate::gc::ll_gc_collect_cycles() };
    reset_lanes();
}

/// The deaths' mark under a standing offer withdraws it to `FREE`, with no
/// wait: the returns go back at the next free, and the next offer waits for a
/// round. Red where the mark recalls only a collector's take, which leaves an
/// offer no collector takes withholding without bound.
#[test]
fn the_mark_under_an_offer_withdraws_it() {
    let _g = test_guard();
    reset_lanes();
    let mut arena = Arena::new();
    let _ = a_record_with_nothing_standing();
    garbage_rings(&mut arena, THRESHOLD / 2, "OfferMarkNode");
    assert!(unsafe { offer_at(THRESHOLD) });
    let waits = token().waits();

    let _ = free_dead_slots(DEATHS_MARK - 1);
    assert_eq!(state(token().read()), OFFERED, "short of the mark");
    let _ = free_dead_slots(1);
    assert_eq!(token().read(), FREE, "the mark withdrew the offer");
    assert_eq!(token().recall_level(), RECALL_NONE);
    assert_eq!(token().waits(), waits, "with no wait");
    assert!(
        !unsafe { offer_at(THRESHOLD) },
        "the next waits for a round"
    );

    let _ = free_dead_slots(1);
    assert_eq!(foreign_withheld_count(), 0, "the next free gave back");
    assert_eq!(unsafe { ll_gc_maybe_collect() }, 0);
    testing::begin_a_round();
    let _ = unsafe { crate::gc::ll_gc_collect_cycles() };
    reset_lanes();
}
