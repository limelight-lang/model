//! What `ll_thread_exit` does when a destructor calls it — from inside a
//! collection, an ordinary teardown or the exit's own sequence: the request
//! is recorded and the exit runs at the thread's top, never under the frames
//! that are still using the heap (`dev/DECISIONS.md`, "an exit requested
//! inside a collection runs at the thread's top").
//!
//! Every case runs on a thread of its own, because the thread it reads about
//! is gone by the end: the heap is null, and the guard's own call at the
//! closure's end finds an exited thread.

use super::*;
use crate::cycle::collect::take_exit_residue;
use crate::memory::heap::{
    exit_sequences, ll_thread_exit, ll_thread_init, thread_entity_heap, thread_exit_pending,
};

/// How many destructors called `ll_thread_exit`, and whether the heap was
/// still there when the call returned to them.
static EXIT_REQUESTS: AtomicUsize = AtomicUsize::new(0);
static HEAP_ALIVE_AFTER_THE_CALL: AtomicUsize = AtomicUsize::new(0);

/// A destructor that asks for the thread's exit, which is what user code
/// inside a collection can do, and reads whether the heap survived the call.
unsafe extern "C" fn exit_requesting_destructor(_object: *mut Object) {
    EXIT_REQUESTS.fetch_add(1, Ordering::Relaxed);
    ll_thread_exit();
    HEAP_ALIVE_AFTER_THE_CALL.fetch_add(
        usize::from(!thread_entity_heap().is_null()),
        Ordering::Relaxed,
    );
}

/// What a case reads on its thread after the request and after the exit it
/// then makes at the top.
struct Readings {
    heap_alive_after_the_collection: bool,
    pending_after_the_collection: bool,
    residue_after_the_collection: bool,
    heap_alive_after_the_top: bool,
    pending_after_the_top: bool,
    residue_after_the_top: bool,
}

/// Read the thread's state, run the exit as the thread's top would, and read
/// it again.
fn exit_at_the_top() -> Readings {
    let heap_alive_after_the_collection = !thread_entity_heap().is_null();
    let pending_after_the_collection = thread_exit_pending();
    let residue_after_the_collection = take_exit_residue().is_some();
    ll_thread_exit();
    Readings {
        heap_alive_after_the_collection,
        pending_after_the_collection,
        residue_after_the_collection,
        heap_alive_after_the_top: !thread_entity_heap().is_null(),
        pending_after_the_top: thread_exit_pending(),
        residue_after_the_top: take_exit_residue().is_some(),
    }
}

/// The readings every deferred request shares: the heap survives the
/// collection with the request pending and no exit run, and the call at the
/// top runs it.
fn a_request_waited_for_the_top(readings: &Readings) {
    assert!(
        readings.heap_alive_after_the_collection,
        "the heap is still there when the collection returns"
    );
    assert!(
        readings.pending_after_the_collection,
        "with the request pending"
    );
    assert!(
        !readings.residue_after_the_collection,
        "and no exit sequence has run"
    );
    assert!(
        !readings.heap_alive_after_the_top,
        "the call at the top took the heap"
    );
    assert!(!readings.pending_after_the_top, "and cleared the request");
    assert!(
        readings.residue_after_the_top,
        "having run the exit's collection"
    );
}

/// A request from a destructor of a collection off the poll: the collection
/// finishes on a live heap, the request waits, and the call at the top runs
/// the exit.
#[test]
fn a_request_from_a_poll_collections_destructor_waits_for_the_top() {
    let _g = test_guard();
    EXIT_REQUESTS.store(0, Ordering::Relaxed);
    HEAP_ALIVE_AFTER_THE_CALL.store(0, Ordering::Relaxed);

    let (freed, readings) = std::thread::spawn(|| {
        assert!(ll_thread_init(), "the pool served this thread");
        let class = node_class(
            "ExitRequestingNode",
            exit_requesting_destructor as *const (),
        );
        let mut arena = Arena::new();
        let _members = unsafe { ring(&mut arena, [class, class]) };
        drop(arena);

        let freed = unsafe { ll_gc_collect_cycles() };
        (freed, exit_at_the_top())
    })
    .join()
    .unwrap();

    assert_eq!(freed, 2, "the collection tore its ring down");
    assert_eq!(
        EXIT_REQUESTS.load(Ordering::Relaxed),
        2,
        "both destructors asked"
    );
    assert_eq!(
        HEAP_ALIVE_AFTER_THE_CALL.load(Ordering::Relaxed),
        2,
        "and each returned to a live heap"
    );
    a_request_waited_for_the_top(&readings);
}

/// A request from a destructor of an ordinary teardown — no collection, the
/// dying object at count zero with its cells still populated — waits the
/// same way: the teardown's frame goes on to free the slot into a live heap.
#[test]
fn a_request_from_an_ordinary_teardowns_destructor_waits_for_the_top() {
    let _g = test_guard();
    EXIT_REQUESTS.store(0, Ordering::Relaxed);
    HEAP_ALIVE_AFTER_THE_CALL.store(0, Ordering::Relaxed);

    let readings = std::thread::spawn(|| {
        assert!(ll_thread_init(), "the pool served this thread");
        let dying = node_class(
            "ExitRequestingDyingNode",
            exit_requesting_destructor as *const (),
        );
        let mut arena = Arena::new();
        let mut context = LLContext { arena: &mut arena };
        let object = unsafe { new_constructed(&mut context, dying, MemoryCategory::GcHeap) };
        assert!(unsafe { ll_release(object as *mut RcHeader) });
        unsafe { ll_object_die(object) };
        drop(arena);
        exit_at_the_top()
    })
    .join()
    .unwrap();

    assert_eq!(EXIT_REQUESTS.load(Ordering::Relaxed), 1);
    assert_eq!(HEAP_ALIVE_AFTER_THE_CALL.load(Ordering::Relaxed), 1);
    a_request_waited_for_the_top(&readings);
}

/// A request from a destructor of the exit's own collection is absorbed: the
/// exit in progress ends the thread, once.
#[test]
fn a_request_from_the_exits_own_collection_is_absorbed() {
    let _g = test_guard();
    EXIT_REQUESTS.store(0, Ordering::Relaxed);
    HEAP_ALIVE_AFTER_THE_CALL.store(0, Ordering::Relaxed);

    let (heap_gone, residue, sequences, pending) = std::thread::spawn(|| {
        assert!(ll_thread_init(), "the pool served this thread");
        let class = node_class(
            "ExitRequestingAtExitNode",
            exit_requesting_destructor as *const (),
        );
        let mut arena = Arena::new();
        let _members = unsafe { ring(&mut arena, [class, class]) };
        drop(arena);

        ll_thread_exit();
        (
            thread_entity_heap().is_null(),
            take_exit_residue(),
            exit_sequences(),
            thread_exit_pending(),
        )
    })
    .join()
    .unwrap();

    assert_eq!(EXIT_REQUESTS.load(Ordering::Relaxed), 2);
    assert_eq!(
        HEAP_ALIVE_AFTER_THE_CALL.load(Ordering::Relaxed),
        2,
        "the request returned to each destructor with the heap still there"
    );
    assert!(heap_gone);
    assert_eq!(
        sequences, 1,
        "the requests started no second sequence under the first"
    );
    assert!(!pending, "and left no request standing");
    let residue = residue.expect("the exit in progress ran its collection");
    assert_eq!(residue.freed, 2, "and that collection took the ring, once");
}
