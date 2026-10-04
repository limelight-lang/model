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
//! - **A blocking stretch stands for a checkpoint, and nothing buries it.**
//!   The stretch's entry carries the tags before it to an ask that takes it;
//!   a withdrawal leaves a stretch standing; a late poll answer never lands
//!   after a withdrawal; leaving a stretch against an ask leaves the byte
//!   holding nothing (`dev/design/recycler-over-counts.md`, §5b).
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

// The blocking stretch (`dev/design/recycler-over-counts.md`, §5b): two more
// values of the byte, and every transition one compare-and-swap from the
// value read, as `TraceToken::{ask_for_the_checkpoint,
// withdraw_the_checkpoint, enter_blocking, leave_blocking,
// reach_the_checkpoint}` make them.

const BLOCKING: u8 = 3;
const BLOCKING_ASKED: u8 = 4;

// The loops below start from a guessed value where the code starts from a
// load: a compare-and-swap from a wrong guess fails and reads the value, which
// is the load's work, so the two are one protocol. Loom 0.7 is incomplete on
// the load's form — with a load ahead of the withdrawal's compare-and-swap it
// never schedules a late answer between them, an outcome the C11 model allows
// (2026-10-04, a two-thread case in the scratchpad) — and complete on this
// one, which exhibits it.

/// The ask: true where a blocking stretch answers it at once.
fn ask(shared: &Shared) -> bool {
    let mut seen = NONE;
    loop {
        let (asked, blocking) = if seen == BLOCKING {
            (BLOCKING_ASKED, true)
        } else {
            (ASKED, false)
        };
        match shared
            .checkpoint
            .compare_exchange(seen, asked, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) => return blocking,
            Err(now) => seen = now,
        }
        thread::yield_now();
    }
}

/// The withdrawal; `store` is the defective form, a store of nothing.
fn withdraw(shared: &Shared, store: bool) {
    if store {
        shared.checkpoint.store(NONE, Ordering::Relaxed);
        return;
    }
    let mut seen = ASKED;
    loop {
        let withdrawn = match seen {
            ASKED | REACHED => NONE,
            BLOCKING_ASKED => BLOCKING,
            _ => return,
        };
        match shared.checkpoint.compare_exchange(
            seen,
            withdrawn,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => return,
            Err(now) => seen = now,
        }
        thread::yield_now();
    }
}

/// The wait: whether the checkpoint was reached within the bound.
fn wait(shared: &Shared) -> bool {
    for _ in 0..READINGS {
        if matches!(
            shared.checkpoint.load(Ordering::Acquire),
            REACHED | BLOCKING | BLOCKING_ASKED
        ) {
            return true;
        }
        thread::yield_now();
    }
    false
}

/// The poll's answer; `store` is the defective form, a load and a store —
/// the store written as a swap, which loom orders against the withdrawal's
/// compare-and-swap where a plain store it leaves unordered (`dev/WORKFLOW.md`,
/// "Loom").
fn answer(shared: &Shared, store: bool) {
    if shared.checkpoint.load(Ordering::Acquire) == ASKED {
        if store {
            let _ = shared.checkpoint.swap(REACHED, Ordering::Release);
        } else {
            let _ = shared.checkpoint.compare_exchange(
                ASKED,
                REACHED,
                Ordering::Release,
                Ordering::Acquire,
            );
        }
    }
}

/// Entering the stretch, `entry` the ordering of its swap.
fn enter(shared: &Shared, entry: Ordering) {
    let _ = shared.checkpoint.swap(BLOCKING, entry);
}

/// Leaving it.
fn leave(shared: &Shared) {
    let mut seen = BLOCKING;
    while seen == BLOCKING || seen == BLOCKING_ASKED {
        match shared
            .checkpoint
            .compare_exchange(seen, NONE, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) => return,
            Err(now) => seen = now,
        }
    }
}

/// A stretch against an ask: the mutator tags a member, then blocks; an ask
/// that takes the stretch as its checkpoint must read the tag.
fn a_stretch_against_an_ask(entry: Ordering) {
    let shared = shared(0);

    let collector = {
        let shared = shared.clone();
        thread::spawn(move || {
            let reached = ask(&shared) || wait(&shared);
            let tag = shared.tag.load(Ordering::Relaxed);
            withdraw(&shared, false);
            reached.then_some(tag)
        })
    };

    let _ = shared.tag.swap(WINDOW, Ordering::Relaxed);
    enter(&shared, entry);

    if collector.join().unwrap() == Some(0) {
        panic!("a stretch answered an ask before the tag it follows");
    }
}

#[test]
fn checkpoint_model_a_stretch_carries_the_tags_before_it() {
    loom::model(|| a_stretch_against_an_ask(Ordering::AcqRel));
}

#[test]
#[should_panic(expected = "a stretch answered an ask before the tag it follows")]
fn checkpoint_model_a_relaxed_stretch_carries_nothing() {
    loom::model(|| a_stretch_against_an_ask(Ordering::Relaxed));
}

/// A withdrawal against a stretch: the stretch the mutator stands in must
/// outlive the collector's withdrawal of its ask.
fn a_withdrawal_against_a_stretch(store: bool) {
    let shared = shared(0);

    let collector = {
        let shared = shared.clone();
        thread::spawn(move || {
            let _ = ask(&shared) || wait(&shared);
            withdraw(&shared, store);
        })
    };

    enter(&shared, Ordering::AcqRel);
    collector.join().unwrap();

    if shared.checkpoint.load(Ordering::Relaxed) != BLOCKING {
        panic!("the withdrawal buried a stretch");
    }
}

#[test]
fn checkpoint_model_a_withdrawal_leaves_a_stretch_standing() {
    loom::model(|| a_withdrawal_against_a_stretch(false));
}

#[test]
#[should_panic(expected = "the withdrawal buried a stretch")]
fn checkpoint_model_a_withdrawal_by_store_buries_it() {
    loom::model(|| a_withdrawal_against_a_stretch(true));
}

/// A late answer against a withdrawal: an answer to an ask the collector
/// withdrew must not stand after the withdrawal.
fn a_late_answer_against_a_withdrawal(store: bool) {
    let shared = shared(0);

    let collector = {
        let shared = shared.clone();
        thread::spawn(move || {
            let _ = ask(&shared);
            withdraw(&shared, false);
        })
    };

    answer(&shared, store);
    collector.join().unwrap();

    if shared.checkpoint.load(Ordering::Relaxed) != NONE {
        panic!("an answer stood after its ask was withdrawn");
    }
}

#[test]
fn checkpoint_model_a_late_answer_never_lands() {
    loom::model(|| a_late_answer_against_a_withdrawal(false));
}

#[test]
#[should_panic(expected = "an answer stood after its ask was withdrawn")]
fn checkpoint_model_a_load_and_a_store_answer_late() {
    loom::model(|| a_late_answer_against_a_withdrawal(true));
}

/// Leaving a stretch against an ask: whatever the order, the byte ends
/// holding nothing — no stretch the running mutator would answer with, and no
/// ask a later grant would find.
fn leaving_against_an_ask() {
    let shared = shared(0);

    let collector = {
        let shared = shared.clone();
        thread::spawn(move || {
            let _ = ask(&shared) || wait(&shared);
            withdraw(&shared, false);
        })
    };

    enter(&shared, Ordering::AcqRel);
    leave(&shared);
    collector.join().unwrap();

    if shared.checkpoint.load(Ordering::Relaxed) != NONE {
        panic!("a stretch or an ask outlived both");
    }
}

#[test]
fn checkpoint_model_leaving_against_an_ask_leaves_nothing() {
    loom::model(leaving_against_an_ask);
}
