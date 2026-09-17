//! The token handshake under real threads: the readings the design names
//! for its stress test (`rfc/dev/design/trace-token-handshake.md`, "(d) The
//! instrument story", and the probes the third and fourth rounds add). A
//! mutator blocked on a pipe read gets no batch while blocked and one
//! within a round of its first poll; sleepers beside an active mutator
//! leave its batch interval alone; a mutator freeing at full rate under
//! continuous requests balances its consents against the grants served,
//! makes every withheld return at its next safepoint and leaves the pool's
//! count of blocks out where it found it, with `POSTED` skips at least the
//! batches minus the collections; a wake landing inside a round starts the
//! next round without the timer; and a sleeper that wakes while the active
//! mutator is being served is released within one batch of its consent,
//! as is the pressure collection it fires.
//!
//! Every case is a measurement probe, run one at a time in a release
//! build — `cargo test --release --lib -- --ignored under_stress --test-threads=1`
//! — under the crate's own request wait rather than the harness's, and its
//! figures are recorded in `dev/BENCHMARKS.md`. The collector's outcomes
//! are counted by `testing::take_outcomes`, the mutator's consents and
//! refusals by its token, and the rounds by their timestamps.

use super::*;
use crate::class::Class;
use crate::cycle::queue::verdicts::verdict_count;
use crate::cycle::queue::{candidate_count, deferred_count};
use crate::cycle::testing::long_ring;
use crate::cycle::token::{COLLECTOR, FREE, POSTED, REQUESTED, state, word};
use crate::memory::arena::Arena;
use crate::memory::block_pool::BlockPool;
use std::io::{Read, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// Members of a mutator's garbage ring: the threshold, so that one batch at
/// the initial K takes the ring whole and the collection over its verdicts
/// frees the known count.
const RING: usize = SOFT_THRESHOLD;

const _: () = assert!(RING <= INITIAL_BATCH);

/// How long a sleeper stays blocked: twice the timer's maximum, so that the
/// timer reaches its maximum and rounds at it while the sleeper is silent.
const BLOCKED_FOR: Duration = Duration::from_secs(2 * FALLBACK_INTERVAL_MAX.as_secs());

/// The bound a wake's round, a consent's batch and a released sleeper are
/// held to: far under the timer's minimum times a round's few visits, and
/// far over a scheduling delay on a loaded box.
const A_WAKES_DELAY: Duration = Duration::from_millis(50);

/// Rings the active mutator registers per interval sample, and the leading
/// samples dropped as warm-up: the first rounds pay each sleeper's one
/// waited request before the sleeper is marked silent.
const SAMPLES: usize = 24;
const WARM_UP: usize = 4;

/// How long the full-rate mutator churns.
const CHURN: Duration = Duration::from_secs(2);

/// Dead slots the full-rate mutator frees per iteration.
const SLOTS: usize = 2_000;
const SLOT_BYTES: usize = 64;

fn byte_of(record: *mut MutatorRecord) -> u8 {
    unsafe { &*record }.token.read()
}

fn token_of(record: *mut MutatorRecord) -> &'static crate::cycle::token::TraceToken {
    &unsafe { &*record }.token
}

/// A mutator with a ring in its R, blocked on a pipe read until the case
/// writes the pipe; from then on it polls between jobs as a running mutator
/// does, and `freed` sums what its polls freed.
struct Sleeper {
    mutator: Mutator,
    pipe: std::io::PipeWriter,
    freed: Arc<AtomicUsize>,
}

impl Sleeper {
    fn start(class: *const Class) -> Self {
        let (reader, pipe) = std::io::pipe().expect("a pipe");
        let freed = Arc::new(AtomicUsize::new(0));
        let mutator = Mutator::start_polling(freed.clone());
        let class = Sent(class);
        mutator.run(move |arena| {
            let _ = unsafe { long_ring(arena, class.into_inner(), RING) };
        });
        mutator.send(move |_| {
            let mut reader = reader;
            let mut byte = [0u8];
            reader
                .read_exact(&mut byte)
                .expect("the case writes the pipe");
        });
        Self {
            mutator,
            pipe,
            freed,
        }
    }

    fn record(&self) -> *mut MutatorRecord {
        self.mutator.record
    }

    fn byte(&self) -> u8 {
        byte_of(self.record())
    }

    /// Write the pipe: the read returns, and the mutator's polls follow.
    fn wake(&mut self) {
        self.pipe.write_all(&[1]).expect("the reader waits");
    }

    fn freed(&self) -> usize {
        self.freed.load(Ordering::Relaxed)
    }

    /// Wake the sleeper and wait until its polls freed its ring.
    fn release_and_collect(&mut self) {
        self.wake();
        assert!(
            wait_until(|| self.freed() >= RING, A_BIRTH),
            "the woken sleeper was served and freed its ring"
        );
    }
}

/// Birth the elder over `records`, its wait between rounds the timer's own,
/// and wait for its first round.
fn born_over(records: &[*mut MutatorRecord]) {
    testing::confine_rounds_to_records(records);
    testing::wait_between_rounds_for(None);
    testing::permit_births(true);
    let _ = testing::take_rounds();
    let _ = testing::take_round_times();
    let _ = testing::take_outcomes();
    ensure_thread();
    assert!(
        wait_until(|| testing::take_rounds() >= 1, A_BIRTH),
        "the elder was born and made its first round"
    );
}

/// Wait until the elder made `rounds` more rounds, counting across the
/// calls that zero the count.
fn wait_for_rounds(rounds: usize) {
    let mut seen = 0;
    assert!(
        wait_until(
            || {
                seen += testing::take_rounds();
                seen >= rounds
            },
            A_BIRTH
        ),
        "the elder made {rounds} rounds"
    );
}

fn median(samples: &mut [Duration]) -> Duration {
    samples.sort();
    samples[samples.len() / 2]
}

#[test]
#[ignore = "measurement probe; run explicitly with --ignored (release mode)"]
fn a_sleeper_on_a_pipe_gets_no_batch_while_blocked_and_one_within_a_round_of_its_first_poll() {
    let _g = test_guard();
    let _record = record();
    let _end = RetireOnDrop;
    reset_lanes();
    let _wait = testing::HeldRequestWait::crate_own();
    let class = node_class("StressSleeperNode");
    let mut sleeper = Sleeper::start(class);
    born_over(&[sleeper.record()]);

    // Blocked: the request is made once with the wait, then stands.
    std::thread::sleep(BLOCKED_FOR);
    let while_blocked = testing::take_outcomes();
    let rounds_while_blocked = testing::take_round_times().len();
    assert_eq!(while_blocked.batches, 0, "{while_blocked:?}");
    assert_eq!(while_blocked.grants, 0, "{while_blocked:?}");
    assert!(while_blocked.unanswered >= 1, "{while_blocked:?}");
    assert_eq!(
        sleeper.byte(),
        word(REQUESTED, ELDER),
        "the request stands before the pipe is written"
    );
    assert_eq!(sleeper.freed(), 0);

    // Written: the first poll consents and wakes the collector, whose
    // checkpoint serves the batch; the poll after reads `POSTED` and
    // collects the ring out of P.
    let written_at = Instant::now();
    sleeper.wake();
    assert!(
        wait_until(|| sleeper.freed() >= RING, A_BIRTH),
        "the sleeper's polls collected its ring"
    );
    let served_within = written_at.elapsed();
    let after = testing::take_outcomes();
    let rounds = testing::take_round_times();
    assert_eq!(after.batches, 1, "{after:?}");
    assert_eq!(after.grants, 1, "{after:?}");
    assert_eq!(sleeper.freed(), RING, "the disposition freed the ring");
    let round_after_the_poll = rounds
        .iter()
        .filter(|(start, _)| *start >= written_at)
        .count();
    assert!(
        served_within < FALLBACK_INTERVAL_MAX,
        "served in {served_within:?}: by the consent's wake and not by the timer at its maximum"
    );
    println!(
        "stress sleeper: blocked {BLOCKED_FOR:?}, {rounds_while_blocked} rounds while blocked, \
         {} unanswered, batch and disposition {served_within:?} after the pipe was written, \
         {round_after_the_poll} rounds from the write to the disposition",
        while_blocked.unanswered
    );

    testing::retire();
    drop(sleeper);
}

/// Intervals between the collections over P of the active mutator — this
/// thread — registering a ring at a time beside `sleepers` sleepers, the
/// warm-up dropped.
fn batch_intervals_beside(sleepers: usize, class: *const Class) -> Vec<Duration> {
    let mut asleep: Vec<Sleeper> = (0..sleepers).map(|_| Sleeper::start(class)).collect();
    let mut records = vec![record()];
    records.extend(asleep.iter().map(Sleeper::record));
    born_over(&records);

    let mut arena = Arena::new();
    let mut collections = crate::gc::verdict_collections_on_this_thread();
    let mut last = Instant::now();
    let mut intervals = Vec::with_capacity(SAMPLES);
    for sample in 0..SAMPLES {
        let _ = unsafe { long_ring(&mut arena, class, RING) };
        let freed = loop {
            let freed = unsafe { crate::gc::ll_gc_maybe_collect() };
            if crate::gc::verdict_collections_on_this_thread() > collections {
                collections += 1;
                break freed;
            }

            std::thread::sleep(Duration::from_millis(1));
        };
        assert_eq!(freed, RING, "the collection over P freed the ring");
        let now = Instant::now();
        if sample >= WARM_UP {
            intervals.push(now - last);
        }

        last = now;
    }

    for sleeper in &mut asleep {
        sleeper.release_and_collect();
    }

    testing::retire();
    drop(asleep);
    intervals
}

#[test]
#[ignore = "measurement probe; run explicitly with --ignored (release mode)"]
fn sleepers_beside_an_active_mutator_leave_its_batch_interval_alone() {
    let _g = test_guard();
    let _record = record();
    let _end = RetireOnDrop;
    reset_lanes();
    let _wait = testing::HeldRequestWait::crate_own();
    let class = node_class("StressBesideSleepersNode");

    // Three runs alone for the run-to-run spread, then one beside three
    // sleepers, the most the confinement holds beside this thread.
    let alone: Vec<Duration> = (0..3)
        .map(|_| median(&mut batch_intervals_beside(0, class)))
        .collect();
    let beside = median(&mut batch_intervals_beside(3, class));
    let low = *alone.iter().min().expect("three runs");
    let high = *alone.iter().max().expect("three runs");
    let spread = (high - low).max(Duration::from_millis(1));
    println!(
        "stress beside sleepers: batch interval alone {alone:?} (median of {} per run), \
         beside 3 sleepers {beside:?}, spread {spread:?}",
        SAMPLES - WARM_UP
    );
    assert!(
        beside + spread >= low && beside <= high + spread,
        "beside sleepers {beside:?} against alone {low:?}..{high:?} within {spread:?}"
    );
}

/// A class of no properties for the dead slots: the heap census reads a
/// live slot's class word, and a slot with none there is a slot it
/// dereferences.
fn dead_class() -> *const Class {
    static CLASS: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *CLASS.get_or_init(|| crate::class::ClassBuilder::new("StressDeadFake").build() as usize)
        as *const Class
}

/// `SLOTS` dead entities, allocated and stamped, in allocation order.
unsafe fn dead_slots() -> Vec<*mut u8> {
    (0..SLOTS)
        .map(|_| {
            let slot = unsafe { crate::memory::heap::entity_alloc(SLOT_BYTES) };
            assert!(!slot.is_null(), "the heap served");
            let header = slot as *mut crate::refcount::RcHeader;
            unsafe {
                header.write(crate::refcount::RcHeader::new(
                    crate::refcount::MemoryCategory::GcHeap,
                    crate::refcount::EntityKind::Object.to_flags(),
                ));
                crate::refcount::set_header_refcount(header, 0);
                (&raw mut (*(slot as *mut crate::object::Object)).class).write(dead_class());
            }
            slot
        })
        .collect()
}

/// The pool's count of blocks out with this thread's queue at its base
/// block and both spares: what the queue grew by is given back first, so
/// that the count reads the heap's and the collector's blocks alone.
fn blocks_out_with_the_queue_released() -> usize {
    crate::cycle::queue::release_queue_segments();
    assert!(crate::cycle::queue::refill_spares());
    BlockPool::global().blocks_out()
}

/// Free `slots` through the entry that reads the byte.
unsafe fn free_all(slots: &[*mut u8]) {
    for &slot in slots {
        unsafe { crate::memory::stdapi::ll_free(slot) };
    }
}

#[test]
#[ignore = "measurement probe; run explicitly with --ignored (release mode)"]
fn a_mutator_freeing_at_full_rate_under_continuous_requests_balances_its_ledger() {
    let _g = test_guard();
    let record = record();
    let _end = RetireOnDrop;
    reset_lanes();
    let _wait = testing::HeldRequestWait::crate_own();
    let class = node_class("StressChurnNode");
    let token = token_of(record);
    let consents_before = token.consents();
    let refusals_before = token.refusals();
    born_over(&[record]);

    // One iteration: a ring registered at the threshold, every slot freed
    // through the reading, one poll. Ahead of the count, a warm-up
    // iteration and the poll that returns what it withheld, so that the
    // pool's count of blocks out is read with this thread's heap blocks
    // and the collector's workspace already drawn.
    let mut arena = Arena::new();
    let mut freed = 0;
    let mut rings = 0;
    let iterate = |arena: &mut Arena| -> usize {
        let _ = unsafe { long_ring(arena, class, RING) };
        let slots = unsafe { dead_slots() };
        unsafe { free_all(&slots) };
        unsafe { crate::gc::ll_gc_maybe_collect() }
    };
    for _ in 0..4 {
        freed += iterate(&mut arena);
        rings += 1;
    }
    assert!(
        wait_until(
            || unsafe { crate::gc::ll_gc_maybe_collect() } == 0 && candidate_count() == 0,
            A_BIRTH
        ),
        "the warm-up rings were collected"
    );
    let _ = testing::take_outcomes();
    let collections_before = crate::gc::verdict_collections_on_this_thread();
    let consents_at_start = token.consents();
    let blocks_out_before = blocks_out_with_the_queue_released();
    let rings_before = rings;
    let freed_before = freed;

    let started = Instant::now();
    let mut iterations = 0;
    while started.elapsed() < CHURN {
        freed += iterate(&mut arena);
        rings += 1;
        iterations += 1;
    }
    let churned = started.elapsed();

    // The drain: every ring collected and R empty, so that no request is
    // in flight when the collector retires — a request its standing
    // array's drop withdrew would be a consent with no grant served.
    assert!(
        wait_until(
            || {
                freed += unsafe { crate::gc::ll_gc_maybe_collect() };
                candidate_count() == 0 && verdict_count() == 0 && state(byte_of(record)) == FREE
            },
            A_BIRTH
        ),
        "R and P drained"
    );
    std::thread::sleep(2 * FALLBACK_INTERVAL_MIN);
    assert_eq!(state(byte_of(record)), FREE);

    // The safepoint after the last holder: nothing withheld, the deferred
    // lane empty of this run's rings, every ring freed.
    freed += unsafe { crate::gc::ll_gc_maybe_collect() };
    assert_eq!(
        crate::cycle::deferred_slot_reuse::foreign_withheld_count(),
        0,
        "every withheld return was made"
    );
    assert_eq!(crate::cycle::deferred_slot_reuse::deferred_slot_count(), 0);
    assert_eq!(deferred_count(), 0, "garbage is never read live");
    assert_eq!(
        freed - freed_before,
        (rings - rings_before) * RING,
        "every ring registered during the churn was freed"
    );

    // The pool: the slots freed again after the drain, and the count of
    // blocks out where the warm-up left it, read while the collector still
    // holds its own base block.
    let slots = unsafe { dead_slots() };
    unsafe { free_all(&slots) };
    let _ = unsafe { crate::gc::ll_gc_maybe_collect() };
    let blocks_out_after = blocks_out_with_the_queue_released();
    let outcomes = testing::take_outcomes();
    testing::retire();
    let collections = crate::gc::verdict_collections_on_this_thread() - collections_before;
    let consents = token.consents() - consents_at_start;
    let refusals = token.refusals() - refusals_before;

    println!(
        "stress churn: {iterations} iterations of {RING} registered and {SLOTS} freed in {churned:?} \
         ({:.1} µs per iteration); collector {outcomes:?}; mutator consents {consents} \
         (of {} since the record's birth), refusals {refusals}, collections over P {collections}; \
         blocks out {blocks_out_before} before, {blocks_out_after} after",
        churned.as_secs_f64() * 1e6 / iterations as f64,
        token.consents() - consents_before,
    );
    assert_eq!(
        consents, outcomes.grants,
        "every consent is a grant the collector served"
    );
    // A refusal whose collection closed before the withdrawal read the
    // byte back is `FREE` to the collector, a record moved on: the
    // mutator's count bounds the collector's from above.
    assert!(
        refusals >= outcomes.refusals,
        "refusals: the mutator made {refusals}, the collector read {}",
        outcomes.refusals
    );
    assert!(
        outcomes.posted + collections >= outcomes.batches,
        "POSTED skips {} + collections {collections} >= batches {}",
        outcomes.posted,
        outcomes.batches
    );
    assert_eq!(
        blocks_out_after, blocks_out_before,
        "the pool's count is balanced"
    );
}

/// The instant `at` stands inside round `(start, end)`.
fn inside(round: &(Instant, Option<Instant>), at: Instant) -> bool {
    round.0 <= at && round.1.is_some_and(|end| at <= end)
}

#[test]
#[ignore = "measurement probe; run explicitly with --ignored (release mode)"]
fn a_wake_landing_inside_a_round_starts_the_next_round_without_the_timer() {
    let _g = test_guard();
    let record = record();
    let _end = RetireOnDrop;
    reset_lanes();
    // The stranger the collector waits on: a ring at the threshold, and
    // nothing done at its byte until the case says.
    let stranger = Mutator::start_idling_with(|_| {});
    let class = Sent(node_class("StressWakeNode"));
    stranger.run(move |arena| {
        let _ = unsafe { long_ring(arena, class.into_inner(), RING) };
    });
    // A wait long enough to land a wake inside it, and a sleep between
    // rounds the wake's skip is measured against.
    let wait = Duration::from_millis(300);
    let sleep = Duration::from_secs(2);
    let _wait = testing::HeldRequestWait::of(wait);
    testing::confine_rounds_to_records(&[record, stranger.record]);
    testing::wait_between_rounds_for(Some(sleep));
    testing::permit_births(true);
    let _ = testing::take_rounds();
    let _ = testing::take_round_times();
    let _ = testing::take_outcomes();
    ensure_thread();

    // The first round waits on the stranger; a poll's wake from this
    // thread lands inside the wait, the checkpoint after it serves nothing,
    // and the sleep after the round is skipped: the second round starts at
    // once.
    assert!(
        wait_until(|| state(byte_of(stranger.record)) == REQUESTED, A_BIRTH),
        "the first round's request landed"
    );
    std::thread::sleep(wait / 3);
    let woke_at = Instant::now();
    crate::cycle::queue::make_a_signal_due();
    assert_eq!(unsafe { crate::gc::ll_gc_maybe_collect() }, 0);
    wait_for_rounds(2);
    let rounds = testing::take_round_times();
    assert!(rounds.len() >= 2, "{rounds:?}");
    assert!(
        inside(&rounds[0], woke_at),
        "the wake landed inside the first round"
    );
    let first_end = rounds[0].1.expect("the first round ended");
    let gap_after_the_wake = rounds[1].0 - first_end;
    assert!(
        gap_after_the_wake < A_WAKES_DELAY,
        "the second round started {gap_after_the_wake:?} after the first, against a sleep of {sleep:?}"
    );
    let outcomes = testing::take_outcomes();
    assert!(outcomes.unanswered >= 2, "{outcomes:?}");
    assert_eq!(outcomes.batches, 0, "{outcomes:?}");

    // The second round left the request standing on the now silent
    // stranger and the thread sleeps; the stranger's consent lands between
    // rounds, and the third round starts at once and serves it.
    assert_eq!(state(byte_of(stranger.record)), REQUESTED, "standing");
    let consented_at = Instant::now();
    assert_eq!(
        stranger.run(|_| crate::cycle::token::read_and_act_on_this_thread()),
        crate::cycle::token::Reading::Collector
    );
    wait_for_rounds(1);
    let rounds = testing::take_round_times();
    let third_start = rounds.last().expect("a third round").0;
    let gap_after_the_consent = third_start - consented_at;
    assert!(
        gap_after_the_consent < A_WAKES_DELAY,
        "the third round started {gap_after_the_consent:?} after the consent, against a sleep of {sleep:?}"
    );
    assert!(
        testing::take_outcomes().batches >= 1,
        "the round served the standing grant"
    );
    println!(
        "stress wake: the round after a wake inside a {wait:?} wait started {gap_after_the_wake:?} \
         after it, the round after a consent between rounds {gap_after_the_consent:?} after the \
         consent, the sleep between rounds pinned at {sleep:?}"
    );

    testing::retire();
    stranger.run(|_| {
        crate::cycle::token::read_and_act_on_this_thread();
        assert_eq!(unsafe { crate::gc::ll_gc_maybe_collect() }, RING);
    });
    drop(stranger);
}

/// A sleeper standing silent with its ring at the threshold beside this
/// thread, the collector born over both, and a batch of this thread's held
/// open between its post and its advance until `let_go` is sent.
struct HeldBatch {
    sleeper: Mutator,
    let_go: std::sync::mpsc::Sender<()>,
    _arena: Arena,
}

fn a_batch_held_open_beside_a_silent_sleeper(class: *const Class) -> HeldBatch {
    let sleeper = Mutator::start_idling_with(|_| {});
    let sent = Sent(class);
    sleeper.run(move |arena| {
        let _ = unsafe { long_ring(arena, sent.into_inner(), RING) };
    });
    born_over(&[record(), sleeper.record]);
    let mut unanswered = 0;
    assert!(
        wait_until(
            || {
                unanswered += testing::take_outcomes().unanswered;
                unanswered >= 1 && state(byte_of(sleeper.record)) == REQUESTED
            },
            A_BIRTH
        ),
        "the sleeper's request stands"
    );

    let (let_go, held) = std::sync::mpsc::channel();
    testing::make_the_next_batch_wait_before_its_advance(held);
    let mut arena = Arena::new();
    let _ = unsafe { long_ring(&mut arena, class, RING) };
    assert!(
        wait_until(|| byte_of(record()) == word(COLLECTOR, ELDER), A_BIRTH),
        "this thread consented and the batch holds its token"
    );
    HeldBatch {
        sleeper,
        let_go,
        _arena: arena,
    }
}

#[test]
#[ignore = "measurement probe; run explicitly with --ignored (release mode)"]
fn a_sleeper_waking_during_the_active_mutators_batch_is_released_within_one_batch() {
    let _g = test_guard();
    let _record = record();
    let _end = RetireOnDrop;
    reset_lanes();
    let _wait = testing::HeldRequestWait::crate_own();
    let class = node_class("StressWakingSleeperNode");

    // The sleeper consents while this thread's batch is held open; its
    // release follows the active batch's end by the checkpoint before the
    // collector's next request.
    let held = a_batch_held_open_beside_a_silent_sleeper(class);
    let consented_at = Instant::now();
    assert_eq!(
        held.sleeper
            .run(|_| crate::cycle::token::read_and_act_on_this_thread()),
        crate::cycle::token::Reading::Collector
    );
    std::thread::sleep(A_WAKES_DELAY);
    assert_eq!(
        state(byte_of(held.sleeper.record)),
        COLLECTOR,
        "not served while the active batch holds the collector"
    );
    let let_go_at = Instant::now();
    held.let_go.send(()).expect("the batch waits");
    assert!(
        wait_until(|| state(byte_of(held.sleeper.record)) == POSTED, A_BIRTH),
        "the sleeper was released"
    );
    let released_after_the_batch = let_go_at.elapsed();
    let consent_to_release = consented_at.elapsed();
    assert!(
        released_after_the_batch < A_WAKES_DELAY,
        "released {released_after_the_batch:?} after the active batch ended"
    );
    println!(
        "stress waking sleeper: released {released_after_the_batch:?} after the active batch \
         ended, {consent_to_release:?} after its consent, the active batch held open for \
         {:?}",
        let_go_at - consented_at
    );

    // Both dispositions, then the end.
    assert_eq!(unsafe { crate::gc::ll_gc_maybe_collect() }, RING);
    held.sleeper.run(|_| {
        assert_eq!(unsafe { crate::gc::ll_gc_maybe_collect() }, RING);
    });
    testing::retire();
    drop(held);
}

#[test]
#[ignore = "measurement probe; run explicitly with --ignored (release mode)"]
fn a_pressure_collection_fired_by_a_waking_sleeper_ends_within_one_batch() {
    let _g = test_guard();
    let _record = record();
    let _end = RetireOnDrop;
    reset_lanes();
    let _wait = testing::HeldRequestWait::crate_own();
    let class = node_class("StressPressureSleeperNode");

    // The sleeper consents and fires a pressure collection at once: its
    // take waits under the grant, is served at the checkpoint after the
    // active batch, and the collection runs over what the batch left.
    let held = a_batch_held_open_beside_a_silent_sleeper(class);
    let (tell, ended) = std::sync::mpsc::channel();
    held.sleeper.send(move |_| {
        assert_eq!(
            crate::cycle::token::read_and_act_on_this_thread(),
            crate::cycle::token::Reading::Collector
        );
        let started = Instant::now();
        let freed = unsafe { crate::cycle::collect::collect_under_pressure() };
        tell.send(Sent((started, started.elapsed(), freed)))
            .expect("the case waits");
    });
    std::thread::sleep(A_WAKES_DELAY);
    assert!(
        ended.try_recv().is_err(),
        "the pressure collection waits while the active batch holds the collector"
    );
    let let_go_at = Instant::now();
    held.let_go.send(()).expect("the batch waits");
    let (started, took, freed) = ended
        .recv_timeout(A_BIRTH)
        .expect("the pressure collection ended")
        .into_inner();
    let ended_after_the_batch = let_go_at.elapsed();
    assert!(
        ended_after_the_batch < A_WAKES_DELAY,
        "ended {ended_after_the_batch:?} after the active batch ended"
    );
    // The pressure collection is the sleeper's disposition: the ring the
    // batch proposed, or the ring whole if the take won before the batch.
    assert_eq!(
        freed, RING,
        "the sleeper's ring was freed by its own collection"
    );
    println!(
        "stress pressure sleeper: the collection fired {:?} before the active batch let go \
         ended {ended_after_the_batch:?} after it, taking {took:?} in all",
        let_go_at - started
    );

    assert_eq!(unsafe { crate::gc::ll_gc_maybe_collect() }, RING);
    testing::retire();
    drop(held);
}
