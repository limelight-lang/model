//! A collector cap of zero, set before any work: the dial reaches zero, the
//! elder's rounds keep the epoch clock and request no token, and where a
//! round would have taken R — at the threshold, standing past the interval,
//! or merged into — the elder asks the mutator to collect it in line, which
//! under any other cap the poll leaves to the collector.

use super::*;
use crate::cycle::queue::{candidate_count, deferred_count};
use crate::cycle::token::{ASKED, FREE};
use crate::gc::{ll_gc_collect_cycles, ll_gc_maybe_collect};
use crate::memory::arena::Arena;
use std::time::Duration;

/// The cap at zero while the guard stands, and the crate's default again when
/// it drops, on a panic as on a return: the cap is the process's, and a case
/// that failed with it at zero would have every later case's polls collect.
pub(super) struct CapAtZero;

impl CapAtZero {
    pub(super) fn set() -> Self {
        crate::gc::ll_gc_set_collector_cap(0);
        Self
    }
}

impl Drop for CapAtZero {
    fn drop(&mut self) {
        set_collector_cap(DEFAULT_COLLECTOR_CAP);
    }
}

/// Register `rings` garbage rings of two members each on this thread: two
/// entries of R per ring, and nothing live.
fn garbage_rings(arena: &mut Arena, rings: usize, name: &str) {
    let class = node_class(name);
    for _ in 0..rings {
        let _ = unsafe { crate::cycle::testing::ring(arena, [class, class]) };
    }
}

/// Rings whose entries fill R to one pair short of the threshold.
const RINGS_SHORT_OF_THE_THRESHOLD: usize = SOFT_THRESHOLD / 2 - 1;

#[test]
fn the_dial_reaches_zero_and_clamps_at_the_maximum() {
    let _g = test_guard();
    let _cap = CapAtZero::set();
    assert!(
        collectors_capped_at_zero(),
        "zero is a cap, not a clamp to one"
    );

    crate::gc::ll_gc_set_collector_cap(MAX_COLLECTORS + 3);
    assert_eq!(collector_cap(), MAX_COLLECTORS);
    assert!(!collectors_capped_at_zero());
}

/// The elder's ask for `record`, made from a thread of the case's as the
/// round makes it from the elder's, against the clock now.
fn asked_by_the_elder(record: *mut MutatorRecord, threshold: usize) -> Served {
    let sent = Sent(record);
    std::thread::spawn(move || unsafe {
        ask_for_an_in_line_collection(sent.into_inner(), threshold, serve_clock_now())
    })
    .join()
    .expect("the ask ran")
}

/// This thread's record with no standing instant and every merge accounted
/// for, so that a ring below the threshold reads as one the round leaves.
fn a_record_with_nothing_standing() -> *mut MutatorRecord {
    let record = record();
    let mutator = unsafe { &*record };
    mutator.note_standing_since(0);
    mutator.note_merges_seen(mutator.merges());
    record
}

/// Under a cap of zero the elder's reading of R at the threshold asks for an
/// in-line collection, the ask over an empty P, and the mutator's next poll
/// collects R whole; one pair of entries short of it the elder asks nothing.
/// Red on the reading of the ask that armed the collection over P alone,
/// which found P empty and freed nothing, and on the elder that asked at no
/// count.
#[test]
fn the_elder_under_cap_zero_asks_for_r_at_the_threshold_and_the_poll_collects_it() {
    let _g = test_guard();
    reset_lanes();
    let _cap = CapAtZero::set();
    let mut arena = Arena::new();
    let record = a_record_with_nothing_standing();

    garbage_rings(&mut arena, RINGS_SHORT_OF_THE_THRESHOLD, "ShortOfZeroNode");
    assert_eq!(candidate_count(), SOFT_THRESHOLD - 2);
    assert_eq!(
        asked_by_the_elder(record, SOFT_THRESHOLD),
        Served::Idle,
        "below the threshold the elder asks nothing"
    );
    assert_eq!(unsafe { &*record }.token.read(), FREE);
    assert_eq!(unsafe { ll_gc_maybe_collect() }, 0);

    garbage_rings(&mut arena, 1, "AtZeroNode");
    assert_eq!(asked_by_the_elder(record, SOFT_THRESHOLD), Served::Asked);
    assert_eq!(unsafe { &*record }.token.read(), ASKED, "over an empty P");
    assert_eq!(
        asked_by_the_elder(record, SOFT_THRESHOLD),
        Served::Posted,
        "a second ask finds the first standing"
    );
    assert_eq!(
        unsafe { ll_gc_maybe_collect() },
        SOFT_THRESHOLD,
        "the poll collected every ring in line"
    );
    assert_eq!(candidate_count(), 0);
    assert_eq!(unsafe { &*record }.token.read(), FREE);
}

/// Under a cap of zero a ring standing below the threshold is asked for once
/// it has stood the interval, as the round would have taken it: the first
/// visit stamps the instant and asks nothing, a visit past the interval asks,
/// and the poll frees the ring. Red on the elder that asked at the threshold
/// alone, which left a quiet thread's garbage to its exit.
#[test]
fn a_ring_standing_below_the_threshold_under_cap_zero_is_asked_for_after_the_interval() {
    let _g = test_guard();
    reset_lanes();
    let _cap = CapAtZero::set();
    let _interval = StandingInterval::of(Duration::from_millis(1));
    let mut arena = Arena::new();
    let record = a_record_with_nothing_standing();

    garbage_rings(&mut arena, 2, "StandingAtZeroNode");
    assert_eq!(
        asked_by_the_elder(record, SOFT_THRESHOLD),
        Served::Idle,
        "the first visit stamps the instant"
    );
    std::thread::sleep(Duration::from_millis(5));
    assert_eq!(asked_by_the_elder(record, SOFT_THRESHOLD), Served::Asked);
    assert_eq!(unsafe { ll_gc_maybe_collect() }, 4);
    assert_ne!(
        unsafe { &*record }.standing_since(),
        0,
        "the ask restamped the instant, as a grant's release does"
    );
}

/// The ask is read on the slot free as at the poll: a death under it arms R
/// whole. Red on a reading that armed the collection over P there.
#[test]
fn a_slot_free_under_the_ask_arms_r_whole() {
    let _g = test_guard();
    reset_lanes();
    let _cap = CapAtZero::set();
    let mut arena = Arena::new();
    let record = a_record_with_nothing_standing();
    garbage_rings(&mut arena, SOFT_THRESHOLD / 2, "FreedUnderTheAskNode");
    assert_eq!(asked_by_the_elder(record, SOFT_THRESHOLD), Served::Asked);

    let lone = unsafe {
        super::the_batch::object(
            &mut arena,
            super::the_batch::keeper_class("LoneUnderTheAsk"),
        )
    };
    assert!(
        unsafe { crate::refcount::ll_release(lone.cast()) },
        "the death"
    );
    unsafe { crate::object::ll_object_die(lone) };
    assert_eq!(crate::gc::arming(), crate::gc::Arming::AllRoots);
    assert_eq!(unsafe { ll_gc_maybe_collect() }, SOFT_THRESHOLD);
}

/// An ask the mutator reads after the cap went back up is still a collection
/// over R whole: the byte says what was asked, not the cap at the reading.
#[test]
fn an_ask_read_after_the_cap_went_up_collects_r_whole() {
    let _g = test_guard();
    reset_lanes();
    let cap = CapAtZero::set();
    let mut arena = Arena::new();
    let record = a_record_with_nothing_standing();
    garbage_rings(&mut arena, SOFT_THRESHOLD / 2, "AskBeforeTheCapNode");
    assert_eq!(asked_by_the_elder(record, SOFT_THRESHOLD), Served::Asked);

    drop(cap);
    assert!(!collectors_capped_at_zero());
    assert_eq!(unsafe { ll_gc_maybe_collect() }, SOFT_THRESHOLD);
}

/// The same ring at the threshold under the default cap is the collector's:
/// the poll arms nothing and R stands for a round.
#[test]
fn a_poll_under_the_default_cap_leaves_r_at_the_threshold_to_the_collector() {
    let _g = test_guard();
    reset_lanes();
    let mut arena = Arena::new();

    garbage_rings(&mut arena, SOFT_THRESHOLD / 2, "DefaultCapNode");
    assert_eq!(unsafe { ll_gc_maybe_collect() }, 0);
    assert_eq!(candidate_count(), SOFT_THRESHOLD, "R stands for a round");
    assert_eq!(unsafe { ll_gc_collect_cycles() }, SOFT_THRESHOLD);
}

/// A live ring the in-line collection deferred, which then became garbage
/// behind its deferred roots: the lane occupied, R empty, and a poll before
/// the epoch's turn firing nothing.
fn a_ring_deferred_then_dropped(arena: &mut Arena, name: &str) {
    let ring = unsafe { crate::cycle::testing::long_ring(arena, node_class(name), 2) };
    unsafe { crate::refcount::ll_retain(ring[0].cast()) };
    assert_eq!(unsafe { ll_gc_collect_cycles() }, 0, "the ring is held");
    assert_ne!(deferred_count(), 0, "and the close deferred its roots");
    assert_eq!(candidate_count(), 0);

    assert!(!unsafe { crate::refcount::ll_release(ring[0].cast()) });
    assert_eq!(
        candidate_count(),
        0,
        "a member already a candidate registers nothing"
    );
    assert_eq!(
        unsafe { ll_gc_maybe_collect() },
        0,
        "the lane waits for the epoch's turn"
    );
}

/// Under a cap of zero a lane the poll merged back at the epoch's turn is
/// asked for at the elder's next visit, as a take would have taken it, and
/// the poll after the ask frees the ring. Red on the elder that asked for
/// nothing, which left the garbage behind a deferred root to a take no round
/// makes under the cap.
#[test]
fn a_merge_under_cap_zero_is_asked_for_and_collected() {
    let _g = test_guard();
    reset_lanes();
    let _cap = CapAtZero::set();
    let mut arena = Arena::new();
    let record = a_record_with_nothing_standing();
    a_ring_deferred_then_dropped(&mut arena, "MergedAtZeroNode");

    crate::cycle::epoch::turn_this_threads_cell();
    assert_eq!(unsafe { ll_gc_maybe_collect() }, 0, "the poll merges");
    assert_eq!(deferred_count(), 0);
    assert_eq!(asked_by_the_elder(record, SOFT_THRESHOLD), Served::Asked);
    assert_eq!(
        unsafe { ll_gc_maybe_collect() },
        2,
        "the poll after the ask freed the ring"
    );
    assert_eq!(candidate_count(), 0);
}

/// Under the default cap the same merge arms nothing: the merged roots stand
/// in R for the collector's next round.
#[test]
fn a_merge_under_the_default_cap_leaves_the_roots_to_the_collector() {
    let _g = test_guard();
    reset_lanes();
    let mut arena = Arena::new();
    a_ring_deferred_then_dropped(&mut arena, "MergedByDefaultNode");

    crate::cycle::epoch::turn_this_threads_cell();
    assert_eq!(unsafe { ll_gc_maybe_collect() }, 0);
    assert_eq!(deferred_count(), 0, "the poll merged the lane");
    assert_ne!(candidate_count(), 0, "and its roots stand in R");
    assert_eq!(unsafe { ll_gc_collect_cycles() }, 2);
}

/// The epoch interval of the elder case, and the crate's own again when the
/// guard drops.
struct EpochInterval;

impl EpochInterval {
    fn of(interval: Duration) -> Self {
        testing::advance_epochs_after(Some(interval));
        Self
    }
}

impl Drop for EpochInterval {
    fn drop(&mut self) {
        testing::advance_epochs_after(None);
    }
}

/// X for the elder case: short enough that two turns fit a case, long enough
/// that a round is not an advance every time.
const X: Duration = Duration::from_millis(20);

/// Under a cap of zero the elder, born by the ordinary path, advances the
/// epoch of a mutator whose R stands at the threshold, never requests its
/// token and asks it to collect: no grant and no batch, and the mutator's
/// polls free every ring. Red on the round that served every record at the
/// threshold whatever the cap, which granted and took the ring.
#[test]
fn the_elder_under_cap_zero_keeps_the_clock_and_takes_nothing() {
    let _g = test_guard();
    let _end = RetireOnDrop;
    let _cap = CapAtZero::set();
    let _x = EpochInterval::of(X);
    let freed = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mutator = Mutator::start_polling(std::sync::Arc::clone(&freed));
    let record = unsafe { &*mutator.record };
    let class = Sent(node_class("ElderAtZeroNode"));
    let held = mutator.run(move |arena| {
        let class = class.into_inner();
        for _ in 0..SOFT_THRESHOLD / 2 {
            let _ = unsafe { crate::cycle::testing::ring(arena, [class, class]) };
        }
        candidate_count()
    });
    assert_eq!(held, SOFT_THRESHOLD);

    let _ = testing::take_spawns();
    born_over(&[mutator.record]);
    testing::wait_between_rounds_for(Some(X / 4));
    let turnovers = record.turnovers();
    assert!(
        wait_until(|| record.turnovers() >= turnovers + 2, A_BIRTH),
        "the elder's rounds advanced the epoch twice"
    );
    assert!(
        wait_until(
            || freed.load(std::sync::atomic::Ordering::Relaxed) == SOFT_THRESHOLD,
            A_BIRTH
        ),
        "the mutator's polls collected every ring in line"
    );

    let outcomes = testing::take_outcomes();
    testing::retire();
    assert_eq!(outcomes.grants, 0, "no token was granted: {outcomes:?}");
    assert_eq!(outcomes.batches, 0);
    assert_eq!(outcomes.asked, 1, "one ask, and the poll collected over it");
    assert_eq!(record.token.read(), FREE);
    assert_eq!(testing::take_spawns(), 1, "and no sibling was born");
    assert_eq!(mutator.run(|_| candidate_count()), 0);
}
