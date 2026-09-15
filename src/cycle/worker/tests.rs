//! The handoff between an owner's poll and a collector thread, driven from a
//! thread that stands in for the collector: the poll offers its lane on a
//! request and only then, a worker takes the offer under the token, traces
//! it through the collector's reader, marks and posts, and the owner's next
//! poll collects what was proposed while the roots read live are back in the
//! lane. An offer a worker never took is the owner's again before a
//! collection of its own and at its exit, and a posted chain the owner never
//! picked up is drained by the exit; under a closed gate a poll neither picks
//! up nor offers.

use super::*;
use crate::class::{Class, ClassBuilder};
use crate::cycle::testing::{Sent, ring};
use crate::gc::{ll_gc_collect_cycles, ll_gc_maybe_collect};
use crate::memory::arena::Arena;
use crate::memory::block_pool::test_guard;
use crate::object::Object;
use crate::refcount::{RcHeader, ll_release, ll_retain};
use std::sync::atomic::{AtomicUsize, Ordering};

fn node_class(name: &str, destructor: *const ()) -> *const Class {
    ClassBuilder::new(name)
        .prop("next", true)
        .destructor(destructor)
        .build()
}

static DESTRUCTORS: AtomicUsize = AtomicUsize::new(0);

unsafe extern "C" fn counting_destructor(_object: *mut Object) {
    DESTRUCTORS.fetch_add(1, Ordering::Relaxed);
}

/// What a destructor of [`gate_probing_destructor`]'s class read when it
/// polled from inside the collection: whether the proposal still stood and
/// whether an offer had been made.
static PROPOSAL_STOOD_INSIDE: AtomicUsize = AtomicUsize::new(0);
static OFFERED_INSIDE: AtomicUsize = AtomicUsize::new(0);

unsafe extern "C" fn gate_probing_destructor(_object: *mut Object) {
    DESTRUCTORS.fetch_add(1, Ordering::Relaxed);
    let record = owner_record::this_thread_record();
    unsafe {
        owner_record::request(record);
        ll_gc_maybe_collect();
        if owner_record::proposal_stands(record) {
            PROPOSAL_STOOD_INSIDE.fetch_add(1, Ordering::Relaxed);
        }
        if owner_record::offer_stands(record) {
            OFFERED_INSIDE.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// This thread's record, which the guard's initialisation drew.
fn record() -> *mut OwnerRecord {
    let record = owner_record::this_thread_record();
    assert!(
        !record.is_null(),
        "the guard's init drew this thread's record"
    );
    record
}

/// A ring of `members` counting nodes, every member registered.
unsafe fn garbage_ring<const MEMBERS: usize>(
    arena: &mut Arena,
    name: &str,
) -> [*mut Object; MEMBERS] {
    let class = node_class(name, counting_destructor as *const ());
    unsafe { ring(arena, [class; MEMBERS]) }
}

/// Run [`serve`] for this thread's record on a collector thread of its own,
/// and answer what it did.
fn served_by_a_collector_thread(record: *mut OwnerRecord) -> Served {
    let record = Sent(record);
    std::thread::spawn(move || {
        assert!(
            crate::memory::heap::ll_thread_init(),
            "the pool served the collector thread"
        );
        let served = unsafe { serve(record.into_inner()) };
        crate::memory::heap::ll_thread_exit();
        served
    })
    .join()
    .expect("the collector thread returned")
}

#[test]
fn a_poll_offers_its_lane_on_a_request_and_only_then() {
    let _g = test_guard();
    crate::cycle::queue::release_queue_segments();
    let mut arena = Arena::new();
    let record = record();
    let _ring = unsafe { garbage_ring::<3>(&mut arena, "OfferRing") };
    let registered = crate::cycle::queue::registered_count();
    assert_eq!(registered, 3);
    crate::gc::disarm();

    unsafe { ll_gc_maybe_collect() };
    assert!(
        !unsafe { owner_record::offer_stands(record) },
        "no request, no offer"
    );
    assert_eq!(crate::cycle::queue::registered_count(), 3);

    // A ring's registrations can draw the reserve and arm this thread, and an
    // armed poll fires rather than offers.
    crate::gc::disarm();
    unsafe { owner_record::request(record) };
    unsafe { ll_gc_maybe_collect() };
    assert!(
        unsafe { owner_record::offer_stands(record) },
        "the poll offered the lane"
    );
    assert!(
        !unsafe { owner_record::take_request(record) },
        "and spent the request"
    );
    assert_eq!(
        crate::cycle::queue::registered_count(),
        0,
        "the lane is empty: its chain is in the outbox"
    );

    // A ring's registrations can draw the reserve and arm this thread, and an
    // armed poll fires rather than offers.
    crate::gc::disarm();
    unsafe { owner_record::request(record) };
    unsafe { ll_gc_maybe_collect() };
    assert!(
        !unsafe { owner_record::take_request(record) },
        "a second request is spent too"
    );
    assert!(
        unsafe { owner_record::offer_stands(record) },
        "and the standing offer is the same one"
    );

    // The offer is reclaimed by a collection of this thread's own, and every
    // root of it is traced: the ring dies here.
    DESTRUCTORS.store(0, Ordering::Relaxed);
    let freed = unsafe { ll_gc_collect_cycles() };
    assert_eq!(freed, 3, "the reclaimed chain's ring was collected");
    assert_eq!(DESTRUCTORS.load(Ordering::Relaxed), 3);
    assert!(!unsafe { owner_record::offer_stands(record) });
}

#[test]
fn an_armed_poll_fires_instead_of_offering() {
    let _g = test_guard();
    crate::cycle::queue::release_queue_segments();
    let mut arena = Arena::new();
    let record = record();
    let _ring = unsafe { garbage_ring::<2>(&mut arena, "ArmedRing") };
    DESTRUCTORS.store(0, Ordering::Relaxed);
    unsafe { owner_record::request(record) };
    crate::gc::arm();
    let freed = unsafe { ll_gc_maybe_collect() };
    assert_eq!(freed, 2, "the fire traced the lane itself");
    assert!(
        !unsafe { owner_record::offer_stands(record) },
        "and nothing was offered"
    );
    assert!(
        unsafe { owner_record::take_request(record) },
        "the request stands for the next poll"
    );
}

#[test]
fn a_collector_thread_takes_traces_marks_and_posts_and_the_pickup_collects() {
    let _g = test_guard();
    crate::cycle::queue::release_queue_segments();
    let mut arena = Arena::new();
    let record = record();
    let garbage = unsafe { garbage_ring::<3>(&mut arena, "ProposedRing") };
    let live = unsafe { garbage_ring::<2>(&mut arena, "LiveRing") };
    // Held from outside for the whole case, so the trace reads it live and
    // the owner's reading agrees.
    unsafe { ll_retain(live[0] as *mut RcHeader) };
    let _ = garbage;

    // A ring's registrations can draw the reserve and arm this thread, and an
    // armed poll fires rather than offers.
    crate::gc::disarm();
    unsafe { owner_record::request(record) };
    unsafe { ll_gc_maybe_collect() };
    assert!(unsafe { owner_record::offer_stands(record) });

    let served = served_by_a_collector_thread(record);
    assert_eq!(
        served,
        Served::Posted {
            roots: 5,
            proposed: 3
        },
        "the three members of the garbage ring were proposed, the live ring's two read live"
    );
    assert!(
        !unsafe { (*record).token.is_held() },
        "the collector released the token after its post"
    );
    assert!(unsafe { owner_record::proposal_stands(record) });
    assert!(!unsafe { owner_record::offer_stands(record) });
    assert_eq!(
        crate::cycle::queue::registered_count(),
        0,
        "nothing is in the lane while the chain is out"
    );

    // Served again with nothing offered: a skip, and the token untouched.
    assert_eq!(served_by_a_collector_thread(record), Served::NothingOffered);

    DESTRUCTORS.store(0, Ordering::Relaxed);
    let census = crate::cycle::census::arm();
    let freed = unsafe { ll_gc_maybe_collect() };
    let report = crate::cycle::census::take();
    drop(census);
    assert_eq!(freed, 3, "the pickup collected the proposed ring");
    assert_eq!(DESTRUCTORS.load(Ordering::Relaxed), 3);
    assert_eq!(
        report.scan.expect("the pickup's scan was read").roots,
        3,
        "the pickup traced the proposed roots alone"
    );
    assert!(
        !unsafe { owner_record::proposal_stands(record) },
        "the inbox is empty after the pickup"
    );
    assert_eq!(
        crate::cycle::queue::registered_count(),
        2,
        "the live ring's roots are back in the lane, read live and not proposed"
    );
    assert_eq!(
        crate::cycle::queue::deferred_count(),
        0,
        "in the active lane: the pickup did not trace them, so nothing read them live"
    );

    unsafe { ll_release(live[0] as *mut RcHeader) };
    assert_eq!(
        unsafe { ll_gc_collect_cycles() },
        2,
        "and collectible once let go"
    );
}

#[test]
fn a_collector_thread_skips_a_held_token_and_an_unpicked_proposal() {
    let _g = test_guard();
    crate::cycle::queue::release_queue_segments();
    let mut arena = Arena::new();
    let record = record();
    let _ring = unsafe { garbage_ring::<2>(&mut arena, "SkipRing") };
    // A ring's registrations can draw the reserve and arm this thread, and an
    // armed poll fires rather than offers.
    crate::gc::disarm();
    unsafe { owner_record::request(record) };
    unsafe { ll_gc_maybe_collect() };

    let claim = crate::cycle::token::HeldToken::take();
    assert_eq!(served_by_a_collector_thread(record), Served::TokenHeld);
    drop(claim);
    assert!(
        unsafe { owner_record::offer_stands(record) },
        "the offer stands"
    );

    assert!(matches!(
        served_by_a_collector_thread(record),
        Served::Posted { roots: 2, .. }
    ));
    // A poll picks up before it offers, so an offer beside an unpicked
    // proposal is made here by hand, the way no poll makes one.
    let _second = unsafe { garbage_ring::<1>(&mut arena, "SkipRingTwo") };
    let by_hand = crate::cycle::queue::detach_candidates();
    assert!(unsafe { owner_record::offer(record, by_hand.into_word()) });
    assert_eq!(
        served_by_a_collector_thread(record),
        Served::ProposalUnpicked,
        "one chain in the inbox at a time"
    );
    assert!(
        !unsafe { (*record).token.is_held() },
        "the skip released the token"
    );
    assert!(unsafe { owner_record::offer_stands(record) });

    DESTRUCTORS.store(0, Ordering::Relaxed);
    assert_eq!(unsafe { ll_gc_maybe_collect() }, 2, "the pickup");
    assert_eq!(unsafe { ll_gc_collect_cycles() }, 1, "the reclaimed offer");
    assert_eq!(DESTRUCTORS.load(Ordering::Relaxed), 3);
}

#[test]
fn an_offer_is_reclaimed_before_a_pressure_collection() {
    let _g = test_guard();
    crate::cycle::queue::release_queue_segments();
    let mut arena = Arena::new();
    let record = record();
    let _ring = unsafe { garbage_ring::<3>(&mut arena, "PressureRing") };
    // A ring's registrations can draw the reserve and arm this thread, and an
    // armed poll fires rather than offers.
    crate::gc::disarm();
    unsafe { owner_record::request(record) };
    unsafe { ll_gc_maybe_collect() };
    assert!(unsafe { owner_record::offer_stands(record) });

    DESTRUCTORS.store(0, Ordering::Relaxed);
    let freed = unsafe { crate::cycle::collect::collect_under_pressure() };
    assert_eq!(
        freed, 3,
        "the pressure path took the offer back and traced it"
    );
    assert!(!unsafe { owner_record::offer_stands(record) });
}

#[test]
fn the_exit_takes_back_its_offer_and_drains_its_inbox() {
    let _g = test_guard();
    let (offered_residue, posted_residue) = crate::cycle::testing::on_a_fresh_thread(|| {
        let record = owner_record::this_thread_record();
        let mut arena = Arena::new();
        DESTRUCTORS.store(0, Ordering::Relaxed);
        let _ring = unsafe { garbage_ring::<3>(&mut arena, "ExitOfferRing") };
        unsafe { owner_record::request(record) };
        unsafe { ll_gc_maybe_collect() };
        assert!(unsafe { owner_record::offer_stands(record) });
        crate::memory::heap::ll_thread_exit();
        let offered = crate::cycle::collect::take_exit_residue().expect("the exit collected");

        assert!(crate::memory::heap::ll_thread_init());
        let record = owner_record::this_thread_record();
        let mut arena = Arena::new();
        let _ring = unsafe { garbage_ring::<2>(&mut arena, "ExitPostedRing") };
        unsafe { owner_record::request(record) };
        unsafe { ll_gc_maybe_collect() };
        assert!(matches!(
            served_by_a_collector_thread(record),
            Served::Posted { roots: 2, .. }
        ));
        assert!(unsafe { owner_record::proposal_stands(record) });
        crate::memory::heap::ll_thread_exit();
        let posted = crate::cycle::collect::take_exit_residue().expect("the exit collected");
        (offered, posted)
    });
    assert_eq!(
        offered_residue.freed, 3,
        "the exit reclaimed the offer and collected its ring"
    );
    assert_eq!(offered_residue.registered, 0);
    assert_eq!(
        posted_residue.freed, 2,
        "the exit drained the inbox and collected its ring"
    );
    assert_eq!(posted_residue.registered, 0);
    assert_eq!(DESTRUCTORS.load(Ordering::Relaxed), 5);
}

#[test]
fn an_in_line_collection_drains_the_inbox_before_it_traces() {
    let _g = test_guard();
    crate::cycle::queue::release_queue_segments();
    let mut arena = Arena::new();
    let record = record();
    let _ring = unsafe { garbage_ring::<3>(&mut arena, "DrainedRing") };
    crate::gc::disarm();
    unsafe { owner_record::request(record) };
    unsafe { ll_gc_maybe_collect() };
    assert!(matches!(
        served_by_a_collector_thread(record),
        Served::Posted { roots: 3, .. }
    ));
    assert!(unsafe { owner_record::proposal_stands(record) });

    // The explicit fire, and the pressure path below it, take the posted
    // chain into the lane before they detach it: a proposal is garbage a
    // collection short of memory would otherwise not see.
    DESTRUCTORS.store(0, Ordering::Relaxed);
    assert_eq!(
        unsafe { ll_gc_collect_cycles() },
        3,
        "the fire collected the posted ring"
    );
    assert!(!unsafe { owner_record::proposal_stands(record) });

    let _ring = unsafe { garbage_ring::<2>(&mut arena, "DrainedRingTwo") };
    crate::gc::disarm();
    unsafe { owner_record::request(record) };
    unsafe { ll_gc_maybe_collect() };
    assert!(matches!(
        served_by_a_collector_thread(record),
        Served::Posted { roots: 2, .. }
    ));
    assert_eq!(
        unsafe { crate::cycle::collect::collect_under_pressure() },
        2,
        "the pressure path collected the posted ring"
    );
    assert_eq!(DESTRUCTORS.load(Ordering::Relaxed), 5);
}

#[test]
fn a_chain_with_no_proposed_root_goes_back_without_a_window() {
    let _g = test_guard();
    crate::cycle::queue::release_queue_segments();
    let mut arena = Arena::new();
    let record = record();
    let live = unsafe { garbage_ring::<2>(&mut arena, "UnproposedRing") };
    unsafe { ll_retain(live[0] as *mut RcHeader) };
    crate::gc::disarm();
    unsafe { owner_record::request(record) };
    unsafe { ll_gc_maybe_collect() };
    assert_eq!(
        served_by_a_collector_thread(record),
        Served::Posted {
            roots: 2,
            proposed: 0
        }
    );

    let census = crate::cycle::census::arm();
    assert_eq!(unsafe { ll_gc_maybe_collect() }, 0);
    let report = crate::cycle::census::take();
    drop(census);
    assert!(
        report.scan.is_none(),
        "no window was opened for a chain with nothing to trace"
    );
    assert!(!unsafe { owner_record::proposal_stands(record) });
    assert_eq!(
        crate::cycle::queue::registered_count(),
        2,
        "both roots are back in the lane"
    );
    unsafe { ll_release(live[0] as *mut RcHeader) };
    assert_eq!(unsafe { ll_gc_collect_cycles() }, 2);
}

#[test]
fn a_trace_that_panics_still_posts_its_chain() {
    let _g = test_guard();
    crate::cycle::queue::release_queue_segments();
    let mut arena = Arena::new();
    let record = record();
    let _ring = unsafe { garbage_ring::<2>(&mut arena, "PanickingRing") };
    crate::gc::disarm();
    unsafe { owner_record::request(record) };
    unsafe { ll_gc_maybe_collect() };
    assert!(unsafe { owner_record::offer_stands(record) });

    // The guard of a taken chain, driven the way `serve` drives it, with a
    // panic where the trace would run.
    let record_word = record as usize;
    let outcome = std::panic::catch_unwind(move || {
        let record = record_word as *mut OwnerRecord;
        let taken = unsafe { TakenChain::take(record) }.expect("the offer stood");
        let _taken = taken;
        panic!("the trace panicked");
    });
    assert!(outcome.is_err());
    assert!(
        unsafe { owner_record::proposal_stands(record) },
        "the chain was posted from the unwind"
    );
    assert!(!unsafe { owner_record::offer_stands(record) });
    DESTRUCTORS.store(0, Ordering::Relaxed);
    assert_eq!(
        unsafe { ll_gc_collect_cycles() },
        2,
        "and every root of it is collected by the next in-line collection"
    );
}

#[test]
fn a_closed_gate_does_not_offer() {
    let _g = test_guard();
    crate::cycle::queue::release_queue_segments();
    let mut arena = Arena::new();
    let record = record();
    // A ring whose destructors poll from inside the collection that tears
    // it down, each setting a request first: the gate is closed there, and
    // the lane — which the teardown's own registrations are filling — is
    // not offered. Nothing can be posted for those polls to pick up: the
    // collection reclaimed the outbox before it took the token.
    let probing = node_class("GateProbing", gate_probing_destructor as *const ());
    let _probe = unsafe { ring(&mut arena, [probing; 2]) };
    DESTRUCTORS.store(0, Ordering::Relaxed);
    PROPOSAL_STOOD_INSIDE.store(0, Ordering::Relaxed);
    OFFERED_INSIDE.store(0, Ordering::Relaxed);
    let freed = unsafe { ll_gc_collect_cycles() };
    assert_eq!(freed, 2, "the probing ring was torn down");
    assert_eq!(DESTRUCTORS.load(Ordering::Relaxed), 2);
    assert_eq!(
        OFFERED_INSIDE.load(Ordering::Relaxed),
        0,
        "neither destructor's poll offered the lane"
    );
    assert_eq!(PROPOSAL_STOOD_INSIDE.load(Ordering::Relaxed), 0);
    assert!(
        unsafe { owner_record::take_request(record) },
        "the request stands for a poll at a clean point"
    );
}
