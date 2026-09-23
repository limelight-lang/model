//! The take of a candidate ring that stands below the round's threshold:
//! the interval it must stand for, and whose figure the round counts it
//! against — the crate's, the embedder's through the ABI, or a case's
//! override, each outranking the one before it — and the round's three
//! branches over R, which the serves here make on the case's own thread: a
//! ring at the threshold served as it is today with no interval to count,
//! an empty ring an idle round, and a ring below the threshold stamped at
//! the visit that first reads it standing and taken as an ordinary batch an
//! interval later. What a batch leaves — the verdicts and whatever the
//! window registered — stands an interval of its own, counted from the
//! batch's end.

use super::*;
use crate::cycle::testing::long_ring;
use crate::cycle::worker::testing::HeldRequestWait;

/// Zero for the embedder's interval when the guard drops: a case that
/// failed between the setter and its own zero would otherwise leave every
/// later case's rounds taking at its figure.
struct EmbeddersInterval;

impl Drop for EmbeddersInterval {
    fn drop(&mut self) {
        set_standing_interval(Duration::ZERO);
    }
}

/// The embedder's interval replaces the crate's, and zero restores it.
#[test]
fn the_embedders_interval_replaces_the_crates_and_zero_restores_it() {
    let _g = test_guard();
    let _embedders = EmbeddersInterval;
    testing::take_standing_after(None);
    assert_eq!(standing_interval(), STANDING_INTERVAL);

    crate::gc::ll_gc_set_standing_interval(3);
    assert_eq!(standing_interval(), Duration::from_millis(3));

    crate::gc::ll_gc_set_standing_interval(0);
    assert_eq!(
        standing_interval(),
        STANDING_INTERVAL,
        "zero restores the crate's"
    );
}

/// A case's override outranks the embedder's figure, and the embedder's
/// stands again when the override goes: a case reaching the take by running
/// rounds against a millisecond leaves an embedded runtime's dial where it
/// was.
#[test]
fn a_cases_override_outranks_the_embedders_interval() {
    let _g = test_guard();
    let _embedders = EmbeddersInterval;
    crate::gc::ll_gc_set_standing_interval(3);

    let case = StandingInterval::of(Duration::from_millis(1));
    assert_eq!(standing_interval(), Duration::from_millis(1));

    drop(case);
    assert_eq!(
        standing_interval(),
        Duration::from_millis(3),
        "the embedder's figure stands again"
    );
}

/// A slot no collector thread is born into under the default cap and no
/// other case names.
const SLOT: usize = 7;

/// The round's threshold for the cases below: a ring of two stands under
/// it, and the take is the only thing that can reach such a ring.
const THRESHOLD: usize = 4;

/// Members of a ring that stands below the threshold.
const STANDING_RING: usize = 2;

/// One serve of `record` by the case's thread, inside the walk `standing`
/// stands for: the count of expired waits carries from serve to serve, which
/// is what the bound is read on. A serve that stands in for a round of its
/// own is `the_standing_list`'s `serve_as_its_own_round`.
fn serve_within_one_walk(record: *mut MutatorRecord, standing: &mut Standing) -> Served {
    unsafe { serve(record, SLOT, THRESHOLD, standing, serve_clock_now()) }
}

/// A ring below the threshold is not served at the visit that first reads
/// it, and is taken as an ordinary batch at the first visit an interval
/// later.
#[test]
fn a_ring_below_the_threshold_is_taken_an_interval_after_it_first_stood() {
    let _g = test_guard();
    reset_lanes();
    let _interval = StandingInterval::of(Duration::from_millis(1));
    let class = Sent(node_class("TakeAfterIntervalNode"));
    let mutator = Mutator::start();
    mutator.run(move |arena| {
        let _ = unsafe { long_ring(arena, class.into_inner(), STANDING_RING) };
    });
    let mut standing = Standing::new(SLOT);

    assert_eq!(
        serve_within_one_walk(mutator.record, &mut standing),
        Served::Idle,
        "the visit that first reads the ring takes nothing"
    );

    let record = unsafe { &*mutator.record };
    let stood_since = record.standing_since();
    assert_ne!(stood_since, 0, "and stamps the instant it read it at");

    std::thread::sleep(Duration::from_millis(3));
    let before_the_take = serve_clock_now();
    assert!(
        matches!(
            serve_within_one_walk(mutator.record, &mut standing),
            Served::Batch {
                roots: STANDING_RING,
                ..
            }
        ),
        "the visit an interval later takes the ring"
    );
    assert!(
        record.standing_since() > before_the_take,
        "the batch's end restarts the instant, so that the write-back and \
         whatever the window registered stand an interval of their own"
    );

    mutator.run(|_| unsafe {
        crate::gc::ll_gc_maybe_collect();
    });
}

/// A ring at the threshold is served as it is today, and the instant is
/// cleared: no interval is counted for a ring the threshold reaches, and a
/// sub-threshold instant left standing would have the round take it a
/// second time at its own cadence. The mutator here never reads its byte,
/// so the serve answers `Unanswered` and the word it left is the case's to
/// read.
#[test]
fn a_ring_at_the_threshold_leaves_no_instant_to_count() {
    let _g = test_guard();
    reset_lanes();
    let _interval = StandingInterval::of(Duration::from_millis(1));
    let _wait = HeldRequestWait::of(Duration::from_millis(2));
    let class = Sent(node_class("TakeAtTheThresholdNode"));
    let mutator = Mutator::start_idling_with(|_| {});
    mutator.run(move |arena| {
        let _ = unsafe { long_ring(arena, class.into_inner(), STANDING_RING) };
    });
    let mut standing = Standing::new(SLOT);
    let record = unsafe { &*mutator.record };

    assert_eq!(
        serve_within_one_walk(mutator.record, &mut standing),
        Served::Idle
    );
    assert_ne!(record.standing_since(), 0, "the sub-threshold ring stands");

    let class = Sent(node_class("TakeAtTheThresholdNode"));
    mutator.run(move |arena| {
        let _ = unsafe { long_ring(arena, class.into_inner(), THRESHOLD) };
    });
    assert_eq!(
        serve_within_one_walk(mutator.record, &mut standing),
        Served::Unanswered,
        "the ring reaches the threshold and the sleeper does not answer"
    );
    assert_eq!(
        record.standing_since(),
        0,
        "a ring at the threshold leaves no interval to count"
    );

    drop(standing);
    mutator.run(|_| unsafe {
        crate::gc::ll_gc_collect_cycles();
    });
}

/// A ring read empty clears the instant: a ring that stood and was taken
/// whole by its own mutator's collection counts its interval again from
/// the round that reads the next registration.
#[test]
fn a_ring_read_empty_clears_the_instant() {
    let _g = test_guard();
    reset_lanes();
    let _interval = StandingInterval::of(Duration::from_millis(1));
    let class = Sent(node_class("TakeOfAnEmptiedRingNode"));
    let mutator = Mutator::start();
    mutator.run(move |arena| {
        let _ = unsafe { long_ring(arena, class.into_inner(), STANDING_RING) };
    });
    let mut standing = Standing::new(SLOT);
    let record = unsafe { &*mutator.record };

    assert_eq!(
        serve_within_one_walk(mutator.record, &mut standing),
        Served::Idle
    );
    assert_ne!(record.standing_since(), 0, "the sub-threshold ring stands");

    // The mutator's own collection takes R whole, below any threshold.
    mutator.run(|_| unsafe {
        crate::gc::ll_gc_collect_cycles();
    });
    assert_eq!(
        serve_within_one_walk(mutator.record, &mut standing),
        Served::Idle,
        "an empty ring is an idle round"
    );
    assert_eq!(
        record.standing_since(),
        0,
        "and the instant it stood at is cleared"
    );
}

/// What a take leaves behind — the verdicts the mutator disposes of, and
/// whatever it registered after them — stands an interval of its own,
/// counted from the batch's end: the serve that follows the disposition
/// takes nothing, where a round at its own cadence would have taken the
/// ring again had the instant stayed where the take found it.
///
/// The mutator here reads its byte and arms, and the case fires the
/// collection by hand: a poll of its own between the batch and the last
/// reading could drain R, which is branch 2 and clears the instant the
/// case is reading.
#[test]
fn what_a_take_leaves_stands_its_own_interval_from_the_batch() {
    let _g = test_guard();
    reset_lanes();
    let _interval = StandingInterval::of(Duration::from_millis(1));
    let class = Sent(node_class("TakeWriteBackNode"));
    let mutator = Mutator::start_idling_with(|_| {
        crate::cycle::token::read_and_act_on_this_thread();
    });
    mutator.run(move |arena| {
        let _ = unsafe { long_ring(arena, class.into_inner(), STANDING_RING) };
    });
    let mut standing = Standing::new(SLOT);
    let record = unsafe { &*mutator.record };

    assert_eq!(
        serve_within_one_walk(mutator.record, &mut standing),
        Served::Idle
    );
    std::thread::sleep(Duration::from_millis(3));
    let before_the_take = serve_clock_now();
    assert!(matches!(
        serve_within_one_walk(mutator.record, &mut standing),
        Served::Batch { .. }
    ));
    let stamped_by_the_batch = record.standing_since();
    assert!(
        stamped_by_the_batch > before_the_take,
        "the instant the ring that follows stands against is the batch's end"
    );

    // The disposition of the batch, and what the mutator registers after
    // it stands against the instant the batch left, which no cadence of
    // the round's reaches.
    mutator.run(|_| unsafe {
        crate::gc::ll_gc_maybe_collect();
    });
    assert_eq!(
        record.token.read(),
        crate::cycle::token::FREE,
        "the mutator disposed of the batch"
    );
    testing::take_standing_after(Some(Duration::from_secs(60)));
    let class = Sent(node_class("TakeWriteBackNode"));
    mutator.run(move |arena| {
        let _ = unsafe { long_ring(arena, class.into_inner(), STANDING_RING) };
    });

    assert_eq!(
        serve_within_one_walk(mutator.record, &mut standing),
        Served::Idle,
        "the ring that stands after the take is inside its own interval"
    );
    assert_eq!(
        record.standing_since(),
        stamped_by_the_batch,
        "counted from the batch's end, not from the request's landing"
    );
}

/// A take whose request the sleeper never answered stands, and the round
/// that meets it again answers without a swap: the record is in the list
/// for the checkpoints, and a failed compare-and-swap on a sleeping
/// mutator's byte, round after round, is what the standing form exists to
/// spare.
#[test]
fn a_take_already_standing_is_answered_without_a_swap() {
    let _g = test_guard();
    reset_lanes();
    let _interval = StandingInterval::of(Duration::from_millis(1));
    let _wait = HeldRequestWait::of(Duration::from_millis(2));
    let class = Sent(node_class("TakeStandingNode"));
    let mutator = Mutator::start_idling_with(|_| {});
    mutator.run(move |arena| {
        let _ = unsafe { long_ring(arena, class.into_inner(), STANDING_RING) };
    });
    let mut standing = Standing::new(SLOT);
    let record = unsafe { &*mutator.record };

    assert_eq!(
        serve_within_one_walk(mutator.record, &mut standing),
        Served::Idle
    );
    std::thread::sleep(Duration::from_millis(3));
    assert_eq!(
        serve_within_one_walk(mutator.record, &mut standing),
        Served::Unanswered,
        "the take's request stands on a mutator that never reads its byte"
    );
    assert!(record.is_standing());
    let stood_since = record.standing_since();
    let _ = testing::take_refused_requests();

    assert_eq!(
        serve_within_one_walk(mutator.record, &mut standing),
        Served::Unanswered,
        "and the next round answers the standing request"
    );
    assert_eq!(
        testing::take_refused_requests(),
        0,
        "without a swap of its own"
    );
    assert_eq!(
        record.standing_since(),
        stood_since,
        "and leaves the instant as it is"
    );

    drop(standing);
    mutator.run(|_| unsafe {
        crate::gc::ll_gc_collect_cycles();
    });
}

/// A grant that makes no batch restarts the instant all the same: the
/// window the take costs the mutator was opened, and an instant left
/// standing across it would have the next round take again at its own
/// cadence. The grant here finds R drained: the mutator collects in line
/// between the serve's reading and its request.
#[test]
fn a_grant_that_makes_no_batch_restarts_the_instant() {
    let _g = test_guard();
    reset_lanes();
    let _interval = StandingInterval::of(Duration::from_millis(1));
    let class = Sent(node_class("TakeOfADrainedRingNode"));
    let mutator = Mutator::start();
    mutator.run(move |arena| {
        let _ = unsafe { long_ring(arena, class.into_inner(), STANDING_RING) };
    });
    let mut standing = Standing::new(SLOT);
    let record = unsafe { &*mutator.record };

    assert_eq!(
        serve_within_one_walk(mutator.record, &mut standing),
        Served::Idle
    );
    std::thread::sleep(Duration::from_millis(3));

    // The drain runs on the mutator's thread and returns its token to
    // `FREE`, so the request that follows the reading is granted over a
    // ring the peek finds empty.
    let jobs = mutator.jobs.clone();
    testing::before_the_next_request(Box::new(move || {
        let (tell, told) = std::sync::mpsc::channel();
        jobs.send(Box::new(move |_: &mut crate::memory::arena::Arena| {
            unsafe { crate::gc::ll_gc_collect_cycles() };
            tell.send(()).expect("the case waits");
        }))
        .expect("the mutator thread runs");
        told.recv().expect("the collection in line ran");
    }));
    let before_the_take = serve_clock_now();
    assert_eq!(
        serve_within_one_walk(mutator.record, &mut standing),
        Served::Idle,
        "the grant found R drained under it"
    );
    assert!(
        record.standing_since() > before_the_take,
        "and the grant's end restarted the instant"
    );
}

/// The round reads the serve clock once per record, and the take and the
/// epoch's advance share that reading: a record whose ring the round stamps
/// carries the same instant in both words.
#[test]
fn one_visits_clock_reading_is_shared_by_the_take_and_the_advance() {
    let _g = test_guard();
    reset_lanes();
    let _interval = StandingInterval::of(Duration::from_secs(60));
    let class = Sent(node_class("TakeSharedClockNode"));
    let mutator = Mutator::start();
    mutator.run(move |arena| {
        let _ = unsafe { long_ring(arena, class.into_inner(), STANDING_RING) };
    });
    let record = unsafe { &*mutator.record };
    let _ = record.take_new_life();
    record.note_advanced_at(0);
    record.name_to_collector(SLOT);
    testing::confine_rounds_to(mutator.record);
    let mut standing = Standing::new(SLOT);

    let _ = round(SLOT, THRESHOLD, &mut standing);

    let stood_since = record.standing_since();
    assert_ne!(stood_since, 0, "the round stamped the standing ring");
    assert_eq!(
        stood_since,
        record.advanced_at(),
        "and the epoch's first visit after the serve stamped the same instant"
    );

    testing::confine_rounds_to_records(&[]);
    record.name_to_collector(ELDER);
    mutator.run(|_| unsafe {
        crate::gc::ll_gc_collect_cycles();
    });
}

/// A take is over the ring whole and feeds K nothing: K is the collector's
/// estimate of what a producing mutator offers per batch, and four takes of
/// three roots each would double it toward the bound and hand the thread's
/// first real batch to the budget with every root unwalked.
#[test]
fn a_take_leaves_the_batch_size_where_it_found_it() {
    let _g = test_guard();
    reset_lanes();
    let _interval = StandingInterval::of(Duration::from_millis(1));
    let class = Sent(node_class("TakeUnderKNode"));
    let mutator = Mutator::start();
    mutator.run(move |arena| {
        let _ = unsafe { long_ring(arena, class.into_inner(), STANDING_RING) };
    });
    let mut standing = Standing::new(SLOT);
    let record = unsafe { &*mutator.record };
    assert_eq!(record.batch_size(), 0, "no batch has sized K yet");

    assert_eq!(
        serve_within_one_walk(mutator.record, &mut standing),
        Served::Idle
    );
    std::thread::sleep(Duration::from_millis(3));
    assert!(matches!(
        serve_within_one_walk(mutator.record, &mut standing),
        Served::Batch {
            roots: STANDING_RING,
            ..
        }
    ));
    assert_eq!(
        record.batch_size(),
        0,
        "the take took the ring whole and left K alone"
    );

    mutator.run(|_| unsafe {
        crate::gc::ll_gc_maybe_collect();
    });
}

/// The ring under the token decides the batch's form, not the request that
/// won it: a take whose request stood while its owner slept, and whose
/// ring crossed the threshold meanwhile, is served as the threshold batch
/// it now is — K clamped and sized — so that the checkpoint, which cannot
/// know which kind of request it serves, needs no kind of its own.
#[test]
fn a_ring_that_crossed_the_threshold_under_a_standing_take_is_a_threshold_batch() {
    let _g = test_guard();
    reset_lanes();
    let _interval = StandingInterval::of(Duration::from_millis(1));
    let _wait = HeldRequestWait::of(Duration::from_millis(2));
    let class = Sent(node_class("TakeCrossingNode"));
    let mutator = Mutator::start_idling_with(|_| {});
    mutator.run(move |arena| {
        let _ = unsafe { long_ring(arena, class.into_inner(), STANDING_RING) };
    });
    let mut standing = Standing::new(SLOT);
    let record = unsafe { &*mutator.record };

    assert_eq!(
        serve_within_one_walk(mutator.record, &mut standing),
        Served::Idle
    );
    std::thread::sleep(Duration::from_millis(3));
    assert_eq!(
        serve_within_one_walk(mutator.record, &mut standing),
        Served::Unanswered,
        "the take's request stands on the sleeping mutator"
    );
    assert!(record.is_standing());

    // The ring crosses the threshold while the request stands, and the
    // mutator then reads its byte and consents.
    let class = Sent(node_class("TakeCrossingNode"));
    mutator.run(move |arena| {
        let _ = unsafe { long_ring(arena, class.into_inner(), THRESHOLD) };
    });
    mutator.run(|_| {
        crate::cycle::token::read_and_act_on_this_thread();
    });

    // K below the ring's six, so that a threshold batch takes its whole
    // clamp and doubles K, where a take would clamp at three and leave it.
    record.set_batch_size(THRESHOLD);
    assert_eq!(
        standing.checkpoint(THRESHOLD),
        1,
        "the checkpoint served the grant the consent left"
    );
    assert_eq!(
        record.batch_size(),
        THRESHOLD * 2,
        "served as a threshold batch, which sizes K by what it completed"
    );

    mutator.run(|_| {
        crate::cycle::token::read_and_act_on_this_thread();
        unsafe { crate::gc::ll_gc_maybe_collect() };
    });
    mutator.run(|_| unsafe {
        crate::gc::ll_gc_collect_cycles();
    });
}

/// The take's clamp is the ring, not K: a mutator whose deep graph has
/// halved K to one has its standing ring taken whole all the same, where a
/// take under K would carry it off one root per interval.
#[test]
fn a_take_is_clamped_by_the_ring_and_not_by_k() {
    let _g = test_guard();
    reset_lanes();
    let _interval = StandingInterval::of(Duration::from_millis(1));
    let class = Sent(node_class("TakeUnderAHalvedKNode"));
    let mutator = Mutator::start();
    mutator.run(move |arena| {
        let _ = unsafe { long_ring(arena, class.into_inner(), THRESHOLD - 1) };
    });
    let mut standing = Standing::new(SLOT);
    let record = unsafe { &*mutator.record };
    record.set_batch_size(1);

    assert_eq!(
        serve_within_one_walk(mutator.record, &mut standing),
        Served::Idle
    );
    std::thread::sleep(Duration::from_millis(3));
    assert!(
        matches!(
            serve_within_one_walk(mutator.record, &mut standing),
            Served::Batch { roots: 3, .. }
        ),
        "the ring whole, three roots, against a K of one"
    );
    assert_eq!(record.batch_size(), 1, "and K where the take found it");

    mutator.run(|_| unsafe {
        crate::gc::ll_gc_maybe_collect();
    });
}

/// A round spends at most `EXPIRED_WAITS_PER_ROUND` consent waits on
/// mutators that do not answer: past the bound every request that lands is
/// left standing at once, to be served at a checkpoint when its mutator
/// wakes. Without the bound a pool of threads that sleep and wake in turn
/// costs the round one wait per thread per interval, which the standing
/// form does not bound by itself.
#[test]
fn a_round_spends_no_more_than_its_bound_of_expired_waits() {
    let _g = test_guard();
    reset_lanes();
    let _interval = StandingInterval::of(Duration::from_millis(1));
    let wait = Duration::from_millis(300);
    let _wait = HeldRequestWait::of(wait);
    let sleepers: Vec<Mutator> = (0..EXPIRED_WAITS_PER_ROUND + 1)
        .map(|index| {
            let class = Sent(node_class(&format!("ExpiredWaitNode{index}")));
            let mutator = Mutator::start_idling_with(|_| {});
            mutator.run(move |arena| {
                let _ = unsafe { long_ring(arena, class.into_inner(), STANDING_RING) };
            });
            mutator
        })
        .collect();
    let mut standing = Standing::new(SLOT);

    // The visit that reads each ring standing, and the interval after it.
    for sleeper in &sleepers {
        assert_eq!(
            serve_within_one_walk(sleeper.record, &mut standing),
            Served::Idle
        );
    }
    std::thread::sleep(Duration::from_millis(3));

    let walk = std::time::Instant::now();
    for sleeper in &sleepers {
        assert_eq!(
            serve_within_one_walk(sleeper.record, &mut standing),
            Served::Unanswered,
            "every request stands on a mutator that never reads its byte"
        );
    }
    let spent = walk.elapsed();
    assert!(
        sleepers
            .iter()
            .all(|sleeper| unsafe { &*sleeper.record }.is_standing()),
        "and every record is in the list for the checkpoints"
    );
    assert!(
        spent < wait.mul_f32(EXPIRED_WAITS_PER_ROUND as f32 + 0.5),
        "the walk spent {spent:?} on waits, past its bound of \
         {EXPIRED_WAITS_PER_ROUND}"
    );

    drop(standing);
    for sleeper in &sleepers {
        sleeper.run(|_| unsafe {
            crate::gc::ll_gc_collect_cycles();
        });
    }
}

/// The bound is one walk's: the next round pays it again, so a mutator
/// that slept through one round's requests is waited for at the next.
#[test]
fn the_bound_on_expired_waits_is_one_walks() {
    let _g = test_guard();
    reset_lanes();
    let _interval = StandingInterval::of(Duration::from_millis(1));
    let wait = Duration::from_millis(50);
    let _wait = HeldRequestWait::of(wait);
    let sleepers: Vec<Mutator> = (0..EXPIRED_WAITS_PER_ROUND + 2)
        .map(|index| {
            let class = Sent(node_class(&format!("OneWalksBoundNode{index}")));
            let mutator = Mutator::start_idling_with(|_| {});
            mutator.run(move |arena| {
                let _ = unsafe { long_ring(arena, class.into_inner(), STANDING_RING) };
            });
            mutator
        })
        .collect();
    let mut standing = Standing::new(SLOT);
    for sleeper in &sleepers {
        assert_eq!(
            serve_within_one_walk(sleeper.record, &mut standing),
            Served::Idle
        );
    }
    std::thread::sleep(Duration::from_millis(3));

    for sleeper in sleepers.iter().take(EXPIRED_WAITS_PER_ROUND) {
        assert_eq!(
            serve_within_one_walk(sleeper.record, &mut standing),
            Served::Unanswered
        );
    }

    let past_the_bound = std::time::Instant::now();
    assert_eq!(
        serve_within_one_walk(sleepers[EXPIRED_WAITS_PER_ROUND].record, &mut standing),
        Served::Unanswered
    );
    assert!(
        past_the_bound.elapsed() < wait / 2,
        "the request past the bound waits for nothing"
    );

    standing.start_a_round();
    let next_round = std::time::Instant::now();
    assert_eq!(
        serve_within_one_walk(sleepers[EXPIRED_WAITS_PER_ROUND + 1].record, &mut standing),
        Served::Unanswered
    );
    assert!(
        next_round.elapsed() >= wait.mul_f32(0.8),
        "and the next walk waits out its own bound again"
    );

    drop(standing);
    for sleeper in &sleepers {
        sleeper.run(|_| unsafe {
            crate::gc::ll_gc_collect_cycles();
        });
    }
}

/// The round's start is where the walk's count of expired waits is
/// cleared: a collector whose last round spent its bound waits again at
/// the next, however long it slept between them.
#[test]
fn a_round_starts_with_its_waits_unspent() {
    let _g = test_guard();
    let mut standing = Standing::new(SLOT);
    for _ in 0..EXPIRED_WAITS_PER_ROUND {
        standing.note_an_expired_wait();
    }
    assert!(standing.spent_its_waits());

    // No record is named to this slot, so the round reads none and its
    // start is all the case is after.
    let _ = round(SLOT, THRESHOLD, &mut standing);
    assert!(
        !standing.spent_its_waits(),
        "the round's start cleared the walk's count"
    );
}

/// The bound is over the walk and not over the take: a mutator read at the
/// threshold, which no interval gates, is left standing at once past the
/// bound as a take's mutator is.
#[test]
fn the_bound_covers_the_threshold_path_too() {
    let _g = test_guard();
    reset_lanes();
    let wait = Duration::from_millis(50);
    let _wait = HeldRequestWait::of(wait);
    let sleepers: Vec<Mutator> = (0..EXPIRED_WAITS_PER_ROUND + 1)
        .map(|index| {
            let class = Sent(node_class(&format!("ThresholdBoundNode{index}")));
            let mutator = Mutator::start_idling_with(|_| {});
            mutator.run(move |arena| {
                let _ = unsafe { long_ring(arena, class.into_inner(), THRESHOLD) };
            });
            mutator
        })
        .collect();
    let mut standing = Standing::new(SLOT);

    for sleeper in sleepers.iter().take(EXPIRED_WAITS_PER_ROUND) {
        assert_eq!(
            serve_within_one_walk(sleeper.record, &mut standing),
            Served::Unanswered,
            "a ring at the threshold is requested and waited for"
        );
    }

    let past_the_bound = std::time::Instant::now();
    assert_eq!(
        serve_within_one_walk(sleepers[EXPIRED_WAITS_PER_ROUND].record, &mut standing),
        Served::Unanswered
    );
    assert!(
        past_the_bound.elapsed() < wait / 2,
        "and the one past the bound stands without a wait of its own"
    );
    assert!(unsafe { &*sleepers[EXPIRED_WAITS_PER_ROUND].record }.is_standing());

    drop(standing);
    for sleeper in &sleepers {
        sleeper.run(|_| unsafe {
            crate::gc::ll_gc_collect_cycles();
        });
    }
}
