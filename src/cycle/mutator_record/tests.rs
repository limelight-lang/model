//! A record outlives its thread and says, through its token alone, whether
//! anyone may claim it: never before its thread's initialisation is complete,
//! never after its exit's final claim, and only through the record while its
//! thread lives. The mutator's own claim is told from a foreign holder's by
//! the free path, which withholds under the second and not the first. The
//! record is four lines — the token's, the reader's, the writer's and a
//! spare — drawn at `ll_thread_init` beside the base block, so that its
//! refusal is a thread that never starts.

use super::*;
use crate::cycle::testing::Sent;
use crate::cycle::token::{HeldToken, collector_is_tracing_this_thread, this_thread_token};
use crate::memory::block_pool::test_guard;
use std::sync::mpsc;

/// Start a thread through `ll_thread_init`, hand its token pointer out, and
/// hold the thread alive until `release` says otherwise; the thread's exit
/// runs through the guard `ll_thread_init` arms.
fn a_started_thread() -> (
    *const TraceToken,
    *mut MutatorRecord,
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
fn the_mutator_s_own_claim_withholds_nothing() {
    let _g = test_guard();
    assert!(!collector_is_tracing_this_thread());
    let claim = HeldToken::take();
    assert!(
        unsafe { (*this_thread_token()).is_held() },
        "the mutator holds its token"
    );
    assert!(
        !collector_is_tracing_this_thread(),
        "the free path reads the mutator's own claim as no foreign holder"
    );
    assert!(
        !crate::cycle::deferred_slot_reuse::returns_are_withheld(),
        "a return under the mutator's own claim is made"
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
    assert!(collector_is_tracing_this_thread());
    assert!(crate::cycle::deferred_slot_reuse::returns_are_withheld());
    holder.release();
    assert!(!collector_is_tracing_this_thread());
}

#[test]
fn a_record_is_carved_from_a_gc_block_on_a_line_boundary() {
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
        (record as usize - BlockHeader::payload_start(block) as usize) % size_of::<MutatorRecord>(),
        0
    );
    let _ = token;
    drop(release);
    thread.join().expect("the thread exited");
}

#[test]
fn a_record_is_four_lines_and_a_block_holds_255() {
    assert_eq!(size_of::<MutatorRecord>(), 256);
    assert_eq!(std::mem::offset_of!(MutatorRecord, token), 0);
    assert_eq!(
        std::mem::offset_of!(MutatorRecord, reader),
        64,
        "the collector's words are the second line"
    );
    assert_eq!(
        std::mem::offset_of!(MutatorRecord, writer),
        128,
        "the mutator's words are the third"
    );
    assert_eq!(
        std::mem::offset_of!(MutatorRecord, hold),
        192,
        "the hold word the collector and the exit share is the fourth"
    );
    assert_eq!(RECORDS_PER_BLOCK, 255);
}

/// The record is drawn beside the base block, and a registry that cannot
/// carve one refuses the thread the way a refused base block does: nothing
/// of the thread is left funded.
///
/// What the thread drew is read on the thread, through the GC-metadata
/// ledger, rather than off the pool: under `debug-journal` the init's first
/// record site opens the thread's journal ring, which the registry keeps
/// past the thread and which is not this case's subject.
#[test]
fn a_refused_record_is_a_thread_that_never_starts() {
    let _g = test_guard();

    let (started, base_block, record, drawn_there) = std::thread::spawn(|| {
        refuse_record_draws(true);
        let started = crate::memory::heap::ll_thread_init();
        refuse_record_draws(false);
        (
            started,
            crate::cycle::queue::queue_base().is_null(),
            this_thread_record().is_null(),
            crate::memory::gc_metadata::thread_stats(),
        )
    })
    .join()
    .expect("the thread returned");

    assert!(!started, "the thread reports that it did not start");
    assert!(
        base_block,
        "the base block drawn before the record went back"
    );
    assert!(record, "and the thread holds no record");
    assert_eq!(
        drawn_there.current_blocks(),
        0,
        "nothing of the GC metadata the init drew stayed with the thread"
    );
}

/// A record off the free list is reset in place: its reader's and writer's
/// lines are the next thread's own — R's words empty, P's naming the block
/// drawn for that life — and the token word is the one the exit left rather
/// than a rewritten one.
#[test]
fn a_retaken_record_starts_with_fresh_lines() {
    let _g = test_guard();
    let (token, record, release, thread) = a_started_thread();
    let first_life_block = verdict_block(record);
    assert!(
        !first_life_block.is_null(),
        "P's block came with the record"
    );
    scribble_lines_for_test(record);
    pin_for_test(record, true);
    drop(release);
    thread.join().expect("the thread exited");
    assert!(unsafe { (*token).is_held() });
    assert!(!lines_are_fresh(record), "the scribble outlived the exit");
    assert!(
        verdict_block(record).is_null(),
        "P's block went back with the record"
    );

    // The re-take is made on a fresh thread, which names the record it
    // takes: the list's top moves under the parallel harness, and the pin
    // keeps every other thread off this record until the reading is made.
    let sent = Sent(record);
    let (retook_it, lines_fresh) = std::thread::spawn(move || {
        let record = sent.into_inner();
        take_this_record_for_test(record);
        assert!(crate::memory::heap::ll_thread_init());
        let mine = this_thread_record();
        (mine == record, lines_are_fresh(mine))
    })
    .join()
    .expect("the thread returned");
    assert!(retook_it, "the named record was the one taken");
    assert!(lines_fresh, "the re-take reset the lines in place");
    pin_for_test(record, false);
}
