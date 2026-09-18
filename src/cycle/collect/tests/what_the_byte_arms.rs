//! The collector's batch arms the mutator's collection, through the token byte
//! (`rfc/dev/design/trace-token-handshake.md`, "The fourth round"): a batch
//! that posted leaves `POSTED`, the mutator's reading of it arms the
//! collection over P, that collection disposes of P whole on every ending
//! and releases `FREE`, a request is consented to at a slot free and at the
//! poll, and a byte at `POSTED` is a skip for the round.
//!
//! The stand-in collector is `cycle::queue::verdicts::testing::post_batch`
//! on a thread of its own, which claims, posts and releases as the
//! collector's batch does, less the trace.

use super::*;
use crate::cycle::queue::verdicts::testing::{Posted, post_batch};
use crate::cycle::queue::verdicts::{Verdict, verdict_count};
use crate::cycle::queue::{candidate_count, deferred_count};
use crate::cycle::testing::Sent;
use crate::cycle::token::{
    COLLECTOR, FREE, POSTED, Reading, read_and_act_on_this_thread, state, word,
};
use crate::cycle::worker::ELDER;
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

/// The stand-in posts `verdict` for the first `k` roots of R, from a thread
/// of its own, and answers what it did.
fn stand_in_posts(k: usize, verdict: Verdict) -> Posted {
    let record = Sent(crate::cycle::mutator_record::this_thread_record());
    std::thread::spawn(move || unsafe { post_batch(record.into_inner(), k, |_| verdict) })
        .join()
        .expect("the stand-in finished")
}

fn reset() {
    crate::cycle::queue::verdicts::discard_standing_verdicts();
    crate::cycle::queue::release_queue_segments();
    crate::gc::disarm();
}

/// One live root held by a keeper, registered in R by the release the
/// keeper's hold survives.
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

#[test]
fn a_batch_of_read_live_verdicts_alone_is_disposed_of_by_the_collection_it_arms() {
    let _g = test_guard();
    reset();
    assert!(crate::cycle::queue::refill_spares());
    let node = node_class("TriggerLiveNode");
    let mut arena = Arena::new();
    let keepers = [
        unsafe { kept_root(&mut arena, node, "TriggerKeeperA") },
        unsafe { kept_root(&mut arena, node, "TriggerKeeperB") },
    ];
    assert_eq!(candidate_count(), 2);

    assert_eq!(stand_in_posts(2, Verdict::ReadLive), Posted::Batch(2));
    assert_eq!(state(byte()), POSTED, "the batch that posted left POSTED");
    assert_eq!(candidate_count(), 0, "the batch took both roots out of R");

    // One poll: the reading arms the collection over P, which proposes
    // nothing, defers both roots and releases `FREE`.
    assert_eq!(unsafe { ll_gc_maybe_collect() }, 0);
    assert_eq!(state(byte()), FREE);
    assert_eq!(verdict_count(), 0, "P's front advanced past both");
    assert_eq!(deferred_count(), 2, "read live, they wait for the epoch");
    assert_eq!(crate::gc::arming(), Arming::None);

    for keeper in keepers {
        unsafe {
            assert!(ll_release(keeper as *mut RcHeader));
            ll_object_die(keeper);
        }
    }
    reset();
}

#[test]
fn the_reading_arms_for_the_verdicts_and_an_arming_for_r_outranks_it() {
    let _g = test_guard();
    reset();
    assert!(crate::cycle::queue::refill_spares());
    let node = node_class("TriggerArmNode");
    let mut arena = Arena::new();
    let keeper = unsafe { kept_root(&mut arena, node, "TriggerArmKeeper") };
    assert_eq!(stand_in_posts(1, Verdict::ReadLive), Posted::Batch(1));

    assert_eq!(read_and_act_on_this_thread(), Reading::Posted);
    assert_eq!(crate::gc::arming(), Arming::Verdicts);
    assert_eq!(state(byte()), POSTED, "the reading writes nothing");
    // A second reading arms again, idempotently.
    assert_eq!(read_and_act_on_this_thread(), Reading::Posted);
    assert_eq!(crate::gc::arming(), Arming::Verdicts);
    // An arming for R whole outranks it and is not lowered by a reading.
    crate::gc::arm();
    assert_eq!(read_and_act_on_this_thread(), Reading::Posted);
    assert_eq!(crate::gc::arming(), Arming::AllRoots);

    // The collection over R whole disposes of P in it and releases `FREE`.
    assert_eq!(unsafe { ll_gc_maybe_collect() }, 0);
    assert_eq!(state(byte()), FREE);
    assert_eq!(verdict_count(), 0);
    assert_eq!(crate::gc::arming(), Arming::None);

    unsafe {
        assert!(ll_release(keeper as *mut RcHeader));
        ll_object_die(keeper);
    }
    reset();
}

#[test]
fn a_retirement_pass_holds_at_posted_and_the_poll_after_it_frees_the_byte() {
    let _g = test_guard();
    reset();
    assert!(crate::cycle::queue::refill_spares());
    let node = node_class("TriggerDeathNode");
    let mut arena = Arena::new();
    let mut context = LLContext { arena: &mut arena };
    // A candidate whose death completes in place: registered by the first
    // decrement, dead at the second, its slot withheld until a retirement.
    let object = unsafe { new_constructed(&mut context, node, MemoryCategory::GcHeap) };
    unsafe {
        ll_retain(object as *mut RcHeader);
        assert!(!ll_release(object as *mut RcHeader));
        assert_eq!(candidate_count(), 1);
        assert!(ll_release(object as *mut RcHeader));
        ll_object_die(object);
    }

    assert_eq!(stand_in_posts(1, Verdict::ZeroCount), Posted::Batch(1));
    assert_eq!(state(byte()), POSTED);

    // The teardown-refusal retirement pass's form: the byte is left as it
    // is, the completed death is retired in place, P's front stays.
    {
        let _hold = crate::cycle::token::HeldToken::take_or_hold_posted();
        assert_eq!(state(byte()), POSTED, "held at POSTED, unswapped");
        unsafe { crate::cycle::queue::retire_candidates() };
    }
    assert_eq!(state(byte()), POSTED, "and left standing after the pass");
    assert_eq!(
        verdict_count(),
        1,
        "the entry is nulled in place, not advanced past"
    );

    // The poll's collection over P finds nothing to dispose of but the
    // null, advances past it and releases `FREE`.
    assert_eq!(unsafe { ll_gc_maybe_collect() }, 0);
    assert_eq!(state(byte()), FREE);
    assert_eq!(verdict_count(), 0);
    reset();
}

#[test]
fn a_proposed_root_whose_trace_was_refused_is_written_back_into_r_behind_a_free_byte() {
    let _g = test_guard();
    reset();
    assert!(crate::cycle::queue::refill_spares());
    let node = node_class("TriggerRefusedNode");
    // A ring long enough that its trace outgrows the workspace, so that a
    // pool budget of zero refuses the trace inside its rows.
    let size = unsafe { (*node).object_size } as usize;
    let class_index = crate::memory::heap::size_class_index(size).expect("a size class serves it");
    let slots =
        crate::memory::block_pool::BLOCK_PAYLOAD / crate::memory::heap::SIZE_CLASSES[class_index];
    let array = crate::cycle::shadow::bytes_for(slots as u32);
    let blocks = crate::cycle::arena::WORKSPACE_BUMP_BYTES / array + 2;
    let mut arena = Arena::new();
    let ring = unsafe { long_ring(&mut arena, node, blocks * slots) };
    let members = ring.len();
    assert_eq!(candidate_count(), members);

    assert_eq!(stand_in_posts(1, Verdict::Proposed), Posted::Batch(1));
    assert_eq!(state(byte()), POSTED);
    assert_eq!(candidate_count(), members - 1);

    let refused = {
        crate::memory::critical::drain_for_test();
        let _budgeted = crate::memory::block_pool::budget_blocks(0);
        unsafe { ll_gc_maybe_collect() }
    };
    assert_eq!(refused, 0, "the trace was refused");
    assert_eq!(state(byte()), FREE, "the close released FREE all the same");
    assert_eq!(verdict_count(), 0, "P's front advanced past the root");
    assert_eq!(
        candidate_count(),
        members,
        "and the root was written back into R as a registration"
    );

    // With the pool answering, the ring is collected out of R whole.
    assert_eq!(unsafe { ll_gc_collect_cycles() }, members);
    let _ = ring;
    reset();
}

#[test]
fn a_posted_mutator_is_a_skip_for_the_round_and_p_stands() {
    let _g = test_guard();
    reset();
    assert!(crate::cycle::queue::refill_spares());
    let node = node_class("TriggerSkipNode");
    let mut arena = Arena::new();
    let _ring = unsafe { ring(&mut arena, [node, node]) };
    assert_eq!(stand_in_posts(2, Verdict::Proposed), Posted::Batch(2));
    assert_eq!(state(byte()), POSTED);
    // Work in R again, so that the round's idle test passes and its request
    // is what meets the byte.
    let _second = unsafe { ring(&mut arena, [node, node]) };
    assert_eq!(candidate_count(), 2);

    let record = Sent(crate::cycle::mutator_record::this_thread_record());
    let served = crate::cycle::worker::testing::consent_while(std::thread::spawn(move || {
        assert!(crate::memory::heap::ll_thread_init());
        unsafe { crate::cycle::worker::testing::serve_alone(record.into_inner()) }
    }));
    assert_eq!(served, crate::cycle::worker::Served::Posted);
    assert_eq!(verdict_count(), 2, "P unchanged");
    assert_eq!(candidate_count(), 2, "and R");
    assert_eq!(state(byte()), POSTED, "and the byte too");

    assert_eq!(
        unsafe { ll_gc_maybe_collect() },
        2,
        "the first ring is collected out of P, and nothing of R is read"
    );
    assert_eq!(state(byte()), FREE);
    assert_eq!(candidate_count(), 2);
    assert_eq!(unsafe { ll_gc_collect_cycles() }, 2);
    reset();
}

/// A request from a collector thread, consented to by this thread's poll
/// and by its slot free entry: the byte reads `COLLECTOR|s` after either,
/// and the collector reads the grant.
#[test]
fn a_request_is_consented_to_at_the_poll_and_at_a_slot_free() {
    let _g = test_guard();
    reset();
    let node = node_class("TriggerConsentNode");
    let mut arena = Arena::new();
    let record = Sent(crate::cycle::mutator_record::this_thread_record());
    let token = || unsafe { &(*crate::cycle::mutator_record::this_thread_record()).token };

    for by_the_poll in [true, false] {
        let (requested_tell, requested) = std::sync::mpsc::channel();
        let (granted_tell, granted) = std::sync::mpsc::channel();
        let record = Sent(record.0);
        // The stand-in waits as the collector does — on the slot's word until
        // the consent's wake — and tells the case when its request stands,
        // so that neither thread spins on the byte.
        let collector = std::thread::spawn(move || {
            crate::cycle::worker::testing::stand_in_as_the_elder();
            let token = unsafe { &(*record.into_inner()).token };
            assert_eq!(token.request(ELDER), Ok(()));
            requested_tell.send(()).expect("the case waits");
            while token.read() != word(COLLECTOR, ELDER) {
                crate::cycle::worker::testing::wait_for_the_elders_wake(
                    std::time::Duration::from_secs(10),
                );
            }
            granted_tell.send(()).expect("the case waits");
            token.release_claim(ELDER, false);
            crate::cycle::worker::testing::stand_down_as_the_elder();
        });

        // The request stands before the consent.
        requested.recv().expect("the stand-in requested");
        assert_eq!(state(token().read()), crate::cycle::token::REQUESTED);
        if by_the_poll {
            unsafe { ll_gc_maybe_collect() };
        } else {
            // A slot free: an object dying with no window open frees its
            // slot through the entry that reads the byte.
            let mut context = LLContext { arena: &mut arena };
            let object = unsafe { new_constructed(&mut context, node, MemoryCategory::GcHeap) };
            unsafe {
                assert!(ll_release(object as *mut RcHeader));
                ll_object_die(object);
            }
        }
        granted.recv().expect("the collector read the grant");
        collector.join().expect("the collector finished");
        assert_eq!(state(token().read()), FREE);
    }
    reset();
}

/// A silent mutator: its request is left standing with no wait, and served
/// at the collector's next checkpoint once it consents.
#[test]
fn a_silent_mutators_request_stands_and_is_served_at_a_checkpoint() {
    let _g = test_guard();
    reset();
    assert!(crate::cycle::queue::refill_spares());
    let node = node_class("TriggerSilentNode");
    let mut arena = Arena::new();
    let _ring = unsafe { ring(&mut arena, [node, node]) };
    // The bound itself is what this case is about: short, so that a serve
    // this thread does not answer withdraws within the case.
    let _wait =
        crate::cycle::worker::testing::HeldRequestWait::of(std::time::Duration::from_millis(2));
    let record = Sent(crate::cycle::mutator_record::this_thread_record());
    let (serve_tell, serve) = std::sync::mpsc::channel::<()>();
    let (served_tell, served) = std::sync::mpsc::channel();
    let collector = std::thread::spawn(move || {
        assert!(crate::memory::heap::ll_thread_init());
        let record = record.into_inner();
        let mut standing = crate::cycle::worker::Standing::new(ELDER);
        while serve.recv().is_ok() {
            let outcome = unsafe { crate::cycle::worker::serve(record, ELDER, 1, &mut standing) };
            served_tell
                .send((outcome, standing.batches_served_for_test()))
                .expect("the case waits");
        }
    });

    // This thread consents to nothing: the first serve waits its bound,
    // withdraws and marks the mutator silent.
    serve_tell.send(()).expect("the collector loops");
    assert_eq!(
        served.recv().expect("the collector answered"),
        (crate::cycle::worker::Served::Unanswered, 0)
    );
    assert!(unsafe { &*crate::cycle::mutator_record::this_thread_record() }.is_silent());
    assert_eq!(state(byte()), FREE, "withdrawn");

    // The second serve leaves its request standing with no wait.
    serve_tell.send(()).expect("the collector loops");
    assert_eq!(
        served.recv().expect("the collector answered"),
        (crate::cycle::worker::Served::Unanswered, 0)
    );
    assert_eq!(
        state(byte()),
        word(crate::cycle::token::REQUESTED, ELDER),
        "standing"
    );

    // The mutator answers, and the next serve's checkpoint serves the grant
    // before it makes any request of its own; that request finds R empty
    // and answers idle.
    assert_eq!(read_and_act_on_this_thread(), Reading::Collector);
    serve_tell.send(()).expect("the collector loops");
    assert_eq!(
        served.recv().expect("the collector answered"),
        (crate::cycle::worker::Served::Idle, 1)
    );
    assert!(!unsafe { &*crate::cycle::mutator_record::this_thread_record() }.is_silent());
    assert_eq!(verdict_count(), 2);
    drop(serve_tell);
    collector.join().expect("the collector finished");

    assert_eq!(unsafe { ll_gc_maybe_collect() }, 2);
    reset();
}

#[test]
fn the_pressure_path_spends_an_arming_for_the_verdicts() {
    let _g = test_guard();
    reset();
    crate::gc::arm_for_the_verdicts();
    assert_eq!(crate::gc::arming(), Arming::Verdicts);
    unsafe { crate::cycle::collect::collect_under_pressure() };
    assert_eq!(
        crate::gc::arming(),
        Arming::None,
        "P was disposed of whole in it"
    );

    crate::gc::arm();
    unsafe { crate::cycle::collect::collect_under_pressure() };
    assert_eq!(
        crate::gc::arming(),
        Arming::AllRoots,
        "an arming for R whole stands: the pressure path's endings own it"
    );
    reset();
}

/// The collection the byte arms reads no root of R, and retires R's
/// completed deaths all the same: its close compacts the ring whole
/// (`cycle::queue::dispose_candidates`), so a death registered after the
/// batch took its own roots out gives its slot back at this fire.
#[test]
fn the_fire_the_byte_arms_retires_a_death_standing_in_r() {
    let _g = test_guard();
    reset();
    assert!(crate::cycle::queue::refill_spares());
    let node = node_class("ArmedFireNode");
    let mut arena = Arena::new();
    let keeper = unsafe { kept_root(&mut arena, node, "ArmedFireKeeper") };
    assert_eq!(stand_in_posts(1, Verdict::ReadLive), Posted::Batch(1));
    assert_eq!(state(byte()), POSTED);

    // A death of its own, registered after the batch took its root out of R.
    let class = ClassBuilder::new("ArmedFireDeath").build();
    let mut context = LLContext { arena: &mut arena };
    let dying = unsafe { new_constructed(&mut context, class, MemoryCategory::GcHeap) };
    unsafe {
        crate::refcount::ll_retain(dying as *mut RcHeader);
        assert!(
            !ll_release(dying as *mut RcHeader),
            "the non-final decrement registers it"
        );
        assert!(ll_release(dying as *mut RcHeader));
        ll_object_die(dying);
    }
    assert_eq!(candidate_count(), 1, "the death stands in R");

    assert_eq!(unsafe { ll_gc_maybe_collect() }, 0);
    assert_eq!(
        candidate_count(),
        0,
        "the fire over P retired the death standing in R"
    );
    assert_eq!(state(byte()), FREE);

    unsafe {
        assert!(ll_release(keeper as *mut RcHeader));
        ll_object_die(keeper);
    }
    unsafe { crate::cycle::queue::retire_candidates() };
    reset();
}

/// A poll with nothing armed and a byte at `FREE` reads no lane: a completed
/// death stands in R across any number of polls, its slot withheld from the
/// allocator, until something runs a retirement pass. By ruling the poll runs
/// none of its own (`dev/DECISIONS.md`, "the safepoint poll takes the free
/// path's road"), and the hold is bounded by the collector's serve threshold;
/// what takes a quiet thread's garbage after a time is `PLAN.md`, "A quiet
/// thread's garbage is taken after X".
#[test]
fn an_unarmed_poll_leaves_a_completed_death_registered() {
    let _g = test_guard();
    reset();
    assert!(crate::cycle::queue::refill_spares());
    let mut arena = Arena::new();
    let class = ClassBuilder::new("UnarmedPollDeath").build();
    let mut context = LLContext { arena: &mut arena };
    let dying = unsafe { new_constructed(&mut context, class, MemoryCategory::GcHeap) };
    unsafe {
        crate::refcount::ll_retain(dying as *mut RcHeader);
        assert!(
            !ll_release(dying as *mut RcHeader),
            "the non-final decrement registers it"
        );
        assert!(ll_release(dying as *mut RcHeader));
        ll_object_die(dying);
    }
    assert_eq!(candidate_count(), 1);
    assert_eq!(state(byte()), FREE);
    assert_eq!(crate::gc::arming(), Arming::None);

    for _ in 0..5 {
        assert_eq!(unsafe { ll_gc_maybe_collect() }, 0);
    }
    assert_eq!(
        candidate_count(),
        1,
        "five polls read nothing and the death stands"
    );

    unsafe { crate::cycle::queue::retire_candidates() };
    assert_eq!(candidate_count(), 0, "a retirement pass is what clears it");
    reset();
}
