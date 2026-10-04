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
//! - **An answer to an ask the collector withdrew never answers the next
//!   grant's ask.** The answer is a load and a store, not one swap, so the
//!   mutator can read an ask, the collector withdraw it and release, and the
//!   store land late. One ask stands per grant — one Δ-test a batch, one
//!   batch a grant — and the next ask follows the mutator's consent, a
//!   release the mutator makes after that store in program order; so the
//!   collector that asks again reads only an answer made after the consent,
//!   which carries every tag stored before it.
//! - **The stale clear never buries a fresh tag.** The clear is a one-byte
//!   compare-and-swap from the stale number to 0; a load and a store in its
//!   place lose a tag the mutator wrote between them.
//! - **The window number the collector tests for is the one the mutator
//!   tags with.** The mutator advances its window before its consent, a
//!   release; the collector's grant is an acquire.
//!
//! # What it does not check
//!
//! The other half of the proof: "its count is the value read" needs the
//! trace's reads to come before T, so that no count the trace read is one
//! the mutator wrote after its checkpoint. The chain is the trace, then the
//! ask's release store, which the poll's acquire load of the ask reads, then
//! every write the mutator makes after its answer. A trace whose relaxed
//! read returns a write made after the answer, while the mutator's load read
//! the ask, is load buffering, which loom does not model (its README); so no
//! case here fails when the ask or the poll's load is relaxed, and the
//! poll's acquire load is the one ordering this file cannot defend. It is
//! defended by hand, at [`TraceToken::reach_the_checkpoint`](super::TraceToken::reach_the_checkpoint).
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

/// An ask the mutator reads, withdrawn and released before its answer lands,
/// then a new grant and its ask: `consent` is the ordering of the consent's
/// swap. The collector that reads an answer to its second ask must read the
/// tag the mutator stored after consenting to that grant.
fn an_answer_across_two_grants(consent: Ordering) {
    let shared = shared(0);
    shared.token.store(COLLECTOR, Ordering::Relaxed);

    let collector = {
        let shared = shared.clone();
        thread::spawn(move || {
            let _ = ask_and_wait(&shared);
            shared.token.store(REQUESTED, Ordering::Release);
            while shared.token.load(Ordering::Acquire) != COLLECTOR {
                thread::yield_now();
            }
            if ask_and_wait(&shared) {
                Some(shared.tag.load(Ordering::Relaxed))
            } else {
                None
            }
        })
    };

    reach(&shared, Ordering::Release);
    while shared
        .token
        .compare_exchange(REQUESTED, COLLECTOR, consent, Ordering::Relaxed)
        .is_err()
    {
        thread::yield_now();
    }
    shared.tag.store(WINDOW, Ordering::Relaxed);
    reach(&shared, Ordering::Release);

    if collector.join().unwrap() == Some(0) {
        panic!("an answer from before the consent answered the next grant");
    }
}

#[test]
fn checkpoint_model_an_answer_never_crosses_a_consent() {
    loom::model(|| an_answer_across_two_grants(Ordering::Release));
}

#[test]
#[should_panic(expected = "an answer from before the consent answered the next grant")]
fn checkpoint_model_a_relaxed_consent_lets_it_cross() {
    loom::model(|| an_answer_across_two_grants(Ordering::Relaxed));
}

/// The collector clears a stale tag while the mutator — reaching a weakly
/// held member after T — writes a fresh one. `swap` selects the protocol's
/// compare-and-swap; false is a load and a store.
///
/// The mutator's tag is a plain relaxed byte store, written here as a swap:
/// loom 0.7 can leave a plain store and a racing read-modify-write unordered
/// in the modification order, so that a load after both returns the
/// compare-and-swap's 0 over the store's 7 — an outcome the C11 model
/// forbids, whichever of the two comes first. A swap in the store's place
/// takes the same position in the modification order and is ordered.
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
