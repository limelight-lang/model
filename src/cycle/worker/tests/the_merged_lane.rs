//! A deferred lane merged back into R: the round takes a merged ring below
//! the threshold at its next visit whatever shape the ring was packed into,
//! a batch that took less than its clamp leaves K where it stood, and a
//! batch reads its backlog by R's count rather than off the front block.
//!
//! This thread is the mutator; the collector is a thread of the case's on a
//! slot no collector thread is born into, serving at the soft threshold, so
//! that a ring of a few entries reads below it unless its blocks say
//! otherwise.

use super::the_batch::{keeper_class, kept_root, object, release_keeper, served_by_a_collector};
use super::*;
use crate::cycle::queue::verdicts::discard_standing_verdicts;
use crate::cycle::queue::{
    candidate_count, deferred_count, fill_tail_block, reoffer_deferred_if_epoch_moved,
    segment_count,
};
use crate::cycle::token::{COLLECTOR, word};
use crate::gc::ll_gc_maybe_collect;
use crate::memory::arena::Arena;
use crate::object::Object;
use crate::refcount::RcHeader;
use crate::ring::{BLOCK_ENTRIES, Reader};
use std::time::Duration;

/// A slot no collector thread is born into under the default cap and no
/// other case names.
const SLOT: usize = 6;

/// One serve of this thread's record at [`SOFT_THRESHOLD`] by a collector
/// thread of the case's, this thread consenting meanwhile as its poll would.
fn served_at_the_soft_threshold() -> Served {
    let sent = Sent(record());
    testing::consent_while(std::thread::spawn(move || {
        assert!(
            crate::memory::heap::ll_thread_init(),
            "the pool served the collector thread"
        );
        let mut standing = Standing::new(SLOT);
        unsafe {
            serve(
                sent.into_inner(),
                SLOT,
                SOFT_THRESHOLD,
                &mut standing,
                serve_clock_now(),
            )
        }
    }))
}

/// `count` roots held by keepers, read live by a batch and deferred by this
/// thread's collection over P: the lane stands occupied, R holds a block
/// and nothing in it. Answers the keepers.
fn a_lane_of(arena: &mut Arena, count: usize, name: &str) -> Vec<*mut Object> {
    let node = node_class(name);
    let keepers: Vec<*mut Object> = (0..count)
        .map(|_| unsafe { kept_root(arena, node, name) }.1)
        .collect();
    assert!(matches!(
        served_by_a_collector(),
        Served::Batch { roots, complete: true, .. } if roots == count
    ));
    assert_eq!(unsafe { ll_gc_maybe_collect() }, 0, "every root read live");
    assert_eq!(deferred_count(), count, "and the close deferred each");
    assert_eq!(candidate_count(), 0);
    assert_eq!(segment_count(), 1, "R keeps its block");
    keepers
}

/// Dispose of what a case left: the verdicts, the byte, the keepers and the
/// lanes.
fn clear_up(keepers: Vec<*mut Object>) {
    discard_standing_verdicts();
    unsafe { &*record() }.clear_posted_for_test();
    for keeper in keepers {
        unsafe { release_keeper(keeper) };
    }
    reset_lanes();
}

/// Roots of the lane the K case merges: fewer than the starting K and more
/// than a take's clamp could explain.
const LANE: usize = 8;

/// A poll that merges the lane and consents to a request in the same pass
/// returns before it fires, so the grant meets the merged ring unpacked: its
/// front block is not its tail block, the ring reads at the threshold, and
/// the batch is clamped to K. Taking the eight roots it holds says nothing
/// about what the thread offers per batch, and K stays where it stood; red
/// on the rule that doubled K after every completed batch of that form.
#[test]
#[cfg_attr(
    feature = "collector-chain",
    ignore = "under the chain the collector keeps a root read live or unwalked in its chain, not in P (`crate::cycle::chain`)"
)]
fn a_batch_short_of_its_clamp_leaves_k_after_a_consenting_merge() {
    let _g = test_guard();
    reset_lanes();
    let mut arena = Arena::new();
    let keepers = a_lane_of(&mut arena, LANE, "ConsentingMergeNode");
    let record = unsafe { &*record() };
    record.set_batch_size(INITIAL_BATCH);

    crate::cycle::epoch::turn_this_threads_cell();
    record
        .token
        .request(SLOT)
        .expect("a round's request lands on the free byte");
    assert_eq!(
        unsafe { ll_gc_maybe_collect() },
        0,
        "the poll consented and fired nothing"
    );
    assert_eq!(deferred_count(), 0, "it merged the lane first");
    assert_eq!(record.token.read(), word(COLLECTOR, SLOT));

    assert!(matches!(
        served_at_the_soft_threshold(),
        Served::Batch { roots: LANE, .. }
    ));
    assert_eq!(
        record.batch_size(),
        INITIAL_BATCH,
        "a batch that took less than its clamp leaves K"
    );

    clear_up(keepers);
}

/// A merged lane the owner packed into R's one block reads below the
/// threshold off the front block, and the round takes it at its next visit
/// all the same, because the merge moved the count the round compares with
/// the one the last grant saw; red on the round that read only the ring's
/// shape and stamped the instant. The grant records the count, so a ring
/// registered after it stands its interval.
#[test]
#[cfg_attr(
    feature = "collector-chain",
    ignore = "under the chain the collector keeps a root read live or unwalked in its chain, not in P (`crate::cycle::chain`)"
)]
fn a_packed_merged_lane_is_taken_at_the_next_round() {
    let _g = test_guard();
    reset_lanes();
    let _interval = StandingInterval::of(Duration::from_secs(60));
    let mut arena = Arena::new();
    let mut keepers = a_lane_of(&mut arena, 2, "PackedMergeNode");
    let record = unsafe { &*record() };
    record.set_batch_size(INITIAL_BATCH);

    crate::cycle::epoch::turn_this_threads_cell();
    assert!(reoffer_deferred_if_epoch_moved());
    assert_eq!(segment_count(), 2, "the lane stands after R's block");
    {
        let _claim = crate::cycle::token::HeldToken::take();
        unsafe { crate::cycle::queue::retire_candidates() };
    }
    assert!(
        !unsafe { Reader::new(record.candidate_ring()) }.has_at_least(SOFT_THRESHOLD),
        "and the owner packed it into that block, which reads below the threshold"
    );
    assert_eq!(candidate_count(), 2);

    assert_eq!(
        served_at_the_soft_threshold(),
        Served::Batch {
            roots: 2,
            complete: true,
            backlog: false,
        },
        "the round after the merge took the ring"
    );
    assert_eq!(record.batch_size(), INITIAL_BATCH, "as a take leaves K");

    assert_eq!(unsafe { ll_gc_maybe_collect() }, 0);
    keepers.push(unsafe { kept_root(&mut arena, node_class("PackedMergeNode"), "Drip") }.1);
    assert_eq!(
        served_at_the_soft_threshold(),
        Served::Idle,
        "the grant saw the merge, and the drip stands its interval"
    );

    clear_up(keepers);
}

/// A round that reads the ring empty has seen every merge before its
/// reading, since a collection over R whole traced what the merge brought: a
/// ring registered after it stands its interval rather than being taken as
/// a merge.
#[test]
#[cfg_attr(
    feature = "collector-chain",
    ignore = "under the chain the collector keeps a root read live or unwalked in its chain, not in P (`crate::cycle::chain`)"
)]
fn a_ring_read_empty_after_a_merge_sees_the_merge() {
    let _g = test_guard();
    reset_lanes();
    let _interval = StandingInterval::of(Duration::from_secs(60));
    let mut arena = Arena::new();
    let mut keepers = a_lane_of(&mut arena, 2, "EmptiedMergeNode");

    crate::cycle::epoch::turn_this_threads_cell();
    assert!(reoffer_deferred_if_epoch_moved());
    assert_eq!(
        unsafe { crate::gc::ll_gc_collect_cycles() },
        0,
        "the owner's collection read the merged roots live"
    );
    assert_eq!(candidate_count(), 0);
    assert_eq!(served_at_the_soft_threshold(), Served::Idle);

    keepers.push(unsafe { kept_root(&mut arena, node_class("EmptiedMergeNode"), "Drip") }.1);
    assert_eq!(
        served_at_the_soft_threshold(),
        Served::Idle,
        "the drip stands its interval"
    );

    clear_up(keepers);
}

/// A batch over a ring of three blocks — the front block read out, the next
/// holding one registration, a merged lane of two after it — takes the one
/// and leaves two, which is no backlog at any threshold above two. Red on
/// the reading off the front block: the commit moves the front into the
/// middle block, which is not the tail block, and that reads as a ring at
/// the threshold.
#[test]
#[cfg_attr(
    feature = "collector-chain",
    ignore = "under the chain the collector keeps a root read live or unwalked in its chain, not in P (`crate::cycle::chain`)"
)]
fn a_batch_over_three_blocks_reads_no_backlog_below_the_threshold() {
    let _g = test_guard();
    reset_lanes();
    let mut arena = Arena::new();
    let mut keepers = a_lane_of(&mut arena, 2, "ThreeBlocksNode");
    let record = unsafe { &*record() };

    // R's block filled with a root's entry before the root registers, so its
    // registration opens the next block, and the filler read out by hand
    // under the owner's claim.
    let node = node_class("ThreeBlocksNode");
    let root = unsafe { object(&mut arena, node) };
    let keeper = unsafe { object(&mut arena, keeper_class("ThreeBlocksKeeper")) };
    unsafe {
        crate::test_support::store_prop(
            &mut arena,
            keeper,
            crate::test_support::prop_offset(0),
            root,
        );
    }
    keepers.push(keeper);
    fill_tail_block(root as *mut RcHeader);
    assert!(
        !unsafe { crate::refcount::ll_release(root as *mut RcHeader) },
        "the keeper holds it"
    );
    assert_eq!(segment_count(), 2);
    {
        let _claim = crate::cycle::token::HeldToken::take();
        unsafe { Reader::new(record.candidate_ring()) }.advance(BLOCK_ENTRIES);
    }
    assert_eq!(candidate_count(), 1);

    crate::cycle::epoch::turn_this_threads_cell();
    assert!(reoffer_deferred_if_epoch_moved());
    assert_eq!(segment_count(), 3);
    assert_eq!(candidate_count(), 3);

    assert_eq!(
        served_at_the_soft_threshold(),
        Served::Batch {
            roots: 1,
            complete: true,
            backlog: false,
        },
        "two left behind the batch are no backlog"
    );

    clear_up(keepers);
}

/// A merge that lands between the round's load of the merge count and its
/// reading of the ring is read in the ring and missed by the count, so the
/// round stamps the packed ring as standing and the next round takes it:
/// the merge is late by a round and never lost. Red with the two loads the
/// other way round, where the ring reads empty before the merge and the
/// count after it, and the empty ring records as seen a merge whose roots
/// then stand the whole interval.
#[test]
#[cfg_attr(
    feature = "collector-chain",
    ignore = "under the chain the collector keeps a root read live or unwalked in its chain, not in P (`crate::cycle::chain`)"
)]
fn a_merge_between_the_count_and_the_reading_is_taken_a_round_later() {
    let _g = test_guard();
    reset_lanes();
    let _interval = StandingInterval::of(Duration::from_secs(60));
    let mut arena = Arena::new();
    let keepers = a_lane_of(&mut arena, 2, "MidReadingMergeNode");
    crate::cycle::epoch::turn_this_threads_cell();

    // The collector's hook asks this thread to merge and pack, and waits.
    let (merge_now, asked) = std::sync::mpsc::channel::<()>();
    let (merged, told) = std::sync::mpsc::channel::<()>();
    testing::at_the_next_reading(Box::new(move || {
        merge_now.send(()).expect("the mutator waits for the ask");
        told.recv().expect("the mutator merged");
    }));
    let sent = Sent(record());
    let collector = std::thread::spawn(move || {
        assert!(crate::memory::heap::ll_thread_init());
        let mut standing = Standing::new(SLOT);
        unsafe {
            serve(
                sent.into_inner(),
                SLOT,
                SOFT_THRESHOLD,
                &mut standing,
                serve_clock_now(),
            )
        }
    });
    asked.recv().expect("the round reached its reading");
    assert!(reoffer_deferred_if_epoch_moved());
    {
        let _claim = crate::cycle::token::HeldToken::take();
        unsafe { crate::cycle::queue::retire_candidates() };
    }
    merged.send(()).expect("the hook waits");
    assert_eq!(
        testing::consent_while(collector),
        Served::Idle,
        "the round read the packed ring and a count from before the merge"
    );

    assert_eq!(
        served_at_the_soft_threshold(),
        Served::Batch {
            roots: 2,
            complete: true,
            backlog: false,
        },
        "the next round took the merge"
    );

    clear_up(keepers);
}

/// A merge that lands while a grant stands — the poll re-offers under a
/// collector's grant — is past the count the grant read before its peek,
/// so the grant's release leaves it unseen and the next round takes the
/// merged ring however the owner packed it. Red with the count read at the
/// release, which records the merge the batch never read.
#[test]
#[cfg_attr(
    feature = "collector-chain",
    ignore = "under the chain the collector keeps a root read live or unwalked in its chain, not in P (`crate::cycle::chain`)"
)]
fn a_merge_under_a_grant_is_taken_at_the_next_round() {
    let _g = test_guard();
    reset_lanes();
    let _interval = StandingInterval::of(Duration::from_secs(60));
    let mut arena = Arena::new();
    let mut keepers = a_lane_of(&mut arena, 2, "UnderGrantMergeNode");
    crate::cycle::epoch::turn_this_threads_cell();
    keepers.push(unsafe { kept_root(&mut arena, node_class("UnderGrantMergeNode"), "Drip") }.1);

    // A batch over the one registration waits between its post and its
    // advance; this thread consents, and merges while it waits.
    let (release, waiting_until) = std::sync::mpsc::channel::<()>();
    testing::make_the_next_batch_wait_before_its_advance(waiting_until);
    let sent = Sent(record());
    let collector = std::thread::spawn(move || {
        assert!(crate::memory::heap::ll_thread_init());
        let mut standing = Standing::new(SLOT);
        unsafe { serve(sent.into_inner(), SLOT, 1, &mut standing, serve_clock_now()) }
    });
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while crate::cycle::queue::verdicts::verdict_count() == 0 {
        assert!(std::time::Instant::now() < deadline, "the batch posted");
        crate::cycle::token::read_and_act_on_this_thread();
        std::thread::yield_now();
    }
    assert!(reoffer_deferred_if_epoch_moved(), "merged under the grant");
    release.send(()).expect("the batch waits");
    assert!(matches!(
        testing::consent_while(collector),
        Served::Batch { roots: 1, .. }
    ));

    discard_standing_verdicts();
    let record = unsafe { &*record() };
    record.clear_posted_for_test();
    {
        let _claim = crate::cycle::token::HeldToken::take();
        unsafe { crate::cycle::queue::retire_candidates() };
    }
    assert!(
        !unsafe { Reader::new(record.candidate_ring()) }.has_at_least(SOFT_THRESHOLD),
        "the owner packed the merged lane below the threshold"
    );

    assert_eq!(
        served_at_the_soft_threshold(),
        Served::Batch {
            roots: 2,
            complete: true,
            backlog: false,
        },
        "the round after the grant took the merge"
    );

    clear_up(keepers);
}

/// Only the turnover's merge moves the count: the pressure path and the
/// exit splice the lane through the same call and trace what they merged
/// at once, and a round that took those roots again would repeat the trace.
#[test]
#[cfg_attr(
    feature = "collector-chain",
    ignore = "under the chain the collector keeps a root read live or unwalked in its chain, not in P (`crate::cycle::chain`)"
)]
fn only_the_turnovers_merge_is_counted() {
    let _g = test_guard();
    reset_lanes();
    let mut arena = Arena::new();
    let keepers = a_lane_of(&mut arena, 2, "CountedMergeNode");
    let record = unsafe { &*record() };
    let merges = record.merges();

    crate::cycle::queue::reoffer_deferred_candidates();
    assert_eq!(deferred_count(), 0, "the lane merged");
    assert_eq!(record.merges(), merges, "uncounted, as the pressure path's");

    assert_eq!(unsafe { crate::gc::ll_gc_collect_cycles() }, 0);
    assert_eq!(deferred_count(), 2, "deferred again");
    crate::cycle::epoch::turn_this_threads_cell();
    assert!(reoffer_deferred_if_epoch_moved());
    assert_eq!(
        record.merges(),
        merges.wrapping_add(1),
        "the turnover's counted"
    );

    clear_up(keepers);
}
