//! The clock of R's appends: exact at a thread's start, never below the
//! entries truly younger than the cut, a wrapped ring still a cut deep, and
//! a quiet thread's entries old after the cut through the poll's tick.

use super::testing::{append_quietly, reset_the_clock};
use super::*;

const CUT: Duration = Duration::from_millis(100);
const MS: u64 = 1_000_000;

/// Appends at the instants of `marks` in ms, `entries` each, ticking at each
/// as the poll would; answers the true count younger than the cut at `at`.
fn truly_young(marks: &[(u64, u64)], at: u64) -> u64 {
    marks
        .iter()
        .filter(|&&(instant, _)| instant * MS + CUT.as_nanos() as u64 > at)
        .map(|&(_, entries)| entries)
        .sum()
}

#[test]
fn every_append_is_young_before_a_bucket_is_a_cut_old() {
    reset_the_clock();
    append_quietly(10);
    tick(1 * MS, CUT);
    append_quietly(5);
    assert_eq!(young(50 * MS, CUT), 15, "a thread younger than the cut");
    assert_eq!(
        young(100 * MS, CUT),
        15,
        "no bucket opened at or before the cut's limit"
    );
}

#[test]
fn the_young_count_is_never_below_the_truly_young() {
    reset_the_clock();
    let mut marks = Vec::new();
    for step in 1..=400u64 {
        let instant = step * 3 * MS;
        let entries = step % 7 + 1;
        append_quietly(entries);
        marks.push((step * 3, entries));
        tick(instant, CUT);
        let read = young(instant, CUT);
        let truth = truly_young(&marks, instant);
        assert!(read >= truth, "at {step}: read {read}, truly {truth}");
        assert!(
            read <= truth + 2 * 7 * (CUT.as_nanos() as u64 / 16 / (3 * MS) + 1),
            "at {step}: read {read} far past truly {truth}, a bucket or two"
        );
    }
}

#[test]
fn a_wrapped_ring_keeps_a_bucket_a_cut_old() {
    reset_the_clock();
    // Ticks every sixteenth of the cut, far past the ring's length.
    let width = CUT.as_nanos() as u64 / 16;
    for step in 1..=10 * BUCKETS as u64 {
        append_quietly(1);
        tick(step * width, CUT);
    }
    let at = 10 * BUCKETS as u64 * width;
    let read = young(at, CUT);
    assert!(read < APPENDS.with(Cell::get), "a bucket a cut old stands");
    assert!(read >= 16, "and every entry of the cut reads young: {read}");
}

#[test]
fn a_quiet_threads_entries_read_old_a_cut_after_the_polls_next_tick() {
    reset_the_clock();
    tick(1 * MS, CUT);
    append_quietly(30);
    // The poll's tick inside a sixteenth of the cut opens nothing.
    tick(2 * MS, CUT);
    assert_eq!(young(50 * MS, CUT), 30);
    // The next poll's tick, nothing appended since: the bucket that dates
    // the thirty, which read young until a cut past it.
    tick(150 * MS, CUT);
    assert_eq!(young(150 * MS, CUT), 30, "dated at the tick, not before");
    assert_eq!(young(249 * MS, CUT), 30);
    assert_eq!(
        young(250 * MS, CUT),
        0,
        "a cut past the tick that dated them"
    );
}

#[test]
fn a_tick_opens_no_bucket_inside_a_sixteenth_of_the_cut_or_with_no_appends() {
    reset_the_clock();
    append_quietly(1);
    tick(10 * MS, CUT);
    let newest = NEWEST.with(Cell::get);
    tick(200 * MS, CUT);
    assert_eq!(NEWEST.with(Cell::get), newest, "no append since");
    append_quietly(1);
    tick(12 * MS, CUT);
    assert_eq!(NEWEST.with(Cell::get), newest, "inside a sixteenth");
    tick(17 * MS, CUT);
    assert_ne!(NEWEST.with(Cell::get), newest, "past a sixteenth, with one");
}

#[test]
fn appends_across_a_multiple_of_the_stride_read_the_clock_with_a_cut() {
    reset_the_clock();
    testing::cut_at(Some(CUT));
    note_appends(TICK_EVERY - 1);
    assert_eq!(
        OPENED.with(|opened| opened[0].get()),
        0,
        "short of the stride"
    );
    note_appends(300);
    assert_ne!(
        OPENED.with(|opened| opened[0].get()),
        0,
        "a splice past the stride, landing on no multiple"
    );
    testing::cut_at(None);
}

#[test]
fn appends_count_with_no_cut_and_read_no_clock() {
    reset_the_clock();
    testing::cut_at(Some(Duration::ZERO));
    note_appends(1_000);
    assert_eq!(APPENDS.with(Cell::get), 1_000);
    assert_eq!(OPENED.with(|opened| opened[0].get()), 0);
    testing::cut_at(None);
}
