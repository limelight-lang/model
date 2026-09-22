//! The standing list: a request a mutator did not answer inside the wait
//! stays on its byte and its record joins the collector's list, with no
//! capacity; a checkpoint walks the list only after a consent or a refusal,
//! serves one grant and releases the rest without a batch; a released
//! mutator's next request is pushed with no wait; a record whose thread
//! exited under a standing request is walked past by the registry until the
//! pass drops it; and a retired collector leaves every record unlinked
//! (`dev/design/the-standing-request-lives-on-the-record.md`).
//!
//! The collector here is the case's own thread with a `Standing` of its own,
//! on a slot no thread stands in, so that the slot's byte-event number moves
//! only for this case's consents and refusals; the one case that needs the
//! elder's own end births it.

use super::*;
use crate::class::Class;
use crate::cycle::mutator_record::{
    Ring, a_thread_asking_for, link_for_test, pin_for_test, registry_lists_free,
    ring_left_to_a_holder,
};
use crate::cycle::testing::long_ring;
use crate::cycle::token::{
    COLLECTOR, FREE, POSTED, REQUESTED, read_and_act_on_this_thread, state, word,
};
use crate::cycle::worker::testing::HeldRequestWait;
use crate::memory::block_pool::test_guard;
use std::time::Duration;

/// A slot no collector thread is born into under the default cap and no
/// other case names.
const SLOT: usize = 6;

/// Members of a sleeper's ring: the threshold, so that one batch at the
/// initial K takes it whole.
const RING: usize = SOFT_THRESHOLD;

// The burst case reads `Served::Idle` after the pass served a sleeper: its
// own reading finds R empty because one batch took the ring whole.
const _: () = assert!(RING <= INITIAL_BATCH);

/// A mutator with a ring in its R that reads its byte only when the case
/// tells it to: asleep to every request until then.
struct Sleeper {
    mutator: Mutator,
}

impl Sleeper {
    fn start(class: *const Class) -> Self {
        let mutator = Mutator::start_idling_with(|_| {});
        let class = Sent(class);
        mutator.run(move |arena| {
            let _ = unsafe { long_ring(arena, class.into_inner(), RING) };
        });
        Self { mutator }
    }

    fn record(&self) -> *mut MutatorRecord {
        self.mutator.record
    }

    fn byte(&self) -> u8 {
        unsafe { &*self.record() }.token.read()
    }

    fn is_standing(&self) -> bool {
        unsafe { &*self.record() }.is_standing()
    }

    /// One reading of the byte on the sleeper's thread: a consent to a
    /// standing request, an arming on `POSTED`.
    fn read_the_byte(&self) {
        self.mutator.run(|_| {
            read_and_act_on_this_thread();
        });
    }

    /// One poll on the sleeper's thread: the collection over P after a
    /// batch, which puts the byte back to `FREE`.
    fn poll(&self) {
        self.mutator.run(|_| unsafe {
            crate::gc::ll_gc_maybe_collect();
        });
    }

    /// Let the ring go and collect it, on the sleeper's thread.
    fn end(self) {
        self.mutator.run(|_| unsafe {
            crate::gc::ll_gc_collect_cycles();
        });
        drop(self.mutator);
    }
}

fn serve_on_this_thread(record: *mut MutatorRecord, standing: &mut Standing) -> Served {
    unsafe { serve(record, SLOT, 1, standing) }
}

/// Twenty sleepers, all left standing by one sweep of requests, and every
/// one served once the sleepers answer, one per pass, the rest released
/// without a batch and pushed again with no wait, in the walk's order.
#[test]
fn twenty_sleepers_stand_and_are_all_served_on_waking() {
    let _g = test_guard();
    reset_lanes();
    let _wait = HeldRequestWait::of(Duration::from_millis(2));
    let class = node_class("StandingListNode");
    let sleepers: Vec<Sleeper> = (0..20).map(|_| Sleeper::start(class)).collect();
    let mut standing = Standing::new(SLOT);
    let _ = testing::take_outcomes();
    let _ = testing::take_releases_unserved();

    for sleeper in &sleepers {
        assert_eq!(
            serve_on_this_thread(sleeper.record(), &mut standing),
            Served::Unanswered
        );
        assert_eq!(
            sleeper.byte(),
            word(REQUESTED, SLOT),
            "standing past the wait"
        );
        assert!(sleeper.is_standing());
    }
    assert_eq!(standing.standing_for_test().len(), 20, "no capacity");

    // A burst: every sleeper answers, the first serve's pass serves the
    // first of them and releases the other nineteen without a batch.
    for sleeper in &sleepers {
        sleeper.read_the_byte();
        assert_eq!(sleeper.byte(), word(COLLECTOR, SLOT));
    }
    assert_eq!(
        serve_on_this_thread(sleepers[0].record(), &mut standing),
        Served::Idle,
        "the pass served this one, whose R its own reading then found empty"
    );
    assert_eq!(state(sleepers[0].byte()), POSTED);
    assert_eq!(testing::take_outcomes().batches, 1);
    assert_eq!(testing::take_releases_unserved(), 19);
    assert!(standing.standing_for_test().is_empty());
    for sleeper in &sleepers[1..] {
        assert_eq!(sleeper.byte(), FREE, "released without a batch");
        assert!(unsafe { &*sleeper.record() }.was_released_unserved());
        assert!(!sleeper.is_standing());
    }

    // The released are asleep again to the walk: their requests are pushed
    // with no wait, the mark cleared. This walk runs backwards, so that the
    // list's order is the walk's and not the release's.
    for sleeper in sleepers[1..].iter().rev() {
        assert_eq!(
            serve_on_this_thread(sleeper.record(), &mut standing),
            Served::Unanswered
        );
        assert!(!unsafe { &*sleeper.record() }.was_released_unserved());
        assert!(sleeper.is_standing());
    }
    assert_eq!(standing.standing_for_test().len(), 19);

    // Round after round: the standing consent, one served per pass, the
    // list's head first — the last pushed above — and then in the walk's
    // order, which every re-push after a release follows.
    sleepers[0].poll();
    let mut served_in_order = Vec::new();
    for _ in 0..19 {
        for sleeper in &sleepers {
            if sleeper.byte() == word(REQUESTED, SLOT) {
                sleeper.read_the_byte();
            }
        }
        let standing_before = standing.standing_for_test();
        for sleeper in &sleepers {
            let _ = serve_on_this_thread(sleeper.record(), &mut standing);
        }
        let posted: Vec<usize> = sleepers
            .iter()
            .enumerate()
            .filter(|(_, sleeper)| state(sleeper.byte()) == POSTED)
            .map(|(index, _)| index)
            .collect();
        assert_eq!(posted.len(), 1, "one batch per pass");
        assert_eq!(
            sleepers[posted[0]].record(),
            standing_before[0],
            "the first standing was the one served"
        );
        served_in_order.push(posted[0]);
        sleepers[posted[0]].poll();
    }
    let mut expected = vec![19];
    expected.extend(1..19);
    assert_eq!(served_in_order, expected);
    assert!(standing.standing_for_test().is_empty());
    assert_eq!(testing::take_outcomes().batches, 19);

    drop(standing);
    for sleeper in sleepers {
        sleeper.end();
    }
    reset_lanes();
}

/// A record whose thread exited under a standing request stays linked, and
/// the registry walks past it — a thread asking for it by name is served
/// another record — until the collector's next checkpoint, which the exit's
/// refusal admitted, drops it.
#[test]
fn a_record_that_exited_under_a_standing_request_is_handed_out_after_the_pass() {
    let _g = test_guard();
    reset_lanes();
    let _wait = HeldRequestWait::of(Duration::from_millis(2));
    let class = node_class("ExitedStandingNode");
    let sleeper = Sleeper::start(class);
    let record = sleeper.record();
    let mut standing = Standing::new(SLOT);
    assert_eq!(
        serve_on_this_thread(record, &mut standing),
        Served::Unanswered
    );
    assert!(sleeper.is_standing());

    pin_for_test(record, true);
    sleeper.end();
    assert!(crate::cycle::mutator_record::registry_lists_free(record));
    assert!(
        unsafe { &*record }.is_standing(),
        "the exit's refusal left the record in the list"
    );
    assert!(
        !a_thread_asking_for(record),
        "a linked record is walked past"
    );

    // The exit's refusal moved the slot's byte number; the next checkpoint
    // passes and drops the record that moved on.
    let _ = testing::take_passes();
    assert_eq!(
        serve_on_this_thread(super::record(), &mut standing),
        Served::Idle,
        "this thread's own R is empty"
    );
    assert_eq!(testing::take_passes(), 1);
    assert!(!unsafe { &*record }.is_standing(), "dropped by the pass");
    assert!(a_thread_asking_for(record), "and handed out");
    pin_for_test(record, false);
    reset_lanes();
}

/// A request refused by the mutator's own take — its exit inside the
/// reading — finds the record unlinked before the reading's hold goes: the
/// ring the exit left to the hold is still the hold's at the refusal, so the
/// hand-back that puts the record's blocks on their way follows the unlink,
/// and the registry's gate, which the hand-back orders after, reads the link
/// words clear. A link followed by a failed request is published by nothing
/// else (`MutatorRecord::standing_next`).
#[test]
fn a_refused_request_unlinks_before_the_readings_hold_goes() {
    let _g = test_guard();
    reset_lanes();
    let _wait = HeldRequestWait::of(Duration::from_millis(2));
    let class = node_class("RefusedUnderHoldNode");
    let sleeper = Sleeper::start(class);
    let record = sleeper.record();
    let mut standing = Standing::new(SLOT);
    pin_for_test(record, true);

    // Between the reading's loads and the request: the sleeper's thread
    // exits, its take of the token the refusal the request meets, its rings
    // left to the hold.
    let exiting = Sent(sleeper);
    testing::before_the_next_request(Box::new(move || {
        drop(exiting.into_inner());
    }));
    let (told, left_at_the_refusal) = std::sync::mpsc::channel::<bool>();
    let seen = Sent(record);
    testing::at_the_next_refusal(Box::new(move || {
        told.send(ring_left_to_a_holder(seen.into_inner(), Ring::Candidates))
            .expect("the case waits");
    }));

    assert_eq!(
        serve_on_this_thread(record, &mut standing),
        Served::TokenHeld
    );
    assert!(
        left_at_the_refusal
            .recv()
            .expect("the refusal ran the hook"),
        "the exit's ring is still the hold's at the refusal: the hand-back follows the unlink"
    );
    assert!(!unsafe { &*record }.is_standing());
    assert!(standing.standing_for_test().is_empty());
    assert!(
        !ring_left_to_a_holder(record, Ring::Candidates),
        "returned by the hand-back"
    );
    assert!(registry_lists_free(record));
    pin_for_test(record, false);
    reset_lanes();
}

/// A consent that lands between the serve's reading hold and its request:
/// the request reads the grant back, the grant is served there, and the
/// record leaves the list before the batch.
#[test]
fn a_consent_between_the_reading_and_the_request_is_served_with_the_record_unlinked() {
    let _g = test_guard();
    reset_lanes();
    let _wait = HeldRequestWait::of(Duration::from_millis(2));
    let class = node_class("LateConsentNode");
    let sleeper = Sleeper::start(class);
    let record = sleeper.record();
    let mut standing = Standing::new(SLOT);
    assert_eq!(
        serve_on_this_thread(record, &mut standing),
        Served::Unanswered
    );
    assert!(sleeper.is_standing());

    // Inside the reading, on this thread: the sleeper reads its byte and
    // consents. The checkpoint at the top of the serve ran before that and
    // read nothing, so the grant is what the request reads back.
    let jobs = sleeper.mutator.jobs.clone();
    let (told, consented) = std::sync::mpsc::channel::<()>();
    testing::at_the_next_reading(Box::new(move || {
        jobs.send(Box::new(move |_| {
            read_and_act_on_this_thread();
            told.send(()).expect("the reading waits");
        }))
        .expect("the sleeper runs");
        consented.recv().expect("the sleeper consented");
    }));
    let _ = testing::take_passes();
    let served = serve_on_this_thread(record, &mut standing);
    assert!(
        matches!(served, Served::Batch { .. }),
        "the grant read back at the request was served: {served:?}"
    );
    assert_eq!(
        testing::take_passes(),
        0,
        "no pass: the consent came after the checkpoint"
    );
    assert!(!sleeper.is_standing(), "unlinked before the batch");
    assert!(standing.standing_for_test().is_empty());

    sleeper.poll();
    sleeper.end();
    reset_lanes();
}

/// A checkpoint walks the list only after a consent or a refusal on the
/// slot: idle serves beside a standing sleeper make no pass, and the
/// sleeper's one consent makes one, which serves it.
#[test]
fn a_checkpoint_passes_once_per_byte_event_and_not_per_serve() {
    let _g = test_guard();
    reset_lanes();
    let _wait = HeldRequestWait::of(Duration::from_millis(2));
    let class = node_class("PassCountNode");
    let sleeper = Sleeper::start(class);
    let mut standing = Standing::new(SLOT);
    assert_eq!(
        serve_on_this_thread(sleeper.record(), &mut standing),
        Served::Unanswered
    );
    let _ = testing::take_passes();
    let _ = testing::take_outcomes();

    for _ in 0..5 {
        assert_eq!(
            serve_on_this_thread(super::record(), &mut standing),
            Served::Idle
        );
    }
    assert_eq!(testing::take_passes(), 0, "no byte event, no pass");
    assert_eq!(testing::take_outcomes().batches, 0);

    sleeper.read_the_byte();
    assert_eq!(
        serve_on_this_thread(super::record(), &mut standing),
        Served::Idle
    );
    assert_eq!(testing::take_passes(), 1, "the consent admitted one pass");
    assert_eq!(
        testing::take_outcomes().batches,
        1,
        "which served the grant"
    );
    assert!(!sleeper.is_standing());

    sleeper.poll();
    sleeper.end();
    reset_lanes();
}

/// A retired collector withdraws every standing request and leaves every
/// record unlinked: the elder, born over sleepers whose requests stood past
/// the wait, ends, and the sleepers' bytes read `FREE` with their records in
/// no list.
#[test]
fn a_retired_collector_leaves_every_record_unlinked() {
    let _g = test_guard();
    reset_lanes();
    let _wait = HeldRequestWait::of(Duration::from_millis(2));
    let class = node_class("RetiredStandingNode");
    let sleepers: Vec<Sleeper> = (0..3).map(|_| Sleeper::start(class)).collect();
    let records: Vec<*mut MutatorRecord> = sleepers.iter().map(Sleeper::record).collect();
    let _end = RetireOnDrop;
    testing::confine_rounds_to_records(&records);
    testing::wait_between_rounds_for(Some(PAST_THE_CASE));
    testing::permit_births(true);
    let _ = testing::take_rounds();
    ensure_thread();
    assert!(
        wait_until(
            || sleepers.iter().all(|sleeper| sleeper.is_standing()),
            A_BIRTH
        ),
        "the elder's round left every request standing"
    );
    for sleeper in &sleepers {
        assert_eq!(sleeper.byte(), word(REQUESTED, ELDER));
    }

    testing::retire();
    for sleeper in &sleepers {
        assert_eq!(sleeper.byte(), FREE, "withdrawn at the thread's end");
        assert!(!sleeper.is_standing(), "and unlinked");
    }

    for sleeper in sleepers {
        sleeper.end();
    }
    reset_lanes();
}

/// The list's own shape: a push appends at the tail and is idempotent, a
/// forget takes a record out from the first, the middle or the last place
/// and is a no-op on an unlinked one, and the hand link the registry case
/// stands in with reads as the list's own.
#[test]
fn a_push_appends_and_a_forget_unlinks_from_any_place() {
    let _g = test_guard();
    reset_lanes();
    let class = node_class("ListShapeNode");
    let sleepers: Vec<Sleeper> = (0..3).map(|_| Sleeper::start(class)).collect();
    let records: Vec<*mut MutatorRecord> = sleepers.iter().map(Sleeper::record).collect();
    let mut standing = Standing::new(SLOT);

    for &record in &records {
        standing.push(record);
    }
    standing.push(records[1]);
    assert_eq!(standing.standing_for_test(), records, "appended, once each");
    assert!(
        records
            .iter()
            .all(|&record| unsafe { &*record }.is_standing())
    );

    standing.forget(unsafe { &*records[1] });
    assert_eq!(standing.standing_for_test(), vec![records[0], records[2]]);
    assert!(!unsafe { &*records[1] }.is_standing());
    standing.forget(unsafe { &*records[1] });
    standing.forget(unsafe { &*records[0] });
    assert_eq!(standing.standing_for_test(), vec![records[2]]);
    standing.forget(unsafe { &*records[2] });
    assert!(standing.standing_for_test().is_empty());
    assert!(
        records
            .iter()
            .all(|&record| !unsafe { &*record }.is_standing())
    );

    standing.push(records[2]);
    standing.push(records[0]);
    assert_eq!(standing.standing_for_test(), vec![records[2], records[0]]);
    standing.forget(unsafe { &*records[0] });
    standing.forget(unsafe { &*records[2] });

    link_for_test(records[0], true);
    assert!(
        unsafe { &*records[0] }.is_standing(),
        "the hand link reads as standing"
    );
    link_for_test(records[0], false);
    assert!(!unsafe { &*records[0] }.is_standing());

    drop(standing);
    for sleeper in sleepers {
        sleeper.end();
    }
    reset_lanes();
}
