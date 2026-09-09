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
use crate::memory::block_pool::{force_oom, test_guard};
use crate::memory::context::LLContext;
use crate::object::{Object, new_constructed};
use crate::refcount::{MemoryCategory, RcHeader, SlotState, ll_release, ll_retain, slot_state};
use crate::test_support::{prop_offset, store_prop};
use std::sync::atomic::{AtomicUsize, Ordering};

#[test]
fn a_completed_collection_retires_its_dead_candidates() {
    let _g = test_guard();
    crate::cycle::queue::release_queue_segments();
    let mut arena = Arena::new();
    let class = node_class("RetiredRing", counting_destructor as *const ());
    let _ring = unsafe { ring(&mut arena, [class; 3]) };
    assert_eq!(unsafe { ll_gc_collect_cycles() }, 3);
    assert_eq!(crate::cycle::queue::candidate_count(), 0);
}

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

/// A bound that covers only roots this path cannot free is not a reading of an
/// empty heap. The lane here opens with a live root, so every halving keeps it
/// first and the bounded rounds meet nothing unreachable at all; the ring
/// behind it is neither freed nor lost, and the thread is armed for the poll
/// that can take it.
#[test]
fn a_bounded_round_that_frees_nothing_hands_the_rest_to_the_poll() {
    let _g = test_guard();
    let class = node_class(
        "CollectPressureLiveFirstNode",
        counting_destructor as *const (),
    );
    let mut arena = Arena::new();

    // A registered candidate that is not garbage: the second reference is this
    // frame's, so the non-final decrement registers it and leaves it live. It
    // is the oldest record of the lane, which is where every prefix starts.
    let mut context = LLContext { arena: &mut arena };
    let live = unsafe { new_constructed(&mut context, class, MemoryCategory::GcHeap) };
    unsafe { ll_retain(live as *mut RcHeader) };
    assert!(!unsafe { ll_release(live as *mut RcHeader) });

    let members = unsafe { long_ring(&mut arena, class, MEMBER_CAPACITY as usize + 1) };
    DESTRUCTOR_RUNS.store(0, Ordering::Relaxed);
    crate::gc::disarm();

    assert_eq!(
        unsafe { collect_under_pressure() },
        0,
        "no bound over this lane holds the ring's membership"
    );
    assert!(
        crate::gc::is_armed(),
        "so the collection hands the rest to the poll rather than reading its \
         own bound as an empty heap"
    );
    assert_eq!(
        unsafe { slot_state(members[0] as *mut RcHeader) },
        SlotState::Live,
        "and the ring stands, every registration with it"
    );

    assert_eq!(
        unsafe { ll_gc_maybe_collect() },
        members.len(),
        "the poll it armed collects the ring and leaves the live root alone"
    );
    assert_eq!(
        unsafe { slot_state(live as *mut RcHeader) },
        SlotState::Live
    );

    unsafe {
        assert!(ll_release(live as *mut RcHeader));
        crate::object::ll_object_die(live);
    }
}

/// A component that cannot fit in the bounded harvest region even alone.
/// Retiring the pairs removes their roots;
/// the remaining component still exceeds the region even at a one-root bound.
/// The poll keeps rows rather than harvesting and can collect it.
#[test]
fn a_component_past_the_harvest_capacity_is_left_for_the_poll() {
    let _g = test_guard();
    let class = node_class(
        "CollectPressureDeadPrefixNode",
        counting_destructor as *const (),
    );
    let mut arena = Arena::new();

    // Exactly what one harvest holds, registered first, and one component past
    // it behind them: the first bound frees the pairs, whose entries retire
    // before the next round tries the remaining component.
    let pairs = MEMBER_CAPACITY as usize / 2;
    let mut members = Vec::with_capacity(pairs * 2);
    for _ in 0..pairs {
        members.extend_from_slice(&unsafe { ring(&mut arena, [class, class]) });
    }

    let ring = unsafe { long_ring(&mut arena, class, MEMBER_CAPACITY as usize + 1) };
    DESTRUCTOR_RUNS.store(0, Ordering::Relaxed);
    crate::gc::disarm();

    assert_eq!(
        unsafe { collect_under_pressure() },
        members.len(),
        "the pairs are freed, and the component past the region is not"
    );
    assert!(
        crate::gc::is_armed(),
        "the remaining component exceeds the region even at a one-root bound"
    );
    assert_eq!(
        unsafe { slot_state(ring[0] as *mut RcHeader) },
        SlotState::Live
    );

    assert_eq!(
        unsafe { ll_gc_maybe_collect() },
        ring.len(),
        "and the poll it armed collects the component"
    );
}

/// A collection the memory manager refuses reads no lane, so it proves nothing
/// about one and hands it to the poll the way a bounded round does. Staged at
/// the first allocation a collection makes — the workspace, which a thread's
/// first collection draws — because every refusal past it ends the same way.
#[test]
fn a_refused_collection_hands_the_lane_to_the_poll() {
    let _g = test_guard();

    let (freed, armed) = std::thread::spawn(|| {
        assert!(
            crate::memory::heap::ll_thread_init(),
            "the pool served this thread"
        );

        let class = node_class("CollectRefusedNode", counting_destructor as *const ());
        let mut arena = Arena::new();
        let members = unsafe { ring(&mut arena, [class, class]) };
        crate::gc::disarm();

        let oom = force_oom();
        let freed = unsafe { collect_under_pressure() };
        drop(oom);

        let armed = crate::gc::is_armed();
        assert_eq!(
            unsafe { slot_state(members[0] as *mut RcHeader) },
            SlotState::Live,
            "the ring stands, the collection having read nothing"
        );

        assert_eq!(
            unsafe { ll_gc_maybe_collect() },
            members.len(),
            "and the poll it armed collects it once the pool serves again"
        );

        (freed, armed)
    })
    .join()
    .unwrap();

    assert_eq!(freed, 0, "a refused collection frees nothing");
    assert!(
        armed,
        "and arms the thread rather than reporting an empty lane"
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

/// A collection asked for from inside an arena reset answers zero. The reset
/// is the other place this crate runs user destructors, and between
/// `promote::retain_block` and `promote::place_survivor_lists` a promoted
/// survivor stands in a block stamped retained with no occupant list — the
/// state `memory::retained::register` forbids a trace to read.
///
/// The garbage ring is what makes the answer a verdict: an unguarded
/// collection here frees it and answers two, so a zero says the gate refused
/// rather than that there was nothing to collect.
#[test]
fn a_collection_reached_from_an_arena_reset_is_refused() {
    let _g = test_guard();
    // Built here rather than through `node_class`, which registers whatever
    // pointer it is handed: this ring wants no destructor at all.
    let garbage = ClassBuilder::new("CollectDuringResetGarbage")
        .prop("next", true)
        .build();
    let dying = node_class("CollectDuringResetNode", collecting_destructor as *const ());

    let mut arena = Arena::new();
    let ring = unsafe { ring(&mut arena, [garbage, garbage]) };
    let mut context = LLContext { arena: &mut arena };
    let in_arena = unsafe { new_constructed(&mut context, dying, MemoryCategory::RequestArena) };
    NESTED_CALLS.store(0, Ordering::Relaxed);
    NESTED_ANSWERS.store(0, Ordering::Relaxed);

    unsafe { crate::promote::arena_reset_full(&mut arena) };

    assert_eq!(
        NESTED_CALLS.load(Ordering::Relaxed),
        1,
        "the arena entity's destructor asked for a collection"
    );
    assert_eq!(
        NESTED_ANSWERS.load(Ordering::Relaxed),
        0,
        "and the reset refused it, with a ring standing that an unguarded one would free"
    );

    // The refusal ends with the reset, so the ring is still collectable.
    assert_eq!(
        unsafe { ll_gc_collect_cycles() },
        2,
        "and the collection the reset refused runs once the reset is over"
    );
    let _ = (ring, in_arena);
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

mod what_a_collection_asks_the_allocator;
