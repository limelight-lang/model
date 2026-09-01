//! Where the escrow's storage comes from: one pool block per thread
//! life, drawn at init and given back at exit.
//!
//! What the ruling asks of this module is a coupling rather than a
//! structure (`rfc/dev/DECISIONS.md`, "the escrow's floor is
//! allocator-issued"): every registered thread has a floor, because a
//! thread whose floor was refused never started. So the cases here are
//! the draw, the refusal that ends a thread, the draw a thread the
//! runtime never registered makes for itself, and the return.
//!
//! **The two aborts have no test**, and neither has the escrow's own
//! overflow abort that predates them: nothing in this crate ends a
//! process and comes back to report it. What is tested instead is every
//! path that reaches them.

use super::*;

use crate::memory::block_pool::{BLOCK_KIND_GC_METADATA, BlockPool, FORCE_OOM, load_block_kind};
use crate::memory::gc_metadata::{GcBlockRole, stats};
use std::sync::atomic::Ordering;

/// The kind stamped on a block, which the collector reads for every
/// block in every carved region.
fn kind_of(block: *mut crate::memory::block_pool::BlockHeader) -> u32 {
    unsafe { load_block_kind(&raw const (*block).kind) }
}

/// The floor is one block, it is out of the pool while the thread holds
/// it, and it carries the stamp that keeps a trace out of it.
#[test]
fn the_floor_is_one_stamped_block_out_of_the_pool() {
    let _g = test_guard();
    reset();
    // The reserve empties both times so that `blocks_out` sees the
    // return: the floor goes back through `critical::give_back`, which
    // keeps it when the reserve is below capacity.
    crate::memory::critical::drain_for_test();

    let pool = BlockPool::global();
    assert!(!floor().is_null(), "the guard's `ll_thread_init` drew one");
    let with = pool.blocks_out();

    release_floor();
    crate::memory::critical::drain_for_test();
    assert_eq!(pool.blocks_out() + 1, with, "the floor is one block");

    assert!(take_floor(), "and the thread takes another");
    assert_eq!(pool.blocks_out(), with);
    assert_eq!(kind_of(floor()), BLOCK_KIND_GC_METADATA);
}

/// The drain empties the queue and leaves the floor alone: the floor
/// belongs to the thread's life, and a live thread stripped of it would
/// draw a second one at its next enrolment.
#[test]
fn a_drain_leaves_the_floor_where_it_is() {
    let _g = test_guard();
    let held = floor();
    assert!(!held.is_null(), "the guard's `ll_thread_init` drew one");
    reset();
    assert_eq!(floor(), held, "an empty queue's drain kept it");

    // Funded, so that the release below reaches the queue rather than the
    // escrow: what this test is about is a drain that has segments to
    // give back, which is the drain the floor has to survive.
    assert!(replenish(), "the cells start full");
    let mut header = candidate(2);
    assert!(unsafe { !release(&raw mut header) });
    assert_eq!(segment_count(), 1, "the entry is in a segment");

    drain();

    assert_eq!(segment_count(), 0, "which the drain gave back");
    assert_eq!(floor(), held, "and the thread still has its floor");

    reset();
}

/// A pool that refuses the floor refuses the thread, and the refusal is
/// the answer `ll_thread_init` gives its caller.
#[test]
fn a_refused_floor_is_a_thread_that_never_starts() {
    let _g = test_guard();
    let pool = BlockPool::global();
    let before = pool.blocks_out();
    let floors_before = stats().current_blocks(GcBlockRole::QueueFloor);

    // On a thread of its own, because the refusal is about a thread that
    // has no floor yet and this one has held its since the guard.
    let (started, floorless) = std::thread::spawn(|| {
        FORCE_OOM.store(true, Ordering::Relaxed);
        let started = crate::memory::heap::ll_thread_init();
        FORCE_OOM.store(false, Ordering::Relaxed);
        (started, floor().is_null())
    })
    .join()
    .unwrap();

    assert!(!started, "the thread reports that it did not start");
    assert!(floorless, "and holds nothing to be given back");
    assert_eq!(pool.blocks_out(), before);
    assert_eq!(
        stats().current_blocks(GcBlockRole::QueueFloor),
        floors_before
    );
}

/// A thread the runtime never registered draws its floor at its first
/// enrolment, and the exit guard that draw armed gives it back.
///
/// This is the population the ruling names: self-initialising allocation
/// and releaser-only FFI consumers, which reach entity work without ever
/// calling `ll_thread_init`. Its first release finds no live segment, no
/// spare and an untouched reserve, so it lands in the escrow — which is
/// the tier that needs the floor to exist at all.
///
/// **The `debug-journal` build cannot hold an unregistered thread**, so
/// there the case does not exist rather than failing. The first record
/// site this thread reaches is the one `BlockPool::get` raises inside the
/// lazy draw itself, and a thread's first record runs `ll_thread_init`
/// from within the journal (`journal::mod`, "A thread can reach a record
/// site without ever having initialised the runtime"). That init fills
/// the spare cells, so the entry lands in a segment rather than in the
/// escrow: the thread is registered by the time the enrolment finishes,
/// which is the one thing this test needs it not to be.
#[test]
#[cfg_attr(
    feature = "debug-journal",
    ignore = "the journal registers every thread at its first record site"
)]
fn an_unregistered_thread_draws_its_floor_at_the_first_enrolment() {
    let _g = test_guard();
    let pool = BlockPool::global();
    let before = pool.blocks_out();

    let (had_floor, kind, escrowed, enrolled) = std::thread::spawn(|| {
        assert!(floor().is_null(), "nothing has run on this thread yet");

        let mut header = candidate(2);
        let entity = &raw mut header;
        assert!(unsafe { !release(entity) });

        let drawn = floor();
        (
            !drawn.is_null(),
            kind_of(drawn),
            escrowed_count(),
            enrolled_count(),
        )
    })
    .join()
    .unwrap();

    assert!(had_floor, "the enrolment drew one rather than aborting");
    assert_eq!(kind, BLOCK_KIND_GC_METADATA);
    assert_eq!(escrowed, 1, "every door but the floor refused");
    assert_eq!(enrolled, 0, "so nothing reached the queue itself");
    assert_eq!(
        pool.blocks_out(),
        before,
        "and the exit the draw armed gave the floor back"
    );
}

/// A thread's whole life gives every block back, and the draw that
/// re-enters itself gives back the second one.
///
/// The re-entry is the `debug-journal` build's: `BlockPool::get` raises a
/// record, a thread's first record runs `ll_thread_init` from inside the
/// journal, and that call reaches the floor draw the outer one is still
/// inside. Without the cell being read again after the draw, the outer
/// call writes over the inner call's block and strands it for the life of
/// the process — one per registered thread, in the build turned on to
/// investigate memory.
///
/// **The bound is one-sided, and that is what makes it stable.** A leak
/// can only put the counter above it; the traffic this test does not
/// control only puts it below. Under `debug-journal` the thread retires a
/// ring the registry keeps so a dead thread's records stay readable, which
/// is the `+ 1` (`journal::retire_ring`); registering that ring also frees
/// whatever rings an earlier retirement left pending, and each of those
/// hands a block back inside the bracket (`journal::take_pending`).
#[test]
fn a_threads_whole_life_gives_every_block_back() {
    let _g = test_guard();
    let pool = BlockPool::global();
    let before = pool.blocks_out();

    std::thread::spawn(|| {
        assert!(
            crate::memory::heap::ll_thread_init(),
            "the pool served this thread"
        );
        assert!(!floor().is_null(), "which drew it a floor");
    })
    .join()
    .unwrap();

    let kept = if cfg!(feature = "debug-journal") {
        1
    } else {
        0
    };
    let after = pool.blocks_out();
    assert!(
        after <= before + kept,
        "the thread took a block with it: {after} out against {before} before"
    );
}

/// A thread whose teardown will never run is not funded at all: it is
/// told it did not start, and everything drawn for it goes back.
///
/// The population is a thread reaching the runtime from inside TLS
/// teardown, past the destruction of the exit guard's slot — nothing
/// rebuilds one, so a floor drawn for it would be a block the process
/// never sees again. `FORCE_GUARD_UNARMED` is how that state is entered
/// on demand; it names the guard and nothing else, so the pool, the heap
/// and the reserves answer normally throughout.
#[test]
fn a_thread_nothing_will_tear_down_is_not_funded() {
    let _g = test_guard();
    let pool = BlockPool::global();
    let before = pool.blocks_out();
    let floors_before = stats().current_blocks(GcBlockRole::QueueFloor);
    let segments_before = stats().current_blocks(GcBlockRole::QueueSegment);

    let started = std::thread::spawn(|| {
        crate::memory::heap::FORCE_GUARD_UNARMED.store(true, Ordering::Relaxed);
        let started = crate::memory::heap::ll_thread_init();
        crate::memory::heap::FORCE_GUARD_UNARMED.store(false, Ordering::Relaxed);
        assert!(floor().is_null(), "and it holds no floor");
        started
    })
    .join()
    .unwrap();

    assert!(!started, "the thread reports that it did not start");
    assert!(
        pool.blocks_out() <= before,
        "and left nothing out of the pool"
    );
    assert_eq!(
        stats().current_blocks(GcBlockRole::QueueFloor),
        floors_before
    );
    assert_eq!(
        stats().current_blocks(GcBlockRole::QueueSegment),
        segments_before
    );
}
