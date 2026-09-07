//! What a collection does, which of them run, and what the two paths through
//! it differ by.
//!
//! Every property here is the driver's rather than any one module's below it:
//! a garbage ring reaches the frees through the whole order, an armed poll is
//! the only poll that fires, a collection reached from inside a collection is
//! refused instead of opening a second window over the first one's rows, and
//! the path under pressure comes back for what its harvest region could not
//! hold — or arms the thread where no bound on the roots would make it fit.

use super::*;
use crate::class::{Class, ClassBuilder};
use crate::cycle::members::MEMBER_CAPACITY;
use crate::cycle::testing::ring;
use crate::gc::{ll_gc_collect_cycles, ll_gc_maybe_collect};
use crate::memory::arena::Arena;
use crate::memory::block_pool::test_guard;
use crate::memory::context::LLContext;
use crate::object::{Object, new_constructed};
use crate::refcount::{MemoryCategory, RcHeader, SlotState, ll_release, slot_state};
use crate::test_support::{prop_offset, store_prop};
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

/// A ring of `MEMBERS` objects of `class`, hand-built because the shared
/// fixture takes its size at compile time and these cases need one the
/// harvest region cannot hold ([`ring`], and `crate::cycle::testing`).
///
/// Every creation reference is spent, so the ring is held by its own edges and
/// every member comes back registered as a candidate — the state a root of a
/// real collection is in.
///
/// # Safety
/// As [`ring`]: a quiescent heap under `memory::block_pool::test_guard`, and
/// `class` carries one Box property at `prop_offset(0)`.
unsafe fn long_ring(arena: &mut Arena, class: *const Class, members: usize) -> Vec<*mut Object> {
    let mut context = LLContext { arena: &mut *arena };
    let ring: Vec<*mut Object> = (0..members)
        .map(|_| unsafe { new_constructed(&mut context, class, MemoryCategory::GcHeap) })
        .collect();

    unsafe {
        for (index, &member) in ring.iter().enumerate() {
            store_prop(arena, member, prop_offset(0), ring[(index + 1) % members]);
        }

        for &member in &ring {
            assert!(
                !ll_release(member as *mut RcHeader),
                "an edge of the ring holds this member"
            );
        }
    }

    ring
}

/// The population the harvest region cannot hold in one reading: the driver
/// traces again over half the roots, tears down what fits, and comes back for
/// the rest on the memory the first teardown returned. What it must not do is
/// leave any of it behind.
#[test]
fn a_population_past_the_harvest_region_is_collected_over_several_traces() {
    let _g = test_guard();
    let class = node_class("CollectPressurePairNode", counting_destructor as *const ());
    let mut arena = Arena::new();

    // Rings of two rather than one ring, so that a bound on the roots is a
    // bound on the members reached: each ring's closure is its own pair, and
    // half the roots reach half the population.
    let pairs = MEMBER_CAPACITY as usize;
    let mut members = Vec::with_capacity(pairs * 2);
    for _ in 0..pairs {
        members.extend_from_slice(&unsafe { ring(&mut arena, [class, class]) });
    }

    assert!(
        members.len() > MEMBER_CAPACITY as usize,
        "the fixture is past what one harvest can hold"
    );
    DESTRUCTOR_RUNS.store(0, Ordering::Relaxed);

    assert_eq!(
        unsafe { collect_under_pressure() },
        members.len(),
        "every member was freed, over as many traces as the region needed"
    );
    assert_eq!(
        DESTRUCTOR_RUNS.load(Ordering::Relaxed),
        members.len(),
        "and each of them ran its destructor once"
    );

    for &member in &members {
        assert_eq!(
            unsafe { slot_state(member as *mut RcHeader) },
            SlotState::DeadInPlace
        );
    }

    assert_eq!(
        unsafe { ll_gc_collect_cycles() },
        0,
        "and nothing of the population was left behind"
    );
}

/// One component past the region's capacity: no bound on the roots makes it
/// fit, because a single root reaches the whole of it. The loop halves down to
/// one root, gives up, and arms the thread — the poll's collection keeps its
/// rows and has no region to overflow.
#[test]
fn a_component_past_the_region_ends_the_pressure_path_and_arms_the_thread() {
    let _g = test_guard();
    let class = node_class("CollectPressureLongNode", counting_destructor as *const ());
    let mut arena = Arena::new();
    let members = unsafe { long_ring(&mut arena, class, MEMBER_CAPACITY as usize + 1) };
    DESTRUCTOR_RUNS.store(0, Ordering::Relaxed);
    crate::gc::disarm();

    assert_eq!(
        unsafe { collect_under_pressure() },
        0,
        "the pressure path frees nothing it cannot hold the membership of"
    );
    assert_eq!(
        DESTRUCTOR_RUNS.load(Ordering::Relaxed),
        0,
        "and it tears nothing down on the way"
    );
    assert!(
        crate::gc::is_armed(),
        "it arms the thread instead, the poll's collection having no region to \
         overflow"
    );
    assert_eq!(
        unsafe { slot_state(members[0] as *mut RcHeader) },
        SlotState::Live,
        "the component stands, every registration with it"
    );

    assert_eq!(
        unsafe { ll_gc_maybe_collect() },
        members.len(),
        "and the poll it armed collects the whole of it"
    );
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
