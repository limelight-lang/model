//! The collector's epoch clock: the first visit of a life stamps the instant
//! and advances nothing, X of the collector's clock or sixty-four of its
//! batches advance the cell and the byte the poll reads, a new life advances
//! at the first visit, and what the mutator does between visits moves
//! nothing — a thread whose live roots the collector takes oftener than X has
//! its deferred lane re-offered after X.

use super::*;

/// The visit as `read_one_record` makes it, against the clock now.
fn visit(record: &MutatorRecord) {
    advance_the_epoch_if_due(record, serve_clock_now());
}

/// Advance epochs after `interval` for the case, and after the module's own
/// again when the guard drops.
struct EpochInterval;

impl EpochInterval {
    fn of(interval: Duration) -> Self {
        testing::advance_epochs_after(Some(interval));
        Self
    }
}

impl Drop for EpochInterval {
    fn drop(&mut self) {
        testing::advance_epochs_after(None);
    }
}

/// A visit reads the record's instant against X: nothing but the stamp at the
/// first visit of a life, nothing inside X, the advance once X has passed
/// between two visits.
#[test]
fn the_first_visit_stamps_and_a_visit_after_x_advances() {
    let _g = test_guard();
    // A minute for the two visits that must fall inside X — a preemption
    // between them cannot reach it — and a millisecond for the sleep.
    let _x = EpochInterval::of(Duration::from_secs(60));
    let record = unsafe { &*record() };
    let _ = record.take_new_life();
    record.note_advanced_at(0);
    let turnovers = record.turnovers();

    visit(record);
    assert_eq!(
        record.turnovers(),
        turnovers,
        "the first visit of a life advances nothing"
    );
    let stamped = record.advanced_at();
    assert_ne!(stamped, 0, "and it stamped the instant");

    visit(record);
    assert_eq!(
        record.turnovers(),
        turnovers,
        "a second visit inside X advances nothing"
    );
    assert_eq!(
        record.advanced_at(),
        stamped,
        "and leaves the instant, so X counts from the first"
    );

    testing::advance_epochs_after(Some(Duration::from_millis(1)));
    std::thread::sleep(Duration::from_millis(5));
    visit(record);
    assert_eq!(
        record.turnovers(),
        turnovers + 1,
        "X passed between two visits"
    );
    assert_eq!(
        record.turnover_byte(),
        (turnovers + 1) as u8,
        "and the byte the poll reads follows the cell"
    );
    assert!(record.advanced_at() > stamped, "and the advance restamped");
}

/// Sixty-four batches advance the epoch inside X, and the count starts again
/// from the advance: a thread the collector serves at a high rate turns over
/// at its batches' rate rather than waiting out X.
#[test]
fn sixty_four_batches_advance_the_epoch_before_x() {
    let _g = test_guard();
    let _x = EpochInterval::of(Duration::from_secs(60));
    let record = unsafe { &*record() };
    let _ = record.take_new_life();
    // An advance by hand, for a count of zero and an instant of now.
    record.advance_the_epoch(serve_clock_now());
    let turnovers = record.turnovers();

    for _ in 1..crate::cycle::epoch::BATCHES_PER_EPOCH {
        record.note_batch();
    }
    visit(record);
    assert_eq!(
        record.turnovers(),
        turnovers,
        "sixty-three batches inside X advance nothing"
    );

    record.note_batch();
    visit(record);
    assert_eq!(
        record.turnovers(),
        turnovers + 1,
        "the sixty-fourth advanced the epoch inside X"
    );
    assert_eq!(
        record.batches_since_the_advance(),
        0,
        "and the count starts again"
    );
}

/// The registry's note of a new life is advanced past at the collector's next
/// visit, with no X to wait for and no instant stamped before it, and the
/// note is taken down: a stamp the last life wrote reads stale against the
/// new one (`crate::cycle::epoch`, "A record's next life").
#[test]
fn a_new_life_advances_at_the_first_visit() {
    let _g = test_guard();
    let _x = EpochInterval::of(Duration::from_secs(60));
    let record = unsafe { &*record() };
    record.note_advanced_at(0);
    crate::cycle::mutator_record::note_new_life_for_test(std::ptr::from_ref(record).cast_mut());
    let turnovers = record.turnovers();

    visit(record);
    assert_eq!(
        record.turnovers(),
        turnovers + 1,
        "the first visit of the new life advanced the epoch"
    );
    assert_ne!(record.advanced_at(), 0, "and stamped the instant");
    assert!(!record.take_new_life(), "and took the note down");
}

/// A slot no collector thread is born into under the default cap and no
/// other case names.
const SLOT: usize = 5;

/// A thread whose deferred lane stands occupied while the collector takes
/// its live roots oftener than X has the lane re-offered once X has passed:
/// real takes through the round, all live, the mutator's poll collecting over
/// P after each. What the case reads is the lane's mirror, which moves only
/// when the poll splices the lane back into R. Red on the clock this replaces,
/// the mutator's own commits: every collection over P closed a commit, the
/// collector read the clock as moving and never asked, and the lane waited for
/// sixty-four of them or for pressure or exit.
#[test]
#[cfg_attr(
    feature = "collector-chain",
    ignore = "under the chain the collector keeps a root read live or unwalked in its chain, not in P (`crate::cycle::chain`)"
)]
fn a_lane_behind_all_live_takes_oftener_than_x_is_reoffered_after_x() {
    let _g = test_guard();
    const X: Duration = Duration::from_millis(40);
    let _x = EpochInterval::of(X);
    let freed = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mutator = Mutator::start_polling(freed);
    let record = unsafe { &*mutator.record };
    record.name_to_collector(SLOT);
    testing::confine_rounds_to(mutator.record);
    let class = Sent(node_class("QuietLaneNode"));
    let mut standing = Standing::new(SLOT);

    // One live ring registered, taken by a round and collected over P by the
    // mutator's poll: every verdict reads live, and the close defers it.
    let mut keepers = Vec::new();
    let mut take_a_live_ring = |standing: &mut Standing| {
        let class = Sent(class.0);
        keepers.push(mutator.run(move |arena| {
            let ring = unsafe { crate::cycle::testing::long_ring(arena, class.into_inner(), 2) };
            unsafe { crate::refcount::ll_retain(ring[0].cast()) };
            Sent(ring[0])
        }));
        assert!(
            round(SLOT, ANY_ENTRY, standing).made_a_batch,
            "the round took the ring"
        );
        mutator.run(|_| unsafe { crate::gc::ll_gc_maybe_collect() })
    };

    let first = std::time::Instant::now();
    let _ = take_a_live_ring(&mut standing);
    assert_ne!(
        mutator.run(|_| crate::cycle::queue::deferred_count()),
        0,
        "the first take's live roots stand in the deferred lane"
    );
    let before = mutator.run(|_| crate::cycle::queue::deferred_turnover_mirror());

    // A fifth of X between takes, so that they come oftener than X unless the
    // box stalls the case; the one assertion is after the last of them.
    while first.elapsed() < X + X / 2 {
        std::thread::sleep(X / 5);
        let _ = take_a_live_ring(&mut standing);
    }

    let after = mutator.run(|_| crate::cycle::queue::deferred_turnover_mirror());
    assert_ne!(
        after, before,
        "X passed through the takes, and the lane was re-offered"
    );

    testing::confine_rounds_to_records(&[]);
    record.name_to_collector(ELDER);
    let keepers = Sent(keepers);
    mutator.run(move |_| unsafe {
        for keeper in keepers.into_inner() {
            crate::refcount::ll_release(keeper.into_inner().cast());
        }
        crate::gc::ll_gc_collect_cycles();
    });
}

/// A batch made at the visit that advances the epoch traces on the turned
/// epoch: a garbage ring behind a member stamped mature in the epoch that
/// ended is proposed, and the mutator's next poll frees it. Red with the
/// advance after the serve: the batch prunes the edge into the member, whose
/// stamp still reads fresh, reads the root live, and the ring waits for the
/// advance after.
#[test]
fn a_batch_at_the_advancing_visit_traces_on_the_turned_epoch() {
    let _g = test_guard();
    let _x = EpochInterval::of(Duration::from_millis(1));
    let freed = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mutator = Mutator::start_polling(std::sync::Arc::clone(&freed));
    let record = unsafe { &*mutator.record };
    record.name_to_collector(SLOT);
    testing::confine_rounds_to(mutator.record);
    let class = Sent(node_class("AdvancingVisitNode"));
    let epoch = crate::cycle::epoch::epoch_of(record.turnovers());

    // The root holds the member through the creation reference moved into
    // it, so no release ever registers the member; the member holds the
    // root. The member carries a mature stamp of the epoch now standing.
    mutator.run(move |arena| unsafe {
        let class = class.into_inner();
        let mut context = crate::memory::context::LLContext { arena: &mut *arena };
        let category = crate::refcount::MemoryCategory::GcHeap;
        let root = crate::object::new_constructed(&mut context, class, category);
        let member = crate::object::new_constructed(&mut context, class, category);
        crate::cycle::testing::move_prop(root, crate::test_support::prop_offset(0), member);
        crate::test_support::store_prop(arena, member, crate::test_support::prop_offset(0), root);
        crate::refcount::write_maturation_stamp(
            member.cast(),
            crate::refcount::MaturationStamp {
                epoch,
                age: crate::cycle::mark::TRAVERSAL_AGE_THRESHOLD,
            },
        );
        assert!(
            !crate::refcount::ll_release(root.cast()),
            "the member holds the root, so the release registers it"
        );
    });

    // X has passed since the instant, so this visit advances. The clock's
    // base is fixed at its first reading, which may be this case's.
    let _ = record.take_new_life();
    record.note_advanced_at(serve_clock_now());
    std::thread::sleep(Duration::from_millis(5));
    let turnovers = record.turnovers();
    let mut standing = Standing::new(SLOT);
    assert!(
        round(SLOT, ANY_ENTRY, &mut standing).made_a_batch,
        "the round took the root"
    );
    assert_eq!(record.turnovers(), turnovers + 1, "and the visit advanced");

    let polled = mutator.run(|_| unsafe { crate::gc::ll_gc_maybe_collect() });
    assert_eq!(
        polled + freed.load(std::sync::atomic::Ordering::Relaxed),
        2,
        "the batch traced past the stale stamp and proposed the ring"
    );

    testing::confine_rounds_to_records(&[]);
    record.name_to_collector(ELDER);
}

/// Zero for the embedder's interval when the guard drops: a case that
/// failed between the setter and its own zero would otherwise leave every
/// later case's rounds advancing at its figure.
struct EmbeddersInterval;

impl Drop for EmbeddersInterval {
    fn drop(&mut self) {
        set_epoch_interval(Duration::ZERO);
    }
}

/// The embedder's interval replaces the crate's, and zero restores it; a
/// case's override, when set, outranks both.
#[test]
fn the_embedders_interval_replaces_the_default_and_zero_restores_it() {
    let _g = test_guard();
    let _embedders = EmbeddersInterval;
    testing::advance_epochs_after(None);
    assert_eq!(epoch_interval(), EPOCH_INTERVAL);
    crate::gc::ll_gc_set_epoch_interval(3);
    assert_eq!(epoch_interval(), Duration::from_millis(3));
    let _x = EpochInterval::of(Duration::from_millis(1));
    assert_eq!(
        epoch_interval(),
        Duration::from_millis(1),
        "the case's override outranks the embedder's"
    );
    crate::gc::ll_gc_set_epoch_interval(0);
    drop(_x);
    assert_eq!(
        epoch_interval(),
        EPOCH_INTERVAL,
        "zero restores the crate's"
    );
}
