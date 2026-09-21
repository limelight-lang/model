//! The collector's request for a turnover: made after X has passed between
//! two serves of a mutator that reached nothing, and never before a stamp
//! exists, inside X, or after a serve that found the mutator's own clock
//! moving — a batch, or a token the mutator or another collector holds.

use super::*;

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

    ask_for_a_turnover_if_quiet(record, Served::Idle);
    assert!(
        !record.turnover_is_requested(),
        "the first serve of a life stamps and asks nothing"
    );
    let stamped = record.served_at();
    assert_ne!(stamped, 0, "and it stamped");

    ask_for_a_turnover_if_quiet(record, Served::Idle);
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
    ask_for_a_turnover_if_quiet(record, Served::Unanswered);
    assert!(
        record.turnover_is_requested(),
        "X passed between two serves that reached nothing"
    );
    assert!(record.served_at() > stamped, "and the ask restamped");
    record.clear_turnover_request();
}

/// A serve that found the mutator's own clock moving restamps and asks
/// nothing, however long the stamp stood: a batch, a token held, a `POSTED`
/// not yet disposed of.
#[test]
fn a_mutator_whose_clock_moves_is_restamped_and_not_asked() {
    let _g = test_guard();
    let _x = QuietInterval::of(Duration::from_millis(1));
    let record = unsafe { &*record() };
    record.clear_turnover_request();
    record.note_served_at(1);

    for served in [
        Served::Batch {
            roots: 1,
            complete: true,
            backlog: false,
        },
        Served::TokenHeld,
        Served::Posted,
    ] {
        record.note_served_at(1);
        ask_for_a_turnover_if_quiet(record, served);
        assert!(
            !record.turnover_is_requested(),
            "{served:?} asked nothing though the stamp stood since the base"
        );
        assert!(record.served_at() > 1, "{served:?} restamped");
    }
}

/// The embedder's interval replaces the crate's, and zero restores it; a
/// case's override, when set, outranks both.
#[test]
fn the_embedders_interval_replaces_the_default_and_zero_restores_it() {
    let _g = test_guard();
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
    ask_for_a_turnover_if_quiet(record, Served::Idle);
    let stamped = record.served_at();
    assert_ne!(stamped, 0, "the first serve stamped");

    std::thread::sleep(Duration::from_millis(5));
    record.note_commit();
    ask_for_a_turnover_if_quiet(record, Served::Idle);
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
