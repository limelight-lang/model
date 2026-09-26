//! Completed deaths of candidates retired by their count: the free path
//! counts each death a queue entry withholds, and the one that reaches
//! [`DEATHS_TO_RETIRE`] arms the poll for a retirement pass over R, which
//! runs under the thread's own token at the next clean poll, traces nothing
//! and gives the withheld slots back. A ring at the collector's threshold
//! raises the collector's signal instead, the deferred lane waits for its
//! turnover, and a pass that returned little doubles the next count.

use super::*;
use crate::cycle::queue::{DEATHS_TO_RETIRE, candidate_count, deferred_count, take_queue_work};
use crate::gc::Arming;

/// The count that arms the pass.
const D: usize = DEATHS_TO_RETIRE as usize;

fn plain_class(name: &str) -> *const Class {
    ClassBuilder::new(name).prop("next", true).build()
}

fn reset() {
    crate::cycle::queue::verdicts::discard_standing_verdicts();
    crate::cycle::queue::release_queue_segments();
    crate::memory::critical::drain_for_test();
    crate::gc::disarm();
    assert!(crate::cycle::queue::refill_spares());
}

/// `count` candidates registered at a non-final decrement whose deaths then
/// completed in place, each slot withheld by its entry in R.
unsafe fn completed_deaths(arena: &mut Arena, class: *const Class, count: usize) {
    for _ in 0..count {
        let mut context = LLContext { arena: &mut *arena };
        let entity = unsafe { new_constructed(&mut context, class, MemoryCategory::GcHeap) }
            as *mut RcHeader;
        unsafe {
            ll_retain(entity);
            assert!(!ll_release(entity), "registered at the non-final decrement");
            assert!(ll_release(entity));
            ll_object_die(entity as *mut Object);
        }
    }
}

/// `count` candidates registered and held live by the case's own count,
/// the spares refilled every [`POLL_STRIDE`](crate::cycle::queue::POLL_STRIDE)
/// registrations as a poll would: a run past that with no poll overflows the
/// base block's buffer, which aborts.
unsafe fn live_candidates(
    arena: &mut Arena,
    class: *const Class,
    count: usize,
) -> Vec<*mut RcHeader> {
    (0..count)
        .map(|index| {
            if index % crate::cycle::queue::POLL_STRIDE == 0 {
                crate::cycle::queue::refill_and_drain();
            }
            let mut context = LLContext { arena: &mut *arena };
            let entity = unsafe { new_constructed(&mut context, class, MemoryCategory::GcHeap) }
                as *mut RcHeader;
            unsafe {
                ll_retain(entity);
                assert!(!ll_release(entity), "the case holds it");
            }
            entity
        })
        .collect()
}

/// Release what [`live_candidates`] holds and collect it.
unsafe fn let_go(live: Vec<*mut RcHeader>) {
    for entity in live {
        unsafe {
            if ll_release(entity) {
                ll_object_die(entity as *mut Object);
            }
        }
    }
    unsafe { ll_gc_collect_cycles() };
}

/// The D-th completed death arms the poll, and the poll gives every withheld
/// slot back; red on the tree whose poll read no arming for it, where the D
/// entries stood in R through any number of polls.
#[test]
fn the_deaths_that_reach_the_count_are_retired_at_the_next_poll() {
    let _g = test_guard();
    reset();
    let mut arena = Arena::new();
    unsafe { completed_deaths(&mut arena, plain_class("CountedDeathNode"), D) };
    assert_eq!(candidate_count(), D);
    assert_eq!(crate::gc::arming(), Arming::Retire, "the D-th death armed");

    assert_eq!(unsafe { ll_gc_maybe_collect() }, 0);
    assert_eq!(candidate_count(), 0, "the pass gave every slot back");
    assert!(!crate::gc::is_armed());
    reset();
}

/// One death short of the count arms nothing, and the poll leaves R as it
/// stood.
#[test]
fn one_death_short_of_the_count_arms_nothing() {
    let _g = test_guard();
    reset();
    let mut arena = Arena::new();
    unsafe { completed_deaths(&mut arena, plain_class("ShortCountNode"), D - 1) };
    assert!(!crate::gc::is_armed());

    assert_eq!(unsafe { ll_gc_maybe_collect() }, 0);
    assert_eq!(candidate_count(), D - 1);
    reset();
}

/// A collector's grant keeps the ring from the pass: the poll that reads the
/// grant returns before the arming, which stands, and the poll after the
/// release makes the pass.
#[test]
fn a_grant_defers_the_pass_to_the_poll_after_its_release() {
    let _g = test_guard();
    reset();
    let mut arena = Arena::new();
    unsafe { completed_deaths(&mut arena, plain_class("GrantedCountNode"), D) };
    let grant = Grant::claim();

    assert_eq!(unsafe { ll_gc_maybe_collect() }, 0);
    assert_eq!(candidate_count(), D, "no pass under the grant");
    assert_eq!(crate::gc::arming(), Arming::Retire, "and the arming stands");

    drop(grant);
    assert_eq!(unsafe { ll_gc_maybe_collect() }, 0);
    assert_eq!(candidate_count(), 0, "the next poll made it");
    reset();
}

/// A slot no collector thread is born into.
const ELDER_STAND_IN: usize = 7;

/// A stand-in collector's grant on this thread's token, released when
/// dropped, on the unwind too: a case that failed under the grant would
/// otherwise leave its thread's exit waiting for a release nobody makes.
struct Grant;

impl Grant {
    fn claim() -> Self {
        assert!(this_threads_token().claim_for_test(ELDER_STAND_IN));
        Self
    }
}

impl Drop for Grant {
    fn drop(&mut self) {
        this_threads_token().release_claim(ELDER_STAND_IN, false);
    }
}

fn this_threads_token() -> &'static crate::cycle::token::TraceToken {
    unsafe { &(*crate::cycle::mutator_record::this_thread_record()).token }
}

/// A ring at the collector's threshold is the collector's to batch, whose
/// batches post its completed deaths into P for the collection over P to
/// retire: the arming is spent with no pass, the poll's signal raised for a
/// collector that may not be born yet, and the count started again, so that
/// D more deaths arm the pass again.
#[test]
fn a_ring_at_the_threshold_signals_the_collector_instead_of_a_pass() {
    let _g = test_guard();
    reset();
    let mut arena = Arena::new();
    let class = plain_class("ThresholdCountNode");
    let live = unsafe { live_candidates(&mut arena, class, crate::cycle::worker::SOFT_THRESHOLD) };
    unsafe { completed_deaths(&mut arena, class, D) };
    let standing = crate::cycle::worker::SOFT_THRESHOLD + D;
    assert_eq!(candidate_count(), standing);
    let _ = take_queue_work();

    assert_eq!(unsafe { ll_gc_maybe_collect() }, 0);
    assert_eq!(candidate_count(), standing, "no pass");
    assert_eq!(take_queue_work().records_read, 0);
    assert!(!crate::gc::is_armed(), "and the arming spent");
    assert!(
        crate::cycle::queue::take_the_signal_for_test(),
        "the collector signalled, no thread having received it"
    );

    unsafe { completed_deaths(&mut arena, class, D) };
    assert_eq!(crate::gc::arming(), Arming::Retire, "the count began again");

    crate::gc::disarm();
    unsafe { let_go(live) };
    reset();
}

/// The pass reads R and leaves the deferred lane to its turnover: behind a
/// lane of a hundred thousand live entries it reads the D entries of R and
/// nothing of the lane.
#[test]
fn the_pass_reads_r_and_not_the_deferred_lane() {
    let _g = test_guard();
    reset();
    let mut arena = Arena::new();
    let class = plain_class("LaneCountNode");
    let lane = 100_000;
    let live = unsafe { live_candidates(&mut arena, class, lane) };
    // A collection's close defers as far as the spares fund the lane's
    // growth and leaves the rest in R, so the lane fills over several.
    for _ in 0..lane {
        crate::cycle::queue::refill_and_drain();
        assert_eq!(unsafe { ll_gc_collect_cycles() }, 0);
        if deferred_count() == lane {
            break;
        }
    }
    assert_eq!(deferred_count(), lane, "every live root deferred");
    unsafe { completed_deaths(&mut arena, class, D) };
    assert_eq!(candidate_count(), D);
    let _ = take_queue_work();

    assert_eq!(unsafe { ll_gc_maybe_collect() }, 0);
    assert_eq!(candidate_count(), 0, "the pass retired R's deaths");
    let work = take_queue_work();
    assert!(
        work.records_read <= D,
        "the pass read {} records, R holding {D}",
        work.records_read
    );
    assert_eq!(deferred_count(), lane, "and left the lane");

    crate::cycle::epoch::turn_this_threads_cell();
    assert!(crate::cycle::queue::reoffer_deferred_if_epoch_moved());
    unsafe { let_go(live) };
    reset();
}

/// A compaction that reads R reads it whole and starts the count again:
/// deaths counted before a collection over R do not bring the pass forward
/// after it. The collection over P lowers the count by the deaths it frees
/// at R's front and keeps the rest
/// (`what_the_byte_arms::the_fire_the_byte_arms_keeps_the_count_of_the_deaths_it_leaves`).
#[test]
fn a_compaction_starts_the_count_again() {
    let _g = test_guard();
    reset();
    let mut arena = Arena::new();
    let class = plain_class("RecountedDeathNode");
    unsafe { completed_deaths(&mut arena, class, D - 1) };
    assert_eq!(unsafe { ll_gc_collect_cycles() }, 0);
    assert_eq!(candidate_count(), 0, "the collection retired them");

    unsafe { completed_deaths(&mut arena, class, 1) };
    assert!(!crate::gc::is_armed(), "one death since the compaction");
    reset();
}

/// A collection retires what the pass would have, so its close spends an
/// arming for the pass as it spends one for P: a garbage ring of D
/// registered members dies inside the collection, each death withheld by
/// its entry and counted, and the count's arming goes with the close.
#[test]
fn a_collection_spends_the_arming_its_own_deaths_made() {
    let _g = test_guard();
    reset();
    let mut arena = Arena::new();
    let _ = unsafe { long_ring(&mut arena, plain_class("SpentCountNode"), D) };
    assert!(!crate::gc::is_armed());

    assert_eq!(unsafe { ll_gc_collect_cycles() }, D, "the ring died");
    assert!(!crate::gc::is_armed(), "and the close spent the arming");
    reset();
}

/// The count's arming is the lowest: a thread armed for R whole stays so.
#[test]
fn the_count_does_not_lower_a_higher_arming() {
    let _g = test_guard();
    reset();
    let mut arena = Arena::new();
    crate::gc::arm();
    unsafe { completed_deaths(&mut arena, plain_class("OutrankedCountNode"), D) };
    assert_eq!(crate::gc::arming(), Arming::AllRoots);
    reset();
}

/// The pass takes the thread's own token, and a request that landed on the
/// byte after the poll's reading is refused by that take and its collector
/// woken, rather than consented to by a return inside the pass while the
/// pass rewrites R.
#[test]
fn a_request_after_the_reading_is_refused_by_the_pass() {
    let _g = test_guard();
    reset();
    let mut arena = Arena::new();
    unsafe { completed_deaths(&mut arena, plain_class("RequestedPassNode"), D) };
    let token = this_threads_token();
    let refusals = token.refusals();
    token.request_for_test(crate::cycle::token::word(
        crate::cycle::token::REQUESTED,
        ELDER_STAND_IN,
    ));

    unsafe { crate::cycle::queue::retire_at_the_poll() };
    assert_eq!(candidate_count(), 0);
    assert_eq!(
        token.read(),
        crate::cycle::token::FREE,
        "released after the pass"
    );
    assert_eq!(
        token.refusals(),
        refusals + 1,
        "the take refused the request"
    );
    reset();
}

/// A grant a return consented to after the poll's reading is read again
/// before the take, which would wait out the collector's batch: the pass
/// stands for the next poll.
#[test]
fn a_grant_read_again_before_the_take_defers_the_pass() {
    let _g = test_guard();
    reset();
    let mut arena = Arena::new();
    unsafe { completed_deaths(&mut arena, plain_class("RereadPassNode"), D) };
    crate::gc::disarm();
    let grant = Grant::claim();

    unsafe { crate::cycle::queue::retire_at_the_poll() };
    assert_eq!(candidate_count(), D, "no pass under the grant");
    assert_eq!(crate::gc::arming(), Arming::Retire, "and the pass stands");

    drop(grant);
    crate::gc::disarm();
    reset();
}

/// The pass's retirements are no disposition of the collector's batch, and
/// leave its timer no note to shorten on.
#[test]
fn the_pass_makes_no_note_for_the_collectors_timer() {
    let _g = test_guard();
    reset();
    let mut arena = Arena::new();
    let record = unsafe { &*crate::cycle::mutator_record::this_thread_record() };
    let _ = record.take_freeing_disposition_note();
    unsafe { completed_deaths(&mut arena, plain_class("UnnotedPassNode"), D) };

    assert_eq!(unsafe { ll_gc_maybe_collect() }, 0);
    assert_eq!(candidate_count(), 0);
    assert!(!record.take_freeing_disposition_note());
    reset();
}

/// Deaths the pass cannot retire — the deferred lane's — double the count
/// the next pass waits for, and a pass that returns half of its count
/// brings it back: a lane dying behind a small R costs a pass per a growing
/// number of deaths rather than per D.
#[test]
fn a_pass_that_returns_little_doubles_the_count_it_waits_for() {
    let _g = test_guard();
    reset();
    let mut arena = Arena::new();
    let class = plain_class("BackedOffPassNode");

    unsafe { deaths_in_the_lane(&mut arena, class, D) };
    assert_eq!(
        crate::gc::arming(),
        Arming::Retire,
        "the lane's deaths armed"
    );
    assert_eq!(unsafe { ll_gc_maybe_collect() }, 0);
    assert_eq!(deferred_count(), D, "and the pass read none of them");

    // After that pass the count is twice D: half in the lane, which the pass
    // cannot return, and half in R, which it returns — half of its count,
    // which starts it again.
    unsafe { deaths_in_the_lane(&mut arena, class, D) };
    unsafe { completed_deaths(&mut arena, class, D - 1) };
    assert!(!crate::gc::is_armed(), "the count waits for twice D");
    unsafe { completed_deaths(&mut arena, class, 1) };
    assert_eq!(crate::gc::arming(), Arming::Retire);
    assert_eq!(unsafe { ll_gc_maybe_collect() }, 0);
    assert_eq!(candidate_count(), 0, "the pass returned R's half");

    unsafe { completed_deaths(&mut arena, class, D) };
    assert_eq!(crate::gc::arming(), Arming::Retire, "and D arms again");
    reset();
}

/// `count` live candidates deferred into the lane by a collection, then let
/// die there, each death counted and withheld by its lane entry, behind
/// whatever the lane held. The collection's compaction starts the count
/// again before the deaths.
unsafe fn deaths_in_the_lane(arena: &mut Arena, class: *const Class, count: usize) {
    let lane = unsafe { live_candidates(arena, class, count) };
    let standing = deferred_count();
    assert_eq!(unsafe { ll_gc_collect_cycles() }, 0);
    assert_eq!(deferred_count(), standing + count, "the lane holds them");
    for entity in lane {
        unsafe {
            assert!(ll_release(entity));
            ll_object_die(entity as *mut Object);
        }
    }
}
