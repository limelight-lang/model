//! A held token blocks the collection of the thread it belongs to, and nothing
//! else: that thread keeps allocating, storing and registering, another thread
//! collects its own graph meanwhile, and the blocked collection runs once the
//! holder releases. The token is held through the trace's last row read and
//! released before the first destructor, on both paths, and a thread that may
//! not collect never waits for it.
//!
//! **Whether a collection waited is read off the token's own count of
//! waits**, because a case that only terminates terminates most easily when
//! the wait is never taken: a holder that lets go before the owner reaches
//! the wait proves nothing, so every holder here lets go only once the
//! count says the owner is waiting.

use super::*;
use crate::class::{Class, ClassBuilder};
use crate::cycle::testing::ring;
use crate::gc::ll_gc_collect_cycles;
use crate::memory::arena::Arena;
use crate::memory::block_pool::test_guard;
use crate::object::Object;
use std::sync::atomic::AtomicUsize;
use std::sync::mpsc;
use std::time::{Duration, Instant};

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
    if TOKEN.with(TraceToken::is_held) {
        HELD_IN_A_DESTRUCTOR.fetch_add(1, Ordering::Relaxed);
    }
}

unsafe extern "C" fn counting_destructor(_object: *mut Object) {
    DESTRUCTORS.fetch_add(1, Ordering::Relaxed);
}

/// A token pointer handed to another thread. The pointee is the test
/// thread's thread-local, and the guard that carries this joins the holder
/// before the test thread returns, which is what keeps the pointer valid.
struct Handed(*const TraceToken);

unsafe impl Send for Handed {}

impl Handed {
    /// The pointer, through a method so that a closure captures the wrapper
    /// rather than its field.
    fn token(&self) -> *const TraceToken {
        self.0
    }
}

/// Spin until the token's count of waits passes `before`, or a bound
/// passes — so a case whose owner never waits fails on an assertion rather
/// than hanging.
fn wait_for_a_waiter(token: *const TraceToken, before: usize) {
    let bound = Instant::now() + Duration::from_secs(10);
    while unsafe { (*token).waits() } == before && Instant::now() < bound {
        std::thread::yield_now();
    }
}

/// The calling thread's token, held from another thread — the stand-in for a
/// collector tracing this mutator's graph — until [`release`](Self::release)
/// or the guard's drop. The drop releases the holder and joins it, on the
/// unwind as well as on the return, so a failed assertion never leaves a
/// thread writing into a freed thread-local.
///
/// With `until_waited` the holder lets go on its own once the owner has gone
/// to wait on the token; without it, at `release`.
struct HeldByACollector {
    release: Option<mpsc::Sender<()>>,
    collector: Option<std::thread::JoinHandle<()>>,
}

impl HeldByACollector {
    /// Returns once the collector holds the token.
    fn take(token: *const TraceToken, until_waited: bool) -> Self {
        let handed = Handed(token);
        let (held_sender, held) = mpsc::channel();
        let (release, release_receiver) = mpsc::channel::<()>();
        let collector = std::thread::spawn(move || {
            let token = handed.token();
            let waits_before = unsafe { (*token).waits() };
            assert!(unsafe { (*token).try_take() }, "the owner was not tracing");
            held_sender.send(()).expect("the owner waits for this");
            if until_waited {
                wait_for_a_waiter(token, waits_before);
            } else {
                let _ = release_receiver.recv_timeout(Duration::from_secs(10));
            }

            unsafe { (*token).release() };
        });
        held.recv().expect("the collector took the token");
        Self {
            release: Some(release),
            collector: Some(collector),
        }
    }

    /// Let the holder go, and wait for it.
    fn release(&mut self) {
        drop(self.release.take());
        if let Some(collector) = self.collector.take() {
            collector.join().expect("the collector returned");
        }
    }
}

impl Drop for HeldByACollector {
    fn drop(&mut self) {
        self.release();
    }
}

#[test]
fn a_held_token_blocks_this_thread_s_collection_alone_until_the_release() {
    let _g = test_guard();
    crate::cycle::queue::release_queue_segments();
    DESTRUCTORS.store(0, Ordering::Relaxed);
    let class = node_class("TokenHeldRing", counting_destructor as *const ());
    let mut arena = Arena::new();
    let waits_before = TOKEN.with(TraceToken::waits);

    let mut held = HeldByACollector::take(this_thread_token(), false);

    // Registration, release and allocation proceed on the claimed thread:
    // the ring is allocated, linked and every creation reference spent while
    // a collector holds this thread's token.
    let _members = unsafe { ring(&mut arena, [class; 3]) };
    assert_eq!(crate::cycle::queue::candidate_count(), 3);

    // Once this thread's collection is waiting, another thread collects its
    // own graph over its own token — the second collector's trace of another
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
        TOKEN.with(TraceToken::waits) > waits_before,
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

/// The right to trace ends after the trace's last row read — the scan's end
/// off the poll, the harvest sweep under pressure — and before the exact
/// validation and the first destructor.
#[test]
fn the_token_is_released_before_the_first_destructor_on_both_paths() {
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
        0,
        "a destructor ran with this thread's token held"
    );
    assert!(
        !TOKEN.with(TraceToken::is_held),
        "the token is free after a collection"
    );
}

/// Eligibility is checked before the wait: inside a reset this thread may not
/// collect, so a held token is never waited for.
#[test]
fn a_thread_that_may_not_collect_does_not_wait_for_its_held_token() {
    let _g = test_guard();
    let waits_before = TOKEN.with(TraceToken::waits);
    let mut held = HeldByACollector::take(this_thread_token(), false);

    let window = crate::memory::reset_window::opened();
    assert_eq!(
        unsafe { ll_gc_collect_cycles() },
        0,
        "refused, inside a reset"
    );
    drop(window);
    assert_eq!(
        TOKEN.with(TraceToken::waits),
        waits_before,
        "the refusal went to wait on the token"
    );

    held.release();
    assert!(!TOKEN.with(TraceToken::is_held));
}

/// The wait itself: a take that finds the token held returns once the holder
/// releases, and a release that races the waiter's test is not lost.
#[test]
fn a_take_that_finds_the_token_held_returns_at_the_release() {
    let token = TraceToken::new();
    assert!(token.try_take());
    assert!(!token.try_take(), "held twice");

    std::thread::scope(|scope| {
        let waiter = scope.spawn(|| {
            token.take();
            token.is_held()
        });
        wait_for_a_waiter(&token, 0);
        token.release();
        assert!(
            waiter.join().expect("the waiter returned"),
            "the waiter holds it now"
        );
    });

    assert_eq!(token.waits(), 1, "one wait, ended by the release");
    token.release();
    assert!(!token.is_held());
}

/// The path under pressure takes the token round by round, so a held token
/// blocks it the way it blocks the path off the poll.
#[test]
fn a_held_token_blocks_the_collection_under_pressure_too() {
    let _g = test_guard();
    crate::cycle::queue::release_queue_segments();
    DESTRUCTORS.store(0, Ordering::Relaxed);
    let class = node_class("TokenHeldPressureRing", counting_destructor as *const ());
    let mut arena = Arena::new();
    let _members = unsafe { ring(&mut arena, [class; 2]) };
    let waits_before = TOKEN.with(TraceToken::waits);

    let mut held = HeldByACollector::take(this_thread_token(), true);
    let freed = unsafe { crate::cycle::collect::collect_under_pressure() };
    assert!(
        TOKEN.with(TraceToken::waits) > waits_before,
        "the round went to wait on its held token"
    );
    assert_eq!(freed, 2);
    held.release();
}
