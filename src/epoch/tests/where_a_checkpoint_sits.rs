//! The checkpoint rides the death branch of a release, so a
//! non-final release carries no test at all. A batched run splits it
//! around the run — the ack at entry, the pickup after — because a
//! pickup before the run judges against transients the run itself is
//! about to release, and a loop whose only checkpoints are scope
//! exits would present every pickup with the same held reference.

use super::*;

/// The checkpoint rides the death branch of `ll_release` (decision
/// 2026-07-27): a non-final release carries no test, the `1 → 0`
/// release acks a pending handshake.
#[test]
fn a_release_hitting_zero_is_a_checkpoint_and_a_non_final_one_is_not() {
    let _g = crate::memory::block_pool::test_guard();
    let cls = ClassBuilder::new("DeathCheckpoint").build();
    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };
    let obj = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
    unsafe { ll_retain(obj as *mut RcHeader) }; // rc 2

    let before = handshake_acks();
    request_handshake();
    assert!(!unsafe { ll_release(obj as *mut RcHeader) }); // rc 2 → 1
    assert_eq!(handshake_acks(), before, "non-final release: no checkpoint");

    assert!(unsafe { ll_release(obj as *mut RcHeader) }); // rc 1 → 0
    assert_eq!(handshake_acks(), before + 1, "the death branch acks");
    unsafe { crate::object::ll_object_die(obj) };
}

/// The batched contract splits the checkpoint around the run
/// (`rfc/model/gc/rc-walk.md`, "Batched releases", amendment
/// 2026-07-28): `ll_gc_checkpoint_ack` fronts the run — ack only,
/// never a pickup — `ll_release_batch` carries no test, and the
/// trailing `ll_gc_checkpoint` picks up. Pinned on a death-free
/// run: those are exactly the runs where a pre-run pickup would
/// judge against transients the run is about to return
/// (the phase-lock shape).
#[test]
fn a_batched_run_acks_at_entry_and_picks_up_after_it() {
    let _g = crate::memory::block_pool::test_guard();
    DESTRUCTS.store(0, Ordering::Relaxed);
    let ring_cls = ClassBuilder::new("BatchedRunRing")
        .prop("child", true)
        .destructor(counting_destructor as *const ())
        .build();
    let cls = ClassBuilder::new("BatchedRelease").build();
    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };
    let a = unsafe { new_constructed(&mut ctx, ring_cls, MemoryCategory::GcHeap) };
    let b = unsafe { new_constructed(&mut ctx, ring_cls, MemoryCategory::GcHeap) };
    unsafe {
        tie(a, 16, b);
        tie(b, 16, a);
    }

    let obj = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
    unsafe { ll_retain(obj as *mut RcHeader) }; // rc 2: the run's release is non-final

    post_confirmation(vec![a as *mut RcHeader, b as *mut RcHeader]);
    let before = handshake_acks();
    request_handshake();

    unsafe { crate::gc::ll_gc_checkpoint_ack() };
    assert_eq!(handshake_acks(), before + 1, "the front acks");
    assert_eq!(
        outstanding_verdicts(),
        1,
        "ack only: no pickup before the run"
    );

    assert!(!unsafe { crate::refcount::ll_release_batch(obj as *mut RcHeader) });
    assert_eq!(outstanding_verdicts(), 1, "the run itself never picks up");

    unsafe { crate::gc::ll_gc_checkpoint() };
    assert_eq!(outstanding_verdicts(), 0, "the trailing call picks up");
    assert_eq!(
        DESTRUCTS.load(Ordering::Relaxed),
        2,
        "the posted ring drained"
    );

    unsafe {
        assert!(ll_release(obj as *mut RcHeader));
        crate::object::ll_object_die(obj);
    }

    arena.reset(|_| {});
}

/// `ll_release_vector` acks once at entry — before any death, one
/// ack for the whole batch — and runs the destructors in vector
/// order (`rfc/model/memory/bulk-operations.md`). The entry
/// position is pinned from inside the first destructor: it runs
/// before any teardown-exit checkpoint could ack in entry's stead.
#[test]
fn a_vector_release_acks_once_and_dies_in_order() {
    use std::sync::Mutex;
    static ORDER: Mutex<Vec<usize>> = Mutex::new(Vec::new());
    static ACKS_AT_FIRST_DEATH: AtomicUsize = AtomicUsize::new(usize::MAX);
    unsafe extern "C" fn recording(obj: *mut Object) {
        if ORDER.lock().unwrap().is_empty() {
            ACKS_AT_FIRST_DEATH.store(handshake_acks() as usize, Ordering::Relaxed);
        }

        ORDER.lock().unwrap().push(obj as usize);
    }

    let _g = crate::memory::block_pool::test_guard();
    ORDER.lock().unwrap().clear();
    ACKS_AT_FIRST_DEATH.store(usize::MAX, Ordering::Relaxed);
    let cls = ClassBuilder::new("VectorRelease")
        .destructor(recording as *const ())
        .build();
    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };
    let objects: Vec<*mut RcHeader> = (0..3)
        .map(|_| unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) as *mut RcHeader })
        .collect();

    let before = handshake_acks();
    request_handshake();
    unsafe { crate::object::ll_release_vector(objects.as_ptr(), objects.len()) };
    assert_eq!(handshake_acks(), before + 1, "one ack for the whole vector");
    assert_eq!(
        ACKS_AT_FIRST_DEATH.load(Ordering::Relaxed),
        (before + 1) as usize,
        "the ack preceded the first death"
    );

    let order = ORDER.lock().unwrap();
    let expected: Vec<usize> = objects.iter().map(|&p| p as usize).collect();
    assert_eq!(*order, expected, "destructors in vector order");
}

/// The vector pickup trails the run (amendment 2026-07-28). The
/// phase-lock shape: a component is posted while the vector still
/// holds the reference keeping it alive. A pre-run pickup judges
/// against that transient — exact-test mismatch, message dropped,
/// garbage survives; and a loop whose only checkpoints are scope
/// exits presents *every* pickup with the same held borrow. The
/// trailing pickup judges after the release and collects.
#[test]
fn a_vector_release_picks_up_after_the_run_not_before() {
    let _g = crate::memory::block_pool::test_guard();
    DESTRUCTS.store(0, Ordering::Relaxed);
    let cls = ClassBuilder::new("VectorPhaseLock")
        .prop("child", true)
        .destructor(counting_destructor as *const ())
        .build();
    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };
    let a = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
    let b = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
    unsafe {
        tie(a, 16, b);
        tie(b, 16, a);
        ll_retain(a as *mut RcHeader); // the vector's transient
    }

    post_confirmation(vec![a as *mut RcHeader, b as *mut RcHeader]);
    let transients = [a as *mut RcHeader];
    unsafe { crate::object::ll_release_vector(transients.as_ptr(), transients.len()) };

    assert_eq!(
        outstanding_verdicts(),
        0,
        "the trailing pickup served the message"
    );
    assert_eq!(
        DESTRUCTS.load(Ordering::Relaxed),
        2,
        "judged after the release: collected"
    );
    let seen = walked_addresses();
    assert!(!seen.contains(&(a as usize)) && !seen.contains(&(b as usize)));
    arena.reset(|_| {});
}

#[test]
fn a_requested_handshake_is_acked_at_the_next_checkpoint() {
    let _g = crate::memory::block_pool::test_guard();
    let before = handshake_acks();
    checkpoint(); // nothing requested: no ack
    assert_eq!(handshake_acks(), before);

    request_handshake();
    checkpoint();
    assert_eq!(handshake_acks(), before + 1, "one callback, one ack");
    checkpoint(); // flag was consumed: no second ack
    assert_eq!(handshake_acks(), before + 1);
}
