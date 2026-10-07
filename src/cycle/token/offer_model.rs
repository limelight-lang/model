//! A `loom` model of the offer between one mutator and one collector
//! (`dev/design/recycler-over-counts.md`, §5f): the token byte, the frame the
//! offer carries beside it, one member's count and its window tag in header
//! byte 7.
//!
//! It models a **copy of the protocol** rather than the code, as
//! `free_path_model` does and for the same reason: the token stands in a
//! record reached through a thread-local, and the tag is written through
//! thread-local state. Keep the two in step by hand: the offer is
//! [`TraceToken::offer`](super::TraceToken::offer), the take
//! [`TraceToken::take_the_offer`](super::TraceToken::take_the_offer), the
//! withdrawal [`TraceToken::withdraw_the_offer`](super::TraceToken::withdraw_the_offer)
//! and the release [`TraceToken::release_claim_to`](super::TraceToken::release_claim_to);
//! a count store is `crate::refcount::refcount_store` after
//! `crate::refcount::tag_with_the_window`; the stale clear is
//! `crate::refcount::clear_a_stale_window_tag`; the Δ-test that reads them is
//! `crate::cycle::delta_test::test_the_set_by_its_tags`, whose acquire fence
//! follows the trace's reads.
//!
//! # What it checks
//!
//! Each property with a twin that breaks it by the one ordering it rests on.
//!
//! - **A count the trace reads carries the tag stored before it.** Every
//!   count store tags first and stores with a release; the trace reads the
//!   count relaxed and the Δ-test reads the tag after an acquire fence. A
//!   relaxed count store lets the trace read the new count and the Δ-test
//!   the tag from before it, and prove a set the mutator touched.
//! - **The take reads the frame the offer carries.** The frame is stored
//!   before the offer's swap, a release; the take's swap is an acquire.
//! - **A frame withdrawn and offered again is read whole.** The collector
//!   reads the frame after its take, never before: a read before it can
//!   return the withdrawn offer's frame and take the next.
//! - **No clear of a number 255 frames old erases the same number fresh.**
//!   The last batch's clears come before its release of the byte; the next
//!   offer's swap is an acquire, and every tag of its frame comes after it.
//!   This one has no twin: see below.
//! - **The stale clear never buries a fresh tag.** The clear is a one-byte
//!   compare-and-swap from the stale number to 0; a load and a store in its
//!   place lose a tag the mutator wrote between them.
//!
//! # What it does not check
//!
//! That the offer's acquire is what orders the last batch's clears: loom 0.7
//! orders the read-modify-writes of one location as they execute, and the
//! offer lands only after the release executes, so an offer with a release
//! alone, which the C11 model lets a clear follow in the tag's modification
//! order, passes here too (2026-10-05). The case stands as a check that the
//! protocol's form passes, and the acquire is argued at
//! [`TraceToken::offer`](super::TraceToken::offer).
//!
//! That the trace's reads come before the mutator's writes after the offer
//! it read is no claim of the offer's: a write after the offer is tagged
//! with its frame whenever the trace reads it, and the tag decides. What the
//! fence cannot make visible is a write the trace did not read, which the
//! proof does not need.
//!
//! # Running it
//!
//! ```text
//! RUSTFLAGS="--cfg loom" cargo test --lib --features gc-window offer_model
//! ```
//!
//! Not part of the commit gate (`dev/WORKFLOW.md`), as `free_path_model`.

use loom::sync::Arc;
use loom::sync::atomic::{AtomicU8, Ordering, fence};
use loom::thread;

const FREE: u8 = 0;
const COLLECTOR: u8 = 3;
const OFFERED: u8 = 5;

/// The frame an offer carries, the one an offer made again carries, and a
/// number that predates both.
const FRAME: u8 = 7;
const NEXT_FRAME: u8 = 8;
const STALE: u8 = 6;

/// A member's count before and after the mutator's write.
const COUNT: u8 = 2;
const WRITTEN: u8 = 3;

struct Shared {
    token: AtomicU8,
    window: AtomicU8,
    tag: AtomicU8,
    count: AtomicU8,
}

fn shared(token: u8, tag: u8) -> Arc<Shared> {
    Arc::new(Shared {
        token: AtomicU8::new(token),
        window: AtomicU8::new(0),
        tag: AtomicU8::new(tag),
        count: AtomicU8::new(COUNT),
    })
}

/// The mutator's count write in the frame: the tag, then the count with
/// `store`. The trace reads the count; the Δ-test fences and reads the tag.
/// A count read written must come with the tag.
fn the_count_carries_its_tag(store: Ordering) {
    let shared = shared(COLLECTOR, 0);

    let collector = {
        let shared = shared.clone();
        thread::spawn(move || {
            let count = shared.count.load(Ordering::Relaxed);
            fence(Ordering::Acquire);
            (count, shared.tag.load(Ordering::Relaxed))
        })
    };

    shared.tag.store(FRAME, Ordering::Relaxed);
    shared.count.store(WRITTEN, store);
    let (count, tag) = collector.join().unwrap();
    if count == WRITTEN && tag != FRAME {
        panic!("the trace read a count without its tag");
    }
}

#[test]
fn offer_model_a_released_count_carries_its_tag() {
    loom::model(|| the_count_carries_its_tag(Ordering::Release));
}

#[test]
#[should_panic(expected = "the trace read a count without its tag")]
fn offer_model_a_relaxed_count_does_not() {
    loom::model(|| the_count_carries_its_tag(Ordering::Relaxed));
}

/// The mutator stores the frame and offers with `offer`; the collector that
/// takes reads the frame.
fn the_take_reads_the_frame(offer: Ordering) {
    let shared = shared(FREE, 0);

    let collector = {
        let shared = shared.clone();
        thread::spawn(move || {
            shared
                .token
                .compare_exchange(OFFERED, COLLECTOR, Ordering::Acquire, Ordering::Relaxed)
                .ok()
                .map(|_| shared.window.load(Ordering::Relaxed))
        })
    };

    shared.window.store(FRAME, Ordering::Relaxed);
    let _ = shared
        .token
        .compare_exchange(FREE, OFFERED, offer, Ordering::Relaxed);
    if let Some(window) = collector.join().unwrap() {
        if window != FRAME {
            panic!("the take read a frame the offer did not carry");
        }
    }
}

#[test]
fn offer_model_the_take_reads_the_frame_the_offer_carries() {
    loom::model(|| the_take_reads_the_frame(Ordering::AcqRel));
}

#[test]
#[should_panic(expected = "the take read a frame the offer did not carry")]
fn offer_model_a_relaxed_offer_hides_the_frame() {
    loom::model(|| the_take_reads_the_frame(Ordering::Relaxed));
}

/// The mutator offers, withdraws where no take came first, and offers the
/// next frame; the collector reads the frame `after_the_take` or before it.
/// A take of the second offer must read the second frame.
fn a_frame_offered_again(after_the_take: bool) {
    let shared = shared(FREE, 0);

    let collector = {
        let shared = shared.clone();
        thread::spawn(move || {
            let early = shared.window.load(Ordering::Relaxed);
            let taken = shared
                .token
                .compare_exchange(OFFERED, COLLECTOR, Ordering::Acquire, Ordering::Relaxed)
                .is_ok();
            let window = if after_the_take {
                shared.window.load(Ordering::Relaxed)
            } else {
                early
            };
            taken.then_some(window)
        })
    };

    shared.window.store(FRAME, Ordering::Relaxed);
    let _ = shared
        .token
        .compare_exchange(FREE, OFFERED, Ordering::AcqRel, Ordering::Relaxed);
    let withdrawn = shared
        .token
        .compare_exchange(OFFERED, FREE, Ordering::Relaxed, Ordering::Acquire)
        .is_ok();
    if withdrawn {
        shared.window.store(NEXT_FRAME, Ordering::Relaxed);
        let _ = shared
            .token
            .compare_exchange(FREE, OFFERED, Ordering::AcqRel, Ordering::Relaxed);
    }

    if let Some(window) = collector.join().unwrap() {
        let offered = if withdrawn { NEXT_FRAME } else { FRAME };
        if window != offered {
            panic!("the take read the frame of an offer it did not take");
        }
    }
}

#[test]
fn offer_model_a_frame_read_after_the_take_is_the_one_taken() {
    loom::model(|| a_frame_offered_again(true));
}

#[test]
#[should_panic(expected = "the take read the frame of an offer it did not take")]
fn offer_model_a_frame_read_before_the_take_can_be_withdrawn() {
    loom::model(|| a_frame_offered_again(false));
}

/// The collector's last batch clears a tag of the stale number and releases
/// the byte; the mutator offers with `offer` and, its offer landed, tags a
/// member with the same number fresh. The fresh tag must stand.
///
/// The fresh tag is a plain relaxed byte store, written here as a swap, for
/// the reason [`stale_clear_against_a_fresh_tag`] gives.
fn a_clear_before_the_release(offer: Ordering) {
    let shared = shared(COLLECTOR, STALE);

    let collector = {
        let shared = shared.clone();
        thread::spawn(move || {
            let _ = shared
                .tag
                .compare_exchange(STALE, 0, Ordering::Relaxed, Ordering::Relaxed);
            shared.token.store(FREE, Ordering::Release);
        })
    };

    let offered = shared
        .token
        .compare_exchange(FREE, OFFERED, offer, Ordering::Relaxed)
        .is_ok();
    if offered {
        let _ = shared.tag.swap(STALE, Ordering::Relaxed);
    }
    collector.join().unwrap();

    if offered && shared.tag.load(Ordering::Relaxed) != STALE {
        panic!("a clear of the last batch erased the new frame's tag");
    }
}

#[test]
fn offer_model_the_offers_acquire_orders_the_last_clears() {
    loom::model(|| a_clear_before_the_release(Ordering::AcqRel));
}

/// The collector clears a stale tag while the mutator — reaching a weakly
/// held member in the frame — writes a fresh one. `swap` selects the
/// protocol's compare-and-swap; false is a load and a store.
///
/// The mutator's tag is a plain relaxed byte store, written here as a swap:
/// loom 0.7 can leave a plain store and a racing read-modify-write unordered
/// in the modification order, so that a load after both returns the
/// compare-and-swap's 0 over the store's 7 — an outcome the C11 model
/// forbids, whichever of the two comes first. A swap in the store's place
/// takes the same position in the modification order and is ordered.
fn stale_clear_against_a_fresh_tag(swap: bool) {
    let shared = shared(COLLECTOR, STALE);

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

    let _ = shared.tag.swap(FRAME, Ordering::Relaxed);
    collector.join().unwrap();

    if shared.tag.load(Ordering::Relaxed) != FRAME {
        panic!("the stale clear buried a fresh tag");
    }
}

#[test]
fn offer_model_the_swap_never_buries_a_fresh_tag() {
    loom::model(|| stale_clear_against_a_fresh_tag(true));
}

#[test]
#[should_panic(expected = "the stale clear buried a fresh tag")]
fn offer_model_a_load_and_a_store_bury_it() {
    loom::model(|| stale_clear_against_a_fresh_tag(false));
}
