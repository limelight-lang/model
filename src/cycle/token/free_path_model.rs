//! A `loom` model of the token byte between one mutator and one collector,
//! and of nothing else: the byte, one word for the mutator's last store
//! before a free, one word the collector posts into P before its release.
//!
//! It models a **copy of the protocol** rather than the token's code. The
//! token cannot run under `--cfg loom` — it stands in a record reached
//! through a thread-local — so what is checked here is the ordering
//! argument, on the assumption that the code implements it. Keep the two
//! in step by hand: the collector's request is
//! [`TraceToken::request`](super::TraceToken::request), its withdrawal
//! [`TraceToken::withdraw`](super::TraceToken::withdraw), its release
//! [`TraceToken::release_claim`](super::TraceToken::release_claim); the
//! mutator's reading and consent are
//! [`read_and_act_on_this_thread`](super::read_and_act_on_this_thread) and
//! [`TraceToken::consent`](super::TraceToken::consent), and its take
//! [`TraceToken::take_unless`](super::TraceToken::take_unless). The fenced
//! claimant of the first executions is the form the code had before the
//! consent, kept as the exhibit of what the consent replaces.
//!
//! The mutator's word stands for the storage head an array republishes
//! before it frees the old storage: the mutator stores it, reads the token
//! free, and frees. A claimant whose claim lands after that reading traces
//! the mutator's candidates and the entities the trace reaches, and the
//! array it must see is the one with the new head; a stale head names the storage the mutator is freeing, and the
//! trace strides memory the mutator reuses meanwhile.
//!
//! # What it demonstrated
//!
//! With the mutator's reading an acquire load and the claim an acquire
//! compare-and-swap — the form the deferral was first written in, found by
//! the Code Reviewer of 2026-09-16 — loom finds the execution at once: the
//! mutator reads the token free, the claimant claims, and the claimant reads
//! the old head. This is the store-buffering shape, and on x86 it is the
//! mutator's store buffer: nothing between the head store and the token load
//! drains it. A `SeqCst` fence on each side, the mutator's before its load
//! and the claimant's after its claim, closes it; either fence alone does
//! not, and the three defective configurations stay pinned below as
//! `should_panic`. The pair is the price of a claim the collector makes
//! alone; a claim the mutator consents to with a release swap of its own
//! needs neither (`rfc/dev/design/trace-token-handshake.md`, E3).
//!
//! The release to `POSTED` is the release to `FREE` under another name: the
//! collector's post into P precedes its release store, the mutator's take
//! from `POSTED` is an acquire swap, and the mutator reads the post.
//!
//! The consent is what replaces the fences: the collector requests, the
//! mutator's reading swaps `REQUESTED → COLLECTOR` with a release, and the
//! collector's acquire load of the grant orders every store the mutator
//! made before its reading ahead of the trace; a relaxed consent reads the
//! old head again. A withdrawal that races the consent reads the grant
//! back and serves it, so a request is never both withdrawn and granted.
//!
//! # Running it
//!
//! ```text
//! RUSTFLAGS="--cfg loom" cargo test --lib free_path
//! ```
//!
//! Not part of the commit gate (`dev/WORKFLOW.md`): the model has no
//! dependency on the crate, so it can only break when someone edits it.

use loom::sync::Arc;
use loom::sync::atomic::{AtomicU8, AtomicUsize, Ordering, fence};
use loom::thread;

const FREE: u8 = 0;
const MUTATOR: u8 = 1;
const REQUESTED: u8 = 2;
const COLLECTOR: u8 = 3;
const POSTED: u8 = 4;

struct Shared {
    word: AtomicU8,
    head: AtomicUsize,
    posted: AtomicUsize,
}

fn shared() -> Arc<Shared> {
    Arc::new(Shared {
        word: AtomicU8::new(FREE),
        head: AtomicUsize::new(0),
        posted: AtomicUsize::new(0),
    })
}

/// One execution of mutator against claimant.
///
/// `mutator_fenced` selects `fence(SeqCst)` between the mutator's head store
/// and its token load; `claimant_fenced` selects `fence(SeqCst)` between the
/// claimant's compare-and-swap and its head load. Both false is the protocol
/// as first written.
fn execution(mutator_fenced: bool, claimant_fenced: bool) {
    let shared = shared();

    let claimant = {
        let shared = shared.clone();
        thread::spawn(move || {
            let claimed = shared
                .word
                .compare_exchange(FREE, COLLECTOR, Ordering::Acquire, Ordering::Relaxed)
                .is_ok();
            if !claimed {
                return None;
            }

            if claimant_fenced {
                fence(Ordering::SeqCst);
            }

            Some(shared.head.load(Ordering::Acquire))
        })
    };

    // The mutator republishes the head, then asks whether a trace holds the
    // token before it frees the old storage.
    shared.head.store(1, Ordering::Release);
    if mutator_fenced {
        fence(Ordering::SeqCst);
    }

    let freed = shared.word.load(Ordering::Acquire) != COLLECTOR;

    let traced = claimant.join().unwrap();
    if freed && traced == Some(0) {
        panic!("a trace read the storage the mutator freed");
    }
}

#[test]
fn free_path_two_fences_order_the_free_against_the_take() {
    loom::model(|| execution(true, true));
}

#[test]
#[should_panic(expected = "a trace read the storage the mutator freed")]
fn free_path_an_acquire_load_orders_nothing_before_it() {
    loom::model(|| execution(false, false));
}

#[test]
#[should_panic(expected = "a trace read the storage the mutator freed")]
fn free_path_the_mutators_fence_alone_is_not_enough() {
    loom::model(|| execution(true, false));
}

#[test]
#[should_panic(expected = "a trace read the storage the mutator freed")]
fn free_path_the_takers_fence_alone_is_not_enough() {
    loom::model(|| execution(false, true));
}

/// The collector posts a word and releases `POSTED`; the mutator, reading
/// `POSTED`, takes `MUTATOR` and reads the word. `take_ordering` is the
/// success ordering of the mutator's swap: the protocol's `Acquire` reads
/// the post, and a `Relaxed` swap can read the word from before it.
fn posted_execution(take_ordering: Ordering) {
    let shared = shared();

    let collector = {
        let shared = shared.clone();
        thread::spawn(move || {
            assert!(
                shared
                    .word
                    .compare_exchange(FREE, COLLECTOR, Ordering::Acquire, Ordering::Relaxed)
                    .is_ok()
            );
            shared.posted.store(1, Ordering::Relaxed);
            shared.word.store(POSTED, Ordering::Release);
        })
    };

    loop {
        let seen = shared.word.load(Ordering::Relaxed);
        if seen != POSTED {
            thread::yield_now();
            continue;
        }

        if shared
            .word
            .compare_exchange(seen, MUTATOR, take_ordering, Ordering::Relaxed)
            .is_ok()
        {
            break;
        }
    }

    let read = shared.posted.load(Ordering::Relaxed);
    collector.join().unwrap();
    if read == 0 {
        panic!("the collection read P from before the post");
    }
}

#[test]
fn a_take_from_posted_reads_the_batch_the_release_published() {
    loom::model(|| posted_execution(Ordering::Acquire));
}

#[test]
#[should_panic(expected = "the collection read P from before the post")]
fn a_relaxed_take_from_posted_reads_nothing_of_the_batch() {
    loom::model(|| posted_execution(Ordering::Relaxed));
}

/// The collector requests; the mutator republishes the head, reads the
/// request and consents with `consent_ordering`; the collector reads the
/// grant with acquire and then the head. The protocol's `Release` consent
/// reads the new head; a `Relaxed` one can read the old.
fn consent_execution(consent_ordering: Ordering) {
    let shared = shared();

    let collector = {
        let shared = shared.clone();
        thread::spawn(move || {
            if shared
                .word
                .compare_exchange(FREE, REQUESTED, Ordering::Acquire, Ordering::Relaxed)
                .is_err()
            {
                return None;
            }

            loop {
                match shared.word.load(Ordering::Acquire) {
                    COLLECTOR => return Some(shared.head.load(Ordering::Relaxed)),
                    MUTATOR => return None,
                    _ => thread::yield_now(),
                }
            }
        })
    };

    shared.head.store(1, Ordering::Relaxed);
    // The mutator's reading: a request is consented to, anything else is
    // acted on as it stands; the take that follows refuses a request it
    // did not consent to.
    match shared.word.load(Ordering::Acquire) {
        REQUESTED => {
            let _ = shared.word.compare_exchange(
                REQUESTED,
                COLLECTOR,
                consent_ordering,
                Ordering::Acquire,
            );
        }
        FREE => {
            let _ =
                shared
                    .word
                    .compare_exchange(FREE, MUTATOR, Ordering::Acquire, Ordering::Acquire);
            let _ = shared.word.compare_exchange(
                REQUESTED,
                MUTATOR,
                Ordering::Acquire,
                Ordering::Acquire,
            );
        }
        _ => {}
    }

    if collector.join().unwrap() == Some(0) {
        panic!("a trace read the storage the mutator freed");
    }
}

#[test]
fn a_release_consent_orders_the_mutators_stores_before_the_trace() {
    loom::model(|| consent_execution(Ordering::Release));
}

#[test]
#[should_panic(expected = "a trace read the storage the mutator freed")]
fn a_relaxed_consent_orders_nothing_before_the_trace() {
    loom::model(|| consent_execution(Ordering::Relaxed));
}

/// The collector requests and withdraws while the mutator consents: the
/// withdrawal's read-back is the grant, which the collector serves and
/// releases, and the byte ends `FREE` with exactly one of the two having
/// held it. `failure_ordering` is the withdrawal's: the protocol's
/// `Acquire` reads the mutator's stores; a `Relaxed` one can read the old
/// head under the grant.
fn withdrawal_execution(failure_ordering: Ordering) {
    let shared = shared();

    let collector = {
        let shared = shared.clone();
        thread::spawn(move || {
            assert!(
                shared
                    .word
                    .compare_exchange(FREE, REQUESTED, Ordering::Acquire, Ordering::Relaxed)
                    .is_ok()
            );
            match shared
                .word
                .compare_exchange(REQUESTED, FREE, Ordering::Relaxed, failure_ordering)
            {
                Ok(_) => None,
                Err(COLLECTOR) => {
                    let head = shared.head.load(Ordering::Relaxed);
                    shared.word.store(FREE, Ordering::Release);
                    Some(head)
                }
                Err(_) => None,
            }
        })
    };

    shared.head.store(1, Ordering::Relaxed);
    if shared.word.load(Ordering::Acquire) == REQUESTED {
        let _ = shared.word.compare_exchange(
            REQUESTED,
            COLLECTOR,
            Ordering::Release,
            Ordering::Acquire,
        );
    }

    let traced = collector.join().unwrap();
    assert_eq!(
        shared.word.load(Ordering::Relaxed),
        FREE,
        "nobody holds the byte"
    );
    if traced == Some(0) {
        panic!("a trace read the storage the mutator freed");
    }
}

#[test]
fn a_withdrawal_that_reads_the_grant_back_serves_it_and_leaves_the_byte_free() {
    loom::model(|| withdrawal_execution(Ordering::Acquire));
}

#[test]
#[should_panic(expected = "a trace read the storage the mutator freed")]
fn a_relaxed_withdrawal_reads_nothing_of_the_mutator_under_the_grant() {
    loom::model(|| withdrawal_execution(Ordering::Relaxed));
}
