//! A `loom` model of the ring's read-back bracket, and of nothing else:
//! one slot, two payload words, a cursor, one writer and one reader.
//!
//! It models a **copy of the protocol** rather than the journal's code.
//! The ring cannot run under `--cfg loom` — loom replaces every atomic,
//! cell and thread with its own types, while the ring is allocated,
//! reached through a raw pointer and found through a thread-local — so
//! what is checked here is the ordering argument, on the assumption that
//! the code implements it. Keep the two in step by hand: the writer below
//! is [`Ring::write`](super::Ring::write) and the reader is
//! [`Ring::read_at`](super::Ring::read_at).
//!
//! The two payload words stand for the five a reader copies out of a
//! record. The writer sets both to the same value, so a reading that
//! yields two different ones is a reading of two different records — the
//! torn record the lap check exists to refuse.
//!
//! A capacity of two is what makes the lap reachable in three writes: the
//! reader asks about position 0, the writer's third record lands in the
//! same slot, and the check `now - at >= CAPACITY` is what has to catch
//! it. The cursor's release store stays in every configuration below —
//! it is the publication half, and what the model varies is the bracket
//! that decides whether a *lapped* record can pass.
//!
//! # What it demonstrated
//!
//! Run against the bracket as it was first written, loom finds the
//! accepting execution in milliseconds, and finds it with either fence
//! alone as well: three of the four combinations fail and only the
//! repaired one holds. The reader's fence alone is not enough, which is
//! the finding that cost the writer a fence it did not have — an acquire
//! fence synchronizes with a release operation, and what the reader's
//! relaxed loads read from are relaxed stores. That is the whole value of
//! the model — a green run proves little, because loom's own README
//! records that it does not explore load buffering, while a red one
//! exhibits the execution.
//!
//! # Running it
//!
//! ```text
//! RUSTFLAGS="--cfg loom" cargo test --lib ring_bracket
//! ```
//!
//! Not part of the commit gate (`dev/WORKFLOW.md`): the model has no
//! dependency on the crate, so it can only break when someone edits it.

use loom::sync::Arc;
use loom::sync::atomic::{AtomicUsize, Ordering, fence};
use loom::thread;

/// The ring's `CAPACITY`, lowered to the smallest value that has a lap in
/// it. Loom explores every interleaving of every write, and three are
/// enough to reach the slot twice.
const CAPACITY: usize = 2;

struct Ring {
    cursor: AtomicUsize,
    a: AtomicUsize,
    b: AtomicUsize,
}

/// One execution of writer against reader.
///
/// `writer_fenced` selects a release fence before the payload stores;
/// `reader_fenced` selects `fence(Acquire)` before a relaxed closing load
/// over an acquire load. Both false is the bracket as it was first
/// written — a release store closed by an acquire load, each ordering the
/// side the other needs.
///
/// The two fences pair with each other, and that is why neither works
/// alone: what the reader's relaxed loads read from are the writer's
/// relaxed payload stores, which no acquire fence can synchronize with by
/// themselves. A release fence sequenced before them is what the acquire
/// fence has to find.
fn execution(writer_fenced: bool, reader_fenced: bool) {
    let ring = Arc::new(Ring {
        cursor: AtomicUsize::new(0),
        a: AtomicUsize::new(0),
        b: AtomicUsize::new(0),
    });

    let writer = {
        let ring = ring.clone();
        thread::spawn(move || {
            // Position 0, into the slot the reader asks about.
            if writer_fenced {
                fence(Ordering::Release);
            }
            ring.a.store(1, Ordering::Relaxed);
            ring.b.store(1, Ordering::Relaxed);
            ring.cursor.store(1, Ordering::Release);

            // Position 1, into the slot this model does not carry.
            ring.cursor.store(2, Ordering::Release);

            // Position 2 laps the reader: the same slot, a second record.
            if writer_fenced {
                fence(Ordering::Release);
            }
            ring.a.store(2, Ordering::Relaxed);
            ring.b.store(2, Ordering::Relaxed);
            ring.cursor.store(3, Ordering::Release);
        })
    };

    // The mark. A reader asks about a position only after a cursor
    // reading put that position below it, so an execution in which
    // nothing has been published has no window to read.
    if ring.cursor.load(Ordering::Acquire) == 0 {
        writer.join().unwrap();
        return;
    }

    let a = ring.a.load(Ordering::Relaxed);
    let b = ring.b.load(Ordering::Relaxed);

    let now = if reader_fenced {
        fence(Ordering::Acquire);
        ring.cursor.load(Ordering::Relaxed)
    } else {
        ring.cursor.load(Ordering::Acquire)
    };

    if now < CAPACITY {
        assert_eq!(a, b, "a torn record passed the lap check");
    }

    writer.join().unwrap();
}

#[test]
fn ring_bracket_two_fences_refuse_the_lapped_record() {
    loom::model(|| execution(true, true));
}

#[test]
#[should_panic(expected = "a torn record passed the lap check")]
fn ring_bracket_a_store_and_a_load_order_the_wrong_sides() {
    loom::model(|| execution(false, false));
}

#[test]
#[should_panic(expected = "a torn record passed the lap check")]
fn ring_bracket_the_writers_fence_alone_is_not_enough() {
    loom::model(|| execution(true, false));
}

#[test]
#[should_panic(expected = "a torn record passed the lap check")]
fn ring_bracket_the_readers_fence_alone_is_not_enough() {
    loom::model(|| execution(false, true));
}
