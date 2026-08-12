//! A `loom` model of [`StorageHead`](super::head::StorageHead)'s version
//! bracket, and of nothing else: a counter, two data words, one mover and
//! one reader. What it demonstrated is `dev/DECISIONS.md`, "the table's
//! version bracket orders both sides with fences".
//!
//! It models a **copy of the protocol** rather than the crate's code. The
//! head cannot run under `--cfg loom` — loom replaces every atomic, cell
//! and thread with its own types, while the code around the head
//! allocates, holds raw pointers and reaches thread-locals — so what is
//! checked here is the ordering argument, on the assumption that the code
//! implements it. Keep the two in step by hand: the mover below is
//! `StorageHead::begin_move` and `end_move`, the reader is
//! `StorageHead::coherent`.
//!
//! The two data words stand for the four a walker reads together — the
//! storage chunk, the slot count, the element count and the strategy tag.
//! Their number does not enter the argument: they are written by the
//! mover in one window, so a reading that yields values from two windows
//! is a reading of two different states, the incoherent edge that no
//! later phase repairs. The movers the window covers are growth,
//! compaction and the migration between representations, and the last of
//! those is why the counter sits above the representations rather than
//! inside one.
//!
//! # Running it
//!
//! ```text
//! RUSTFLAGS="--cfg loom" cargo test --lib version_bracket
//! ```
//!
//! Not part of the commit gate (`dev/WORKFLOW.md`, "Loom", which also
//! records the limits that decide what a run here proves): the model has
//! no dependency on the crate, so it can only break when someone edits it.

use loom::sync::Arc;
use loom::sync::atomic::{AtomicUsize, Ordering, fence};
use loom::thread;

/// The crate's `COHERENT_READ_ATTEMPTS`, lowered. Loom explores every
/// interleaving of every attempt, and the window this asks about is
/// reachable within two.
const ATTEMPTS: usize = 2;

struct Bracket {
    version: AtomicUsize,
    a: AtomicUsize,
    b: AtomicUsize,
}

/// One execution of mover against reader.
///
/// `writer_fenced` selects a plain store followed by `fence(Release)` over
/// a release store; `reader_fenced` selects `fence(Acquire)` before a plain
/// closing load over an acquire load. Both false is the unfenced bracket —
/// a release store closed by an acquire load, each ordering the side the
/// other needs.
fn execution(writer_fenced: bool, reader_fenced: bool) {
    let table = Arc::new(Bracket {
        version: AtomicUsize::new(0),
        a: AtomicUsize::new(0),
        b: AtomicUsize::new(0),
    });

    let mover = {
        let table = table.clone();
        thread::spawn(move || {
            let v = table.version.load(Ordering::Relaxed);
            if writer_fenced {
                table.version.store(v + 1, Ordering::Relaxed);
                fence(Ordering::Release);
            } else {
                table.version.store(v + 1, Ordering::Release);
            }

            table.a.store(1, Ordering::Relaxed);
            table.b.store(1, Ordering::Relaxed);

            let v = table.version.load(Ordering::Relaxed);
            table.version.store(v + 1, Ordering::Release);
        })
    };

    for _ in 0..ATTEMPTS {
        let before = table.version.load(Ordering::Acquire);
        if before % 2 != 0 {
            continue;
        }

        let a = table.a.load(Ordering::Relaxed);
        let b = table.b.load(Ordering::Relaxed);

        let after = if reader_fenced {
            fence(Ordering::Acquire);
            table.version.load(Ordering::Relaxed)
        } else {
            table.version.load(Ordering::Acquire)
        };

        if after != before {
            continue;
        }

        assert_eq!(a, b, "an incoherent reading was accepted as coherent");
        break;
    }

    mover.join().unwrap();
}

#[test]
fn version_bracket_two_fences_hand_out_one_state() {
    loom::model(|| execution(true, true));
}

#[test]
#[should_panic(expected = "an incoherent reading was accepted")]
fn version_bracket_a_store_and_a_load_order_the_wrong_sides() {
    loom::model(|| execution(false, false));
}

#[test]
#[should_panic(expected = "an incoherent reading was accepted")]
fn version_bracket_the_movers_fence_alone_is_not_enough() {
    loom::model(|| execution(true, false));
}

#[test]
#[should_panic(expected = "an incoherent reading was accepted")]
fn version_bracket_the_readers_fence_alone_is_not_enough() {
    loom::model(|| execution(false, true));
}
