//! A root read live waits longer the more live readings it has survived
//! (`wait-by-readings`, `dev/plans/S65.md`, S65.42): 1, 3 and 7 epoch turns in
//! three lanes; a turn the X arm
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
/// time.
pub(super) const WAITS: &[u32] = &[1, 3, 7, 7];

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
/// the root goes back after 1, 3 and then 7 turns. Red with every lane
/// waiting one turn, and with
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

/// A ring that dies silently behind a root of the longest wait, the third
/// lane, is read `gap` turns after the
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
    // which registers nothing: the root is a candidate already. The take
    // stamped the core at that reading.
    a_reading();
    unsafe { ring.let_the_keeper_go(&mut arena) };
    let turns = gap;
    for _ in 0..turns {
        turn_this_threads_cell();
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

/// The collector's X arm notes its turn, and the arm of the proofs does not.
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

    record.note_epoch_work(0, 1);
    record.note_epoch_work(crate::cycle::epoch::SPENT_PER_PROOF, 0);
    let turnovers = record.turnovers();
    advance_the_epoch_if_due(record, now);
    assert_eq!(record.turnovers(), turnovers + 1, "the proofs turned it");
    assert_eq!(record.x_turns(), x_turns, "and noted no X turn");

    advance_the_epoch_if_due(record, now + epoch_interval().as_nanos() as u64);
    assert_eq!(record.turnovers(), turnovers + 2, "X turned it");
    assert_eq!(record.x_turns(), x_turns.wrapping_add(1), "and noted it");
}
