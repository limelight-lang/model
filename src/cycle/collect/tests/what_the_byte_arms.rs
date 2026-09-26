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
use crate::cycle::queue::verdicts::verdict_count;
use crate::cycle::queue::{candidate_count, deferred_count};
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
    let overflow = crate::cycle::queue::overflow_len();
    let _ = crate::cycle::queue::take_queue_work();

    let refused = {
        crate::memory::critical::drain_for_test();
        let _budgeted = crate::memory::block_pool::budget_blocks(0);
        unsafe { ll_gc_maybe_collect() }
    };
    assert_eq!(refused, 0, "the trace was refused");
    let read = crate::cycle::queue::take_queue_work().records_read;
    assert!(
        read <= 2 + overflow,
        "the close of the refused collection over P read {read} records, P holding 1 and R's stop 1"
    );
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

/// A sleeping mutator: the request it did not answer inside the wait stays
/// on its byte, the next round's request finds its own standing there and
/// waits nothing, and the collector's checkpoint serves the grant once the
/// mutator consents
/// (`dev/design/the-standing-request-lives-on-the-record.md`, "The
/// collector").
#[test]
fn a_sleeping_mutators_request_stands_and_is_served_at_a_checkpoint() {
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
            let outcome = unsafe {
                crate::cycle::worker::serve(
                    record,
                    ELDER,
                    1,
                    &mut standing,
                    crate::cycle::worker::testing::serve_clock_now(),
                )
            };
            served_tell
                .send((outcome, standing.batches_served_for_test()))
                .expect("the case waits");
        }
    });

    // This thread consents to nothing: the first serve waits its bound and
    // leaves the request standing on the byte.
    serve_tell.send(()).expect("the collector loops");
    assert_eq!(
        served.recv().expect("the collector answered"),
        (crate::cycle::worker::Served::Unanswered, 0)
    );
    assert_eq!(
        state(byte()),
        word(crate::cycle::token::REQUESTED, ELDER),
        "standing past the wait"
    );

    // The second serve's own request fails on the standing one and waits
    // nothing.
    serve_tell.send(()).expect("the collector loops");
    assert_eq!(
        served.recv().expect("the collector answered"),
        (crate::cycle::worker::Served::Unanswered, 0)
    );
    assert_eq!(
        state(byte()),
        word(crate::cycle::token::REQUESTED, ELDER),
        "still standing"
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

/// A poll with nothing armed and a byte at `FREE` reads no lane: a completed
/// death stands in R across any number of polls, its slot withheld from the
/// allocator, until something runs a retirement pass. By ruling the poll runs
/// none of its own (`dev/DECISIONS.md`, "the safepoint poll takes the free
/// path's road"), and the hold is bounded by the collector's serve threshold;
/// what takes a quiet thread's garbage after a time is `dev/DECISIONS.md`,
/// "a standing R is taken after an interval of the collector's own, and no
/// request count is capped".
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

/// `count` candidates registered and held live by the case's own count, the
/// spares refilled every [`POLL_STRIDE`](crate::cycle::queue::POLL_STRIDE)
/// registrations as a poll would.
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

/// A candidate registered at a non-final decrement whose death then
/// completed in place, its slot withheld by its entry in R.
unsafe fn completed_death(arena: &mut Arena, class: *const Class) -> *mut RcHeader {
    let mut context = LLContext { arena: &mut *arena };
    let dying =
        unsafe { new_constructed(&mut context, class, MemoryCategory::GcHeap) } as *mut RcHeader;
    unsafe {
        ll_retain(dying);
        assert!(!ll_release(dying), "the non-final decrement registers it");
        assert!(ll_release(dying));
        ll_object_die(dying as *mut Object);
    }
    dying
}

/// The collection the byte arms reads P, the overflow buffer and, of R, the
/// entry that stops its run of completed deaths: behind a thousand live
/// registrations its close reads the one verdict and one entry of R, and R
/// stands as it stood.
#[test]
fn the_fire_the_byte_arms_reads_one_entry_of_r() {
    let _g = test_guard();
    reset();
    assert!(crate::cycle::queue::refill_spares());
    let node = node_class("UnreadRingNode");
    let mut arena = Arena::new();
    let keeper = unsafe { kept_root(&mut arena, node, "UnreadRingKeeper") };
    assert_eq!(stand_in_posts(1, Verdict::ReadLive), Posted::Batch(1));
    let behind = 1_000;
    let live = unsafe { live_candidates(&mut arena, node, behind) };
    assert_eq!(
        candidate_count() + crate::cycle::queue::overflow_len(),
        behind
    );
    let overflow = crate::cycle::queue::overflow_len();
    let _ = crate::cycle::queue::take_queue_work();

    assert_eq!(unsafe { ll_gc_maybe_collect() }, 0);
    assert_eq!(state(byte()), FREE);
    let work = crate::cycle::queue::take_queue_work();
    assert!(
        work.records_read <= 2 + overflow,
        "the fire read {} records, P holding 1, the overflow buffer {overflow} and R's stop 1",
        work.records_read
    );
    assert_eq!(
        candidate_count() + crate::cycle::queue::overflow_len(),
        behind,
        "and R stands"
    );

    unsafe { let_go(live, keeper) };
    reset();
}

/// The members a collection over P tears down behind its batch come back at
/// its own close: a ring whose every member is registered, proposed through
/// one root, returns every slot, and R is empty behind it (S65.20's Critic,
/// finding 1).
#[test]
fn the_fire_over_p_returns_the_members_its_teardown_killed_in_r() {
    let _g = test_guard();
    reset();
    assert!(crate::cycle::queue::refill_spares());
    let node = node_class("TornDownRingNode");
    let mut arena = Arena::new();
    let members = 2_000;
    let ring = unsafe { crate::cycle::testing::long_ring(&mut arena, node, members) };
    assert_eq!(candidate_count(), members);
    assert_eq!(stand_in_posts(1, Verdict::Proposed), Posted::Batch(1));
    assert_eq!(candidate_count(), members - 1);

    assert_eq!(unsafe { ll_gc_maybe_collect() }, members);
    assert_eq!(state(byte()), FREE);
    assert_eq!(verdict_count(), 0);
    assert_eq!(
        candidate_count(),
        0,
        "the close freed every member whose entry stood in R"
    );
    let _ = ring;
    reset();
}

/// The close's run stops at the first entry of R that is not a completed
/// death: the deaths ahead of a live entry are freed, and the deaths behind
/// it stand, with the live entry first.
#[test]
fn the_fire_over_p_stops_its_run_at_a_live_entry() {
    const AHEAD: usize = 5;
    const BEHIND: usize = 3;
    let _g = test_guard();
    reset();
    assert!(crate::cycle::queue::refill_spares());
    let node = node_class("StoppedRunNode");
    let death = ClassBuilder::new("StoppedRunDeath").build();
    let mut arena = Arena::new();
    let keeper = unsafe { kept_root(&mut arena, node, "StoppedRunKeeper") };
    assert_eq!(stand_in_posts(1, Verdict::ReadLive), Posted::Batch(1));
    for _ in 0..AHEAD {
        let _ = unsafe { completed_death(&mut arena, death) };
    }
    let live = unsafe { live_candidates(&mut arena, node, 1) };
    for _ in 0..BEHIND {
        let _ = unsafe { completed_death(&mut arena, death) };
    }
    let overflow = crate::cycle::queue::overflow_len();
    let _ = crate::cycle::queue::take_queue_work();

    assert_eq!(unsafe { ll_gc_maybe_collect() }, 0);
    let work = crate::cycle::queue::take_queue_work();
    assert_eq!(
        work.records_read,
        1 + overflow + AHEAD + 1,
        "P's verdict, the overflow buffer, the run and its stop"
    );
    assert_eq!(candidate_count(), 1 + BEHIND, "the deaths behind it stand");

    unsafe { let_go(live, keeper) };
    assert_eq!(candidate_count(), 0);
    reset();
}

/// A completed death standing in R behind a live entry outlives the
/// collection over P, whose run stops at the live entry, and returns its slot
/// through the collector's next batch, whose zero-count verdict the next
/// collection over P retires.
#[test]
fn a_death_behind_a_live_entry_returns_through_the_collectors_batch() {
    let _g = test_guard();
    reset();
    assert!(crate::cycle::queue::refill_spares());
    let node = node_class("BatchedDeathNode");
    let mut arena = Arena::new();
    let keeper = unsafe { kept_root(&mut arena, node, "BatchedDeathKeeper") };
    assert_eq!(stand_in_posts(1, Verdict::ReadLive), Posted::Batch(1));
    let live = unsafe { live_candidates(&mut arena, node, 1) };
    let _ = unsafe { completed_death(&mut arena, ClassBuilder::new("BatchedDeath").build()) };
    assert_eq!(
        candidate_count(),
        2,
        "the death stands in R behind the live entry"
    );
    let record = unsafe { &*crate::cycle::mutator_record::this_thread_record() };
    let _ = record.take_freeing_disposition_note();

    assert_eq!(unsafe { ll_gc_maybe_collect() }, 0);
    assert_eq!(state(byte()), FREE);
    assert_eq!(candidate_count(), 2, "the fire over P left it in R");
    assert!(
        !record.take_freeing_disposition_note(),
        "and retired nothing"
    );

    // The batch takes both; the live entry's zero-count verdict is a count
    // read and not a death, so the close writes it back into R.
    assert_eq!(stand_in_posts(2, Verdict::ZeroCount), Posted::Batch(2));
    assert_eq!(candidate_count(), 0, "the batch took them out of R");
    assert_eq!(unsafe { ll_gc_maybe_collect() }, 0);
    assert_eq!(state(byte()), FREE);
    assert_eq!(verdict_count(), 0);
    assert_eq!(candidate_count(), 1, "the live entry went back into R");
    assert!(
        record.take_freeing_disposition_note(),
        "the fire over P retired the death out of P"
    );

    unsafe { let_go(live, keeper) };
    reset();
}

/// The collection over P keeps the free path's count for the deaths it leaves:
/// D − 1 deaths behind a live entry stand through the fire with their count,
/// and one after it arms the retirement pass, which returns all D.
#[test]
fn the_fire_the_byte_arms_keeps_the_count_of_the_deaths_it_leaves() {
    const D: usize = crate::cycle::queue::DEATHS_TO_RETIRE as usize;
    let _g = test_guard();
    reset();
    assert!(crate::cycle::queue::refill_spares());
    let node = node_class("CountKeptNode");
    let death = ClassBuilder::new("CountKeptDeath").build();
    let mut arena = Arena::new();
    let keeper = unsafe { kept_root(&mut arena, node, "CountKeptKeeper") };
    assert_eq!(stand_in_posts(1, Verdict::ReadLive), Posted::Batch(1));
    let live = unsafe { live_candidates(&mut arena, node, 1) };
    for _ in 0..D - 1 {
        let _ = unsafe { completed_death(&mut arena, death) };
    }
    assert!(!crate::gc::is_armed());

    assert_eq!(unsafe { ll_gc_maybe_collect() }, 0);
    assert_eq!(state(byte()), FREE);
    assert_eq!(candidate_count(), D, "the run stopped at the live entry");
    let _ = unsafe { completed_death(&mut arena, death) };
    assert_eq!(
        crate::gc::arming(),
        Arming::Retire,
        "the D-th death armed the pass across the fire"
    );
    assert_eq!(unsafe { ll_gc_maybe_collect() }, 0);
    assert_eq!(candidate_count(), 1, "and the pass returned all D");

    unsafe { let_go(live, keeper) };
    reset();
}

/// The deaths the collection over P frees at R's front leave the count:
/// D − 1 deaths freed by the fire and one after it arm nothing.
#[test]
fn the_fire_the_byte_arms_lowers_the_count_by_the_deaths_it_frees() {
    const D: usize = crate::cycle::queue::DEATHS_TO_RETIRE as usize;
    let _g = test_guard();
    reset();
    assert!(crate::cycle::queue::refill_spares());
    let node = node_class("CountLoweredNode");
    let death = ClassBuilder::new("CountLoweredDeath").build();
    let mut arena = Arena::new();
    let keeper = unsafe { kept_root(&mut arena, node, "CountLoweredKeeper") };
    assert_eq!(stand_in_posts(1, Verdict::ReadLive), Posted::Batch(1));
    for _ in 0..D - 1 {
        let _ = unsafe { completed_death(&mut arena, death) };
    }

    assert_eq!(unsafe { ll_gc_maybe_collect() }, 0);
    assert_eq!(candidate_count(), 0, "the fire freed the run");
    let _ = unsafe { completed_death(&mut arena, death) };
    assert!(!crate::gc::is_armed(), "one death after it arms nothing");

    unsafe { let_go(Vec::new(), keeper) };
    reset();
}

/// An arming for the retirement pass that the arming for P outranked at the
/// poll outlives the collection over P where the deaths stand behind a live
/// entry: D deaths behind a posted batch and a live entry are returned by
/// the poll after the fire.
#[test]
fn the_fire_the_byte_arms_leaves_the_pass_its_count_armed() {
    const D: usize = crate::cycle::queue::DEATHS_TO_RETIRE as usize;
    let _g = test_guard();
    reset();
    assert!(crate::cycle::queue::refill_spares());
    let node = node_class("OutrankedPassNode");
    let death = ClassBuilder::new("OutrankedPassDeath").build();
    let mut arena = Arena::new();
    let keeper = unsafe { kept_root(&mut arena, node, "OutrankedPassKeeper") };
    let live = unsafe { live_candidates(&mut arena, node, 1) };
    for _ in 0..D {
        let _ = unsafe { completed_death(&mut arena, death) };
    }
    assert_eq!(crate::gc::arming(), Arming::Retire);
    assert_eq!(stand_in_posts(1, Verdict::ReadLive), Posted::Batch(1));
    assert_eq!(
        candidate_count(),
        1 + D,
        "the batch took the keeper's root alone"
    );

    assert_eq!(unsafe { ll_gc_maybe_collect() }, 0);
    assert_eq!(state(byte()), FREE);
    assert_eq!(
        candidate_count(),
        1 + D,
        "the fire over P stopped at the live entry"
    );
    assert_eq!(
        crate::gc::arming(),
        Arming::Retire,
        "and the pass their count armed"
    );
    assert_eq!(unsafe { ll_gc_maybe_collect() }, 0);
    assert_eq!(candidate_count(), 1, "the pass returned all D");

    unsafe { let_go(live, keeper) };
    reset();
}

/// Take a case's live candidates and its keeper down and retire what they
/// leave: each live one's last reference and death, then the keeper's.
///
/// # Safety
/// The case holds the last reference to each, and nothing else holds them.
unsafe fn let_go(live: Vec<*mut RcHeader>, keeper: *mut Object) {
    for entity in live {
        unsafe {
            assert!(ll_release(entity));
            ll_object_die(entity as *mut Object);
        }
    }
    unsafe {
        assert!(ll_release(keeper as *mut RcHeader));
        ll_object_die(keeper);
        crate::cycle::queue::retire_candidates();
    }
}
