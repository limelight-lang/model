//! A batch that proposed no set owes the mutator P's disposition and no trace
//! window: its release writes `NOTHING_PROPOSED`, the mutator's reading arms
//! [`Arming::Disposal`], and the poll answers every verdict of P without a
//! collection — completed deaths freed, live roots deferred, unwalked ones
//! written back — and releases `FREE`. A batch with a proposed root among its
//! verdicts releases `POSTED` and arms the collection over P, and the
//! disposal's arming outranks the retirement pass's and is outranked by both
//! collections'.
//!
//! The stand-in collector here is
//! `cycle::queue::verdicts::testing::post_batch_released_as_the_collector_does`,
//! which releases by the collector's rule.

use super::*;
use crate::cycle::queue::verdicts::testing::post_batch_released_as_the_collector_does;
use crate::cycle::queue::verdicts::verdict_count;
use crate::cycle::queue::{candidate_count, deferred_count};
use crate::cycle::token::{
    FREE, NOTHING_PROPOSED, POSTED, Reading, read_and_act_on_this_thread, state,
};
use crate::gc::Arming;

fn node_class(name: &str) -> *const Class {
    ClassBuilder::new(name).prop("next", true).build()
}

fn keeper_class(name: &str) -> *const Class {
    ClassBuilder::new(name).prop("kept", true).build()
}

fn byte() -> u8 {
    unsafe {
        (*crate::cycle::mutator_record::this_thread_record())
            .token
            .read()
    }
}

fn reset() {
    crate::cycle::queue::verdicts::discard_standing_verdicts();
    crate::cycle::queue::release_queue_segments();
    crate::gc::disarm();
}

/// One stand-in batch of `k` roots, each answered `verdict`, released as the
/// collector's batch is.
fn batch_posts(k: usize, verdict: Verdict) -> Posted {
    let record = Sent(crate::cycle::mutator_record::this_thread_record());
    std::thread::spawn(move || unsafe {
        post_batch_released_as_the_collector_does(record.into_inner(), k, |_| verdict)
    })
    .join()
    .expect("the stand-in finished")
}

/// One live root held by a keeper, registered in R by the release the
/// keeper's hold survives. Answers the keeper.
unsafe fn kept_root(arena: &mut Arena, node: *const Class, name: &str) -> *mut Object {
    let mut context = LLContext { arena: &mut *arena };
    let root = unsafe { new_constructed(&mut context, node, MemoryCategory::GcHeap) };
    let keeper =
        unsafe { new_constructed(&mut context, keeper_class(name), MemoryCategory::GcHeap) };
    unsafe {
        store_prop(arena, keeper, prop_offset(0), root);
        assert!(!ll_release(root as *mut RcHeader), "the keeper holds it");
    }
    keeper
}

unsafe fn let_go(keepers: &[*mut Object]) {
    for &keeper in keepers {
        unsafe {
            assert!(ll_release(keeper as *mut RcHeader));
            ll_object_die(keeper);
        }
    }
}

/// A candidate whose death completes in place: registered by the first
/// decrement, dead at the second, its slot withheld by its entry in R.
unsafe fn completed_death(arena: &mut Arena, node: *const Class) -> *mut Object {
    let mut context = LLContext { arena };
    let object = unsafe { new_constructed(&mut context, node, MemoryCategory::GcHeap) };
    unsafe {
        ll_retain(object as *mut RcHeader);
        assert!(!ll_release(object as *mut RcHeader));
        assert!(ll_release(object as *mut RcHeader));
        ll_object_die(object);
    }
    object
}

#[test]
fn a_batch_of_live_roots_releases_nothing_proposed_and_the_poll_defers_them_with_no_window() {
    let _g = test_guard();
    reset();
    assert!(crate::cycle::queue::refill_spares());
    let node = node_class("NothingProposedLiveNode");
    let mut arena = Arena::new();
    let keepers = [
        unsafe { kept_root(&mut arena, node, "NothingProposedKeeperA") },
        unsafe { kept_root(&mut arena, node, "NothingProposedKeeperB") },
    ];
    assert_eq!(batch_posts(2, Verdict::ReadLive), Posted::Batch(2));
    assert_eq!(byte(), NOTHING_PROPOSED);
    assert_eq!(
        state(byte()),
        POSTED,
        "every reader of the state reads POSTED"
    );

    // The reading arms the disposition, and writes nothing.
    assert_eq!(read_and_act_on_this_thread(), Reading::Posted);
    assert_eq!(crate::gc::arming(), Arming::Disposal);
    assert_eq!(byte(), NOTHING_PROPOSED);

    let (collections, disposals) = (
        crate::gc::verdict_collections_on_this_thread(),
        crate::gc::disposals_on_this_thread(),
    );
    assert_eq!(unsafe { ll_gc_maybe_collect() }, 0);
    assert_eq!(
        (
            crate::gc::verdict_collections_on_this_thread() - collections,
            crate::gc::disposals_on_this_thread() - disposals,
        ),
        (0, 1),
        "one disposition and no collection over P"
    );
    assert_eq!(state(byte()), FREE);
    assert_eq!(verdict_count(), 0, "P's front advanced past both");
    assert_eq!(deferred_count(), 2, "read live, they wait for the epoch");
    assert_eq!(crate::gc::arming(), Arming::None);

    unsafe { let_go(&keepers) };
    reset();
}

#[test]
fn the_disposition_frees_a_completed_death_the_batch_read() {
    let _g = test_guard();
    reset();
    assert!(crate::cycle::queue::refill_spares());
    let node = node_class("NothingProposedDeathNode");
    let mut arena = Arena::new();
    let withheld = crate::cycle::queue::withheld_by_an_entry();
    let _ = unsafe { completed_death(&mut arena, node) };
    assert_eq!(candidate_count(), 1);
    assert_eq!(
        crate::cycle::queue::withheld_by_an_entry() - withheld,
        1,
        "its entry withholds the slot"
    );

    assert_eq!(batch_posts(1, Verdict::ZeroCount), Posted::Batch(1));
    assert_eq!(byte(), NOTHING_PROPOSED);
    assert_eq!(unsafe { ll_gc_maybe_collect() }, 0);
    assert_eq!(state(byte()), FREE);
    assert_eq!(verdict_count(), 0);
    assert_eq!(
        crate::cycle::queue::withheld_by_an_entry(),
        withheld,
        "the disposition returned the slot"
    );
    reset();
}

#[test]
fn the_disposition_writes_an_unwalked_root_back_into_r() {
    let _g = test_guard();
    reset();
    assert!(crate::cycle::queue::refill_spares());
    let node = node_class("NothingProposedUnwalkedNode");
    let mut arena = Arena::new();
    let keeper = unsafe { kept_root(&mut arena, node, "NothingProposedUnwalkedKeeper") };
    assert_eq!(batch_posts(1, Verdict::Unwalked), Posted::Batch(1));
    assert_eq!(candidate_count(), 0);
    assert_eq!(byte(), NOTHING_PROPOSED);

    assert_eq!(unsafe { ll_gc_maybe_collect() }, 0);
    assert_eq!(state(byte()), FREE);
    assert_eq!(
        (verdict_count(), candidate_count(), deferred_count()),
        (0, 1, 0),
        "back in R untraced, for the next batch"
    );

    unsafe { let_go(&[keeper]) };
    reset();
}

#[test]
fn a_batch_that_proposed_a_set_releases_posted() {
    let _g = test_guard();
    reset();
    assert!(crate::cycle::queue::refill_spares());
    let node = node_class("NothingProposedMixedNode");
    let mut arena = Arena::new();
    let keepers = [
        unsafe { kept_root(&mut arena, node, "NothingProposedMixedKeeperA") },
        unsafe { kept_root(&mut arena, node, "NothingProposedMixedKeeperB") },
    ];
    let mut first = true;
    let record = Sent(crate::cycle::mutator_record::this_thread_record());
    let posted = std::thread::spawn(move || unsafe {
        post_batch_released_as_the_collector_does(record.into_inner(), 2, |_| {
            let verdict = if first {
                Verdict::ReadLive
            } else {
                Verdict::Proposed
            };
            first = false;
            verdict
        })
    })
    .join()
    .expect("the stand-in finished");
    assert_eq!(posted, Posted::Batch(2));
    assert_eq!(byte(), POSTED);
    assert_eq!(read_and_act_on_this_thread(), Reading::Posted);
    assert_eq!(crate::gc::arming(), Arming::Verdicts);

    // The collection over P validates the proposed root exactly, reads it
    // live, and disposes of P whole.
    assert_eq!(unsafe { ll_gc_maybe_collect() }, 0);
    assert_eq!(state(byte()), FREE);
    assert_eq!(verdict_count(), 0);

    unsafe { let_go(&keepers) };
    reset();
}

#[test]
fn the_disposal_outranks_the_retirement_pass_and_is_outranked_by_the_collections() {
    let _g = test_guard();
    reset();
    crate::gc::arm_to_retire();
    crate::gc::arm_for_the_disposal();
    assert_eq!(crate::gc::arming(), Arming::Disposal);
    crate::gc::arm_to_retire();
    assert_eq!(
        crate::gc::arming(),
        Arming::Disposal,
        "a pass does not lower it"
    );
    crate::gc::arm_for_the_verdicts();
    assert_eq!(crate::gc::arming(), Arming::Verdicts);
    crate::gc::arm_for_the_disposal();
    assert_eq!(
        crate::gc::arming(),
        Arming::Verdicts,
        "nor does a disposal a collection"
    );
    crate::gc::arm();
    crate::gc::arm_for_the_disposal();
    assert_eq!(crate::gc::arming(), Arming::AllRoots);
    reset();
}
