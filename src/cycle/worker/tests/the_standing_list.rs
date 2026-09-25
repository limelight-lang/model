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
pub(super) const SLOT: usize = 6;

/// Members of a sleeper's ring: the threshold, so that one batch at the
/// initial K takes it whole.
const RING: usize = SOFT_THRESHOLD;

// The burst case reads `Served::Idle` after the pass served a sleeper: its
// own reading finds R empty because one batch took the ring whole.
const _: () = assert!(RING <= INITIAL_BATCH);

/// A mutator with a ring in its R that reads its byte only when the case
/// tells it to: asleep to every request until then.
pub(super) struct Sleeper {
    pub(super) mutator: Mutator,
}

impl Sleeper {
    /// A sleeper whose R holds `rings` rings of `members` each: a batch
    /// over one of its roots frees that ring alone, where one long ring is
    /// freed whole by its first root.
    fn start_with_rings(class: *const Class, rings: usize, members: usize) -> Self {
        let mutator = Mutator::start_idling_with(|_| {});
        let class = Sent(class);
        mutator.run(move |arena| {
            let class = class.into_inner();
            for _ in 0..rings {
                let _ = unsafe { long_ring(arena, class, members) };
            }
        });
        Self { mutator }
    }

    pub(super) fn start(class: *const Class) -> Self {
        let mutator = Mutator::start_idling_with(|_| {});
        let class = Sent(class);
        mutator.run(move |arena| {
            let _ = unsafe { long_ring(arena, class.into_inner(), RING) };
        });
        Self { mutator }
    }

    pub(super) fn record(&self) -> *mut MutatorRecord {
        self.mutator.record
    }

    pub(super) fn byte(&self) -> u8 {
        unsafe { &*self.record() }.token.read()
    }

    pub(super) fn is_standing(&self) -> bool {
        unsafe { &*self.record() }.is_standing()
    }

    /// One reading of the byte on the sleeper's thread: a consent to a
    /// standing request, an arming on `POSTED`.
    pub(super) fn read_the_byte(&self) {
        self.mutator.run(|_| {
            read_and_act_on_this_thread();
        });
    }

    /// One poll on the sleeper's thread: the collection over P after a
    /// batch, which puts the byte back to `FREE`.
    pub(super) fn poll(&self) {
        self.mutator.run(|_| unsafe {
            crate::gc::ll_gc_maybe_collect();
        });
    }

    /// Let the ring go and collect it, on the sleeper's thread.
    pub(super) fn end(self) {
        self.mutator.run(|_| unsafe {
            crate::gc::ll_gc_collect_cycles();
        });
        drop(self.mutator);
    }
}

/// One serve of `record` on the case's thread, each standing in for a
/// round's visit of its own: the walk's count of expired waits starts
/// empty, so a case that sweeps twenty sleepers reads the wait on every
/// one of them rather than on the first
/// [`super::super::EXPIRED_WAITS_PER_ROUND`]. What the bound does within
/// one walk is `the_take_after_an_interval`'s.
pub(super) fn serve_as_its_own_round(
    record: *mut MutatorRecord,
    standing: &mut Standing,
) -> Served {
    standing.start_a_round();
    unsafe { serve(record, SLOT, 1, standing, serve_clock_now()) }
}

/// Twenty sleepers, all left standing by one sweep of requests, and every
/// one served once the sleepers answer, one per pass, the rest released
/// without a batch and pushed again with no wait, in the walk's order.
#[test]
#[cfg_attr(
    miri,
    ignore = "twenty-one interpreted threads: killed at 50 minutes in a process of its own, twice on 2026-09-22; the list's link, unlink and stamp are covered under Miri by the cases above"
)]
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
            serve_as_its_own_round(sleeper.record(), &mut standing),
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
        serve_as_its_own_round(sleepers[0].record(), &mut standing),
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
            serve_as_its_own_round(sleeper.record(), &mut standing),
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
            let _ = serve_as_its_own_round(sleeper.record(), &mut standing);
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
        serve_as_its_own_round(record, &mut standing),
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
        serve_as_its_own_round(super::record(), &mut standing),
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
        serve_as_its_own_round(record, &mut standing),
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
        serve_as_its_own_round(record, &mut standing),
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
    let served = serve_as_its_own_round(record, &mut standing);
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
        serve_as_its_own_round(sleeper.record(), &mut standing),
        Served::Unanswered
    );
    let _ = testing::take_passes();
    let _ = testing::take_outcomes();

    for _ in 0..5 {
        assert_eq!(
            serve_as_its_own_round(super::record(), &mut standing),
            Served::Idle
        );
    }
    assert_eq!(testing::take_passes(), 0, "no byte event, no pass");
    assert_eq!(testing::take_outcomes().batches, 0);

    sleeper.read_the_byte();
    assert_eq!(
        serve_as_its_own_round(super::record(), &mut standing),
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

    link_for_test(records[0], SLOT, true);
    assert!(
        unsafe { &*records[0] }.is_standing(),
        "the hand link reads as standing"
    );
    link_for_test(records[0], SLOT, false);
    assert!(!unsafe { &*records[0] }.is_standing());

    drop(standing);
    for sleeper in sleepers {
        sleeper.end();
    }
    reset_lanes();
}

/// A batch made at a checkpoint reads its own backlog, and the round that
/// visits the record afterwards reads it out of the list's carried one:
/// the walk that would have made the reading for itself is elsewhere —
/// past its bound of waits, or between rounds — and a birth counts what
/// the round read (`dev/DECISIONS.md`, "a checkpoint carries its batch's
/// backlog and a refusal it read out to the round").
#[test]
fn a_checkpoints_batch_carries_its_backlog_to_the_round() {
    let _g = test_guard();
    reset_lanes();
    let _wait = HeldRequestWait::of(Duration::from_millis(2));
    let class = node_class("CarriedBacklogNode");
    let sleeper = Sleeper::start(class);
    let record = unsafe { &*sleeper.record() };
    // One root per batch, so the batch the checkpoint makes leaves the
    // rest of the ring behind it.
    record.set_batch_size(1);
    let mut standing = Standing::new(SLOT);

    assert_eq!(
        serve_as_its_own_round(sleeper.record(), &mut standing),
        Served::Unanswered
    );
    sleeper.read_the_byte();
    assert_eq!(standing.checkpoint(1), 1, "the pass served the grant");
    assert_eq!(
        standing.backlogged.len(),
        1,
        "and kept the mutator the batch left at the threshold"
    );

    // The round that visits the record reads it out: the serve itself
    // answers `Posted`, the mutator not having disposed of the batch.
    record.name_to_collector(SLOT);
    testing::confine_rounds_to(sleeper.record());
    let outcome = round(SLOT, 1, &mut standing);
    assert_eq!(outcome.backlogged.len(), 1, "the round read the backlog");
    assert_eq!(standing.backlogged.len(), 0, "and the list carries none");

    testing::confine_rounds_to_records(&[]);
    record.name_to_collector(ELDER);
    sleeper.poll();
    sleeper.end();
}

/// A mutator that takes its token over a standing request is collecting in
/// line: the pass reads that and the round counts it as work, so a sibling
/// whose mutators all collect in line is not read idle and ended.
#[test]
fn a_pass_reads_a_refusal_of_a_standing_request_as_work() {
    let _g = test_guard();
    reset_lanes();
    let _wait = HeldRequestWait::of(Duration::from_millis(2));
    let class = node_class("PassWorkNode");
    let sleeper = Sleeper::start(class);
    let mut standing = Standing::new(SLOT);
    assert_eq!(
        serve_as_its_own_round(sleeper.record(), &mut standing),
        Served::Unanswered
    );
    assert!(!standing.saw_work, "nothing read yet");

    // The mutator's own claim over the standing request, held until the
    // case lets it go, so that the pass reads `MUTATOR` rather than the
    // `FREE` a finished collection leaves.
    let (let_go, wait_here) = std::sync::mpsc::channel::<()>();
    let (took, taken) = std::sync::mpsc::channel::<()>();
    sleeper.mutator.send(move |_| {
        let claim = crate::cycle::token::HeldToken::take();
        took.send(()).expect("the case waits");
        let _ = wait_here.recv();
        drop(claim);
    });
    taken.recv().expect("the mutator took its claim");

    assert_eq!(standing.checkpoint(1), 0, "no grant to serve");
    assert!(
        standing.take_saw_work(),
        "the pass read the mutator collecting in line"
    );
    assert!(!standing.take_saw_work(), "and the reading is taken once");

    let_go.send(()).expect("the mutator waits");
    sleeper.end();
}

/// A record the round reads a backlog for twice — once carried out of a
/// checkpoint's batch, once by the walk's own — stands in the round's
/// backlog once: a birth asks for two mutators at the threshold, and one
/// counted twice is not two.
#[test]
fn a_record_stands_in_the_rounds_backlog_once() {
    let _g = test_guard();
    let mut backlogged = Backlogged::default();
    // Addresses rather than records: `Backlogged` compares its entries and
    // never reads through one. A load added to `push` or `drain_into` makes
    // this case undefined behaviour rather than a failure, so it gets a
    // record of its own that day.
    let first = 1 as *mut MutatorRecord;
    let second = 2 as *mut MutatorRecord;
    backlogged.push(first);
    backlogged.push(first);
    assert_eq!(backlogged.len(), 1);
    backlogged.push(second);
    assert_eq!(backlogged.len(), 2);

    let mut carried = Backlogged::default();
    carried.push(first);
    carried.push(2 as *mut MutatorRecord);
    carried.drain_into(&mut backlogged);
    assert_eq!(backlogged.len(), 2, "the drain adds no repeat");
    assert_eq!(carried.len(), 0, "and empties what it carried");
}

/// A record the handover moves is a record no collector's request stands
/// on. The carried backlog makes the two meet: a checkpoint's batch
/// remembers the record, the walk that reaches it afterwards leaves a
/// request standing on it, and both hold at the round's end. Renaming it
/// then would leave the elder's request on a record a sibling reclaims,
/// and the sibling's own list would splice a record out of the elder's
/// (`rfc/dev/design/trace-token-handshake.md`, "The two sides": a record is
/// renamed to another collector or freed only while unlinked).
#[test]
fn the_handover_leaves_a_record_a_request_stands_on() {
    let _g = test_guard();
    reset_lanes();
    let class = node_class("HandoverStandingNode");
    let standing_on = Sleeper::start(class);
    let free_of_requests = Sleeper::start(class);
    let mut standing = Standing::new(SLOT);

    // The elder's request stands on the first record and on no other.
    assert_eq!(
        serve_as_its_own_round(standing_on.record(), &mut standing),
        Served::Unanswered
    );
    assert!(unsafe { &*standing_on.record() }.is_standing());
    assert!(!unsafe { &*free_of_requests.record() }.is_standing());

    // Both stand in a round's backlog, each as the second of a pair, which
    // is what the handover moves.
    let mut with_a_request = Backlogged::default();
    with_a_request.push(free_of_requests.record());
    with_a_request.push(standing_on.record());
    hand_over_half(&with_a_request, SLOT);
    assert_eq!(
        unsafe { &*standing_on.record() }.collector(),
        ELDER,
        "a record a request stands on stays with the collector that made it"
    );

    let mut without_one = Backlogged::default();
    without_one.push(standing_on.record());
    without_one.push(free_of_requests.record());
    hand_over_half(&without_one, SLOT);
    assert_eq!(
        unsafe { &*free_of_requests.record() }.collector(),
        SLOT,
        "and a record free of requests is handed over as before"
    );

    unsafe { &*free_of_requests.record() }.name_to_collector(ELDER);
    drop(standing);
    standing_on.end();
    free_of_requests.end();
}

/// The slot a record of another collector's list is stamped with here.
const OTHER_SLOT: usize = 5;

/// A list's splice reads the record's two words and the frame's two ends,
/// so a record another collector holds would be spliced out of a list that
/// is not the splicer's — silently in a release build, and with the
/// symptoms of a cut list a long way from the cause. The debug build
/// refuses it at both ends: the splice and the push of a record that reads
/// linked already.
#[test]
#[cfg(debug_assertions)]
fn a_record_of_another_collectors_list_is_refused_at_both_ends() {
    let _g = test_guard();
    reset_lanes();
    let class = node_class("ForeignListNode");
    let sleeper = Sleeper::start(class);
    let record = sleeper.record();
    let mut standing = Standing::new(SLOT);

    link_for_test(record, OTHER_SLOT, true);
    let spliced = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        standing.forget(unsafe { &*record });
    }));
    assert!(
        spliced.is_err(),
        "a record of another list is not spliced out of this one"
    );

    let pushed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        standing.push(record);
    }));
    assert!(
        pushed.is_err(),
        "nor read as this list's own stale entry when it is pushed again"
    );

    // Nothing was written by either refusal, so the record is unlinked here
    // as its own collector would unlink it.
    link_for_test(record, OTHER_SLOT, false);
    assert!(!unsafe { &*record }.is_standing());
    drop(standing);
    sleeper.end();
}

/// What the pass read is the round's: a round whose walk never reaches the
/// mutator collecting in line still answers work, because the checkpoint
/// that read it hands the reading to the record the walk does visit. That
/// is what `note_idleness` reads, and a sibling whose mutators all collect
/// in line is not ended for it.
#[test]
fn a_round_reads_the_work_a_checkpoint_saw() {
    let _g = test_guard();
    reset_lanes();
    let _wait = HeldRequestWait::of(Duration::from_millis(2));
    let class = node_class("RoundReadsWorkNode");
    let collecting = Sleeper::start(class);
    let visited = Sleeper::start(class);
    let mut standing = Standing::new(SLOT);

    // The request the round's checkpoint will read back stands on the
    // mutator that then takes its own token.
    assert_eq!(
        serve_as_its_own_round(collecting.record(), &mut standing),
        Served::Unanswered
    );
    let (let_go, wait_here) = std::sync::mpsc::channel::<()>();
    let (took, taken) = std::sync::mpsc::channel::<()>();
    collecting.mutator.send(move |_| {
        let claim = crate::cycle::token::HeldToken::take();
        took.send(()).expect("the case waits");
        let _ = wait_here.recv();
        drop(claim);
    });
    taken.recv().expect("the mutator took its claim");

    // The walk reaches the other record alone, so the work the round
    // answers is the checkpoint's reading and not its own.
    unsafe { &*visited.record() }.name_to_collector(SLOT);
    testing::confine_rounds_to(visited.record());
    let outcome = round(SLOT, 1, &mut standing);
    assert!(
        outcome.saw_work,
        "the round carried the pass's reading of a mutator collecting in line"
    );

    testing::confine_rounds_to_records(&[]);
    unsafe { &*visited.record() }.name_to_collector(ELDER);
    let_go.send(()).expect("the mutator waits");
    drop(standing);
    collecting.end();
    visited.end();
}

/// One round can leave a record both in its backlog and linked in the
/// list: the first checkpoint serves the grant a sleeper's consent left
/// and carries the batch's backlog, and the walk that reaches the same
/// record afterwards — its verdicts disposed of meanwhile, its byte `FREE`
/// again — lands a fresh request on it and leaves it standing. That is the
/// state the handover meets, and it is reachable through `round` and not
/// only by hand.
#[test]
fn a_round_can_carry_a_backlog_for_a_record_it_then_leaves_linked() {
    let _g = test_guard();
    reset_lanes();
    let _wait = HeldRequestWait::of(Duration::from_millis(2));
    let class = node_class("CarriedAndLinkedNode");
    // Many small rings rather than one: a batch of one root frees its own
    // ring alone, so the disposition that follows leaves the rest of R
    // standing and the walk's request has something to serve.
    let sleeper = Sleeper::start_with_rings(class, 32, 2);
    let record = unsafe { &*sleeper.record() };
    // One root per batch, so the checkpoint's batch leaves the rest of the
    // ring behind it and its backlog reading is true.
    record.set_batch_size(1);
    let mut standing = Standing::new(SLOT);

    assert_eq!(
        serve_as_its_own_round(sleeper.record(), &mut standing),
        Served::Unanswered
    );
    sleeper.read_the_byte();

    // At the walk's reading of this record, the mutator disposes of the
    // batch the round's first checkpoint made, so the request that follows
    // lands on `FREE` and stands.
    let jobs = sleeper.mutator.jobs.clone();
    testing::at_the_next_reading(Box::new(move || {
        let (tell, told) = std::sync::mpsc::channel();
        jobs.send(Box::new(move |_: &mut crate::memory::arena::Arena| {
            unsafe { crate::gc::ll_gc_maybe_collect() };
            tell.send(()).expect("the case waits");
        }))
        .expect("the mutator thread runs");
        told.recv().expect("the disposition ran");
    }));

    record.name_to_collector(SLOT);
    testing::confine_rounds_to(sleeper.record());
    let outcome = round(SLOT, 1, &mut standing);

    assert_eq!(
        outcome.backlogged.len(),
        1,
        "the checkpoint's batch left the mutator at the threshold"
    );
    assert!(
        record.is_standing(),
        "and the walk left a fresh request standing on the same record"
    );

    // The handover of that round's backlog leaves it where it is.
    let mut backlogged = Backlogged::default();
    backlogged.push(crate::cycle::mutator_record::this_thread_record());
    for carried in outcome.backlogged.iter() {
        backlogged.push(*carried);
    }
    hand_over_half(&backlogged, OTHER_SLOT);
    assert_eq!(
        record.collector(),
        SLOT,
        "a record of the round's own backlog that a request stands on stays"
    );

    testing::confine_rounds_to_records(&[]);
    record.name_to_collector(ELDER);
    drop(standing);
    sleeper.poll();
    sleeper.end();
}
