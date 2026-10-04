//! A `loom` model of the handshake at the cut-off T between one mutator and
//! one collector (`dev/design/recycler-over-counts.md`, §4.7–8): the
//! checkpoint byte beside the token, one member's window tag in header byte
//! 7, and the window number the consent publishes.
//!
//! It models a **copy of the protocol** rather than the code, as
//! `free_path_model` does and for the same reason: the token stands in a
//! record reached through a thread-local, and the tag is written through
//! thread-local state. Keep the two in step by hand: the collector's ask is
//! [`TraceToken::ask_for_the_checkpoint`](super::TraceToken::ask_for_the_checkpoint),
//! its reading
//! [`TraceToken::checkpoint_reached`](super::TraceToken::checkpoint_reached)
//! and its withdrawal
//! [`TraceToken::withdraw_the_checkpoint`](super::TraceToken::withdraw_the_checkpoint);
//! the mutator's answer is
//! [`TraceToken::reach_the_checkpoint`](super::TraceToken::reach_the_checkpoint);
//! the tag is `crate::refcount::tag_with_the_window` and the stale clear
//! `crate::refcount::clear_a_stale_window_tag`; the Δ-test that reads them is
//! `crate::cycle::delta_test::test_the_set_by_its_tags`.
//!
//! # What it checks
//!
//! - **A tag stored before the answer is read by the collector that reads
//!   the answer.** The mutator's tag is a relaxed byte store; its answer is a
//!   release store after an acquire load of the ask; the collector's reading
//!   is an acquire load. A relaxed answer lets the collector read the answer
//!   and the tag from before it, and prove a set the mutator touched.
//! - **An answer to an ask the collector withdrew is an answer to the next
//!   ask.** The answer is a load and a store, not one swap: the mutator can
//!   read the first ask, the collector withdraw and ask again, and the
//!   mutator's store land after the second ask. That store still follows
//!   every tag the mutator stored before it, so the collector that reads it
//!   reads those tags: the cut-off is the store, not the load.
//! - **The stale clear never buries a fresh tag.** The clear is a one-byte
//!   compare-and-swap from the stale number to 0; a load and a store in its
//!   place lose a tag the mutator wrote between them.
//! - **The window number the collector tests for is the one the mutator
//!   tags with.** The mutator advances its window before its consent, a
//!   release; the collector's grant is an acquire.
//!
//! # Running it
//!
//! ```text
//! RUSTFLAGS="--cfg loom" cargo test --lib --features recycler-over-counts checkpoint_model
//! ```
//!
//! Not part of the commit gate (`dev/WORKFLOW.md`), as `free_path_model`.

use loom::sync::Arc;
use loom::sync::atomic::{AtomicU8, Ordering};
use loom::thread;

const NONE: u8 = 0;
const ASKED: u8 = 1;
const REACHED: u8 = 2;

const FREE: u8 = 0;
const REQUESTED: u8 = 2;
const COLLECTOR: u8 = 3;

/// The window the consent opens, and a number that predates it.
const WINDOW: u8 = 7;
const STALE: u8 = 6;

/// How many times the collector reads the byte before it gives the wait up:
/// the bound `CHECKPOINT_WAIT` stands for.
const READINGS: usize = 2;

struct Shared {
    checkpoint: AtomicU8,
    tag: AtomicU8,
    token: AtomicU8,
    window: AtomicU8,
}

fn shared(tag: u8) -> Arc<Shared> {
    Arc::new(Shared {
        checkpoint: AtomicU8::new(NONE),
        tag: AtomicU8::new(tag),
        token: AtomicU8::new(FREE),
        window: AtomicU8::new(0),
    })
}

/// The mutator's answer at a safepoint checkpoint, `answer` the ordering of
/// its store.
fn reach(shared: &Shared, answer: Ordering) {
    if shared.checkpoint.load(Ordering::Acquire) == ASKED {
        shared.checkpoint.store(REACHED, answer);
    }
}

/// The collector's ask and its bounded wait: whether the mutator answered.
fn ask_and_wait(shared: &Shared) -> bool {
    shared.checkpoint.store(ASKED, Ordering::Release);
    let mut reached = false;
    for _ in 0..READINGS {
        if shared.checkpoint.load(Ordering::Acquire) == REACHED {
            reached = true;
            break;
        }
        thread::yield_now();
    }
    shared.checkpoint.store(NONE, Ordering::Relaxed);
    reached
}

/// The mutator touches the member in the window and then answers; the
/// collector that reads the answer must read the tag.
fn touched_then_answered(answer: Ordering) {
    let shared = shared(0);

    let collector = {
        let shared = shared.clone();
        thread::spawn(move || {
            if ask_and_wait(&shared) {
                Some(shared.tag.load(Ordering::Relaxed))
            } else {
                None
            }
        })
    };

    shared.tag.store(WINDOW, Ordering::Relaxed);
    reach(&shared, answer);
    reach(&shared, answer);

    if collector.join().unwrap() == Some(0) {
        panic!("the collector proved a set the mutator touched before its answer");
    }
}

#[test]
fn checkpoint_model_a_released_answer_carries_the_tags_before_it() {
    loom::model(|| touched_then_answered(Ordering::Release));
}

#[test]
#[should_panic(expected = "the collector proved a set the mutator touched before its answer")]
fn checkpoint_model_a_relaxed_answer_carries_nothing() {
    loom::model(|| touched_then_answered(Ordering::Relaxed));
}

/// Two asks in a row, the first withdrawn: an answer that read the first ask
/// and stored after the second still follows the tag stored before it.
fn withdrawn_then_asked_again() {
    let shared = shared(0);

    let collector = {
        let shared = shared.clone();
        thread::spawn(move || {
            let _ = ask_and_wait(&shared);
            if ask_and_wait(&shared) {
                Some(shared.tag.load(Ordering::Relaxed))
            } else {
                None
            }
        })
    };

    shared.tag.store(WINDOW, Ordering::Relaxed);
    reach(&shared, Ordering::Release);
    reach(&shared, Ordering::Release);

    if collector.join().unwrap() == Some(0) {
        panic!("a late answer to a withdrawn ask hid a tag");
    }
}

#[test]
fn checkpoint_model_a_late_answer_answers_the_next_ask_soundly() {
    loom::model(withdrawn_then_asked_again);
}

/// The collector clears a stale tag while the mutator — reaching a weakly
/// held member after T — writes a fresh one. `swap` selects the protocol's
/// compare-and-swap; false is a load and a store.
///
/// The mutator's tag is a plain relaxed byte store, written here as a swap:
/// loom 0.7 lets a compare-and-swap read past a plain store to the same
/// location (the swap reads 6 and leaves 0 after the store of 7, an outcome
/// the C++ model's read-modify-write atomicity forbids), which a swap in the
/// store's place does not. The swap is the store's value and position in
/// the modification order and nothing more.
fn stale_clear_against_a_fresh_tag(swap: bool) {
    let shared = shared(STALE);

    let collector = {
        let shared = shared.clone();
        thread::spawn(move || {
            if swap {
                let _ = shared
                    .tag
                    .compare_exchange(STALE, 0, Ordering::Relaxed, Ordering::Relaxed);
            } else if shared.tag.load(Ordering::Relaxed) == STALE {
                shared.tag.store(0, Ordering::Relaxed);
            }
        })
    };

    let _ = shared.tag.swap(WINDOW, Ordering::Relaxed);
    collector.join().unwrap();

    if shared.tag.load(Ordering::Relaxed) != WINDOW {
        panic!("the stale clear buried a fresh tag");
    }
}

#[test]
fn checkpoint_model_the_swap_never_buries_a_fresh_tag() {
    loom::model(|| stale_clear_against_a_fresh_tag(true));
}

#[test]
#[should_panic(expected = "the stale clear buried a fresh tag")]
fn checkpoint_model_a_load_and_a_store_bury_it() {
    loom::model(|| stale_clear_against_a_fresh_tag(false));
}

/// The mutator opens the next window and consents; the collector, granted,
/// reads the window it tests the tags for. `consent` is the ordering of the
/// consent's swap.
fn the_window_the_grant_reads(consent: Ordering) {
    let shared = shared(0);
    shared.token.store(REQUESTED, Ordering::Relaxed);

    let collector = {
        let shared = shared.clone();
        thread::spawn(move || {
            loop {
                if shared.token.load(Ordering::Acquire) == COLLECTOR {
                    return shared.window.load(Ordering::Relaxed);
                }
                thread::yield_now();
            }
        })
    };

    shared.window.store(WINDOW, Ordering::Relaxed);
    let _ = shared
        .token
        .compare_exchange(REQUESTED, COLLECTOR, consent, Ordering::Relaxed);

    if collector.join().unwrap() != WINDOW {
        panic!("the collector tested for a window the mutator had left");
    }
}

#[test]
fn checkpoint_model_the_grant_reads_the_window_the_consent_opened() {
    loom::model(|| the_window_the_grant_reads(Ordering::Release));
}

#[test]
#[should_panic(expected = "the collector tested for a window the mutator had left")]
fn checkpoint_model_a_relaxed_consent_hides_the_window() {
    loom::model(|| the_window_the_grant_reads(Ordering::Relaxed));
}
