//! The collector's chain under a grant (`crate::cycle::chain`, built behind
//! the feature `collector-chain`): a batch whose roots all read live posts
//! nothing into P, releases `FREE` and gives its live list back; the roots
//! wait in the chain until the epoch passes their block, and the next batch
//! reads them beside R, the clamp shared; a pool that refuses the chain a
//! block sends the root into P as without the chain; a chained root that
//! dies is found by the death check and posted `ZeroCount`, and the
//! mutator's disposition of P frees it, a death P had no room for being the
//! next check's first; the round serves a chain that is due whatever R reads;
//! and the mutator's collections under pressure and at the exit take the
//! chain back into R.

use super::the_batch::{keeper_class, kept_root, object, release_keeper, served_by_a_collector};
use super::*;
use crate::class::{Class, ClassBuilder};
use crate::cycle::chain::testing::{REFUSE_BLOCKS, dismantle_this_threads, roots_of_this_threads};
use crate::cycle::queue::verdicts::{Verdict, standing_verdicts, verdict_count};
use crate::cycle::queue::{candidate_count, deferred_count};
use crate::cycle::token::{FREE, NOTHING_PROPOSED, state};
use crate::gc::ll_gc_maybe_collect;
use crate::memory::arena::Arena;
use crate::object::Object;
use crate::refcount::RcHeader;
use std::sync::atomic::Ordering;

fn node_class(name: &str) -> *const Class {
    ClassBuilder::new(name).prop("next", true).build()
}

fn byte() -> u8 {
    unsafe { &*record() }.token.read()
}

fn reset() {
    dismantle_this_threads();
    reset_lanes();
    REFUSE_BLOCKS.store(false, Ordering::Relaxed);
    let _ = testing::take_chain_figures();
}

/// `count` live roots registered in R, each held by a keeper.
unsafe fn kept_roots(
    arena: &mut Arena,
    node: *const Class,
    count: usize,
) -> Vec<(*mut RcHeader, *mut Object)> {
    (0..count)
        .map(|index| unsafe { kept_root(arena, node, &format!("ChainKeeper{index}")) })
        .collect()
}

unsafe fn let_go(kept: &[(*mut RcHeader, *mut Object)]) {
    for &(_, keeper) in kept {
        unsafe { release_keeper(keeper) };
    }
}

#[test]
fn a_batch_of_live_roots_posts_nothing_releases_free_and_keeps_them_waiting() {
    let _g = test_guard();
    reset();
    let node = node_class("ChainSilentNode");
    let mut arena = Arena::new();
    let kept = unsafe { kept_roots(&mut arena, node, 2) };
    assert_eq!(candidate_count(), 2);

    assert!(matches!(
        served_by_a_collector(),
        Served::Batch { roots: 2, .. }
    ));
    assert_eq!(
        state(byte()),
        FREE,
        "a batch that posted nothing releases FREE"
    );
    assert_eq!(verdict_count(), 0, "nothing in P");
    assert!(
        unsafe { &*record() }.live_list().is_null(),
        "the list went back on the collector's thread"
    );
    let (ready, waiting) = roots_of_this_threads();
    assert_eq!(
        (ready, waiting, candidate_count()),
        (Vec::new(), kept.iter().map(|&(root, _)| root).collect(), 0),
        "both wait in the chain, and R advanced past them"
    );

    // The mutator's poll finds nothing to do.
    assert_eq!(unsafe { ll_gc_maybe_collect() }, 0);
    assert_eq!(deferred_count(), 0, "no root reached the deferred lane");

    unsafe { let_go(&kept) };
    reset();
}

#[test]
fn a_refused_chain_block_sends_the_root_into_p_and_the_disposition_defers_it() {
    let _g = test_guard();
    reset();
    let node = node_class("ChainRefusedNode");
    let mut arena = Arena::new();
    let kept = unsafe { kept_roots(&mut arena, node, 2) };
    REFUSE_BLOCKS.store(true, Ordering::Relaxed);

    assert!(matches!(
        served_by_a_collector(),
        Served::Batch { roots: 2, .. }
    ));
    REFUSE_BLOCKS.store(false, Ordering::Relaxed);
    assert_eq!(byte(), NOTHING_PROPOSED);
    assert_eq!(
        standing_verdicts()
            .iter()
            .map(|&(_, verdict)| verdict)
            .collect::<Vec<_>>(),
        vec![Verdict::ReadLive, Verdict::ReadLive]
    );
    assert_eq!(testing::take_chain_figures().refusals, 2, "each push asked");

    assert_eq!(unsafe { ll_gc_maybe_collect() }, 0);
    assert_eq!(state(byte()), FREE);
    assert_eq!(
        deferred_count(),
        2,
        "the disposition deferred both, as without the chain"
    );

    unsafe { let_go(&kept) };
    reset();
}

#[test]
fn the_waiting_part_becomes_ready_when_the_epoch_passes_and_the_next_batch_reads_it() {
    let _g = test_guard();
    reset();
    let node = node_class("ChainExpiryNode");
    let mut arena = Arena::new();
    let kept = unsafe { kept_roots(&mut arena, node, 3) };
    assert!(matches!(
        served_by_a_collector(),
        Served::Batch { roots: 3, .. }
    ));
    let _ = testing::take_chain_figures();

    // Within the epoch the chain owes nothing, R is empty, and the round
    // leaves.
    let record = unsafe { &*record() };
    assert!(!crate::cycle::chain::is_due(
        record,
        serve_clock_now(),
        u64::MAX
    ));
    assert_eq!(served_by_a_collector(), Served::Idle);

    crate::cycle::epoch::turn_this_threads_cell();
    assert!(crate::cycle::chain::is_due(
        record,
        serve_clock_now(),
        u64::MAX
    ));
    assert!(matches!(
        served_by_a_collector(),
        Served::Batch { roots: 3, .. }
    ));
    let figures = testing::take_chain_figures();
    assert_eq!(
        (
            figures.roots_from_the_chain,
            figures.roots_from_r,
            figures.pushed_waiting
        ),
        (3, 0, 3),
        "read from the ready part, live again, back to waiting"
    );
    let (ready, waiting) = roots_of_this_threads();
    assert_eq!((ready.len(), waiting.len()), (0, 3));
    assert_eq!(state(byte()), FREE);

    unsafe { let_go(&kept) };
    reset();
}

#[test]
fn a_waiting_part_unchecked_for_the_term_is_served_with_its_stamps_fresh() {
    let _g = test_guard();
    reset();
    let node = node_class("ChainTermNode");
    let mut arena = Arena::new();
    let kept = unsafe { kept_roots(&mut arena, node, 1) };
    assert!(matches!(
        served_by_a_collector(),
        Served::Batch { roots: 1, .. }
    ));

    let record = unsafe { &*record() };
    let checked = record.chain_checked_at();
    testing::take_standing_after(Some(std::time::Duration::ZERO));
    let served = served_by_a_collector();
    testing::take_standing_after(None);
    assert_eq!(
        served,
        Served::Idle,
        "no root to trace: the stamps are fresh"
    );
    assert!(record.chain_checked_at() > checked, "but the check ran");
    assert_eq!(testing::take_chain_figures().headers_checked, 1);

    unsafe { let_go(&kept) };
    reset();
}

#[test]
fn a_chained_root_that_dies_is_posted_zero_count_and_the_disposition_frees_it() {
    let _g = test_guard();
    reset();
    let node = node_class("ChainDeathNode");
    let mut arena = Arena::new();
    let kept = unsafe { kept_roots(&mut arena, node, 2) };
    assert!(matches!(
        served_by_a_collector(),
        Served::Batch { roots: 2, .. }
    ));

    // The first keeper goes: its root's death completes in place, its slot
    // withheld by the candidate bit the chain's entry stands for.
    let withheld = crate::cycle::queue::withheld_by_an_entry();
    unsafe { release_keeper(kept[0].1) };
    assert_eq!(crate::cycle::queue::withheld_by_an_entry(), withheld + 1);

    testing::take_standing_after(Some(std::time::Duration::ZERO));
    let _ = served_by_a_collector();
    testing::take_standing_after(None);
    assert_eq!(
        byte(),
        NOTHING_PROPOSED,
        "the death is a verdict of its own"
    );
    assert_eq!(
        standing_verdicts(),
        vec![(kept[0].0, Verdict::ZeroCount)],
        "posted in the grant that found it"
    );
    assert_eq!(
        roots_of_this_threads().1,
        vec![kept[1].0],
        "taken out of the chain"
    );

    assert_eq!(unsafe { ll_gc_maybe_collect() }, 0);
    assert_eq!(state(byte()), FREE);
    assert_eq!(
        crate::cycle::queue::withheld_by_an_entry(),
        withheld,
        "the disposition returned the slot"
    );

    unsafe { let_go(&kept[1..]) };
    reset();
}

#[test]
fn a_death_p_had_no_room_for_is_the_next_checks_first_post() {
    let _g = test_guard();
    reset();
    let node = node_class("ChainNoRoomNode");
    let mut arena = Arena::new();
    let kept = unsafe { kept_roots(&mut arena, node, 3) };
    assert!(matches!(
        served_by_a_collector(),
        Served::Batch { roots: 3, .. }
    ));

    // Two chained roots die, and P holds one slot: the first death takes
    // it, the second is refused.
    unsafe {
        release_keeper(kept[0].1);
        release_keeper(kept[1].1);
        crate::cycle::queue::verdicts::testing::fill_for_test(crate::ring::BLOCK_ENTRIES - 1);
    }
    testing::take_standing_after(Some(std::time::Duration::ZERO));
    let _ = served_by_a_collector();
    assert_eq!(
        roots_of_this_threads().1,
        vec![kept[1].0, kept[2].0],
        "the refused death stays in the chain"
    );

    // The disposition empties P; the next check starts on the refused death
    // rather than past it.
    assert_eq!(unsafe { ll_gc_maybe_collect() }, 0);
    let _ = served_by_a_collector();
    testing::take_standing_after(None);
    assert_eq!(standing_verdicts(), vec![(kept[1].0, Verdict::ZeroCount)]);
    assert_eq!(roots_of_this_threads().1, vec![kept[2].0]);

    assert_eq!(unsafe { ll_gc_maybe_collect() }, 0);
    unsafe { let_go(&kept[2..]) };
    reset();
}

#[test]
fn beside_a_ready_part_r_takes_its_clamp_and_the_chain_up_to_half_the_bound() {
    let _g = test_guard();
    reset();
    let node = node_class("ChainShareNode");
    let mut arena = Arena::new();
    let chained = unsafe { kept_roots(&mut arena, node, 2 * INITIAL_BATCH) };
    // Batches put every root in the chain, and an advance makes it ready.
    while candidate_count() > 0 {
        assert!(matches!(served_by_a_collector(), Served::Batch { .. }));
    }
    crate::cycle::epoch::turn_this_threads_cell();
    unsafe { &*record() }.set_batch_size(INITIAL_BATCH);
    let in_r = unsafe { kept_roots(&mut arena, node, 2 * INITIAL_BATCH) };
    let _ = testing::take_chain_figures();

    assert!(matches!(served_by_a_collector(), Served::Batch { .. }));
    let figures = testing::take_chain_figures();
    assert_eq!(
        (figures.roots_from_r, figures.roots_from_the_chain),
        (INITIAL_BATCH, 2 * INITIAL_BATCH),
        "R its K = {INITIAL_BATCH}, the ready part all it holds, under half the bound"
    );

    unsafe {
        let_go(&chained);
        let_go(&in_r);
    }
    reset();
}

#[test]
fn where_p_holds_less_than_both_want_r_and_the_chain_share_it_in_halves() {
    let _g = test_guard();
    reset();
    let node = node_class("ChainRoomNode");
    let mut arena = Arena::new();
    let chained = unsafe { kept_roots(&mut arena, node, 40) };
    assert!(matches!(served_by_a_collector(), Served::Batch { .. }));
    crate::cycle::epoch::turn_this_threads_cell();
    let in_r = unsafe { kept_roots(&mut arena, node, 40) };
    unsafe {
        crate::cycle::queue::verdicts::testing::fill_for_test(crate::ring::BLOCK_ENTRIES - 10)
    };
    let _ = testing::take_chain_figures();

    assert!(matches!(served_by_a_collector(), Served::Batch { .. }));
    let figures = testing::take_chain_figures();
    assert_eq!(
        (figures.roots_from_r, figures.roots_from_the_chain),
        (5, 5),
        "ten slots of P, five each"
    );

    unsafe {
        let_go(&chained);
        let_go(&in_r);
    }
    reset();
}

#[test]
fn a_ready_part_alone_takes_the_whole_clamp() {
    let _g = test_guard();
    reset();
    let node = node_class("ChainAloneNode");
    let mut arena = Arena::new();
    let kept = unsafe { kept_roots(&mut arena, node, INITIAL_BATCH / 2) };
    assert!(matches!(served_by_a_collector(), Served::Batch { .. }));
    crate::cycle::epoch::turn_this_threads_cell();
    let _ = testing::take_chain_figures();

    assert!(matches!(served_by_a_collector(), Served::Batch { .. }));
    let figures = testing::take_chain_figures();
    assert_eq!(
        (figures.roots_from_r, figures.roots_from_the_chain),
        (0, INITIAL_BATCH / 2)
    );
    assert_eq!(
        unsafe { &*record() }.batch_size(),
        0,
        "the chain's roots size no K"
    );

    unsafe { let_go(&kept) };
    reset();
}

#[test]
fn the_round_serves_a_due_chain_whatever_r_reads() {
    let _g = test_guard();
    reset();
    let node = node_class("ChainRoundNode");
    let mut arena = Arena::new();
    let kept = unsafe { kept_roots(&mut arena, node, 1) };
    assert!(matches!(
        served_by_a_collector(),
        Served::Batch { roots: 1, .. }
    ));
    crate::cycle::epoch::turn_this_threads_cell();

    let record = unsafe { &*record() };
    assert_eq!(
        decide_the_branch_and_stamp_the_instant(
            record,
            None,
            record.merges(),
            64,
            serve_clock_now(),
            u64::MAX
        ),
        RingRound::Serves,
        "an empty R, and a chain the epoch came for"
    );

    unsafe { let_go(&kept) };
    reset();
}

#[test]
fn a_collection_under_pressure_finds_garbage_the_chain_held() {
    let _g = test_guard();
    reset();
    let node = node_class("ChainPressureNode");
    let mut arena = Arena::new();
    // A ring of two, both members registered, the first held by a keeper.
    let ring = unsafe { crate::cycle::testing::long_ring(&mut arena, node, 2) };
    let keeper = unsafe { object(&mut arena, keeper_class("ChainPressureKeeper")) };
    unsafe {
        crate::test_support::store_prop(
            &mut arena,
            keeper,
            crate::test_support::prop_offset(0),
            ring[0],
        );
    }
    assert!(matches!(
        served_by_a_collector(),
        Served::Batch { roots: 2, .. }
    ));
    assert_eq!(roots_of_this_threads().1.len(), 2, "read live, chained");

    // The keeper goes; the ring is garbage no decrement will register again.
    unsafe { release_keeper(keeper) };
    assert_eq!(candidate_count(), 0);
    assert_eq!(
        unsafe { crate::cycle::collect::collect_under_pressure() },
        2
    );
    assert_eq!(roots_of_this_threads(), (Vec::new(), Vec::new()));
    reset();
}

#[test]
fn the_exit_takes_the_chain_back_into_r_before_its_rounds() {
    let _g = test_guard();
    reset();
    let node = node_class("ChainExitNode");
    let mut arena = Arena::new();
    let kept = unsafe { kept_roots(&mut arena, node, 3) };
    assert!(matches!(
        served_by_a_collector(),
        Served::Batch { roots: 3, .. }
    ));
    assert_eq!(
        crate::cycle::queue::registered_by_lane()[1],
        3,
        "the exit's count reads the chain beside the lane"
    );

    // The keepers go, and the roots' deaths complete behind the chain's
    // entries; the exit's rounds take the chain into R and retire them.
    let withheld = crate::cycle::queue::withheld_by_an_entry();
    unsafe { let_go(&kept) };
    assert_eq!(crate::cycle::queue::withheld_by_an_entry(), withheld + 3);
    let _residue = unsafe { crate::cycle::collect::collect_before_exit() };
    let (ready, waiting) = roots_of_this_threads();
    assert_eq!(
        (
            ready.len(),
            waiting.len(),
            crate::cycle::queue::withheld_by_an_entry()
        ),
        (0, 0, withheld),
        "the rounds took the chain back and returned the slots"
    );
    reset();
}

#[test]
fn beside_a_ready_part_k_grows_only_on_what_r_filled() {
    let _g = test_guard();
    reset();
    let node = node_class("ChainKNode");
    let mut arena = Arena::new();
    let chained = unsafe { kept_roots(&mut arena, node, 8) };
    assert!(matches!(served_by_a_collector(), Served::Batch { .. }));
    crate::cycle::epoch::turn_this_threads_cell();
    let record = unsafe { &*record() };
    record.set_batch_size(INITIAL_BATCH);
    // R at the threshold, P's room cut below K: R takes what the room leaves
    // it, short of K, and K stands, as without the chain.
    let in_r = unsafe { kept_roots(&mut arena, node, 2 * INITIAL_BATCH) };
    unsafe {
        crate::cycle::queue::verdicts::testing::fill_for_test(
            crate::ring::BLOCK_ENTRIES - INITIAL_BATCH / 2,
        )
    };
    assert!(matches!(served_by_a_collector(), Served::Batch { .. }));
    assert_eq!(record.batch_size(), INITIAL_BATCH, "K stands");

    unsafe {
        let_go(&chained);
        let_go(&in_r);
    }
    reset();
}

#[test]
fn with_one_slot_of_p_r_takes_it() {
    let _g = test_guard();
    reset();
    let node = node_class("ChainOneSlotNode");
    let mut arena = Arena::new();
    let chained = unsafe { kept_roots(&mut arena, node, 4) };
    assert!(matches!(served_by_a_collector(), Served::Batch { .. }));
    crate::cycle::epoch::turn_this_threads_cell();
    let in_r = unsafe { kept_roots(&mut arena, node, 4) };
    unsafe {
        crate::cycle::queue::verdicts::testing::fill_for_test(crate::ring::BLOCK_ENTRIES - 1)
    };
    let _ = testing::take_chain_figures();

    assert!(matches!(
        served_by_a_collector(),
        Served::Batch { roots: 1, .. }
    ));
    let figures = testing::take_chain_figures();
    assert_eq!((figures.roots_from_r, figures.roots_from_the_chain), (1, 0));

    unsafe {
        let_go(&chained);
        let_go(&in_r);
    }
    reset();
}

#[test]
fn a_batch_that_proposes_a_set_beside_live_roots_publishes_its_live_list() {
    let _g = test_guard();
    reset();
    let node = node_class("ChainMixedNode");
    let mut arena = Arena::new();
    let kept = unsafe { kept_roots(&mut arena, node, 2) };
    // A garbage ring of two, both members registered.
    let _ring = unsafe { crate::cycle::testing::long_ring(&mut arena, node, 2) };

    assert!(matches!(
        served_by_a_collector(),
        Served::Batch { roots: 4, .. }
    ));
    assert_eq!(state(byte()), crate::cycle::token::POSTED);
    assert!(
        !unsafe { &*record() }.live_list().is_null(),
        "a batch that posted publishes its list, the live roots' cores in it"
    );
    assert_eq!(
        roots_of_this_threads().1.len(),
        2,
        "the live roots wait in the chain"
    );

    // The collection over P stamps from the list and frees the ring.
    assert_eq!(unsafe { ll_gc_maybe_collect() }, 2);
    assert!(unsafe { &*record() }.live_list().is_null());

    unsafe { let_go(&kept) };
    reset();
}
