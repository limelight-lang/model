//! What a collection off the poll does, and when it runs at all.
//!
//! Three properties, and each of them is the driver's rather than any one
//! module's below it: a garbage ring reaches the frees through the whole
//! order, an armed poll is the only poll that fires, and a collection reached
//! from inside a collection is refused instead of opening a second window over
//! the first one's rows.

use super::*;
use crate::class::{Class, ClassBuilder};
use crate::cycle::testing::ring;
use crate::gc::{ll_gc_collect_cycles, ll_gc_maybe_collect};
use crate::memory::arena::Arena;
use crate::memory::block_pool::test_guard;
use crate::object::Object;
use crate::refcount::{RcHeader, SlotState, slot_state};
use std::sync::atomic::{AtomicUsize, Ordering};

/// Destructor bodies run since a case last cleared it.
static DESTRUCTOR_RUNS: AtomicUsize = AtomicUsize::new(0);

unsafe extern "C" fn counting_destructor(_object: *mut Object) {
    DESTRUCTOR_RUNS.fetch_add(1, Ordering::Relaxed);
}

/// Polls made from inside a destructor, and what each of them answered.
static NESTED_ANSWERS: AtomicUsize = AtomicUsize::new(0);
static NESTED_CALLS: AtomicUsize = AtomicUsize::new(0);

/// A destructor that asks for a collection while one is running, which is the
/// re-entry the allocation path can reach for real: user code allocates, an
/// allocation arms, and a poll follows.
unsafe extern "C" fn collecting_destructor(_object: *mut Object) {
    NESTED_CALLS.fetch_add(1, Ordering::Relaxed);
    crate::gc::arm();
    NESTED_ANSWERS.fetch_add(unsafe { ll_gc_maybe_collect() }, Ordering::Relaxed);
}

/// A class with one counted Box property at `prop_offset(0)`, which is what
/// [`ring`] links its members through, and the destructor the case wants.
fn node_class(name: &str, destructor: *const ()) -> *const Class {
    ClassBuilder::new(name)
        .prop("next", true)
        .destructor(destructor)
        .build()
}

/// Every member of a ring nothing holds is freed by one collection, which
/// answers their number, and each of them runs its destructor once on the way.
#[test]
fn a_ring_nothing_holds_is_collected_whole() {
    let _g = test_guard();
    let class = node_class("CollectRingNode", counting_destructor as *const ());
    let mut arena = Arena::new();
    let members = unsafe { ring(&mut arena, [class, class, class]) };
    DESTRUCTOR_RUNS.store(0, Ordering::Relaxed);

    assert_eq!(
        unsafe { ll_gc_collect_cycles() },
        3,
        "the collection answers the members it freed"
    );
    assert_eq!(
        DESTRUCTOR_RUNS.load(Ordering::Relaxed),
        3,
        "and every member ran its destructor once"
    );

    for &member in &members {
        assert_eq!(
            unsafe { slot_state(member as *mut RcHeader) },
            SlotState::DeadInPlace,
            "the member was freed, and its slot waits for its queue entry to \
             be retired"
        );
    }

    // A second collection over the same lane: the entries the merge put back
    // name freed slots, and the trace passes over each of them.
    assert_eq!(
        unsafe { ll_gc_collect_cycles() },
        0,
        "a lane of entries naming dead slots proposes nothing"
    );
}

/// The deferred fire: an unarmed poll collects nothing whatever stands in the
/// lane, and the same poll after an arming collects it. What the pair rules out
/// is a collection at a moment nobody chose — inside `ll_release`, where the
/// mutation is half made, or inside a teardown, where the dying entity is still
/// a root (`rfc/model/gc/cycle/questions.md`, Y14).
#[test]
fn an_armed_poll_fires_and_an_unarmed_one_does_not() {
    let _g = test_guard();
    let class = node_class("CollectDeferredNode", counting_destructor as *const ());
    let mut arena = Arena::new();
    let members = unsafe { ring(&mut arena, [class, class]) };
    DESTRUCTOR_RUNS.store(0, Ordering::Relaxed);

    // The ring's own releases may have armed this thread — the queue's growth
    // draws, and a draw arms — so the flag is read and cleared rather than
    // assumed down.
    assert_eq!(
        unsafe { ll_gc_maybe_collect() },
        0,
        "a poll collects nothing while the garbage is younger than the arming"
    );
    assert!(!crate::gc::is_armed(), "and the poll left the flag down");

    assert_eq!(
        unsafe { ll_gc_maybe_collect() },
        0,
        "an unarmed poll runs no collection at all"
    );
    assert_eq!(
        DESTRUCTOR_RUNS.load(Ordering::Relaxed),
        0,
        "so the ring stands, destructors and all"
    );
    assert_eq!(
        unsafe { slot_state(members[0] as *mut RcHeader) },
        SlotState::Live
    );

    crate::gc::arm();
    assert_eq!(
        unsafe { ll_gc_maybe_collect() },
        2,
        "and the armed poll collects the ring that was there all along"
    );
    assert!(!crate::gc::is_armed(), "the fire disarms the thread");
}

/// A collection asked for from inside a destructor of a collection already
/// running answers zero and opens nothing. The outer collection is unaffected:
/// it holds the rows the inner one would have traced, and a second window on
/// one thread ends the process.
#[test]
fn a_collection_reached_from_a_destructor_is_refused() {
    let _g = test_guard();
    let class = node_class("CollectReentrantNode", collecting_destructor as *const ());
    let mut arena = Arena::new();
    let members = unsafe { ring(&mut arena, [class, class]) };
    NESTED_CALLS.store(0, Ordering::Relaxed);
    NESTED_ANSWERS.store(0, Ordering::Relaxed);

    assert_eq!(
        unsafe { ll_gc_collect_cycles() },
        2,
        "the outer collection frees its ring"
    );
    assert_eq!(
        NESTED_CALLS.load(Ordering::Relaxed),
        2,
        "each destructor asked for a collection of its own"
    );
    assert_eq!(
        NESTED_ANSWERS.load(Ordering::Relaxed),
        0,
        "and each was refused rather than served"
    );

    for &member in &members {
        assert_eq!(
            unsafe { slot_state(member as *mut RcHeader) },
            SlotState::DeadInPlace
        );
    }

    // The refusal is the flag falling with the collection rather than with the
    // process: the next collection of this thread runs.
    assert!(CollectingThread::take().is_some());
}
