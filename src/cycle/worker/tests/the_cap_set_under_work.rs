//! A collector cap set to zero while collectors work, and set back: the next
//! round withdraws a request left standing and releases a grant it reads
//! back with no batch, a trace already running finishes and its `POSTED` is
//! collected over, a second collector's list is withdrawn by its own round,
//! and a cap set back resumes the takes (`dev/S65-PLAN-CRITIC.md`, F5).
//!
//! The collector is the case's thread with a `Standing` of its own on a slot
//! no thread stands in, as in `the_standing_list`.

use super::the_cap_at_zero::CapAtZero;
use super::the_standing_list::{SLOT, Sleeper, serve_as_its_own_round};
use super::*;
use crate::cycle::token::{ASKED, COLLECTOR, FREE, POSTED, REQUESTED, word};
use crate::cycle::worker::testing::HeldRequestWait;
use crate::memory::block_pool::test_guard;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::time::Duration;

/// A second slot no thread stands in, for the case with two collectors.
const SECOND_SLOT: usize = 7;

/// Name `sleeper` to `slot` and confine the rounds to it, so that a round of
/// the slot visits it alone.
fn visited_by(sleeper: &Sleeper, slot: usize) {
    unsafe { &*sleeper.record() }.name_to_collector(slot);
    testing::confine_rounds_to(sleeper.record());
}

/// Hand `sleeper` back to the elder and lift the confinement, then let its
/// ring go.
fn release(sleeper: Sleeper) {
    testing::confine_rounds_to_records(&[]);
    unsafe { &*sleeper.record() }.name_to_collector(ELDER);
    sleeper.end();
    reset_lanes();
}

/// A request left standing on a sleeping mutator when the cap goes to zero
/// is withdrawn by the round, the record unlinked, and the same round's
/// visit asks for the ring in line instead; the mutator's waking consents to
/// nothing, and its poll collects the ring. Red on a round that ran its
/// checkpoint whatever the cap: the list stayed, and the waking consented to
/// a request no batch would serve.
#[test]
fn a_request_standing_when_the_cap_goes_to_zero_is_withdrawn_and_asked_over() {
    let _g = test_guard();
    reset_lanes();
    let _wait = HeldRequestWait::of(Duration::from_millis(2));
    let sleeper = Sleeper::start(node_class("WithdrawnAtZeroNode"));
    visited_by(&sleeper, SLOT);
    let mut standing = Standing::new(SLOT);
    assert_eq!(
        serve_as_its_own_round(sleeper.record(), &mut standing),
        Served::Unanswered
    );
    assert_eq!(sleeper.byte(), word(REQUESTED, SLOT));
    let _ = testing::take_outcomes();

    let cap = CapAtZero::set();
    // An instant stamped and X passed, so that the round's visit advances.
    let turnovers = unsafe { &*sleeper.record() }.turnovers();
    let _ = unsafe { &*sleeper.record() }.take_new_life();
    unsafe { &*sleeper.record() }.note_advanced_at(serve_clock_now());
    testing::advance_epochs_after(Some(Duration::from_millis(1)));
    std::thread::sleep(Duration::from_millis(5));
    let _ = round(SLOT, 1, &mut standing);
    testing::advance_epochs_after(None);
    assert!(
        standing.standing_for_test().is_empty(),
        "the list withdrawn"
    );
    assert!(!sleeper.is_standing(), "and the record unlinked");
    assert_eq!(sleeper.byte(), ASKED, "the visit asked instead");
    assert!(
        unsafe { &*sleeper.record() }.turnovers() > turnovers,
        "and the clock still turned"
    );

    sleeper.read_the_byte();
    assert_eq!(sleeper.byte(), ASKED, "the waking consented to nothing");
    sleeper.poll();
    assert_eq!(testing::take_outcomes().grants, 0, "no grant in the case");
    drop(cap);
    release(sleeper);
}

/// A grant the mutator consented to while the cap went to zero, read back by
/// the round's withdrawal, is released with no batch.
#[test]
fn a_grant_read_back_at_the_cap_of_zero_is_released_with_no_batch() {
    let _g = test_guard();
    reset_lanes();
    let _wait = HeldRequestWait::of(Duration::from_millis(2));
    let sleeper = Sleeper::start(node_class("GrantAtZeroNode"));
    visited_by(&sleeper, SLOT);
    let mut standing = Standing::new(SLOT);
    assert_eq!(
        serve_as_its_own_round(sleeper.record(), &mut standing),
        Served::Unanswered
    );
    sleeper.read_the_byte();
    assert_eq!(sleeper.byte(), word(COLLECTOR, SLOT), "the consent landed");
    let _ = testing::take_outcomes();

    let cap = CapAtZero::set();
    let _ = round(SLOT, 1, &mut standing);
    let outcomes = testing::take_outcomes();
    assert_eq!(
        outcomes.grants, 0,
        "the grant was served no batch: {outcomes:?}"
    );
    assert_eq!(outcomes.batches, 0);
    assert!(!sleeper.is_standing());
    assert_eq!(sleeper.byte(), ASKED, "released, then asked over");

    sleeper.poll();
    drop(cap);
    release(sleeper);
}

/// A trace running when the cap goes to zero finishes: its batch posts every
/// verdict, the next round under the cap asks nothing over the `POSTED` it
/// left, and the mutator's poll collects over P whole.
#[test]
fn a_trace_running_when_the_cap_goes_to_zero_finishes_and_p_is_collected() {
    let _g = test_guard();
    reset_lanes();
    let freed = Arc::new(AtomicUsize::new(0));
    let mutator = Mutator::start_polling(Arc::clone(&freed));
    let record = mutator.record;
    unsafe { &*record }.name_to_collector(SLOT);
    testing::confine_rounds_to(record);
    let class = Sent(node_class("TraceAtZeroNode"));
    mutator.run(move |arena| {
        let class = class.into_inner();
        for _ in 0..SOFT_THRESHOLD / 2 {
            let _ = unsafe { crate::cycle::testing::ring(arena, [class, class]) };
        }
    });

    let cap = std::sync::Arc::new(std::sync::Mutex::new(None));
    let set = Arc::clone(&cap);
    testing::at_the_start_of_the_next_trace(Box::new(move || {
        *set.lock().expect("the case holds no lock") = Some(CapAtZero::set());
    }));
    let mut standing = Standing::new(SLOT);
    let served = round(SLOT, SOFT_THRESHOLD, &mut standing);
    assert!(served.made_a_batch, "the trace ran to its batch");
    assert!(collectors_capped_at_zero(), "under the cap it set");

    let _ = testing::take_outcomes();
    let _ = round(SLOT, SOFT_THRESHOLD, &mut standing);
    assert!(
        wait_until(
            || freed.load(std::sync::atomic::Ordering::Relaxed) == SOFT_THRESHOLD,
            A_BIRTH
        ),
        "the mutator's poll collected over P whole"
    );
    assert_ne!(unsafe { &*record }.token.read(), POSTED);
    assert_eq!(
        testing::take_outcomes().asked,
        0,
        "the round after the batch asked for nothing: R was left empty"
    );
    assert!(
        mutator.run(|_| crate::gc::verdict_collections_on_this_thread()) >= 1,
        "what freed the rings was the collection over P"
    );

    drop(cap.lock().expect("the hook ran").take());
    testing::confine_rounds_to_records(&[]);
    unsafe { &*record }.name_to_collector(ELDER);
    drop(mutator);
    reset_lanes();
}

/// Two collectors, each with a request standing on a mutator of its own: each
/// one's next round under the cap withdraws its own list and no other's.
#[test]
fn each_collector_withdraws_its_own_list_at_its_next_round() {
    let _g = test_guard();
    reset_lanes();
    let _wait = HeldRequestWait::of(Duration::from_millis(2));
    let class = node_class("TwoListsAtZeroNode");
    let first = Sleeper::start(class);
    let second = Sleeper::start(class);
    unsafe { &*first.record() }.name_to_collector(SLOT);
    unsafe { &*second.record() }.name_to_collector(SECOND_SLOT);
    testing::confine_rounds_to_records(&[first.record(), second.record()]);
    let mut first_list = Standing::new(SLOT);
    let mut second_list = Standing::new(SECOND_SLOT);
    first_list.start_a_round();
    second_list.start_a_round();
    assert_eq!(
        unsafe { serve(first.record(), SLOT, 1, &mut first_list, serve_clock_now()) },
        Served::Unanswered
    );
    assert_eq!(
        unsafe {
            serve(
                second.record(),
                SECOND_SLOT,
                1,
                &mut second_list,
                serve_clock_now(),
            )
        },
        Served::Unanswered
    );

    let cap = CapAtZero::set();
    let _ = round(SLOT, 1, &mut first_list);
    assert!(!first.is_standing());
    assert!(second.is_standing(), "the other collector's list stands");
    assert_eq!(second.byte(), word(REQUESTED, SECOND_SLOT));
    let _ = round(SECOND_SLOT, 1, &mut second_list);
    assert!(!second.is_standing(), "until its own round");
    assert_eq!(second.byte(), ASKED);

    first.poll();
    second.poll();
    drop(cap);
    testing::confine_rounds_to_records(&[]);
    unsafe { &*second.record() }.name_to_collector(ELDER);
    release(first);
}

/// A cap set back above zero resumes the takes at the next round: the round
/// that asked under the cap is followed, once the ask is collected over and a
/// ring registered again, by a batch.
#[test]
fn a_cap_set_back_resumes_the_takes() {
    let _g = test_guard();
    reset_lanes();
    let freed = Arc::new(AtomicUsize::new(0));
    let mutator = Mutator::start_polling(Arc::clone(&freed));
    let record = mutator.record;
    unsafe { &*record }.name_to_collector(SLOT);
    testing::confine_rounds_to(record);
    let class = Sent(node_class("ResumedTakesNode"));
    let register = || {
        let class = Sent(class.0);
        mutator.run(move |arena| {
            let class = class.into_inner();
            for _ in 0..SOFT_THRESHOLD / 2 {
                let _ = unsafe { crate::cycle::testing::ring(arena, [class, class]) };
            }
        });
    };
    let mut standing = Standing::new(SLOT);

    let cap = CapAtZero::set();
    register();
    let _ = testing::take_outcomes();
    let _ = round(SLOT, SOFT_THRESHOLD, &mut standing);
    assert_eq!(testing::take_outcomes().asked, 1);
    assert!(wait_until(
        || freed.load(std::sync::atomic::Ordering::Relaxed) == SOFT_THRESHOLD,
        A_BIRTH
    ));

    drop(cap);
    register();
    assert!(
        round(SLOT, SOFT_THRESHOLD, &mut standing).made_a_batch,
        "the round under the cap set back took a batch"
    );
    assert_eq!(testing::take_outcomes().asked, 0);

    testing::confine_rounds_to_records(&[]);
    unsafe { &*record }.name_to_collector(ELDER);
    drop(mutator);
    reset_lanes();
}

/// A consent that lands after the cap was stored as zero, inside the wait of
/// a serve that requested under the positive cap, is read back as a grant
/// and released with no batch: no trace starts after the store. Red on the
/// wait that served every grant it read, which traced a full batch under the
/// cap (the Critic of S65.15, finding 1).
#[test]
fn a_consent_after_the_cap_went_to_zero_starts_no_trace() {
    let _g = test_guard();
    reset_lanes();
    let _wait = HeldRequestWait::of(Duration::from_secs(10));
    let mutator = Mutator::start();
    let record = mutator.record;
    let class = Sent(node_class("ConsentAfterTheStoreNode"));
    mutator.run(move |arena| {
        let _ = unsafe { crate::cycle::testing::long_ring(arena, class.into_inner(), 2) };
    });
    let cap = std::sync::Arc::new(std::sync::Mutex::new(None));
    let set = Arc::clone(&cap);
    testing::after_the_next_consents_swap(Box::new(move || {
        *set.lock().expect("the case holds no lock") = Some(CapAtZero::set());
    }));
    let _ = testing::take_outcomes();

    let mut standing = Standing::new(SLOT);
    let served = serve_as_its_own_round(record, &mut standing);
    assert_eq!(served, Served::Idle, "the grant was released unserved");
    let outcomes = testing::take_outcomes();
    assert_eq!(outcomes.grants, 0, "no batch under the grant: {outcomes:?}");
    assert_eq!(unsafe { &*record }.token.read(), FREE);
    assert!(standing.standing_for_test().is_empty());

    drop(cap.lock().expect("the hook ran").take());
    mutator.run(|_| unsafe {
        crate::gc::ll_gc_collect_cycles();
    });
    drop(mutator);
    reset_lanes();
}

/// A cap set back above zero after a round under it withdrew a standing
/// request: the next request stands as any does, the mutator's consent is
/// read at the next checkpoint, and that checkpoint serves a batch.
#[test]
fn the_takes_resume_after_a_withdrawn_list() {
    let _g = test_guard();
    reset_lanes();
    let _wait = HeldRequestWait::of(Duration::from_millis(2));
    let class = node_class("ResumedAfterWithdrawalNode");
    let sleeper = Sleeper::start(class);
    visited_by(&sleeper, SLOT);
    let mut standing = Standing::new(SLOT);
    assert_eq!(
        serve_as_its_own_round(sleeper.record(), &mut standing),
        Served::Unanswered
    );

    let cap = CapAtZero::set();
    let _ = round(SLOT, 1, &mut standing);
    assert!(!sleeper.is_standing(), "withdrawn under the cap");
    sleeper.poll();
    drop(cap);

    let class = Sent(class);
    sleeper.mutator.run(move |arena| {
        let _ = unsafe { crate::cycle::testing::long_ring(arena, class.into_inner(), 2) };
    });
    let _ = testing::take_outcomes();
    let _ = round(SLOT, 1, &mut standing);
    assert!(sleeper.is_standing(), "a request stands again");
    sleeper.read_the_byte();
    assert_eq!(standing.checkpoint(1), 1, "and its consent is served");
    assert_eq!(testing::take_outcomes().batches, 1);

    sleeper.poll();
    release(sleeper);
}
