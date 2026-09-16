//! The collector thread's life, driven by a case in place of the pressure
//! path: its birth through `ensure_thread`, one thread per process however
//! often it is asked, a refused base block as a birth retried after the
//! interval, a round that reaches the records beyond the caller's and claims
//! nothing of a record on the free list, and a panicking round that hands
//! the word back for the next birth. What a round does for an owner — the
//! batch over the ring behind its writer — is `the_batch`'s.

use super::*;
use crate::cycle::testing::Sent;
use crate::cycle::worker::testing::{self, ThreadState};
use crate::memory::block_pool::test_guard;

/// This thread's record, which the guard's initialisation drew.
fn record() -> *mut OwnerRecord {
    let record = owner_record::this_thread_record();
    assert!(
        !record.is_null(),
        "the guard's init drew this thread's record"
    );
    record
}

/// Ends the collector thread when the case ends, on a panic as on a return,
/// so that a failed case leaves no thread rounding under the next one.
struct RetireOnDrop;

impl Drop for RetireOnDrop {
    fn drop(&mut self) {
        testing::retire();
    }
}

/// Whether `reached` answered true within `within`, asked every millisecond.
fn wait_until(mut reached: impl FnMut() -> bool, within: std::time::Duration) -> bool {
    let deadline = std::time::Instant::now() + within;
    loop {
        if reached() {
            return true;
        }

        if std::time::Instant::now() > deadline {
            return false;
        }

        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

const A_BIRTH: std::time::Duration = std::time::Duration::from_secs(10);

#[test]
fn a_birth_is_one_thread_whose_rounds_claim_and_release_the_record() {
    let _g = test_guard();
    let record = record();
    assert_eq!(testing::thread_state(), ThreadState::Unborn);
    testing::confine_rounds_to(record);
    testing::permit_births(true);
    let _end = RetireOnDrop;
    let _ = testing::take_spawns();
    // A round claims only an owner with work: one garbage ring in R, which
    // the batch takes and the case collects at its end.
    crate::cycle::queue::verdicts::discard_standing_verdicts();
    crate::cycle::queue::release_queue_segments();
    let class = crate::class::ClassBuilder::new("BirthRingNode")
        .prop("next", true)
        .build();
    let mut arena = crate::memory::arena::Arena::new();
    let _ring = unsafe { crate::cycle::testing::ring(&mut arena, [class, class]) };
    let _ = testing::take_owners_served();

    ensure_thread();
    assert!(
        wait_until(|| testing::thread_state() == ThreadState::Alive, A_BIRTH),
        "the call birthed the thread"
    );
    ensure_thread();
    assert_eq!(
        testing::thread_state(),
        ThreadState::Alive,
        "a second call births no second thread"
    );
    assert_eq!(testing::take_spawns(), 1);

    // A round claims this record's token and releases it: the count moves
    // after the serve returns, which is after the release.
    assert!(
        wait_until(|| testing::take_owners_served() >= 1, A_BIRTH),
        "a round claimed and released this thread's record"
    );

    testing::retire();
    assert_eq!(testing::thread_state(), ThreadState::Unborn);
    assert_eq!(
        unsafe { crate::gc::ll_gc_collect_cycles() },
        2,
        "the ring the batch proposed is collected out of P"
    );
    crate::cycle::queue::release_queue_segments();
}

#[test]
fn a_refused_base_block_is_a_birth_a_later_call_repeats() {
    let _g = test_guard();
    let record = record();
    assert_eq!(testing::thread_state(), ThreadState::Unborn);
    testing::confine_rounds_to(record);
    testing::permit_births(true);
    let _end = RetireOnDrop;
    let _ = testing::take_spawns();

    testing::refuse_the_next_births_base_block();
    ensure_thread();
    assert_ne!(
        testing::thread_state(),
        ThreadState::Alive,
        "a thread whose base block was refused never started"
    );
    assert!(
        wait_until(|| testing::thread_state() == ThreadState::Unborn, A_BIRTH),
        "and the process is without a thread again"
    );
    assert_eq!(testing::take_spawns(), 1);

    // A call inside the interval spawns nothing; one after it births.
    ensure_thread();
    assert_eq!(
        testing::take_spawns(),
        0,
        "a refused birth is not retried at once"
    );
    std::thread::sleep(BIRTH_RETRY_INTERVAL);
    ensure_thread();
    assert!(
        wait_until(|| testing::thread_state() == ThreadState::Alive, A_BIRTH),
        "a call after the interval birthed one"
    );
    assert_eq!(testing::take_spawns(), 1);
}

#[test]
fn a_round_reaches_a_record_beyond_the_callers_and_leaves_a_free_one_alone() {
    let _g = test_guard();
    // A record another thread lived in and gave back, pinned so that no
    // thread of another case takes it while this one reads it.
    let free = crate::cycle::testing::on_a_fresh_thread(|| {
        let record = owner_record::this_thread_record();
        owner_record::pin_for_test(record, true);
        Sent(record)
    })
    .into_inner();
    assert!(owner_record::registry_lists_free(free));
    assert!(
        unsafe { (*free).token.is_held() },
        "held by the exit's claim"
    );

    // The round, driven on this thread: it skips this thread's own record,
    // reaches the free one, and claims nothing of it — the exit's claim
    // stands, and a compare-and-swap from free fails on it.
    testing::confine_rounds_to(free);
    let _ = testing::take_records_visited();
    let _ = testing::take_owners_served();
    round();
    assert!(
        testing::take_records_visited() >= 1,
        "the walk reached past the caller's record"
    );
    assert_eq!(testing::take_owners_served(), 0);
    assert!(unsafe { (*free).token.is_held() });
    testing::confine_rounds_to(std::ptr::null_mut());
    owner_record::pin_for_test(free, false);
}

#[test]
fn a_round_that_panics_leaves_the_word_unborn_for_the_next_birth() {
    let _g = test_guard();
    let record = record();
    testing::confine_rounds_to(record);
    testing::permit_births(true);
    let _end = RetireOnDrop;
    let _ = testing::take_spawns();

    ensure_thread();
    assert!(wait_until(
        || testing::thread_state() == ThreadState::Alive,
        A_BIRTH
    ));
    testing::panic_at_the_next_visit();
    assert!(
        wait_until(|| testing::thread_state() == ThreadState::Unborn, A_BIRTH),
        "the panic unwound out of the thread and the word went back"
    );

    // Which is what lets the next call birth again.
    ensure_thread();
    assert!(wait_until(
        || testing::thread_state() == ThreadState::Alive,
        A_BIRTH
    ));
    assert_eq!(testing::take_spawns(), 2);
}

mod the_batch;
