//! A root read live waits longer the more live readings it has survived
//! (`wait-by-readings`, `dev/plans/S65.md`, S65.42): 1, 3 and 7 batch turns in
//! D's three lanes, 1 and 3 in HG's two with the chain at 7; a turn the X arm
//! made releases every wait; the stamp's epoch is sixteen wide, so a silently
//! dead ring read after a wait of eight or twelve turns is traced and freed,
//! where two bits would read its members' stamps as current and prune at
//! them.
//!
//! The residual exposure, named here and not tested as freed: a gap of
//! sixteen turns between two readings of one root meets the epoch again, and
//! a ring whose root is read at that gap each time is pruned at each reading,
//! as today's build is at a gap of four.

use super::generation_fixtures::{KeptRing, a_kept_ring, a_nonzero_epoch, free_the_ring};
use super::the_batch::served_by_a_collector;
use super::*;
use crate::cycle::epoch::turn_this_threads_cell;
use crate::cycle::queue::{
    candidate_count, defer_candidates, deferred_count, read_batch, reoffer_deferred_if_epoch_moved,
};
use crate::gc::ll_gc_maybe_collect;
use crate::memory::arena::Arena;
use crate::refcount::{RcHeader, survived_readings};

pub(super) fn root_of(ring: &KeptRing) -> *mut RcHeader {
    ring.root() as *mut RcHeader
}

/// The waits of a root read live for the first, second, third and fourth
/// time in this build's lanes: under the chain the third reading keeps it in
/// the chain's waiting part instead.
#[cfg(not(feature = "collector-chain"))]
pub(super) const WAITS: &[u32] = &[1, 3, 7, 7];
#[cfg(feature = "collector-chain")]
pub(super) const WAITS: &[u32] = &[1, 3];

/// One batch of the case's collector over this thread's record, and the poll
/// that takes what it posted: a root read live young is deferred at the take.
pub(super) fn a_reading() {
    assert!(matches!(
        served_by_a_collector(),
        Served::Batch { complete: true, .. }
    ));
    assert_eq!(unsafe { ll_gc_maybe_collect() }, 0, "the ring is held");
}

/// [`a_reading`] that defers the root, and the turns its lane then waits
/// before a poll hands it back.
pub(super) fn a_reading_and_its_wait() -> u32 {
    a_reading();
    assert_eq!((candidate_count(), deferred_count()), (0, 1));
    let mut turns = 0;
    loop {
        turn_this_threads_cell();
        turns += 1;
        if reoffer_deferred_if_epoch_moved() {
            assert_eq!((candidate_count(), deferred_count()), (1, 0));
            return turns;
        }

        assert!(turns < 16, "the lane went back within the longest wait");
    }
}

/// Each live reading raises the root's count, and the count picks the lane:
/// the root goes back after 1, 3 and then 7 turns (3 under the chain, whose
/// own wait takes the third). Red with every lane waiting one turn, and with
/// the count left unraised.
#[test]
fn a_root_waits_longer_after_each_live_reading() {
    let _g = test_guard();
    reset_lanes();
    let _ = a_nonzero_epoch();
    let mut arena = Arena::new();
    let ring = unsafe { a_kept_ring(&mut arena, "WaitsByReadings") };

    let waits: Vec<u32> = (0..WAITS.len()).map(|_| a_reading_and_its_wait()).collect();
    assert_eq!(waits, WAITS);
    assert_eq!(
        unsafe { survived_readings(root_of(&ring)) } as usize,
        WAITS.len().min(3),
        "counted, and saturated at three"
    );

    unsafe { free_the_ring(&mut arena, ring) };
}

/// A turn the X arm made hands every lane back at the next poll, the longest
/// wait included. Red with the X mirror unread.
#[test]
fn an_x_turn_releases_every_lane() {
    let _g = test_guard();
    reset_lanes();
    let _ = a_nonzero_epoch();
    let mut arena = Arena::new();
    let ring = unsafe { a_kept_ring(&mut arena, "ReleasedByX") };
    for _ in 1..WAITS.len() {
        let _ = a_reading_and_its_wait();
    }

    // The last reading a lane takes puts the root in the longest lane this
    // build keeps.
    a_reading();
    assert_eq!(deferred_count(), 1);
    assert!(!reoffer_deferred_if_epoch_moved(), "no turn yet");
    unsafe { &*record() }.note_an_x_turn();
    assert!(reoffer_deferred_if_epoch_moved(), "the X turn released it");
    assert_eq!((candidate_count(), deferred_count()), (1, 0));

    unsafe { free_the_ring(&mut arena, ring) };
}

/// A lane whose mirror stands ahead of the collector's byte — the byte a
/// racing advance stored last — reads as not due, where an unsigned
/// difference would read 255 turns late. Red with the difference unsigned.
#[test]
fn a_byte_behind_the_mirror_reads_as_not_due() {
    let _g = test_guard();
    reset_lanes();
    let _ = a_nonzero_epoch();
    let mut arena = Arena::new();
    let ring = unsafe { a_kept_ring(&mut arena, "ByteBehind") };

    let turnovers = unsafe { &*record() }.turnovers();
    defer_candidates(read_batch(), turnovers + 1);
    assert_eq!(deferred_count(), 1);
    assert!(
        !reoffer_deferred_if_epoch_moved(),
        "one turn behind the mirror"
    );
    turn_this_threads_cell();
    assert!(!reoffer_deferred_if_epoch_moved(), "level with it");
    turn_this_threads_cell();
    assert!(reoffer_deferred_if_epoch_moved(), "one turn past it");

    unsafe { free_the_ring(&mut arena, ring) };
}

/// A ring that dies silently behind a root of the longest wait — the third
/// lane, or under the chain its waiting part — is read `gap` turns after the
/// reading that last stamped its members, and freed:
/// the members' stamps are of an epoch sixteen wide, so a gap of eight or
/// twelve does not read them as current. Red on a two-bit epoch, where the
/// reading prunes at the stamped member and defers the root again.
fn a_dead_ring_read_after(gap: u32, name: &str) {
    let _g = test_guard();
    reset_lanes();
    let _ = a_nonzero_epoch();
    let mut arena = Arena::new();
    let mut ring = unsafe { a_kept_ring(&mut arena, name) };
    for _ in 0..2 {
        let _ = a_reading_and_its_wait();
    }

    // The third reading keeps the root at the longest wait; the keeper goes,
    // which registers nothing: the root is a candidate already. In D the
    // take stamped the core at that reading. Under the chain the reading
    // posted nothing into P, so no take stamped it, and the stamps are the
    // second reading's, a lane's wait of three turns before.
    a_reading();
    unsafe { ring.let_the_keeper_go(&mut arena) };
    #[cfg(not(feature = "collector-chain"))]
    let turns = gap;
    #[cfg(feature = "collector-chain")]
    let turns = gap - WAITS[1];
    for _ in 0..turns {
        turn_this_threads_cell();
    }
    // A gap shorter than the chain's wait is a burst of batch turns and then
    // a turn of the X arm, which releases the waiting part whole: its last
    // turn is noted as the X arm's, as `advance_the_epoch_if_due` notes it.
    #[cfg(feature = "collector-chain")]
    if u64::from(turns) < crate::cycle::chain::CHAIN_WAIT {
        unsafe { &*record() }.note_an_x_turn();
    }
    let _ = reoffer_deferred_if_epoch_moved();
    assert!(matches!(
        served_by_a_collector(),
        Served::Batch { complete: true, .. }
    ));
    assert_eq!(
        unsafe { ll_gc_maybe_collect() },
        ring.members.len(),
        "the ring is traced, not pruned at its members' stamps, and freed"
    );
    assert_eq!((candidate_count(), deferred_count()), (0, 0));

    ring.members.clear();
    unsafe { free_the_ring(&mut arena, ring) };
}

#[test]
fn a_ring_dead_behind_an_old_root_is_freed_after_eight_turns() {
    a_dead_ring_read_after(8, "DeadAfterEight");
}

#[test]
fn a_ring_dead_behind_an_old_root_is_freed_after_twelve_turns() {
    a_dead_ring_read_after(12, "DeadAfterTwelve");
}

/// Under the chain a root is young for its first two live readings, which
/// its collector posts into P and the mutator defers, and goes into the
/// chain's waiting part at the third, where it waits seven turns. Red with a
/// root read live once taken for old, and with the chain's wait one turn.
#[cfg(feature = "hold-by-generation")]
#[test]
fn a_root_goes_into_the_chain_at_its_third_live_reading() {
    use crate::cycle::chain::testing::{dismantle_this_threads, roots_of_this_threads};

    let _g = test_guard();
    dismantle_this_threads();
    reset_lanes();
    let _ = a_nonzero_epoch();
    let mut arena = Arena::new();
    let ring = unsafe { a_kept_ring(&mut arena, "ChainAtTheThird") };

    for _ in WAITS {
        let _ = a_reading_and_its_wait();
        assert_eq!(roots_of_this_threads(), (Vec::new(), Vec::new()), "young");
    }

    assert!(matches!(
        served_by_a_collector(),
        Served::Batch { complete: true, .. }
    ));
    assert_eq!(
        roots_of_this_threads(),
        (Vec::new(), vec![root_of(&ring)]),
        "the third reading keeps it in the waiting part"
    );
    let record = unsafe { &*record() };
    for _ in 1..7 {
        turn_this_threads_cell();
        assert!(!crate::cycle::chain::is_due(record, 0, u64::MAX));
    }
    turn_this_threads_cell();
    assert!(crate::cycle::chain::is_due(record, 0, u64::MAX));

    unsafe { free_the_ring(&mut arena, ring) };
    dismantle_this_threads();
}

/// Under the chain a turn the X arm made owes the chain's whole waiting part
/// to the next grant, whatever its blocks' stamps. Red with the flag unread.
#[cfg(feature = "hold-by-generation")]
#[test]
fn an_x_turn_makes_the_chains_waiting_part_due() {
    use crate::cycle::chain::testing::{dismantle_this_threads, roots_of_this_threads};

    let _g = test_guard();
    dismantle_this_threads();
    reset_lanes();
    let _ = a_nonzero_epoch();
    let mut arena = Arena::new();
    let ring = unsafe { a_kept_ring(&mut arena, "ChainReleasedByX") };
    unsafe { crate::cycle::testing::as_of_the_second_generation(root_of(&ring)) };
    assert!(matches!(
        served_by_a_collector(),
        Served::Batch { complete: true, .. }
    ));
    assert_eq!(roots_of_this_threads(), (Vec::new(), vec![root_of(&ring)]));

    let record = unsafe { &*record() };
    assert!(!crate::cycle::chain::is_due(record, 0, u64::MAX));
    record.note_an_x_turn();
    assert!(crate::cycle::chain::is_due(record, 0, u64::MAX));
    // A recall that stops the expiry before its first block leaves the
    // release owed. Red with the owed release dropped at the take.
    unsafe { crate::cycle::chain::expire(record, || true) };
    assert_eq!(roots_of_this_threads(), (Vec::new(), vec![root_of(&ring)]));
    assert!(
        crate::cycle::chain::is_due(record, 0, u64::MAX),
        "the stopped release is still owed"
    );
    unsafe { crate::cycle::chain::expire(record, || false) };
    assert_eq!(
        roots_of_this_threads(),
        (vec![root_of(&ring)], Vec::new()),
        "the whole waiting part is ready"
    );

    unsafe { free_the_ring(&mut arena, ring) };
    dismantle_this_threads();
}

/// The collector's X arm notes its turn, and the arm of 64 batches does not.
/// Red with the arms swapped, and with every advance noted as the X arm's.
#[test]
fn only_the_x_arm_notes_an_x_turn() {
    let _g = test_guard();
    reset_lanes();
    let record = unsafe { &*record() };
    while record.take_new_life() {}
    let now = serve_clock_now();
    record.note_advanced_at(now);
    let x_turns = record.x_turns();

    for _ in 0..crate::cycle::epoch::BATCHES_PER_EPOCH {
        record.note_batch();
    }
    let turnovers = record.turnovers();
    advance_the_epoch_if_due(record, now);
    assert_eq!(record.turnovers(), turnovers + 1, "the batches turned it");
    assert_eq!(record.x_turns(), x_turns, "and noted no X turn");

    advance_the_epoch_if_due(record, now + epoch_interval().as_nanos() as u64);
    assert_eq!(record.turnovers(), turnovers + 2, "X turned it");
    assert_eq!(record.x_turns(), x_turns.wrapping_add(1), "and noted it");
}
