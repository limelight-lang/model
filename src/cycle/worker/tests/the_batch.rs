//! What one serve does for a mutator: the batch it takes from behind the
//! mutator's writer, clamped to P's room; the verdict per root, in R's order;
//! the advance that follows the last post from the return and from the
//! unwind alike; the budget that turns a batch unwalked and halves K; the
//! skip of a mutator collecting in line; and a mutator registering
//! throughout, whose registrations come out once each.
//!
//! The collector is a thread of the case's that calls [`serve`] on this
//! thread's record, started through `ll_thread_init` as the real one is.

use super::*;
use crate::class::{Class, ClassBuilder};
use crate::cycle::queue::verdicts::{
    Verdict, discard_standing_verdicts, standing_verdicts, verdict_count,
};
use crate::cycle::queue::{candidate_count, collect_lane_tokens, deferred_count, refill_spares};
use crate::cycle::testing::{Sent, ring};
use crate::gc::ll_gc_maybe_collect;
use crate::memory::arena::Arena;
use crate::memory::context::LLContext;
use crate::object::{Object, ll_object_die, new_constructed};
use crate::refcount::{MemoryCategory, RcHeader, ll_release, ll_retain};
use crate::ring::BLOCK_ENTRIES;
use std::sync::atomic::{AtomicUsize, Ordering};

/// One serve of this thread's record on a thread of its own, this thread
/// consenting at its byte meanwhile as its poll would, joined. A `POSTED`
/// left by an earlier batch is cleared first: these cases batch again
/// without the collection between that the byte asks for, and dispose of P
/// by hand at their end.
fn served_by_a_collector() -> Served {
    unsafe { &*record() }.token.clear_posted_for_test();
    let sent = Sent(record());
    testing::consent_while(std::thread::spawn(move || {
        assert!(
            crate::memory::heap::ll_thread_init(),
            "the pool served the collector thread"
        );
        unsafe { testing::serve_alone(sent.into_inner()) }
    }))
}

/// A class with one counted Box property at `prop_offset(0)`, which is what
/// [`ring`] links members through.
fn node_class(name: &str) -> *const Class {
    ClassBuilder::new(name)
        .prop("next", true)
        .destructor(counting_destructor as *const ())
        .build()
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

/// A registered candidate whose death then completed in place.
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

/// A registered root a keeper holds: the root, and the keeper the case
/// takes down afterwards.
unsafe fn kept_root(
    arena: &mut Arena,
    node: *const Class,
    name: &str,
) -> (*mut RcHeader, *mut Object) {
    let root = unsafe { object(arena, node) };
    let keeper = unsafe { object(arena, keeper_class(name)) };
    unsafe {
        crate::test_support::store_prop(arena, keeper, crate::test_support::prop_offset(0), root);
        assert!(!ll_release(root as *mut RcHeader), "the keeper holds it");
    }
    (root as *mut RcHeader, keeper)
}

unsafe fn release_keeper(keeper: *mut Object) {
    unsafe {
        assert!(ll_release(keeper as *mut RcHeader));
        ll_object_die(keeper);
    }
}

/// Destructor bodies run since a case last cleared it.
static DESTRUCTOR_RUNS: AtomicUsize = AtomicUsize::new(0);

unsafe extern "C" fn counting_destructor(_object: *mut Object) {
    DESTRUCTOR_RUNS.fetch_add(1, Ordering::Relaxed);
}

/// The verdicts standing in P, in order.
fn verdicts() -> Vec<Verdict> {
    standing_verdicts()
        .iter()
        .map(|&(_, verdict)| verdict)
        .collect()
}

#[test]
fn a_batch_posts_one_verdict_per_root_in_rs_order_and_advances_past_them() {
    let _g = test_guard();
    reset_lanes();
    DESTRUCTOR_RUNS.store(0, Ordering::Relaxed);
    let node = node_class("BatchNode");
    let mut arena = Arena::new();
    // R, in order: a garbage ring's two roots, a kept root, a completed
    // death, a second kept root.
    let _garbage = unsafe { ring(&mut arena, [node, node]) };
    let (_, keeper_a) = unsafe { kept_root(&mut arena, node, "BatchKeeperA") };
    let _dead = unsafe { completed_death(&mut arena, node) };
    let (_, keeper_b) = unsafe { kept_root(&mut arena, node, "BatchKeeperB") };
    assert_eq!(candidate_count(), 5);
    // The completed death ran its destructor on the way; the count from
    // here is the collection's.
    DESTRUCTOR_RUNS.store(0, Ordering::Relaxed);
    let mut expected = Vec::new();
    collect_lane_tokens(&mut expected);

    assert_eq!(
        served_by_a_collector(),
        Served::Batch {
            roots: 5,
            complete: true,
            backlog: false,
        }
    );
    assert_eq!(candidate_count(), 0, "R's front moved past the batch");
    assert_eq!(
        verdicts(),
        vec![
            Verdict::Proposed,
            Verdict::Proposed,
            Verdict::ReadLive,
            Verdict::ZeroCount,
            Verdict::ReadLive,
        ],
        "one verdict per root, in R's order"
    );
    let mut standing = Vec::new();
    collect_lane_tokens(&mut standing);
    assert_eq!(standing, expected, "every token once, now in P");
    assert_eq!(
        record_batch_size(),
        INITIAL_BATCH * 2,
        "a completed batch doubles K"
    );

    // The mutator's poll: the deaths retired and the kept roots deferred up
    // to the first proposal — which is first, so the poll arms and the
    // collection takes the ring out of P and disposes of the rest.
    assert_eq!(
        unsafe { ll_gc_maybe_collect() },
        2,
        "the ring was collected"
    );
    assert_eq!(DESTRUCTOR_RUNS.load(Ordering::Relaxed), 2);
    assert_eq!(verdict_count(), 0);
    assert_eq!(deferred_count(), 2, "the kept roots wait for the turnover");
    assert_eq!(candidate_count(), 0);

    unsafe {
        release_keeper(keeper_a);
        release_keeper(keeper_b);
    }
    reset_lanes();
}

/// This thread's record's batch size.
fn record_batch_size() -> usize {
    unsafe { &*record() }.batch_size()
}

#[test]
fn a_batch_is_clamped_to_ps_room_and_to_k() {
    let _g = test_guard();
    reset_lanes();
    let node = node_class("ClampNode");
    let mut arena = Arena::new();
    let keepers: Vec<*mut Object> = (0..INITIAL_BATCH + 3)
        .map(|index| unsafe { kept_root(&mut arena, node, &format!("ClampKeeper{index}")) }.1)
        .collect();
    assert_eq!(candidate_count(), INITIAL_BATCH + 3);

    // K first: the first batch takes the starting size and leaves three.
    assert_eq!(
        served_by_a_collector(),
        Served::Batch {
            roots: INITIAL_BATCH,
            complete: true,
            backlog: true,
        }
    );
    assert_eq!(candidate_count(), 3);
    assert_eq!(verdict_count(), INITIAL_BATCH);

    // Then P's room: filled to three short of full, the next batch takes
    // three and no more, whatever K says; and a full P takes nothing.
    let room_left = 3;
    unsafe {
        crate::cycle::queue::verdicts::testing::fill_for_test(
            BLOCK_ENTRIES - INITIAL_BATCH - room_left,
        )
    };
    assert_eq!(verdict_count(), BLOCK_ENTRIES - room_left);
    unsafe { kept_root(&mut arena, node, "ClampKeeperLate") };
    assert_eq!(candidate_count(), 4);
    assert_eq!(
        served_by_a_collector(),
        Served::Batch {
            roots: room_left,
            complete: true,
            backlog: true,
        }
    );
    assert_eq!(candidate_count(), 1);
    assert_eq!(
        served_by_a_collector(),
        Served::Idle,
        "a full P takes nothing"
    );
    assert_eq!(candidate_count(), 1);
    assert!(
        !unsafe { &*record() }.token.is_held(),
        "and made no claim to find that out"
    );

    // K's bound: a completed batch from the bound stays at it.
    discard_standing_verdicts();
    unsafe { &*record() }.set_batch_size(BATCH_BOUND);
    assert_eq!(
        served_by_a_collector(),
        Served::Batch {
            roots: 1,
            complete: true,
            backlog: false,
        }
    );
    assert_eq!(
        record_batch_size(),
        BATCH_BOUND,
        "K does not pass its bound"
    );
    assert!(BATCH_BOUND < BLOCK_ENTRIES);

    discard_standing_verdicts();
    for keeper in keepers {
        unsafe { release_keeper(keeper) };
    }
    reset_lanes();
}

#[test]
fn a_batch_that_meets_its_budget_posts_every_root_unwalked_and_halves_k() {
    let _g = test_guard();
    reset_lanes();
    DESTRUCTOR_RUNS.store(0, Ordering::Relaxed);
    let node = node_class("BudgetNode");
    let mut arena = Arena::new();
    let _garbage = unsafe { ring(&mut arena, [node, node]) };
    let (_, keeper) = unsafe { kept_root(&mut arena, node, "BudgetKeeper") };
    unsafe { &*record() }.set_batch_size(16);

    // A trace that may draw no block past the workspace, over a graph the
    // workspace cannot hold: the mark meets the budget, and no colour is a
    // verdict.
    testing::budget_the_next_batch(0);
    let big = node_class("BudgetBigRing");
    let members = unsafe { crate::cycle::testing::long_ring(&mut arena, big, 16_000) };
    assert_eq!(
        served_by_a_collector(),
        Served::Batch {
            roots: 16,
            complete: false,
            backlog: true,
        }
    );
    let posted = verdicts();
    assert_eq!(posted.len(), 16);
    assert!(
        posted.iter().all(|&verdict| verdict == Verdict::Unwalked),
        "no color of a trace that met its budget is a verdict: {posted:?}"
    );
    assert_eq!(record_batch_size(), 8, "K halved on the budget met");

    // The mutator's collection traces the unwalked roots exactly: the small
    // ring and the first members of the big one are its batch's P roots,
    // the rest of the big ring still stands in R, and one trace over both
    // frees everything.
    assert_eq!(unsafe { ll_gc_maybe_collect() }, 2 + members.len());
    assert_eq!(DESTRUCTOR_RUNS.load(Ordering::Relaxed), 2 + members.len());
    assert_eq!(verdict_count(), 0);
    unsafe { release_keeper(keeper) };
    reset_lanes();
}

#[test]
fn the_advance_follows_the_last_post_from_the_unwind_as_well() {
    let _g = test_guard();
    reset_lanes();
    let node = node_class("UnwindNode");
    let mut arena = Arena::new();
    let (_, keeper_a) = unsafe { kept_root(&mut arena, node, "UnwindKeeperA") };
    let (_, keeper_b) = unsafe { kept_root(&mut arena, node, "UnwindKeeperB") };

    testing::panic_before_the_next_advance();
    let sent = Sent(record());
    let outcome = testing::consent_while(std::thread::spawn(move || {
        assert!(crate::memory::heap::ll_thread_init());
        let record = sent.into_inner();
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
            testing::serve_alone(record)
        }))
    }));
    assert!(outcome.is_err(), "the batch panicked where the case asked");
    assert_eq!(
        candidate_count(),
        0,
        "the guard advanced R past the batch from the unwind"
    );
    assert_eq!(
        verdicts(),
        vec![Verdict::ReadLive; 2],
        "and both verdicts stand"
    );
    assert!(
        !unsafe { &*record() }.token.is_held(),
        "and the token was released"
    );

    unsafe {
        release_keeper(keeper_a);
        release_keeper(keeper_b);
    }
    reset_lanes();
}

/// The mutator's own claim, `MUTATOR` on the byte from its take through its
/// close, is what a collector's claim fails on: the collecting word is the
/// mutator's own gate and the collector never reads it.
#[test]
fn a_mutator_collecting_in_line_is_skipped() {
    let _g = test_guard();
    reset_lanes();
    let node = node_class("SkipNode");
    let mut arena = Arena::new();
    let (_, keeper) = unsafe { kept_root(&mut arena, node, "SkipKeeper") };

    let claim = crate::cycle::token::HeldToken::take();
    assert_eq!(served_by_a_collector(), Served::TokenHeld);
    assert_eq!(candidate_count(), 1, "nothing was taken");
    assert_eq!(verdict_count(), 0);
    drop(claim);
    assert_eq!(
        served_by_a_collector(),
        Served::Batch {
            roots: 1,
            complete: true,
            backlog: false,
        }
    );

    discard_standing_verdicts();
    unsafe { release_keeper(keeper) };
    reset_lanes();
}

#[test]
fn a_mutator_registering_throughout_the_batches_loses_no_root_and_doubles_none() {
    let _g = test_guard();
    reset_lanes();
    let node = node_class("ThroughoutNode");
    let mut arena = Arena::new();
    // Every object is built before the batches start, so the collector's
    // trace meets no header published under it; what the mutator does
    // during the batches is release, which registers.
    let count = 600;
    let objects: Vec<*mut Object> = (0..count)
        .map(|_| {
            let object = unsafe { object(&mut arena, node) };
            unsafe { ll_retain(object as *mut RcHeader) };
            object
        })
        .collect();
    let mut expected: Vec<*mut RcHeader> = objects.iter().map(|&o| o as *mut RcHeader).collect();
    expected.sort_unstable();

    // The first half is registered ahead, so the first batch has roots;
    // the batch waits between its last post and its advance, and the
    // second half registers while it stands peeked in R and posted in P.
    let half = count / 2;
    for &object in &objects[..half] {
        assert!(
            !unsafe { ll_release(object as *mut RcHeader) },
            "the case holds it"
        );
    }
    let (release, waiting_until) = std::sync::mpsc::channel::<()>();
    testing::make_the_next_batch_wait_before_its_advance(waiting_until);
    let (stop, stopped) = std::sync::mpsc::channel::<()>();
    let (batched_tell, batched) = std::sync::mpsc::channel::<()>();
    let sent = Sent(record());
    // The collector waits for each consent as the thread does, on the slot's
    // wake word, so that neither side spins on the byte (`dev/WORKFLOW.md`,
    // Miri, "A test thread waits, it does not spin").
    let collector = std::thread::spawn(move || {
        assert!(crate::memory::heap::ll_thread_init());
        testing::stand_in_as_the_elder();
        let record = sent.into_inner();
        let mut batches = 0;
        loop {
            if let Served::Batch { .. } = unsafe { testing::serve_alone(record) } {
                batches += 1;
                batched_tell.send(()).expect("the case counts");
            }
            if stopped.try_recv().is_ok() {
                return batches;
            }
            std::thread::yield_now();
        }
    });

    // The batch stands waiting once its verdicts are in P; this thread
    // consents to the request meanwhile, as its poll would.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while verdict_count() == 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "the first batch posted"
        );
        crate::cycle::token::read_and_act_on_this_thread();
        std::thread::yield_now();
    }
    let posted = verdict_count();
    assert!(posted >= 1 && posted <= INITIAL_BATCH);
    assert_eq!(
        candidate_count(),
        half,
        "R's front has not moved past the waiting batch"
    );
    for &object in &objects[half..] {
        assert!(
            !unsafe { ll_release(object as *mut RcHeader) },
            "the case holds it"
        );
    }
    assert_eq!(
        candidate_count(),
        count,
        "the second half landed behind the waiting batch"
    );
    release.send(()).expect("the batch is waiting");

    // The batches that follow take the rest, until R is empty, so that
    // every root the collector could take is taken. This thread consents
    // to each request and clears the `POSTED` each batch leaves, standing
    // in for the collections a mutator would run between them, which this
    // case makes by hand at its end.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let mut batches_seen = 0;
    loop {
        crate::cycle::token::read_and_act_on_this_thread();
        unsafe { &*record() }.token.clear_posted_for_test();
        while batched.try_recv().is_ok() {
            batches_seen += 1;
        }

        if batches_seen >= 2 && candidate_count() == 0 {
            break;
        }

        assert!(
            std::time::Instant::now() < deadline,
            "the batches drained R: {batches_seen} so far, {} left",
            candidate_count()
        );
        std::thread::yield_now();
    }
    stop.send(()).expect("the collector is looping");
    let batches = collector.join().expect("the collector finished");
    assert!(batches >= 2, "the waiting batch and at least one after it");

    let mut standing = Vec::new();
    collect_lane_tokens(&mut standing);
    standing.sort_unstable();
    assert_eq!(
        standing, expected,
        "every registration once, across R and P, and none twice"
    );

    // The mutator's next collection defers what P holds and traces what R
    // holds live: every root ends in one lane once.
    assert!(refill_spares());
    assert_eq!(unsafe { crate::gc::ll_gc_collect_cycles() }, 0);
    let mut after = Vec::new();
    collect_lane_tokens(&mut after);
    after.sort_unstable();
    assert_eq!(after, expected, "and once after the collection");
    assert_eq!(verdict_count(), 0);

    for object in objects {
        unsafe {
            assert!(ll_release(object as *mut RcHeader));
            ll_object_die(object);
        }
    }
    reset_lanes();
}
