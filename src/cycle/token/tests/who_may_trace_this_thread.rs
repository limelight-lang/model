//! A held token blocks the collection of the thread it belongs to, and nothing
//! else: that thread keeps allocating, storing and registering, another thread
//! collects its own candidates meanwhile, and the blocked collection runs once the
//! holder releases. The mutator holds the token from its take through its
//! close, destructors included, on both paths, and a thread that may not
//! collect never waits for it.
//!
//! **Whether a collection waited is read off the token's own count of
//! waits**, because a case that only terminates terminates most easily when
//! the wait is never taken: a holder that lets go before the mutator reaches
//! the wait proves nothing, so every holder here lets go only once the
//! count says the mutator is waiting.

use super::*;
use crate::class::{Class, ClassBuilder};
use crate::cycle::testing::ring;
use crate::cycle::token::testing::{Handed, HeldByACollector, wait_for_a_waiter};
use crate::gc::ll_gc_collect_cycles;
use crate::memory::arena::Arena;
use crate::memory::block_pool::test_guard;
use crate::object::Object;
use std::sync::atomic::AtomicUsize;

/// A class with one counted Box property at `prop_offset(0)`, which is what
/// [`ring`] links its members through.
fn node_class(name: &str, destructor: *const ()) -> *const Class {
    ClassBuilder::new(name)
        .prop("next", true)
        .destructor(destructor)
        .build()
}

/// How many destructors of this group ran with the calling thread's token
/// held, counted so that one held reading cannot hide behind a free one.
static HELD_IN_A_DESTRUCTOR: AtomicUsize = AtomicUsize::new(0);
static DESTRUCTORS: AtomicUsize = AtomicUsize::new(0);

unsafe extern "C" fn token_reading_destructor(_object: *mut Object) {
    DESTRUCTORS.fetch_add(1, Ordering::Relaxed);
    if unsafe { (*this_thread_token()).is_held() } {
        HELD_IN_A_DESTRUCTOR.fetch_add(1, Ordering::Relaxed);
    }
}

unsafe extern "C" fn counting_destructor(_object: *mut Object) {
    DESTRUCTORS.fetch_add(1, Ordering::Relaxed);
}

#[test]
fn a_held_token_blocks_this_thread_s_collection_alone_until_the_release() {
    let _g = test_guard();
    crate::cycle::queue::release_queue_segments();
    DESTRUCTORS.store(0, Ordering::Relaxed);
    let class = node_class("TokenHeldRing", counting_destructor as *const ());
    let mut arena = Arena::new();
    let waits_before = unsafe { (*this_thread_token()).waits() };

    let mut held = HeldByACollector::take(this_thread_token(), false);

    // Registration, release and allocation proceed on the claimed thread:
    // the ring is allocated, linked and every creation reference spent while
    // a collector holds this thread's token.
    let _members = unsafe { ring(&mut arena, [class; 3]) };
    assert_eq!(crate::cycle::queue::candidate_count(), 3);

    // Once this thread's collection is waiting, another thread collects its
    // own candidates over its own token — the second collector's trace of another
    // thread — and only then lets this one go.
    let token = Handed(this_thread_token());
    let elsewhere = std::thread::spawn(move || {
        wait_for_a_waiter(token.token(), waits_before);
        assert!(
            crate::memory::heap::ll_thread_init(),
            "the pool served this thread"
        );
        let class = node_class("TokenElsewhereRing", counting_destructor as *const ());
        let mut arena = Arena::new();
        let _members = unsafe { ring(&mut arena, [class; 2]) };
        let elsewhere = unsafe { ll_gc_collect_cycles() };
        held.release();
        elsewhere
    });

    let freed = unsafe { ll_gc_collect_cycles() };
    assert!(
        unsafe { (*this_thread_token()).waits() } > waits_before,
        "the collection went to wait on its held token"
    );
    assert_eq!(
        freed, 3,
        "and collected the ring registered while the token was held"
    );
    assert_eq!(
        elsewhere.join().expect("the other thread returned"),
        2,
        "another thread's token is not this one"
    );
    assert_eq!(DESTRUCTORS.load(Ordering::Relaxed), 5);
}

/// The mutator's claim lasts from the take through the close on both paths:
/// held at the trace's last row read — the scan's end off the poll, the
/// harvest sweep under pressure — and still held in every destructor, so a
/// collector's claim fails for the collection's whole length
/// (`rfc/dev/design/trace-token-handshake.md`, E10).
#[test]
fn the_token_is_held_through_the_destructors_on_both_paths() {
    let _g = test_guard();
    crate::cycle::queue::release_queue_segments();
    DESTRUCTORS.store(0, Ordering::Relaxed);
    HELD_IN_A_DESTRUCTOR.store(0, Ordering::Relaxed);
    let class = node_class("TokenReadingRing", token_reading_destructor as *const ());
    let mut arena = Arena::new();

    take_held_at_last_row_read();
    let _off_the_poll = unsafe { ring(&mut arena, [class; 2]) };
    assert_eq!(unsafe { ll_gc_collect_cycles() }, 2);
    assert_eq!(DESTRUCTORS.load(Ordering::Relaxed), 2);
    assert_eq!(
        take_held_at_last_row_read(),
        Some(true),
        "held through the scan's end, the last row read off the poll"
    );

    let _under_pressure = unsafe { ring(&mut arena, [class; 2]) };
    assert_eq!(
        unsafe { crate::cycle::collect::collect_under_pressure() },
        2
    );
    assert_eq!(DESTRUCTORS.load(Ordering::Relaxed), 4);
    assert_eq!(
        take_held_at_last_row_read(),
        Some(true),
        "held through the harvest sweep, the last row read under pressure"
    );

    assert_eq!(
        HELD_IN_A_DESTRUCTOR.load(Ordering::Relaxed),
        4,
        "every destructor ran under this thread's own claim"
    );
    assert!(
        !unsafe { (*this_thread_token()).is_held() },
        "the token is free after a collection"
    );
}

/// Eligibility is checked before the wait: inside a reset this thread may not
/// collect, so a held token is never waited for.
#[test]
fn a_thread_that_may_not_collect_does_not_wait_for_its_held_token() {
    let _g = test_guard();
    let waits_before = unsafe { (*this_thread_token()).waits() };
    let mut held = HeldByACollector::take(this_thread_token(), false);

    let mut window = crate::memory::reset_window::ResetWindow::closed();
    // Any address stands for the arena here: what this case needs is a
    // window open, and nothing reads the identity behind it.
    let guard = crate::memory::reset_window::open(&mut window, std::ptr::dangling_mut());
    assert_eq!(
        unsafe { ll_gc_collect_cycles() },
        0,
        "refused, inside a reset"
    );
    drop(guard);
    assert_eq!(
        unsafe { (*this_thread_token()).waits() },
        waits_before,
        "the refusal went to wait on the token"
    );

    held.release();
    assert!(!unsafe { (*this_thread_token()).is_held() });
}

/// The wait itself: a take that finds a collector's claim on the byte returns
/// once the collector releases, and a release that races the waiter's test is
/// not lost. A claim fails on the initialisation's hold and on another
/// collector's claim alike.
#[test]
fn a_take_that_finds_the_token_held_returns_at_the_release() {
    use crate::cycle::token::{MUTATOR, TookFrom, state};
    let token = TraceToken::new_held();
    assert!(
        !token.claim_for_test(crate::cycle::worker::ELDER),
        "claimed under the initialisation's hold"
    );
    token.release();
    assert!(token.claim_for_test(crate::cycle::worker::ELDER));
    assert!(!token.claim_for_test(1), "claimed twice");

    std::thread::scope(|scope| {
        let waiter = scope.spawn(|| {
            let took = token.take();
            (took, state(token.read()))
        });
        wait_for_a_waiter(&token, 0);
        token.release_claim(crate::cycle::worker::ELDER, false);
        assert_eq!(
            waiter.join().expect("the waiter returned"),
            (TookFrom::Free, MUTATOR),
            "the waiter holds it now"
        );
    });

    assert_ne!(
        token.waits(),
        0,
        "the taker waited, and the release ended it"
    );
    token.release();
    assert!(!token.is_held());
}

/// A take that holds at `POSTED` decides on the byte a collector's release
/// wrote into its wait, not on the byte it first read: a release to `POSTED`
/// leaves the taker holding nothing and the byte at `POSTED`.
#[test]
fn a_take_that_holds_at_posted_holds_after_a_wait_too() {
    use crate::cycle::token::{POSTED, state};
    let token = TraceToken::new_held();
    token.release();
    assert!(token.claim_for_test(crate::cycle::worker::ELDER));

    std::thread::scope(|scope| {
        let waiter = scope.spawn(|| token.take_unless(true));
        wait_for_a_waiter(&token, 0);
        token.release_claim(crate::cycle::worker::ELDER, true);
        assert_eq!(waiter.join().expect("the waiter returned"), None);
    });

    assert_eq!(state(token.read()), POSTED);
}

/// A take from `POSTED` says so, and a take from a standing request refuses
/// it: the byte reads `MUTATOR` after either, and the request's collector is
/// woken to read it (a wake to a slot with no thread is lost, and that is
/// what this case sends).
#[test]
fn a_take_consumes_posted_and_refuses_a_request() {
    use crate::cycle::token::{MUTATOR, REQUESTED, TookFrom, state, word};
    let token = TraceToken::new_held();
    token.release();

    assert!(token.claim_for_test(crate::cycle::worker::ELDER));
    token.release_claim(crate::cycle::worker::ELDER, true);
    assert_eq!(token.take(), TookFrom::Posted);
    assert_eq!(state(token.read()), MUTATOR);
    token.release();

    assert!(token.claim_for_test(crate::cycle::worker::ELDER));
    token.release_claim(crate::cycle::worker::ELDER, false);
    token.request_for_test(word(REQUESTED, 5));
    assert!(
        !token.claim_for_test(crate::cycle::worker::ELDER),
        "claimed over a request"
    );
    assert_eq!(token.take(), TookFrom::Free);
    assert_eq!(state(token.read()), MUTATOR);
    token.release();
}

/// The path under pressure takes the token at its start, the way the path
/// off the poll does, so a held token blocks it the same way.
#[test]
fn a_held_token_blocks_the_collection_under_pressure_too() {
    let _g = test_guard();
    crate::cycle::queue::release_queue_segments();
    DESTRUCTORS.store(0, Ordering::Relaxed);
    let class = node_class("TokenHeldPressureRing", counting_destructor as *const ());
    let mut arena = Arena::new();
    let _members = unsafe { ring(&mut arena, [class; 2]) };
    let waits_before = unsafe { (*this_thread_token()).waits() };

    let mut held = HeldByACollector::take(this_thread_token(), true);
    let freed = unsafe { crate::cycle::collect::collect_under_pressure() };
    assert!(
        unsafe { (*this_thread_token()).waits() } > waits_before,
        "the round went to wait on its held token"
    );
    assert_eq!(freed, 2);
    held.release();
}
