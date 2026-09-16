//! A `loom` model of the free path's reading of the token against a
//! collector's take, and of nothing else: one token word, one word for
//! the owner's last store before a free, one owner and one taker.
//!
//! It models a **copy of the protocol** rather than the token's code. The
//! token cannot run under `--cfg loom` — it stands in a record reached
//! through a thread-local — so what is checked here is the ordering
//! argument, on the assumption that the code implements it. Keep the two
//! in step by hand: the taker below is [`TraceToken::try_take`](super::TraceToken::try_take)
//! and the owner is [`TraceToken::is_held`](super::TraceToken::is_held)
//! read on the free path (`crate::cycle::deferred_slot_reuse`, "A foreign
//! holder of the token").
//!
//! The owner's word stands for the storage head an array republishes
//! before it frees the old storage: the owner stores it, reads the token
//! free, and frees. A taker whose take lands after that reading traces
//! the owner's graph, and the graph it must see is the one with the new
//! head; a stale head names the storage the owner is freeing, and the
//! trace strides memory the owner reuses meanwhile.
//!
//! # What it demonstrated
//!
//! With the owner's reading an acquire load and the take an acquire
//! compare-and-swap — the form the deferral was first written in, found by
//! the Code Reviewer of 2026-09-16 — loom finds the execution at once: the
//! owner reads the token free, the taker takes, and the taker reads the
//! old head. This is the store-buffering shape, and on x86 it is the
//! owner's store buffer: nothing between the head store and the token load
//! drains it. A `SeqCst` fence on each side, the owner's before its load
//! and the taker's after its take, closes it; either fence alone does not,
//! and the three defective configurations stay pinned below as
//! `should_panic`.
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
use loom::sync::atomic::{AtomicBool, AtomicUsize, Ordering, fence};
use loom::thread;

struct Shared {
    held: AtomicBool,
    head: AtomicUsize,
}

/// One execution of owner against taker.
///
/// `owner_fenced` selects `fence(SeqCst)` between the owner's head store
/// and its token load; `taker_fenced` selects `fence(SeqCst)` between the
/// taker's compare-and-swap and its head load. Both false is the protocol
/// as first written.
fn execution(owner_fenced: bool, taker_fenced: bool) {
    let shared = Arc::new(Shared {
        held: AtomicBool::new(false),
        head: AtomicUsize::new(0),
    });

    let taker = {
        let shared = shared.clone();
        thread::spawn(move || {
            let took = shared
                .held
                .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
                .is_ok();
            if !took {
                return None;
            }

            if taker_fenced {
                fence(Ordering::SeqCst);
            }

            Some(shared.head.load(Ordering::Acquire))
        })
    };

    // The owner republishes the head, then asks whether a trace holds the
    // token before it frees the old storage.
    shared.head.store(1, Ordering::Release);
    if owner_fenced {
        fence(Ordering::SeqCst);
    }

    let freed = !shared.held.load(Ordering::Acquire);

    let traced = taker.join().unwrap();
    if freed && traced == Some(0) {
        panic!("a trace read the storage the owner freed");
    }
}

#[test]
fn free_path_two_fences_order_the_free_against_the_take() {
    loom::model(|| execution(true, true));
}

#[test]
#[should_panic(expected = "a trace read the storage the owner freed")]
fn free_path_an_acquire_load_orders_nothing_before_it() {
    loom::model(|| execution(false, false));
}

#[test]
#[should_panic(expected = "a trace read the storage the owner freed")]
fn free_path_the_owners_fence_alone_is_not_enough() {
    loom::model(|| execution(true, false));
}

#[test]
#[should_panic(expected = "a trace read the storage the owner freed")]
fn free_path_the_takers_fence_alone_is_not_enough() {
    loom::model(|| execution(false, true));
}
