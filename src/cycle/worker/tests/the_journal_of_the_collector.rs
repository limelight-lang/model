//! The collector's operations in the debug journal, each kind read once on
//! an input whose answer is known (S67.8 in `dev/plans/S67.md`): a
//! registration and a decrement over a candidate, a batch of garbage rings
//! from its start to the slots its members' deaths return, a grant recalled
//! before its batch, each cause of a turnover, and the lane a turnover hands
//! back. The exits of a trace are read in the cases that build them, in
//! `the_batch.rs`.
//!
//! A case counts its own thread's ring from its start and the ring of the
//! collector thread its serve ran on, never the process's: the suite's other
//! threads journal under the same mask meanwhile.

use super::the_batch::{kept_root, object, release_keeper, served_by_a_collector};
use super::*;
use crate::cycle::queue::deferred_count;
use crate::gc::ll_gc_maybe_collect;
use crate::journal::kinds::*;
use crate::memory::arena::Arena;
use crate::object::{Object, ll_object_die};
use crate::refcount::{RcHeader, ll_release, ll_retain};

/// The first non-final decrement registers the entity; a second, with its
/// candidate bit standing, finds it a candidate already.
#[test]
fn a_decrement_registers_once_and_then_finds_a_candidate() {
    let _sites = journal_the_collector();
    let _g = test_guard();
    reset_lanes();
    let counts = CountsFromHere::from_here();
    let mut arena = Arena::new();
    let entity =
        unsafe { object(&mut arena, node_class("JournalRegisteredNode")) } as *mut RcHeader;

    unsafe {
        ll_retain(entity);
        ll_retain(entity);
        assert!(!ll_release(entity), "three to two registers it");
        assert!(!ll_release(entity), "two to one finds its bit");
    }
    let read = counts.so_far();
    assert_eq!(read.records(KIND_CANDIDATE_REGISTERED, REGISTERED_NOW), 1);
    assert_eq!(
        read.records(KIND_CANDIDATE_REGISTERED, REGISTERED_ALREADY),
        1
    );

    unsafe {
        assert!(ll_release(entity));
        ll_object_die(entity as *mut Object);
    }
    reset_lanes();
}

/// Three garbage rings of two roots each, in one batch: one start at the
/// threshold carrying the six roots, one trace and an end past the first
/// regions, six proposals; the mutator's collection over P reclaims
/// the six members as the one set its commit confirms (`crate::cycle::collect`,
/// "The commit is one component") and retires their slots from P.
#[test]
fn a_batch_of_garbage_rings_is_journaled_from_its_start_to_the_slots_returned() {
    const RINGS: usize = 3;
    let _sites = journal_the_collector();
    let _g = test_guard();
    reset_lanes();
    let node = node_class("JournalRingNode");
    let mut arena = Arena::new();
    for _ in 0..RINGS {
        let _ = unsafe { crate::cycle::testing::ring(&mut arena, [node, node]) };
    }
    let counts = CountsFromHere::from_here();
    unsafe { &*record() }.set_batch_size(2 * RINGS);

    assert_eq!(
        served_by_a_collector(),
        Served::Batch {
            roots: 2 * RINGS,
            complete: true,
            backlog: false,
        }
    );
    let collector = testing::take_the_serving_threads_counts();
    assert_eq!(
        collector.records(KIND_BATCH_START, BATCH_AT_THE_THRESHOLD),
        1
    );
    assert_eq!(collector.records_of_kind(KIND_BATCH_START), 1);
    assert_eq!(
        collector.sum_of_b_of_kind(KIND_BATCH_START),
        (2 * RINGS) as u64
    );
    assert_eq!(collector.records(KIND_BATCH_END, BATCH_END_COMPLETE), 1);
    assert_eq!(collector.records_of_kind(KIND_BATCH_END), 1);
    assert_eq!(
        collector.sum_of_b_of_kind(KIND_BATCH_END),
        1,
        "the mark's first descent ended before the scan"
    );
    assert_eq!(
        collector.records(KIND_ROOT_VERDICT, VERDICT_PROPOSED),
        (2 * RINGS) as u64
    );
    assert_eq!(
        collector.records_of_kind(KIND_ROOT_VERDICT),
        (2 * RINGS) as u64
    );

    assert_eq!(unsafe { ll_gc_maybe_collect() }, 2 * RINGS);
    let mutator = counts.so_far();
    assert_eq!(mutator.records_of_kind(KIND_COMPONENT_RECLAIMED), 1);
    assert_eq!(
        mutator.sum_of_b_of_kind(KIND_COMPONENT_RECLAIMED),
        (2 * RINGS) as u64
    );
    assert_eq!(
        mutator.records(KIND_WITHHELD_SLOT_RETURNED, SLOT_FROM_P),
        (2 * RINGS) as u64,
        "every member was registered, so its slot waited for P's disposition"
    );
    assert_eq!(
        mutator.records_of_kind(KIND_WITHHELD_SLOT_RETURNED),
        (2 * RINGS) as u64
    );
    reset_lanes();
}

/// A recall standing before the batch releases the grant with no batch.
#[test]
fn a_grant_recalled_before_its_batch_is_journaled_as_such() {
    let _sites = journal_the_collector();
    let _g = test_guard();
    reset_lanes();
    let node = node_class("JournalRecalledGrantNode");
    let mut arena = Arena::new();
    let _garbage = unsafe { crate::cycle::testing::ring(&mut arena, [node, node]) };
    unsafe { &*record() }.set_batch_size(2);

    let token = unsafe { &raw const (*record()).token } as usize;
    testing::at_the_next_grant(Box::new(move || {
        unsafe { &*(token as *const crate::cycle::token::TraceToken) }.recall_for_test(true)
    }));
    let served = served_by_a_collector();
    unsafe { &*record() }.token.recall_for_test(false);
    assert_eq!(served, Served::Idle);
    let collector = testing::take_the_serving_threads_counts();
    assert_eq!(
        collector.records(KIND_GRANT_WITHOUT_BATCH, GRANT_RECALLED_BEFORE_THE_BATCH),
        1
    );
    assert_eq!(collector.records_of_kind(KIND_BATCH_START), 0);

    unsafe { crate::gc::ll_gc_collect_cycles() };
    reset_lanes();
}

/// Each cause of a turnover writes its own code: sixty-four batches inside X,
/// X past, a new life, and a test's hand.
#[test]
fn each_cause_of_a_turnover_is_journaled_under_its_code() {
    let _sites = journal_the_collector();
    let _g = test_guard();
    let counts = CountsFromHere::from_here();
    let record = unsafe { &*record() };
    let visit = |record: &MutatorRecord| advance_the_epoch_if_due(record, serve_clock_now());
    let _ = record.take_new_life();
    record.note_advanced_at(0);
    testing::advance_epochs_after(Some(Duration::from_secs(60)));

    visit(record);
    record.note_epoch_work(0, 1);
    record.note_epoch_work(crate::cycle::epoch::SPENT_PER_PROOF, 0);
    visit(record);
    testing::advance_epochs_after(Some(Duration::from_millis(1)));
    std::thread::sleep(Duration::from_millis(5));
    visit(record);
    crate::cycle::mutator_record::note_new_life_for_test(std::ptr::from_ref(record).cast_mut());
    visit(record);
    unsafe { crate::cycle::epoch::turn_the_cell_of(record) };
    testing::advance_epochs_after(None);

    let read = counts.so_far();
    for code in [
        TURNOVER_BY_PROOFS,
        TURNOVER_BY_X,
        TURNOVER_NEW_LIFE,
        TURNOVER_BY_HAND,
    ] {
        assert_eq!(read.records(KIND_TURNOVER, code), 1, "code {code}");
    }
    assert_eq!(read.records_of_kind(KIND_TURNOVER), 4);
}

/// Two kept roots read live are deferred from P by the mutator's disposition,
/// and after a turnover the poll hands their lane back into R in one record
/// carrying both.
#[test]
#[cfg_attr(
    feature = "collector-chain",
    ignore = "under the chain the collector keeps a root read live or unwalked in its chain, not in P (`crate::cycle::chain`)"
)]
fn a_lane_deferred_from_p_is_handed_back_at_the_turn_in_one_record() {
    let _sites = journal_the_collector();
    let _g = test_guard();
    reset_lanes();
    let node = node_class("JournalDeferredNode");
    let mut arena = Arena::new();
    let (_, keeper_a) = unsafe { kept_root(&mut arena, node, "JournalKeeperA") };
    let (_, keeper_b) = unsafe { kept_root(&mut arena, node, "JournalKeeperB") };
    let counts = CountsFromHere::from_here();
    unsafe { &*record() }.set_batch_size(2);

    assert!(matches!(
        served_by_a_collector(),
        Served::Batch { roots: 2, .. }
    ));
    let collector = testing::take_the_serving_threads_counts();
    assert_eq!(collector.records(KIND_ROOT_VERDICT, VERDICT_READ_LIVE), 2);
    unsafe { ll_gc_maybe_collect() };
    assert_eq!(deferred_count(), 2);
    let deferred = counts.so_far();
    assert_eq!(deferred.records(KIND_ROOT_DEFERRED, DEFERRED_FROM_P), 2);
    assert_eq!(deferred.records_of_kind(KIND_REOFFERED), 0);

    unsafe { crate::cycle::epoch::turn_the_cell_of(record()) };
    unsafe { ll_gc_maybe_collect() };
    assert_eq!(deferred_count(), 0, "the poll handed the lane back");
    let handed_back = counts.so_far().since(&deferred);
    #[cfg(not(feature = "wait-by-readings"))]
    let code = REOFFERED_AT_THE_TURN;
    #[cfg(feature = "wait-by-readings")]
    let code = REOFFERED_LANE_DUE;
    assert_eq!(handed_back.records(KIND_REOFFERED, code), 1);
    assert_eq!(handed_back.sum_of_b(KIND_REOFFERED, code), 2);
    assert_eq!(handed_back.records_of_kind(KIND_REOFFERED), 1);

    unsafe {
        release_keeper(keeper_a);
        release_keeper(keeper_b);
    }
    reset_lanes();
}

/// Under the chain, a root the trace did not reach goes to the ready part's
/// tail rather than into P: two kept roots of the second generation, the
/// recall standing at the trace's start.
#[test]
#[cfg(feature = "collector-chain")]
fn an_unwalked_root_the_chain_keeps_is_journaled_as_deferred_into_its_ready_part() {
    let _sites = journal_the_collector();
    let _g = test_guard();
    crate::cycle::chain::testing::dismantle_this_threads();
    reset_lanes();
    let node = node_class("JournalUnwalkedChainNode");
    let mut arena = Arena::new();
    let (_, keeper_a) = unsafe { kept_root(&mut arena, node, "JournalChainKeeperA") };
    let (_, keeper_b) = unsafe { kept_root(&mut arena, node, "JournalChainKeeperB") };
    unsafe { &*record() }.set_batch_size(2);

    let token = unsafe { &raw const (*record()).token } as usize;
    testing::at_the_start_of_the_next_trace(Box::new(move || {
        unsafe { &*(token as *const crate::cycle::token::TraceToken) }.recall_for_test(true)
    }));
    let _ = served_by_a_collector();
    unsafe { &*record() }.token.recall_for_test(false);
    let collector = testing::take_the_serving_threads_counts();
    assert_eq!(collector.records(KIND_ROOT_VERDICT, VERDICT_UNWALKED), 2);
    assert_eq!(
        collector.records(KIND_ROOT_DEFERRED, DEFERRED_INTO_THE_READY_PART),
        2
    );

    unsafe {
        release_keeper(keeper_a);
        release_keeper(keeper_b);
    }
    crate::cycle::chain::testing::dismantle_this_threads();
    reset_lanes();
}
