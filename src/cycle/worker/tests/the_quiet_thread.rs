//! The collector's request for a turnover: made after X has passed between
//! two serves that found the mutator's clock standing, and never before a
//! stamp exists, inside X, or after a serve that found a commit of the
//! mutator's own since the stamp. What a serve reached does not decide it —
//! a thread batched oftener than X with all-live batches is asked like any
//! other quiet thread.

use super::*;

/// The ask against the clock now, as `read_one_record` makes it.
fn ask_the_quiet_thread(record: &MutatorRecord) {
    ask_for_a_turnover_if_quiet(record, serve_clock_now());
}

/// Ask after `interval` for the case, and after the module's own again when
/// the guard drops.
struct QuietInterval;

impl QuietInterval {
    fn of(interval: Duration) -> Self {
        testing::ask_turnovers_after(Some(interval));
        Self
    }
}

impl Drop for QuietInterval {
    fn drop(&mut self) {
        testing::ask_turnovers_after(None);
    }
}

/// A serve reads the record's stamp against X: nothing before a stamp exists,
/// nothing inside X, the request once X has passed between two serves that
/// reached nothing.
#[test]
fn a_mutator_served_nothing_for_x_is_asked_for_a_turnover() {
    let _g = test_guard();
    // A minute for the two serves that must fall inside X — a preemption
    // between them cannot reach it — and a millisecond for the sleep.
    let _x = QuietInterval::of(Duration::from_secs(60));
    let record = unsafe { &*record() };
    record.clear_turnover_request();
    record.note_served_at(0);

    ask_the_quiet_thread(record);
    assert!(
        !record.turnover_is_requested(),
        "the first serve of a life stamps and asks nothing"
    );
    let stamped = record.served_at();
    assert_ne!(stamped, 0, "and it stamped");

    ask_the_quiet_thread(record);
    assert!(
        !record.turnover_is_requested(),
        "a second serve inside X asks nothing"
    );
    assert_eq!(
        record.served_at(),
        stamped,
        "and leaves the stamp, so X counts from the first"
    );

    testing::ask_turnovers_after(Some(Duration::from_millis(1)));
    std::thread::sleep(Duration::from_millis(5));
    ask_the_quiet_thread(record);
    assert!(
        record.turnover_is_requested(),
        "X passed between two serves that reached nothing"
    );
    assert!(record.served_at() > stamped, "and the ask restamped");
    record.clear_turnover_request();
}

/// A thread whose batches all read live is asked like any other quiet
/// thread: the batches move no clock, so they leave the stamp standing and X
/// counts through them. Red on the rule this replaces, where a batch
/// restamped by fiat — such a thread, batched oftener than X, was never
/// asked and its deferred lane waited for pressure or exit
/// (`dev/DECISIONS.md`, "a quiet thread's turnover is the collector's to ask
/// for", amended 2026-09-22).
#[test]
fn a_thread_batched_all_live_oftener_than_x_is_asked_after_x() {
    let _g = test_guard();
    const X: Duration = Duration::from_millis(40);
    let _x = QuietInterval::of(X);
    let record = unsafe { &*record() };
    record.clear_turnover_request();
    record.note_served_at(0);

    // The first serve stamps. Every serve after it is a batch that commits
    // nothing — the mutator's clock stands — a fifth of X apart.
    ask_the_quiet_thread(record);
    let stamped = record.served_at();
    assert_ne!(stamped, 0, "the first serve stamped");
    loop {
        std::thread::sleep(X / 5);
        // A tenth of X of margin, so that the serve below is inside X too
        // where this reading was: the two readings are microseconds apart.
        let inside = serve_clock_now() - stamped < (X - X / 10).as_nanos() as u64;
        ask_the_quiet_thread(record);
        if !inside {
            break;
        }

        assert_eq!(
            record.served_at(),
            stamped,
            "a batch that moved no clock left the stamp"
        );
        assert!(
            !record.turnover_is_requested(),
            "a batch inside X asks nothing"
        );
    }

    assert!(
        record.turnover_is_requested(),
        "X passed through the batches, and the serve asked"
    );
    assert!(record.served_at() > stamped, "and the ask restamped");
    record.clear_turnover_request();
}

/// Zero for the embedder's interval when the guard drops: a case that
/// failed between the setter and its own zero would otherwise leave every
/// later case's rounds asking at its figure.
struct EmbeddersInterval;

impl Drop for EmbeddersInterval {
    fn drop(&mut self) {
        set_quiet_interval(Duration::ZERO);
    }
}

/// The embedder's interval replaces the crate's, and zero restores it; a
/// case's override, when set, outranks both.
#[test]
fn the_embedders_interval_replaces_the_default_and_zero_restores_it() {
    let _g = test_guard();
    let _embedders = EmbeddersInterval;
    testing::ask_turnovers_after(None);
    assert_eq!(quiet_interval(), QUIET_INTERVAL);
    crate::gc::ll_gc_set_quiet_interval(3);
    assert_eq!(quiet_interval(), Duration::from_millis(3));
    let _x = QuietInterval::of(Duration::from_millis(1));
    assert_eq!(
        quiet_interval(),
        Duration::from_millis(1),
        "the case's override outranks the embedder's"
    );
    crate::gc::ll_gc_set_quiet_interval(0);
    drop(_x);
    assert_eq!(
        quiet_interval(),
        QUIET_INTERVAL,
        "zero restores the crate's"
    );
}

/// A mutator whose clock moved since the stamp is restamped and not asked,
/// however long the stamp stood and whatever the serve reached: its stamps
/// age on its own commits, and a request on top of them would re-trace its
/// lane un-pruned once per X for nothing. Red with the clock unread: X has
/// passed, and the serve would ask.
#[test]
fn a_mutator_whose_own_commits_moved_its_clock_is_not_asked() {
    let _g = test_guard();
    let _x = QuietInterval::of(Duration::from_millis(1));
    let record = unsafe { &*record() };
    record.clear_turnover_request();
    record.note_served_at(0);
    ask_the_quiet_thread(record);
    let stamped = record.served_at();
    assert_ne!(stamped, 0, "the first serve stamped");

    std::thread::sleep(Duration::from_millis(5));
    record.note_commit();
    ask_the_quiet_thread(record);
    assert!(
        !record.turnover_is_requested(),
        "a commit of the mutator's own since the stamp is a moving clock, X or no X"
    );
    assert!(record.served_at() > stamped, "and the serve restamped");
    assert!(
        record.clock_stood_since_the_stamp(),
        "the restamp took the clock as it stands"
    );
}
