//! P is one block per thread, drawn with the record and never grown; a
//! stand-in collector posts into it in R's order and clamped to its room;
//! every in-line collection reads its roots into the batch and disposes of
//! the whole prefix at the close; the open-gate poll disposes of its prefix
//! up to the first proposal; and a retirement inside a teardown retires its
//! completed deaths in place.
//!
//! The stand-in is [`testing::post_batch`] on a second thread: it claims the
//! token, takes from R, posts, and advances R's front by the count posted,
//! which is the shape of the collector's batch less the trace.

use super::testing::{Posted, post_batch};
use super::*;

use crate::class::{Class, ClassBuilder};
use crate::cycle::queue::{
    candidate_count, deferred_count, deferred_turnover_mirror, refill_spares,
    release_queue_segments,
};
use crate::cycle::testing::{Sent, ring};
use crate::gc::{ll_gc_collect_cycles, ll_gc_maybe_collect};
use crate::memory::arena::Arena;
use crate::memory::block_pool::{BlockHeader, test_guard};
use crate::memory::context::LLContext;
use crate::memory::heap::block_occupancy;
use crate::object::{Object, ll_object_die, new_constructed};
use crate::refcount::{
    CANDIDATE_BIT, EntityKind, MemoryCategory, ll_release, ll_retain, mutator_flags,
};
use crate::ring::BLOCK_ENTRIES;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Empty every lane and the spare cells, so that a case starts from a known
/// queue on a harness thread another case used.
fn reset() {
    discard_standing_verdicts();
    release_queue_segments();
    crate::memory::critical::drain_for_test();
    crate::gc::disarm();
}

/// [`reset`], then the spare cells filled: a registration on a thread with
/// no ring block and no spare goes to the overflow buffer, which a case
/// about R's front is not reading.
fn start() {
    reset();
    assert!(refill_spares(), "the pool served both spares");
}

/// A header the candidate gate admits at count two: a release of it
/// registers the header and dereferences nothing, which is all a case about
/// the ring's bookkeeping needs.
fn bare_candidate() -> RcHeader {
    let mut header = RcHeader::new(MemoryCategory::GcHeap, EntityKind::Object.to_flags());
    unsafe { ll_retain(&raw mut header) };
    header
}

/// `count` bare headers, boxed so that no header moves under an entry
/// naming it. The box stays where the case put it: a move of a box, as a
/// fresh borrow of it, is a retag that takes the registration's exposed
/// tags off, and the poll's atomic read of a header through its entry then
/// fails under Miri (`dev/WORKFLOW.md`, Miri).
fn bare_headers(count: usize) -> Box<[RcHeader]> {
    (0..count).map(|_| bare_candidate()).collect()
}

/// Register every header of `headers` on this thread and answer the one
/// raw pointer per header the registration exposed, which is the pointer a
/// case reads the header through afterwards.
fn register_bare(headers: &mut [RcHeader]) -> Vec<*mut RcHeader> {
    let pointers: Vec<*mut RcHeader> = headers.iter_mut().map(|header| &raw mut *header).collect();
    for &header in &pointers {
        assert!(unsafe { !ll_release(header) }, "a holder is left");
    }

    pointers
}

/// The stand-in's batch over this thread's record, made on a thread of its
/// own and joined.
fn stand_in_posts(k: usize, verdict_for: impl Fn(usize) -> Verdict + Send + 'static) -> Posted {
    let record = Sent(mutator_record::this_thread_record());
    assert!(!record.0.is_null(), "this thread has a record");
    std::thread::spawn(move || {
        let mut index = 0;
        unsafe {
            post_batch(record.into_inner(), k, |_| {
                let verdict = verdict_for(index);
                index += 1;
                verdict
            })
        }
    })
    .join()
    .expect("the stand-in finished")
}

/// A class with one counted Box property at `prop_offset(0)`, which is what
/// [`ring`] links members through.
fn node_class(name: &str) -> *const Class {
    ClassBuilder::new(name).prop("next", true).build()
}

/// A class with one counted Box property, through which a case holds an
/// object from outside.
fn keeper_class(name: &str) -> *const Class {
    ClassBuilder::new(name).prop("kept", true).build()
}

/// One object of `class` at count one, in this thread's heap.
unsafe fn object(arena: &mut Arena, class: *const Class) -> *mut Object {
    let mut context = LLContext { arena };
    unsafe { new_constructed(&mut context, class, MemoryCategory::GcHeap) }
}

/// A registered candidate whose death then completed in place: the slot is
/// withheld by its entry alone, and a zero-count verdict about it retires
/// the entry.
unsafe fn completed_death(arena: &mut Arena, class: *const Class) -> *mut RcHeader {
    let entity = unsafe { object(arena, class) } as *mut RcHeader;
    unsafe {
        ll_retain(entity);
        assert!(!ll_release(entity), "registered at the non-final decrement");
        assert!(ll_release(entity));
        ll_object_die(entity as *mut Object);
    }
    entity
}

/// Destructor bodies run since a case last cleared it.
static DESTRUCTOR_RUNS: AtomicUsize = AtomicUsize::new(0);

unsafe extern "C" fn counting_destructor(_object: *mut Object) {
    DESTRUCTOR_RUNS.fetch_add(1, Ordering::Relaxed);
}

/// A destructor that keeps `$this` alive past its own death: the count the
/// collector read as zero is above zero again by the time the mutator reads
/// the verdict.
unsafe extern "C" fn resurrecting_destructor(object: *mut Object) {
    unsafe { ll_retain(object as *mut RcHeader) };
}

#[test]
fn p_is_one_block_drawn_with_the_record_and_clamps_the_batch_to_its_room() {
    let _g = test_guard();
    start();
    let record = mutator_record::this_thread_record();
    let block = mutator_record::verdict_block(record);
    assert!(!block.is_null(), "P's block came with the record");
    assert_eq!(verdict_count(), 0);

    // More roots than P holds: the batch is clamped to the room, and R
    // keeps the rest.
    let beyond = 10;
    let mut headers = bare_headers(BLOCK_ENTRIES + beyond);
    let _pointers = register_bare(&mut headers);
    assert_eq!(
        stand_in_posts(BLOCK_ENTRIES + beyond, |_| Verdict::Proposed),
        Posted::Batch(BLOCK_ENTRIES),
        "the batch was clamped to P's room"
    );
    assert_eq!(verdict_count(), BLOCK_ENTRIES, "P is full");
    assert_eq!(
        candidate_count(),
        beyond,
        "R's front moved past exactly the roots posted"
    );
    assert_eq!(
        stand_in_posts(1, |_| Verdict::Proposed),
        Posted::TokenHeld,
        "a P not yet disposed of takes nothing: the byte reads POSTED, and a \
         claim fails on it"
    );
    assert_eq!(
        mutator_record::verdict_block(record),
        block,
        "the same block, for the thread's life"
    );

    // The batch left `POSTED` on the byte, and the mutator's reading of it
    // arms the collection over P and moves nothing: a full P of proposals
    // costs the reading no registration, and the collection is what reads
    // them.
    assert_eq!(
        crate::cycle::token::read_and_act_on_this_thread(),
        crate::cycle::token::Reading::Posted
    );
    let arming = crate::gc::arming();
    crate::gc::disarm();
    assert_eq!(arming, crate::gc::Arming::Verdicts);
    assert_eq!(verdict_count(), BLOCK_ENTRIES, "P stands as it was");
    assert_eq!(candidate_count(), beyond, "and nothing was written into R");
    drop(headers);
    reset();
}

#[test]
fn verdicts_come_back_in_rs_order_and_r_s_front_moves_by_the_count_posted() {
    let _g = test_guard();
    start();
    let mut headers = bare_headers(5);
    let expected = register_bare(&mut headers);
    let verdicts = [
        Verdict::ReadLive,
        Verdict::ZeroCount,
        Verdict::Proposed,
        Verdict::Unwalked,
    ];

    assert_eq!(
        stand_in_posts(4, move |index| verdicts[index]),
        Posted::Batch(4)
    );
    assert_eq!(candidate_count(), 1, "the fifth root stays in R");
    let standing = standing_verdicts();
    assert_eq!(
        standing
            .iter()
            .map(|&(entity, _)| entity)
            .collect::<Vec<_>>(),
        expected[..4],
        "in R's order"
    );
    assert_eq!(
        standing
            .iter()
            .map(|&(_, verdict)| verdict)
            .collect::<Vec<_>>(),
        verdicts,
        "each with the verdict posted about it"
    );

    // No poll reads a verdict: the byte reads `POSTED` after the batch, and
    // the mutator's reading of it arms the collection over P and moves
    // nothing.
    assert_eq!(
        crate::cycle::token::state(unsafe { (*mutator_record::this_thread_record()).token.read() }),
        crate::cycle::token::POSTED
    );
    assert_eq!(
        crate::cycle::token::read_and_act_on_this_thread(),
        crate::cycle::token::Reading::Posted
    );
    let arming = crate::gc::arming();
    crate::gc::disarm();
    assert_eq!(arming, crate::gc::Arming::Verdicts);
    assert_eq!(standing_verdicts().len(), 4, "the reading moved nothing");

    // A holder of the token keeps the stand-in out; its take consumes
    // `POSTED`, and a bare guard's release writes `FREE` over a P this case
    // discards itself.
    let claim = crate::cycle::token::HeldToken::take();
    assert_eq!(stand_in_posts(1, |_| Verdict::Proposed), Posted::TokenHeld);
    drop(claim);
    discard_standing_verdicts();
    drop(headers);
    reset();
}

#[test]
fn a_proposed_root_is_collected_by_the_poll_that_reads_it() {
    for verdict in [Verdict::Proposed, Verdict::Unwalked] {
        let _g = test_guard();
        start();
        DESTRUCTOR_RUNS.store(0, Ordering::Relaxed);
        let class = ClassBuilder::new("VerdictProposedNode")
            .prop("next", true)
            .destructor(counting_destructor as *const ())
            .build();
        let mut arena = Arena::new();
        let _members = unsafe { ring(&mut arena, [class, class]) };
        assert_eq!(stand_in_posts(2, move |_| verdict), Posted::Batch(2));
        assert_eq!(candidate_count(), 0, "the collector took both roots");
        assert!(!crate::gc::is_armed(), "nothing has armed this thread");

        assert_eq!(
            unsafe { ll_gc_maybe_collect() },
            2,
            "the poll read the verdicts, armed itself, and collected the ring"
        );
        assert_eq!(DESTRUCTOR_RUNS.load(Ordering::Relaxed), 2);
        assert_eq!(verdict_count(), 0);
        assert_eq!(candidate_count(), 0, "the close retired both records");
        assert!(!crate::gc::is_armed(), "the fire spent the arming");
        reset();
    }
}

#[test]
fn a_root_read_live_waits_in_the_deferred_lane_against_the_polls_reading() {
    let _g = test_guard();
    start();
    let node = node_class("VerdictLiveNode");
    let mut arena = Arena::new();
    let kept = unsafe { object(&mut arena, node) };
    let keeper = unsafe { object(&mut arena, keeper_class("VerdictLiveKeeper")) };
    unsafe {
        crate::test_support::store_prop(
            &mut arena,
            keeper,
            crate::test_support::prop_offset(0),
            kept,
        );
        assert!(!ll_release(kept as *mut RcHeader), "the keeper holds it");
    }
    assert_eq!(candidate_count(), 1);
    assert_eq!(stand_in_posts(1, |_| Verdict::ReadLive), Posted::Batch(1));
    assert!(refill_spares(), "the lane's block comes from a cell");

    crate::cycle::epoch::turn_to_a_nonzero_epoch();
    let reading = crate::cycle::epoch::this_threads_turnovers();
    assert_eq!(
        unsafe { ll_gc_maybe_collect() },
        0,
        "nothing armed the poll"
    );
    assert_eq!(verdict_count(), 0);
    assert_eq!(candidate_count(), 0);
    assert_eq!(deferred_count(), 1, "the root waits for the turnover");
    assert_eq!(
        deferred_turnover_mirror(),
        reading as u8,
        "the mirror is the reading of the poll's own collection"
    );

    unsafe {
        assert!(ll_release(keeper as *mut RcHeader));
        ll_object_die(keeper);
    }
    reset();
}

#[test]
fn a_zero_count_verdict_retires_a_completed_death_and_keeps_a_resurrection() {
    let _g = test_guard();
    start();
    let mut arena = Arena::new();
    let dead = unsafe { completed_death(&mut arena, node_class("VerdictDeadNode")) };
    let block = BlockHeader::of_ptr(dead as *const u8) as *mut u8;
    let occupied_before = unsafe { block_occupancy(block) };

    let lazarus = ClassBuilder::new("VerdictLazarus")
        .destructor(resurrecting_destructor as *const ())
        .build();
    let risen = unsafe { object(&mut arena, lazarus) } as *mut RcHeader;
    unsafe {
        ll_retain(risen);
        assert!(!ll_release(risen), "registered at the non-final decrement");
        assert!(ll_release(risen), "the count reached zero");
        // The death the collector would read as a zero count, undone by the
        // destructor before the mutator reads the verdict.
        ll_object_die(risen as *mut Object);
    }
    assert_eq!(
        unsafe { crate::refcount::entity_refcount(risen as *mut Object) },
        1
    );
    assert_eq!(candidate_count(), 2);

    assert_eq!(stand_in_posts(2, |_| Verdict::ZeroCount), Posted::Batch(2));
    assert_eq!(unsafe { ll_gc_maybe_collect() }, 0);
    assert_eq!(verdict_count(), 0);
    assert_eq!(
        unsafe { block_occupancy(block) },
        occupied_before - 1,
        "the completed death's slot went back to its block"
    );
    assert_eq!(
        candidate_count(),
        1,
        "the resurrected entity is registered again"
    );
    assert_eq!(
        crate::cycle::queue::entry_at(0),
        risen,
        "in R, as a registration"
    );
    assert_ne!(
        unsafe { mutator_flags(risen) } & CANDIDATE_BIT,
        0,
        "with its candidate bit still set"
    );

    unsafe {
        assert!(ll_release(risen), "the destructor's reference is the last");
        ll_object_die(risen as *mut Object);
    }
    reset();
}

/// What a destructor of this file records about P.
static VERDICTS_SEEN_INSIDE: AtomicUsize = AtomicUsize::new(usize::MAX);

/// A destructor that polls, which is a closed gate: the poll inside it
/// reads no verdict.
unsafe extern "C" fn polling_destructor(_object: *mut Object) {
    unsafe { ll_gc_maybe_collect() };
    VERDICTS_SEEN_INSIDE.store(verdict_count(), Ordering::Relaxed);
}

#[test]
fn a_closed_gate_poll_reads_no_verdict() {
    let _g = test_guard();
    start();
    let mut arena = Arena::new();
    // A live object the case holds, registered at its non-final decrement:
    // a root the poll at the clean point can trace.
    let root = unsafe { object(&mut arena, node_class("VerdictClosedGateNode")) } as *mut RcHeader;
    unsafe {
        ll_retain(root);
        assert!(!ll_release(root));
    }
    assert_eq!(stand_in_posts(1, |_| Verdict::Proposed), Posted::Batch(1));
    assert_eq!(verdict_count(), 1);
    crate::gc::arm();

    let class = ClassBuilder::new("VerdictPollingInside")
        .destructor(polling_destructor as *const ())
        .build();
    let dying = unsafe { object(&mut arena, class) };
    unsafe {
        assert!(ll_release(dying as *mut RcHeader));
        ll_object_die(dying);
    }
    assert_eq!(
        VERDICTS_SEEN_INSIDE.load(Ordering::Relaxed),
        1,
        "the poll inside the teardown left the verdict standing"
    );
    assert!(
        crate::gc::is_armed(),
        "and kept the arming for a clean point"
    );
    assert_eq!(verdict_count(), 1);

    // The next poll at a clean point reads it: the collection traces the
    // root out of P, reads it live, and its close defers it against the
    // reading's count and advances P.
    assert_eq!(
        unsafe { ll_gc_maybe_collect() },
        0,
        "the case holds the root"
    );
    assert_eq!(verdict_count(), 0);
    assert_eq!(
        deferred_count(),
        1,
        "a P root the close read as alive is deferred"
    );
    assert_eq!(candidate_count(), 0);
    unsafe {
        assert!(ll_release(root));
        ll_object_die(root as *mut Object);
    }
    reset();
}

/// A destructor that meets an allocation refusal, as one that allocates
/// under pressure does: the pressure path finds the gate closed by the
/// teardown and retires what it can.
unsafe extern "C" fn pressure_inside_destructor(_object: *mut Object) {
    unsafe { crate::cycle::collect::collect_under_pressure() };
    VERDICTS_SEEN_INSIDE.store(standing_verdicts().len(), Ordering::Relaxed);
}

#[test]
fn the_retirement_inside_a_teardown_retires_ps_completed_deaths_in_place() {
    let _g = test_guard();
    start();
    DESTRUCTOR_RUNS.store(0, Ordering::Relaxed);
    let mut arena = Arena::new();
    let node = node_class("VerdictPrefixDead");
    let ring_node = ClassBuilder::new("VerdictPrefixRing")
        .prop("next", true)
        .destructor(counting_destructor as *const ())
        .build();
    // R, in order: two completed deaths, a garbage ring's two roots, a third
    // completed death. The stand-in posts in that order, so P reads as two
    // deaths ahead of the first proposal and one behind it — and the
    // retirement in place reaches all three.
    let ahead: Vec<*mut RcHeader> = (0..2)
        .map(|_| unsafe { completed_death(&mut arena, node) })
        .collect();
    let members = unsafe { ring(&mut arena, [ring_node, ring_node]) };
    let behind = unsafe { completed_death(&mut arena, node) };
    let shape = [
        Verdict::ZeroCount,
        Verdict::ZeroCount,
        Verdict::Proposed,
        Verdict::Proposed,
        Verdict::ZeroCount,
    ];
    assert_eq!(
        stand_in_posts(5, move |index| shape[index]),
        Posted::Batch(5)
    );
    let block_of = |entity: *mut RcHeader| BlockHeader::of_ptr(entity as *const u8) as *mut u8;
    let occupied_before: Vec<u32> = ahead
        .iter()
        .map(|&entity| unsafe { block_occupancy(block_of(entity)) })
        .collect();

    let class = ClassBuilder::new("VerdictPressureInside")
        .destructor(pressure_inside_destructor as *const ())
        .build();
    let dying = unsafe { object(&mut arena, class) };
    unsafe {
        assert!(ll_release(dying as *mut RcHeader));
        ll_object_die(dying);
    }
    assert_eq!(
        VERDICTS_SEEN_INSIDE.load(Ordering::Relaxed),
        2,
        "every completed death was retired in place and the two proposals stand"
    );
    assert_eq!(
        DESTRUCTOR_RUNS.load(Ordering::Relaxed),
        0,
        "no proposal was collected inside the teardown"
    );
    for (index, &entity) in ahead.iter().enumerate() {
        assert!(
            unsafe { block_occupancy(block_of(entity)) } < occupied_before[index],
            "death {index}'s slot went back to its block"
        );
    }
    assert_eq!(
        standing_verdicts()
            .iter()
            .map(|&(_, verdict)| verdict)
            .collect::<Vec<_>>(),
        shape[2..4],
        "and P's front did not move"
    );
    assert_eq!(
        verdict_count(),
        5,
        "the nulled slots stand until an advance"
    );

    // The poll at a clean point stops at the first proposal, arms, and the
    // collection reads the two roots out of P: the ring is collected, and
    // the close advances P past the whole prefix.
    assert_eq!(unsafe { ll_gc_maybe_collect() }, 2);
    assert_eq!(DESTRUCTOR_RUNS.load(Ordering::Relaxed), 2);
    assert_eq!(verdict_count(), 0);
    assert_eq!(candidate_count(), 0);
    let _ = (members, behind);
    reset();
}

#[test]
fn the_fire_and_the_pressure_path_take_their_roots_out_of_p() {
    let _g = test_guard();
    start();
    DESTRUCTOR_RUNS.store(0, Ordering::Relaxed);
    let class = ClassBuilder::new("VerdictFirstNode")
        .prop("next", true)
        .destructor(counting_destructor as *const ())
        .build();
    let mut arena = Arena::new();

    // The explicit fire, with R empty and the ring's roots in P alone.
    let _fire = unsafe { ring(&mut arena, [class, class]) };
    assert_eq!(stand_in_posts(2, |_| Verdict::Proposed), Posted::Batch(2));
    assert_eq!(candidate_count(), 0);
    assert_eq!(
        unsafe { ll_gc_collect_cycles() },
        2,
        "the fire read P first"
    );
    assert_eq!(verdict_count(), 0);

    // The pressure path, the same way.
    let _pressure = unsafe { ring(&mut arena, [class, class]) };
    assert_eq!(stand_in_posts(2, |_| Verdict::Proposed), Posted::Batch(2));
    assert_eq!(
        unsafe { crate::cycle::collect::collect_under_pressure() },
        2,
        "the pressure path read P first"
    );
    assert!(
        standing_verdicts().is_empty(),
        "the round's retirement answered for both roots in place"
    );
    assert_eq!(DESTRUCTOR_RUNS.load(Ordering::Relaxed), 4);
    crate::gc::disarm();
    reset();
}

/// A batch of verdicts with no root among them is not an empty batch: the
/// collection proposes nothing, and its close is what defers the roots read
/// live and advances P — the exit's rounds included, which is what keeps a
/// ring the collector read live and that died since from leaving with the
/// thread.
#[test]
fn a_batch_of_verdicts_without_a_root_is_disposed_of_by_the_close() {
    let _g = test_guard();
    DESTRUCTOR_RUNS.store(0, Ordering::Relaxed);
    let class = Handed(
        ClassBuilder::new("VerdictLiveExitNode")
            .prop("next", true)
            .destructor(counting_destructor as *const ())
            .build(),
    );
    let residue = std::thread::spawn(move || {
        assert!(
            crate::memory::heap::ll_thread_init(),
            "the pool served this thread"
        );
        let class = class.class();
        let mut arena = Arena::new();
        let _members = unsafe { ring(&mut arena, [class, class]) };
        drop(arena);
        // The collector read the ring live — stale by the time the mutator
        // reads the verdict — and R is empty.
        assert_eq!(stand_in_posts(2, |_| Verdict::ReadLive), Posted::Batch(2));
        assert_eq!(candidate_count(), 0);

        crate::memory::heap::ll_thread_exit();
        crate::cycle::collect::take_exit_residue().expect("the exit ran its collection")
    })
    .join()
    .unwrap();
    assert_eq!(
        residue.freed, 2,
        "the first round deferred the two, the second re-offered and freed them"
    );
    assert_eq!(residue.registered, 0);
    assert_eq!(DESTRUCTOR_RUNS.load(Ordering::Relaxed), 2);
}

/// An unwind out of P's pass at either of its boundaries leaves no entry
/// that a later reading answers for twice: a nulled death whose free raised
/// is freed by the way out and stays null, and a prefix answered for but not
/// advanced past is read again with every entry skipped.
#[test]
fn an_unwind_inside_ps_pass_leaves_no_entry_answered_for_twice() {
    // The free's own point, reached here inside P's pass since R is empty,
    // and the point before the advance.
    for fault in [2, compaction::VERDICT_ADVANCE_CHECKPOINT] {
        let _g = test_guard();
        start();
        let mut arena = Arena::new();
        let node = node_class("VerdictUnwindDead");
        let dead = unsafe { completed_death(&mut arena, node) };
        let kept = unsafe { object(&mut arena, node) };
        let keeper = unsafe { object(&mut arena, keeper_class("VerdictUnwindKeeper")) };
        unsafe {
            crate::test_support::store_prop(
                &mut arena,
                keeper,
                crate::test_support::prop_offset(0),
                kept,
            );
            assert!(!ll_release(kept as *mut RcHeader), "the keeper holds it");
        }
        // Read after the case's last allocation, which may share the block.
        let block = BlockHeader::of_ptr(dead as *const u8) as *mut u8;
        let occupied_before = unsafe { block_occupancy(block) };
        // R, in order: the death, the kept root. P takes both.
        let shape = [Verdict::ZeroCount, Verdict::ReadLive];
        assert_eq!(
            stand_in_posts(2, move |index| shape[index]),
            Posted::Batch(2)
        );

        let batch = crate::cycle::queue::read_batch();
        let _injection = compaction::inject(fault);
        let raised = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            crate::cycle::queue::dispose_candidates(batch, 3);
        }));
        assert!(raised.is_err(), "the boundary was reached: {fault}");
        assert_eq!(
            unsafe { block_occupancy(block) },
            occupied_before - 1,
            "the death's slot went back on the way out, past a fault at {fault}"
        );
        assert_eq!(
            verdict_count(),
            2,
            "P was not advanced, past a fault at {fault}"
        );
        let standing = standing_verdicts();
        if fault == 2 {
            assert_eq!(
                standing,
                vec![(kept as *mut RcHeader, Verdict::ReadLive)],
                "the death is nulled and the root stands, past a fault at {fault}"
            );
            assert_eq!(deferred_count(), 0);
        } else {
            assert!(standing.is_empty(), "every entry was answered for");
            assert_eq!(
                deferred_count(),
                1,
                "the root the collector read as alive was deferred"
            );
        }

        // The next collection reads what stands, answers for nothing twice,
        // and advances.
        assert_eq!(unsafe { ll_gc_collect_cycles() }, 0);
        assert_eq!(verdict_count(), 0);
        assert_eq!(deferred_count(), 1, "the root is in one lane once");
        assert_eq!(candidate_count(), 0);

        unsafe {
            assert!(ll_release(keeper as *mut RcHeader));
            ll_object_die(keeper);
        }
        reset();
    }
}

/// A component the close cannot dispose of — every member's destructor
/// resurrects it — is written back from P into R, each root a registration
/// with its bit still set, and P advances past it.
#[test]
fn a_component_the_close_cannot_dispose_of_is_written_back_from_p_into_r() {
    let _g = test_guard();
    start();
    DESTRUCTOR_RUNS.store(0, Ordering::Relaxed);
    let lazarus = ClassBuilder::new("VerdictLazarusRing")
        .prop("next", true)
        .destructor(resurrecting_destructor as *const ())
        .build();
    let mut arena = Arena::new();
    let members = unsafe { ring(&mut arena, [lazarus, lazarus]) };
    assert_eq!(stand_in_posts(2, |_| Verdict::Proposed), Posted::Batch(2));
    assert_eq!(candidate_count(), 0);

    assert_eq!(
        unsafe { ll_gc_collect_cycles() },
        0,
        "the destructors resurrected the component"
    );
    assert_eq!(verdict_count(), 0, "P advanced past the whole batch");
    assert_eq!(
        candidate_count(),
        2,
        "both roots were written back into R as registrations"
    );
    for &member in &members {
        assert_ne!(
            unsafe { mutator_flags(member as *mut RcHeader) } & CANDIDATE_BIT,
            0,
            "with the candidate bit still set"
        );
    }

    // The resurrection references go, the ring is garbage again, and the
    // destructors do not run twice: the next collection frees it out of R.
    for &member in &members {
        assert!(!unsafe { ll_release(member as *mut RcHeader) });
    }
    assert_eq!(unsafe { ll_gc_collect_cycles() }, 2);
    reset();
}

/// A class pointer handed to the case's thread: the descriptor is immortal.
struct Handed(*const Class);

unsafe impl Send for Handed {}

impl Handed {
    /// The pointer, through a method so that a closure captures the wrapper
    /// rather than its field.
    fn class(&self) -> *const Class {
        self.0
    }
}

#[test]
fn the_exit_reads_p_before_its_rounds() {
    let _g = test_guard();
    DESTRUCTOR_RUNS.store(0, Ordering::Relaxed);
    let class = Handed(
        ClassBuilder::new("VerdictExitNode")
            .prop("next", true)
            .destructor(counting_destructor as *const ())
            .build(),
    );
    let residue = std::thread::spawn(move || {
        assert!(
            crate::memory::heap::ll_thread_init(),
            "the pool served this thread"
        );
        let class = class.class();
        let mut arena = Arena::new();
        let _members = unsafe { ring(&mut arena, [class, class]) };
        drop(arena);
        assert_eq!(stand_in_posts(2, |_| Verdict::Proposed), Posted::Batch(2));
        assert_eq!(candidate_count(), 0, "R is empty; the roots stand in P");

        crate::memory::heap::ll_thread_exit();
        crate::cycle::collect::take_exit_residue().expect("the exit ran its collection")
    })
    .join()
    .unwrap();
    assert_eq!(
        residue.freed, 2,
        "the exit's first round read P into its batch"
    );
    assert_eq!(residue.registered, 0);
    assert_eq!(DESTRUCTOR_RUNS.load(Ordering::Relaxed), 2);
}
