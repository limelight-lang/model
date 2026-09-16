//! The blocks the collector reads before its claim are held for the reading:
//! an owner that exits between the take and the loads leaves R's blocks and
//! P's block in its record, the hand-back returns them, and the registry
//! hands the record out only after that.
//!
//! The collector is this thread, calling [`serve`] on the record of an
//! owner thread the case spawns; the case's act runs on this thread inside
//! the reading, between the take and the loads.

use super::*;
use crate::class::ClassBuilder;
use crate::cycle::testing::Sent;
use crate::memory::block_pool::BlockPool;
use crate::memory::context::LLContext;
use crate::object::new_constructed;
use crate::refcount::{MemoryCategory, RcHeader, ll_release, ll_retain};
use std::sync::mpsc;

/// An owner on a thread of its own, with one registered candidate so that R
/// holds a block: its record, the channel that lets it exit, and the
/// channel it reports its exit on.
struct ExitingOwner {
    record: *mut OwnerRecord,
    go: mpsc::Sender<()>,
    exited: mpsc::Receiver<()>,
    thread: std::thread::JoinHandle<()>,
}

fn owner_waiting_to_exit() -> ExitingOwner {
    let (go, wait) = mpsc::channel::<()>();
    let (report, exited) = mpsc::channel::<()>();
    let (tell, record) = mpsc::channel::<Sent<*mut OwnerRecord>>();
    let thread = std::thread::spawn(move || {
        assert!(
            crate::memory::heap::ll_thread_init(),
            "the pool served the owner thread"
        );
        let record = owner_record::this_thread_record();
        owner_record::pin_for_test(record, true);
        // A live object at count two, released once: registered, and read
        // live by the exit's rounds, so R keeps a block through the exit.
        let mut arena = crate::memory::arena::Arena::new();
        let mut context = LLContext { arena: &mut arena };
        let class = ClassBuilder::new("exiting_owner_root").build();
        let root = unsafe { new_constructed(&mut context, class, MemoryCategory::GcHeap) }
            as *mut RcHeader;
        unsafe {
            ll_retain(root);
            assert!(!ll_release(root), "registered at the non-final decrement");
        }
        tell.send(Sent(record)).expect("the case listens");
        let _ = wait.recv();
        // Before the exit, so that nothing of the owner's returns memory
        // beside the case's reading of the pool.
        drop(context);
        drop(arena);
        crate::memory::heap::ll_thread_exit();
        report.send(()).expect("the case listens");
    });
    let record = record
        .recv()
        .expect("the owner reported its record")
        .into_inner();
    ExitingOwner {
        record,
        go,
        exited,
        thread,
    }
}

/// Blocks R and P hold in `record`, read by a thread that holds them.
fn blocks_left_in(record: *mut OwnerRecord) -> usize {
    let r = unsafe { crate::ring::Quiescent::new((*record).candidate_ring()) }.block_count();
    let p = unsafe { crate::ring::Quiescent::new((*record).verdict_ring()) }.block_count();
    r + p
}

/// One serve of `record` on this thread whose reading runs `act` between
/// the take and the loads, with the rounds confined to the record.
fn served_with_an_act_inside_the_reading(
    record: *mut OwnerRecord,
    act: Box<dyn FnOnce() + Send>,
) -> Served {
    testing::at_the_next_reading(act);
    testing::confine_rounds_to(record);
    let served = unsafe { serve(record, ANY_ENTRY) };
    testing::confine_rounds_to(std::ptr::null_mut());
    served
}

#[test]
fn an_owner_exiting_under_the_reading_leaves_its_blocks_to_the_hand_back() {
    let _g = test_guard();
    let ExitingOwner {
        record,
        go,
        exited,
        thread,
    } = owner_waiting_to_exit();
    assert!(blocks_left_in(record) >= 2, "R holds a block and P its one");

    // Inside the reading, on this thread: the owner exits to the end while
    // the hold stands, and its rings' blocks are still out.
    let (tell, left) = mpsc::channel::<(usize, usize)>();
    let sent = Sent(record);
    let served = served_with_an_act_inside_the_reading(
        record,
        Box::new(move || {
            let record = sent.into_inner();
            go.send(()).expect("the owner waits");
            exited.recv().expect("the owner exited");
            // At capacity, so that the hand-back's returns reach the pool
            // rather than this thread's reserve.
            assert!(crate::memory::critical::replenish());
            let blocks = blocks_left_in(record);
            assert!(blocks >= 2, "the exit left R and P to the hold");
            tell.send((blocks, BlockPool::global().blocks_out()))
                .expect("the case listens");
        }),
    );
    let (blocks, out_before_hand_back) = left.recv().expect("the act ran inside the reading");
    assert!(
        matches!(served, Served::Idle | Served::TokenHeld),
        "nothing to serve of an exited owner"
    );
    assert_eq!(
        BlockPool::global().blocks_out(),
        out_before_hand_back - blocks,
        "the hand-back returned every block the exit left, once"
    );
    assert_eq!(blocks_left_in(record), 0, "and the record names none");
    assert!(owner_record::registry_lists_free(record));
    thread.join().expect("the owner finished");
    owner_record::pin_for_test(record, false);
}

/// A thread that asks the registry for `record` by name, and answers whether
/// it got it.
fn a_thread_asking_for(record: *mut OwnerRecord) -> bool {
    let sent = Sent(record);
    std::thread::spawn(move || {
        let wanted = sent.into_inner();
        owner_record::take_this_record_for_test(wanted);
        assert!(
            crate::memory::heap::ll_thread_init(),
            "the pool served the asking thread"
        );
        let got = owner_record::this_thread_record() == wanted;
        if got {
            assert!(
                owner_record::lines_are_fresh(wanted),
                "a re-taken record starts fresh, P's block installed"
            );
        }
        crate::memory::heap::ll_thread_exit();
        got
    })
    .join()
    .expect("the asking thread finished")
}

#[test]
fn the_registry_hands_the_record_out_only_after_the_hand_back() {
    let _g = test_guard();
    let ExitingOwner {
        record,
        go,
        exited,
        thread,
    } = owner_waiting_to_exit();

    let (tell, asked) = mpsc::channel::<bool>();
    let sent = Sent(record);
    let served = served_with_an_act_inside_the_reading(
        record,
        Box::new(move || {
            let record = sent.into_inner();
            go.send(()).expect("the owner waits");
            exited.recv().expect("the owner exited");
            assert!(
                owner_record::registry_lists_free(record),
                "the exit gave it back"
            );
            // Named, pinned or not: the blocks left under the hold are what
            // refuse it.
            tell.send(a_thread_asking_for(record))
                .expect("the case listens");
        }),
    );
    assert!(
        matches!(served, Served::Idle | Served::TokenHeld),
        "nothing to serve of an exited owner"
    );
    assert!(
        !asked.recv().expect("the act ran inside the reading"),
        "a record whose blocks a hold has is not handed out"
    );
    thread.join().expect("the owner finished");
    assert!(
        a_thread_asking_for(record),
        "after the hand-back the same record is handed out, fresh"
    );
    owner_record::pin_for_test(record, false);
}

/// The exit's decision and the reading's take are ordered whichever comes
/// first: a reading that takes the blocks after the exit has read the hold
/// word clear, and before it has stored, still gets them left — the exit's
/// store fails against the take and is made again as a leave.
#[test]
fn a_take_between_the_exits_load_and_its_store_is_still_left_the_blocks() {
    let _g = test_guard();
    let ExitingOwner {
        record,
        go,
        exited,
        thread,
    } = owner_waiting_to_exit();

    // The exit waits between its load and its store until the reading has
    // taken; the reading waits after its take until the exit has run to the
    // end.
    let (loaded_tx, loaded) = mpsc::channel::<()>();
    let (taken_tx, taken) = mpsc::channel::<()>();
    owner_record::at_the_next_exits_hold_check(Box::new(move || {
        loaded_tx.send(()).expect("the case listens");
        taken.recv().expect("the reading took");
    }));
    go.send(()).expect("the owner waits");
    loaded.recv().expect("the exit reached its check");

    let (tell, left) = mpsc::channel::<(usize, usize)>();
    let sent = Sent(record);
    let served = served_with_an_act_inside_the_reading(
        record,
        Box::new(move || {
            let record = sent.into_inner();
            taken_tx.send(()).expect("the exit waits");
            exited.recv().expect("the owner exited");
            assert!(crate::memory::critical::replenish());
            let blocks = blocks_left_in(record);
            assert!(
                blocks >= 2,
                "the exit left R and P to the hold it found on its store"
            );
            tell.send((blocks, BlockPool::global().blocks_out()))
                .expect("the case listens");
        }),
    );
    let (blocks, out_before_hand_back) = left.recv().expect("the act ran inside the reading");
    assert!(matches!(served, Served::Idle | Served::TokenHeld));
    assert_eq!(
        BlockPool::global().blocks_out(),
        out_before_hand_back - blocks,
        "the hand-back returned what the exit left, once"
    );
    assert_eq!(blocks_left_in(record), 0);
    thread.join().expect("the owner finished");
    owner_record::pin_for_test(record, false);
}
