//! A record outlives its thread and says, through its token alone, whether
//! anyone may claim it: never before its thread's initialisation is complete,
//! never after its exit's final claim, and only through the record while its
//! thread lives. The owner's own claim is told from a foreign holder's by
//! the free path, which withholds under the second and not the first.

use super::*;
use crate::cycle::testing::Sent;
use crate::cycle::token::{HeldToken, held_by_a_foreign_holder, this_thread_token};
use crate::memory::block_pool::test_guard;
use std::sync::mpsc;

/// Start a thread through `ll_thread_init`, hand its token pointer out, and
/// hold the thread alive until `release` says otherwise; the thread's exit
/// runs through the guard `ll_thread_init` arms.
fn a_started_thread() -> (
    *const TraceToken,
    *mut OwnerRecord,
    mpsc::Sender<()>,
    std::thread::JoinHandle<()>,
) {
    let (started, on_start) = mpsc::channel();
    let (release, on_release) = mpsc::channel::<()>();
    let thread = std::thread::spawn(move || {
        assert!(
            crate::memory::heap::ll_thread_init(),
            "the pool served this thread"
        );
        started
            .send((Sent(this_thread_token()), Sent(this_thread_record())))
            .expect("the case waits for this");
        let _ = on_release.recv();
    });
    let (token, record) = on_start.recv().expect("the thread started");
    (token.into_inner(), record.into_inner(), release, thread)
}

#[test]
fn a_record_is_claimable_while_its_thread_lives_and_not_after_its_exit() {
    let _g = test_guard();
    let (token, record, release, thread) = a_started_thread();
    assert!(
        !token.is_null(),
        "the thread's initialisation drew its record"
    );
    assert_eq!(
        unsafe { &raw const (*record).token },
        token,
        "the token is the record's"
    );

    assert!(
        unsafe { (*token).try_take() },
        "a live thread's record is claimable through the record"
    );
    unsafe { (*token).release() };

    assert!(
        !registry_lists_free(record),
        "a live thread's record is nobody else's"
    );
    // Pinned, or another test's thread pops it off the free list between
    // the exit and the reads below.
    pin_for_test(record, true);
    drop(release);
    thread.join().expect("the thread exited");

    assert!(
        unsafe { (*token).is_held() },
        "the exit's final claim stands on the record"
    );
    assert!(
        !unsafe { (*token).try_take() },
        "a released record refuses a claim"
    );
    assert!(
        registry_lists_free(record),
        "the exit gave the record to the registry's free list"
    );
    pin_for_test(record, false);
}

#[test]
fn a_reused_record_is_claimable_once_its_next_thread_has_initialised() {
    let _g = test_guard();
    let (token, record, release, thread) = a_started_thread();
    pin_for_test(record, true);
    drop(release);
    thread.join().expect("the first thread exited");
    assert!(!unsafe { (*token).try_take() }, "held between two lives");
    assert!(registry_lists_free(record));
    pin_for_test(record, false);

    // Another test's thread can pop the first record or push its own on top
    // between the two lives, so the second thread's record is read for its
    // state and not for its identity.
    let (next_token, next_record, release, thread) = a_started_thread();
    assert!(
        !registry_lists_free(next_record),
        "the record the second thread lives in left the free list"
    );
    assert!(
        unsafe { (*next_token).try_take() },
        "the next thread's initialisation released the token"
    );
    unsafe { (*next_token).release() };
    pin_for_test(next_record, true);
    drop(release);
    thread.join().expect("the second thread exited");
    assert!(
        !unsafe { (*next_token).try_take() },
        "held again after the second exit"
    );
    assert!(registry_lists_free(next_record));
    pin_for_test(next_record, false);
}

#[test]
fn a_thread_living_twice_takes_a_record_per_life() {
    let _g = test_guard();
    let (first, second) = std::thread::spawn(|| {
        assert!(crate::memory::heap::ll_thread_init());
        let first = this_thread_record();
        pin_for_test(first, true);
        crate::memory::heap::ll_thread_exit();
        assert!(
            this_thread_record().is_null(),
            "the exit gave the record back"
        );
        assert!(registry_lists_free(first));
        pin_for_test(first, false);

        assert!(crate::memory::heap::ll_thread_init());
        let second = this_thread_record();
        assert!(!second.is_null(), "the second life took a record");
        assert!(
            !registry_lists_free(second),
            "the second life's record is nobody else's to take"
        );
        assert!(unsafe { (*second).token.try_take() }, "and it is claimable");
        unsafe { (*second).token.release() };
        pin_for_test(second, true);
        crate::memory::heap::ll_thread_exit();
        assert!(registry_lists_free(second));
        pin_for_test(second, false);
        (Sent(first), Sent(second))
    })
    .join()
    .expect("the thread lived twice");
    let _ = (first, second);
}

#[test]
fn an_exit_draws_no_record() {
    let _g = test_guard();
    std::thread::spawn(|| {
        assert_eq!(
            records_taken(),
            0,
            "a thread that never initialised has none"
        );
        crate::memory::heap::ll_thread_exit();
        assert_eq!(
            records_taken(),
            0,
            "the exit's claim and its rounds drew none: a thread without a record is \
             reached by no collector, and a record a round drew would go back free"
        );
        assert!(this_thread_record().is_null());
    })
    .join()
    .expect("the thread exited");
}

#[test]
fn the_owner_s_own_claim_withholds_nothing() {
    let _g = test_guard();
    assert!(!held_by_a_foreign_holder());
    let claim = HeldToken::take();
    assert!(
        unsafe { (*this_thread_token()).is_held() },
        "the owner holds its token"
    );
    assert!(
        !held_by_a_foreign_holder(),
        "the free path reads the owner's own claim as no foreign holder"
    );
    assert!(
        !crate::cycle::deferred_slot_reuse::returns_are_withheld(),
        "a return under the owner's own claim is made"
    );

    let nested = HeldToken::take();
    drop(nested);
    assert!(
        unsafe { (*this_thread_token()).is_held() },
        "a nested take's drop releases nothing"
    );
    drop(claim);
    assert!(!unsafe { (*this_thread_token()).is_held() });
}

#[test]
fn a_foreign_holder_is_read_as_one() {
    let _g = test_guard();
    let mut holder =
        crate::cycle::token::testing::HeldByACollector::take(this_thread_token(), false);
    assert!(held_by_a_foreign_holder());
    assert!(crate::cycle::deferred_slot_reuse::returns_are_withheld());
    holder.release();
    assert!(!held_by_a_foreign_holder());
}

#[test]
fn a_record_is_one_line_carved_from_a_gc_block() {
    let _g = test_guard();
    let (token, record, release, thread) = a_started_thread();
    let block = BlockHeader::of_ptr(record as *const u8);
    assert!(
        registry_owns_block(block),
        "the record stands in a block of the registry's chain"
    );
    assert_eq!(
        unsafe { crate::memory::block_pool::load_block_kind(&raw const (*block).kind) },
        crate::memory::block_pool::BLOCK_KIND_GC_METADATA,
        "the block is GC memory"
    );
    assert_eq!((record as usize) % 64, 0, "a record stands on its own line");
    assert_eq!(
        (record as usize - BlockHeader::payload_start(block) as usize) % size_of::<OwnerRecord>(),
        0
    );
    let _ = token;
    drop(release);
    thread.join().expect("the thread exited");
}
