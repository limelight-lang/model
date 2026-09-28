//! A crossing of the entity heap's growth hands back every lane a turn old,
//! and under the chain owes its waiting part the release
//! (`release-on-heap-growth`, `dev/plans/S65.md`, S65.43): the heap's blocks
//! owned grow a quarter over their low-water level since the last release,
//! and at least one block. A ring dead behind a root of a long wait is then
//! freed at the next batch instead of at the wait's end; without growth the
//! waits stand.

use super::generation_fixtures::{KeptRing, a_kept_ring, a_nonzero_epoch, free_the_ring};
use super::the_batch::served_by_a_collector;
use super::the_waits_by_readings::{a_reading, a_reading_and_its_wait};
use super::*;
use crate::cycle::epoch::turn_this_threads_cell;
use crate::cycle::queue::{
    candidate_count, deferred_count, owned_low, reoffer_deferred_if_epoch_moved, set_owned_low,
};
use crate::gc::ll_gc_maybe_collect;
use crate::memory::arena::Arena;
use crate::memory::heap::entity_blocks_owned;

/// Entity slots kept only to grow this thread's entity heap by whole blocks,
/// of a size class no other case allocates: a class the parallel suite's
/// exited threads left abandoned blocks of would be adopted with their
/// objects in them, and such a block never empties.
struct Fillers {
    slots: Vec<*mut u8>,
}

/// The fillers' size, the second largest class.
const FILLER_BYTES: usize = 7168;

impl Fillers {
    fn new() -> Self {
        Self { slots: Vec::new() }
    }

    /// Allocate until the entity heap owns `blocks` blocks: each draw moves
    /// the count by one, so the count stops at `blocks` exactly.
    fn grow_to(&mut self, blocks: u32) {
        while entity_blocks_owned() < blocks {
            let slot = unsafe { crate::memory::heap::entity_alloc(FILLER_BYTES) };
            assert!(!slot.is_null(), "a filler");
            self.slots.push(slot);
        }
    }

    /// Give every filler back; nothing was published into them.
    fn free_all(&mut self) {
        for slot in self.slots.drain(..) {
            unsafe { crate::memory::stdapi::free_unpublished(slot) };
        }
    }
}

impl Drop for Fillers {
    fn drop(&mut self) {
        self.free_all();
    }
}

/// The count at which a crossing fires over the low-water level `low`.
fn the_mark_over(low: u32) -> u32 {
    low + (low / 4).max(1)
}

/// Arm the mark at the heap's count as it stands, and answer the count.
fn arm_the_mark() -> u32 {
    let owned = entity_blocks_owned();
    set_owned_low(owned);
    owned
}

/// The third reading's root, whose ring is let go: in D it waits in the lane
/// of seven turns, under the chain in the chain's waiting part. Answers the
/// ring with its keeper gone.
fn a_ring_dead_behind_the_longest_wait(arena: &mut Arena, name: &str) -> KeptRing {
    let mut ring = unsafe { a_kept_ring(arena, name) };
    for _ in 0..2 {
        let _ = a_reading_and_its_wait();
    }

    a_reading();
    unsafe { ring.let_the_keeper_go(arena) };
    ring
}

/// The collector's next batch and the poll that takes it free the ring.
fn the_next_batch_frees(mut ring: KeptRing, arena: &mut Arena) {
    assert!(matches!(
        served_by_a_collector(),
        Served::Batch { complete: true, .. }
    ));
    assert_eq!(
        unsafe { ll_gc_maybe_collect() },
        ring.members.len(),
        "the ring is traced and freed"
    );
    assert_eq!((candidate_count(), deferred_count()), (0, 0));

    ring.members.clear();
    unsafe { free_the_ring(arena, ring) };
}

/// Where the release put the root: in D back in R, under the chain its waiting
/// part owed to the next grant.
fn the_root_is_released() {
    #[cfg(not(feature = "collector-chain"))]
    assert_eq!(
        (candidate_count(), deferred_count()),
        (1, 0),
        "the lane went back"
    );
    #[cfg(feature = "collector-chain")]
    assert!(
        crate::cycle::chain::is_due(unsafe { &*record() }, 0, u64::MAX),
        "the chain is owed its release"
    );
}

/// Case a: a ring dead behind a root of the longest wait is freed at the batch
/// after the heap's growth crosses the mark, one turn after the reading. Red
/// with the count unread at the poll, and under the chain with the chain not
/// owed.
#[test]
fn a_ring_dead_behind_the_longest_wait_is_freed_after_the_heap_grows() {
    let _g = test_guard();
    #[cfg(feature = "collector-chain")]
    crate::cycle::chain::testing::dismantle_this_threads();
    reset_lanes();
    let _ = a_nonzero_epoch();
    let mut arena = Arena::new();
    let mut fillers = Fillers::new();
    let ring = a_ring_dead_behind_the_longest_wait(&mut arena, "GrowthLongest");

    let low = arm_the_mark();
    turn_this_threads_cell();
    assert!(
        !reoffer_deferred_if_epoch_moved(),
        "a turn is short of the wait"
    );
    fillers.grow_to(the_mark_over(low));
    assert_eq!(unsafe { ll_gc_maybe_collect() }, 0);
    the_root_is_released();

    the_next_batch_frees(ring, &mut arena);
    fillers.free_all();
}

/// Case b: the same behind the wait of three turns, the second reading's lane
/// in both schemes.
#[test]
fn a_ring_dead_behind_the_wait_of_three_is_freed_after_the_heap_grows() {
    let _g = test_guard();
    #[cfg(feature = "collector-chain")]
    crate::cycle::chain::testing::dismantle_this_threads();
    reset_lanes();
    let _ = a_nonzero_epoch();
    let mut arena = Arena::new();
    let mut fillers = Fillers::new();
    let mut ring = unsafe { a_kept_ring(&mut arena, "GrowthThree") };
    let _ = a_reading_and_its_wait();
    a_reading();
    unsafe { ring.let_the_keeper_go(&mut arena) };

    let low = arm_the_mark();
    turn_this_threads_cell();
    assert!(
        !reoffer_deferred_if_epoch_moved(),
        "a turn is short of the wait"
    );
    fillers.grow_to(the_mark_over(low));
    assert_eq!(unsafe { ll_gc_maybe_collect() }, 0);
    assert_eq!(
        (candidate_count(), deferred_count()),
        (1, 0),
        "the lane went back"
    );

    the_next_batch_frees(ring, &mut arena);
    fillers.free_all();
    #[cfg(feature = "collector-chain")]
    crate::cycle::chain::testing::dismantle_this_threads();
}

/// Case c: growth one block short of the mark releases nothing, and without
/// growth the root of the longest wait goes back at its seventh turn. Red with
/// a crossing at every block.
#[cfg(not(feature = "collector-chain"))]
#[test]
fn growth_short_of_the_mark_leaves_the_waits_standing() {
    use super::the_waits_by_readings::WAITS;

    let _g = test_guard();
    reset_lanes();
    let _ = a_nonzero_epoch();
    let mut arena = Arena::new();
    let mut fillers = Fillers::new();
    // A heap of eight blocks or more, so that a quarter of it is two or more.
    fillers.grow_to(entity_blocks_owned().max(8));
    let ring = a_ring_dead_behind_the_longest_wait(&mut arena, "GrowthShort");

    let low = arm_the_mark();
    fillers.grow_to(the_mark_over(low) - 1);
    for turn in 1..=WAITS[2] {
        turn_this_threads_cell();
        assert_eq!(unsafe { ll_gc_maybe_collect() }, 0);
        let expected = if turn < WAITS[2] { (0, 1) } else { (1, 0) };
        assert_eq!(
            (candidate_count(), deferred_count()),
            expected,
            "turn {turn}: the lane goes back at its own wait alone"
        );
    }

    the_next_batch_frees(ring, &mut arena);
    fillers.free_all();
}

/// Case d: a crossing in the turn of the lane's fill releases nothing and
/// keeps the level, so the next turn's first poll, with no further growth,
/// crosses again and releases. Red with the one-turn gap dropped, and with the
/// level re-armed at a crossing that held the lane back.
#[cfg(not(feature = "collector-chain"))]
#[test]
fn a_crossing_in_the_readings_turn_releases_nothing() {
    let _g = test_guard();
    reset_lanes();
    let _ = a_nonzero_epoch();
    let mut arena = Arena::new();
    let mut fillers = Fillers::new();
    let ring = a_ring_dead_behind_the_longest_wait(&mut arena, "GrowthSameTurn");

    let low = arm_the_mark();
    fillers.grow_to(the_mark_over(low));
    assert_eq!(unsafe { ll_gc_maybe_collect() }, 0);
    assert_eq!(
        (candidate_count(), deferred_count()),
        (0, 1),
        "the lane filled in this turn stays"
    );

    assert_eq!(owned_low(), low, "the level is kept");

    turn_this_threads_cell();
    assert_eq!(unsafe { ll_gc_maybe_collect() }, 0);
    assert_eq!(
        (candidate_count(), deferred_count()),
        (1, 0),
        "a turn later the same growth releases it"
    );

    the_next_batch_frees(ring, &mut arena);
    fillers.free_all();
}

/// Case e: a crossing re-arms the mark at the count it read, so growth short
/// of a quarter over that count crosses nothing, however far it stands over
/// the mark before. Red with the level unmoved at the release.
#[test]
fn the_mark_is_re_armed_at_the_crossing() {
    let _g = test_guard();
    reset_lanes();
    let mut fillers = Fillers::new();
    fillers.grow_to(entity_blocks_owned().max(8));
    let low = arm_the_mark();
    let crossings = testing::this_threads_heap_growth_crossings();

    let crossed_at = the_mark_over(low);
    fillers.grow_to(crossed_at);
    assert_eq!(unsafe { ll_gc_maybe_collect() }, 0);
    assert_eq!(testing::this_threads_heap_growth_crossings(), crossings + 1);
    assert_eq!(owned_low(), crossed_at, "re-armed at the count");

    fillers.grow_to(the_mark_over(crossed_at) - 1);
    assert_eq!(unsafe { ll_gc_maybe_collect() }, 0);
    assert_eq!(
        testing::this_threads_heap_growth_crossings(),
        crossings + 1,
        "short of a quarter over the new level"
    );
    fillers.grow_to(the_mark_over(crossed_at));
    assert_eq!(unsafe { ll_gc_maybe_collect() }, 0);
    assert_eq!(testing::this_threads_heap_growth_crossings(), crossings + 2);

    fillers.free_all();
    reset_lanes();
}

/// Case f: the low-water level follows the count down as blocks go back to
/// the pool, so growth by a quarter of the new level crosses. Red with the
/// level never lowered.
#[test]
fn the_low_water_level_follows_the_heap_down() {
    let _g = test_guard();
    reset_lanes();
    let mut fillers = Fillers::new();
    fillers.grow_to(entity_blocks_owned() + 16);
    let high = arm_the_mark();

    fillers.free_all();
    let low = entity_blocks_owned();
    assert!(low + 8 < high, "the emptied blocks went back to the pool");
    assert_eq!(unsafe { ll_gc_maybe_collect() }, 0);
    assert_eq!(owned_low(), low, "the level follows the count down");

    let crossings = testing::this_threads_heap_growth_crossings();
    fillers.grow_to(the_mark_over(low));
    assert_eq!(unsafe { ll_gc_maybe_collect() }, 0);
    assert_eq!(
        testing::this_threads_heap_growth_crossings(),
        crossings + 1,
        "a quarter over the lowered level crosses"
    );

    fillers.free_all();
    reset_lanes();
}

/// The root of a kept ring in the chain's waiting part after one reading, the
/// mutator's lanes empty.
#[cfg(feature = "collector-chain")]
fn a_root_waiting_in_the_chain(arena: &mut Arena, name: &str) -> KeptRing {
    use super::the_waits_by_readings::root_of;
    use crate::cycle::chain::testing::roots_of_this_threads;

    let ring = unsafe { a_kept_ring(arena, name) };
    unsafe { crate::cycle::testing::as_of_the_second_generation(root_of(&ring)) };
    assert!(matches!(
        served_by_a_collector(),
        Served::Batch { complete: true, .. }
    ));
    assert_eq!(roots_of_this_threads(), (Vec::new(), vec![root_of(&ring)]));
    assert_eq!(deferred_count(), 0, "the lanes are empty");
    ring
}

/// Case h: under the chain a crossing with both lanes empty owes the chain its
/// release. Red with the lanes tested before the count.
#[cfg(feature = "collector-chain")]
#[test]
fn a_crossing_with_the_lanes_empty_owes_the_chain() {
    let _g = test_guard();
    crate::cycle::chain::testing::dismantle_this_threads();
    reset_lanes();
    let _ = a_nonzero_epoch();
    let mut arena = Arena::new();
    let mut fillers = Fillers::new();
    let ring = a_root_waiting_in_the_chain(&mut arena, "GrowthChainOwed");

    let record = unsafe { &*record() };
    turn_this_threads_cell();
    assert!(!crate::cycle::chain::is_due(record, 0, u64::MAX));
    let low = arm_the_mark();
    fillers.grow_to(the_mark_over(low));
    assert_eq!(unsafe { ll_gc_maybe_collect() }, 0);
    assert!(
        crate::cycle::chain::is_due(record, 0, u64::MAX),
        "the crossing owes the chain"
    );

    unsafe { free_the_ring(&mut arena, ring) };
    fillers.free_all();
    crate::cycle::chain::testing::dismantle_this_threads();
}

/// Case i: under the chain a waiting part whose oldest block entered in this
/// turn is not owed and keeps the level, so the next turn's first poll owes
/// it with no further growth. Red with the chain's one-turn gap dropped.
#[cfg(feature = "collector-chain")]
#[test]
fn a_waiting_part_entered_this_turn_is_not_owed() {
    let _g = test_guard();
    crate::cycle::chain::testing::dismantle_this_threads();
    reset_lanes();
    let _ = a_nonzero_epoch();
    let mut arena = Arena::new();
    let mut fillers = Fillers::new();
    let ring = a_root_waiting_in_the_chain(&mut arena, "GrowthChainThisTurn");

    let low = arm_the_mark();
    fillers.grow_to(the_mark_over(low));
    assert_eq!(unsafe { ll_gc_maybe_collect() }, 0);
    let record = unsafe { &*record() };
    assert!(
        !crate::cycle::chain::is_due(record, 0, u64::MAX),
        "entered in this turn, not owed"
    );
    assert_eq!(owned_low(), low, "the level is kept");
    turn_this_threads_cell();
    assert_eq!(unsafe { ll_gc_maybe_collect() }, 0);
    assert!(
        crate::cycle::chain::is_due(record, 0, u64::MAX),
        "a turn later the same growth owes it"
    );

    unsafe { free_the_ring(&mut arena, ring) };
    fillers.free_all();
    crate::cycle::chain::testing::dismantle_this_threads();
}
