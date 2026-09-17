//! A `loom` model of the token byte between one mutator and one collector,
//! and of nothing else: the byte, one word for the mutator's last store
//! before a free, one word the collector posts into P before its release.
//!
//! It models a **copy of the protocol** rather than the token's code. The
//! token cannot run under `--cfg loom` — it stands in a record reached
//! through a thread-local — so what is checked here is the ordering
//! argument, on the assumption that the code implements it. Keep the two
//! in step by hand: the claimant below is
//! [`TraceToken::try_claim`](super::TraceToken::try_claim), its release
//! [`TraceToken::release_claim`](super::TraceToken::release_claim), and the
//! mutator is [`TraceToken::collector_is_tracing`](super::TraceToken::collector_is_tracing)
//! read on the free path (`crate::cycle::deferred_slot_reuse`, "A foreign
//! holder of the token") and [`TraceToken::take`](super::TraceToken::take)
//! at a collection's start.
//!
//! The mutator's word stands for the storage head an array republishes
//! before it frees the old storage: the mutator stores it, reads the token
//! free, and frees. A claimant whose claim lands after that reading traces
//! the mutator's graph, and the graph it must see is the one with the new
//! head; a stale head names the storage the mutator is freeing, and the
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
