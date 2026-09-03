//! The working memory a thread draws at its first collection and keeps until
//! it exits, held to the two claims that are the whole reason for keeping it:
//! a second collection asks the memory manager for nothing the first one
//! already took, and what it holds between the two is one manager block,
//! counted as collection's and given back at thread exit.
//!
//! **Every case about a first collection runs on a thread of its own.** The
//! claim is about a thread's *first* collection, and every other thread in this
//! suite has collected already — such a case run on the harness's thread would
//! be measuring a workspace somebody else drew. The last case is about a live
//! workspace rather than a first one and runs where the harness is.
//!
//! The instrument is the pool-request counter rather than the pool's
//! `blocks_out`: the count is per thread, so the parent's traffic cannot
//! reach it, and it counts a request the thread cache serves, which is the
//! request this step exists to remove
//! (`crate::test_support::allocation_probe`).

use super::*;

use crate::cycle::deferred_slot_reuse::ActiveTrace;
use crate::memory::block_pool::{BLOCK_KIND_GC_METADATA, load_block_kind};
use crate::memory::gc_metadata::stats;
use crate::test_support::allocation_probe;

/// One collection over one entity: open the window, meet the entity, close.
///
/// The meeting is what the claim needs. A workspace drawn at the open and
/// then grown past at the first row would cost exactly what no workspace
/// costs, and only an arena that serves a row out of it says otherwise.
fn one_collection(block: *mut u8) {
    let mut trace = ActiveTrace::open().expect("the pool funded the trace window");
    met(unsafe { trace.arena().ensure_row(slot_row(block, 0), 1) });
}

/// What a second collection costs, which is the step's whole claim: the
/// window's own chain and nothing else, the working memory being memory the
/// thread already holds.
///
/// The first collection is bracketed too, and it is the control: two requests
/// there and one here is the difference between drawing a workspace and
/// finding one.
#[test]
fn a_second_collection_on_the_same_thread_draws_no_workspace() {
    let _g = test_guard();

    let (first, second) = std::thread::spawn(|| {
        assert!(
            crate::memory::heap::ll_thread_init(),
            "the pool served this thread"
        );
        let (mut heap, slot, block) = an_entity_block();

        // Bracketed after the fixture: the heap's own blocks are this
        // thread's entity memory and say nothing about the collection.
        let _ = allocation_probe::take_allocations();
        one_collection(block);
        let first = allocation_probe::take_allocations();

        one_collection(block);
        let second = allocation_probe::take_allocations();

        unsafe { heap.free(slot) };
        (first, second)
    })
    .join()
    .unwrap();

    assert_eq!(
        first,
        (0, 2),
        "the first collection drew the workspace and the window's chain"
    );
    assert_eq!(
        second,
        (0, 1),
        "and the second drew the chain alone, the workspace being resident"
    );
}

/// The other half of the same claim, read from the manager rather than from
/// the thread: between two collections the thread stands one block above
/// where it began, and thread exit puts that block back.
///
/// One block and not two — the window's chain goes back at every close, so a
/// figure that stayed up by two would mean a workspace per collection.
#[test]
fn the_workspace_stands_between_collections_and_goes_back_at_exit() {
    let _g = test_guard();
    let outside = stats().current_blocks();

    let (before_collecting, after_first, after_second) = std::thread::spawn(|| {
        assert!(
            crate::memory::heap::ll_thread_init(),
            "the pool served this thread"
        );
        let (mut heap, slot, block) = an_entity_block();

        // The thread's queue base and its spare segments are collection's
        // memory too, and they are drawn at init: the figure this case reads
        // is a difference from them, not from zero.
        let before_collecting = stats().current_blocks();
        one_collection(block);
        let after_first = stats().current_blocks();
        one_collection(block);
        let after_second = stats().current_blocks();

        unsafe { heap.free(slot) };
        (before_collecting, after_first, after_second)
    })
    .join()
    .unwrap();

    assert_eq!(
        after_first,
        before_collecting + 1,
        "the collection kept one block of working memory"
    );
    assert_eq!(
        after_second, after_first,
        "and the second collection kept the same one"
    );
    assert_eq!(
        stats().current_blocks(),
        outside,
        "which the thread's exit gave back"
    );
}

/// Whose memory the workspace is, read from the manager rather than from the
/// arena: a block drawn through `gc_metadata` and stamped as collection's, so
/// the ledger can answer how much memory collection holds. The case also
/// carries the instant of the draw — a thread that has not collected holds
/// none, which is what "at the first collection, not at thread init" means
/// from outside.
#[test]
fn the_workspace_is_a_stamped_manager_block_drawn_by_the_first_collection() {
    let _g = test_guard();

    let kind = std::thread::spawn(|| {
        assert!(
            crate::memory::heap::ll_thread_init(),
            "the pool served this thread"
        );
        assert!(
            crate::cycle::queue::workspace_base().is_null(),
            "an initialised thread that has not collected holds no workspace"
        );

        let (mut heap, slot, block) = an_entity_block();
        one_collection(block);

        let base = crate::cycle::queue::workspace_base();
        assert!(!base.is_null(), "and the collection drew one");
        let kind = unsafe { load_block_kind(&raw const (*base).kind) };

        unsafe { heap.free(slot) };
        kind
    })
    .join()
    .unwrap();

    assert_eq!(
        kind, BLOCK_KIND_GC_METADATA,
        "the workspace is collection's memory to the manager"
    );
}

/// The refusal. One allocation path funds the workspace and it can say no,
/// and then the collection does not start: no window is open, no root has been
/// taken, and the thread holds nothing an abort would have to give back.
#[test]
fn a_refused_workspace_is_a_collection_that_does_not_start() {
    let _g = test_guard();

    let (opened, empty_handed) = std::thread::spawn(|| {
        assert!(
            crate::memory::heap::ll_thread_init(),
            "the pool served this thread"
        );

        let oom = force_oom();
        let opened = ActiveTrace::open().is_some();
        drop(oom);

        (opened, crate::cycle::queue::workspace_base().is_null())
    })
    .join()
    .unwrap();

    assert!(!opened, "the collection did not start");
    assert!(empty_handed, "and the thread holds no workspace");
}

/// Two arenas over one workspace would grant the same bytes twice, so the
/// second is refused where it asks rather than where the rows collide. The
/// test profile unwinds the assertion, which is what makes the case runnable;
/// the release profile ends the process on it.
#[test]
#[should_panic(expected = "a thread bumps one collection workspace at a time")]
fn a_second_arena_over_a_live_one_is_refused() {
    let _g = test_guard();
    let _first = crate::cycle::testing::open_arena();
    let _second = TraceScratchArena::open();
}
