//! A pickup is refused wherever the drain's user code could meet a
//! half-torn graph: inside a synchronous collection, which holds
//! guards on members a message may name; on the death branch between
//! the committing zero store and the dispose that nulls the weak
//! cell, where a `WeakRef` would still resolve to the dying entity;
//! and inside the drain itself, where an allocating destructor must
//! be served without starting the next message.

use super::*;

/// The pickup gate refuses messages while a synchronous collection
/// runs (`rfc/model/gc/rc-walk.md`, "When the collector runs",
/// step 4): the collection is drain-class — it holds guards on
/// members an epoch message may name. Checkpoints fire inside
/// `walk::collect_cycles` (a destructor reaching the poll, every
/// teardown exit on the sever path); each must leave the message
/// pending.
#[test]
fn a_synchronous_collection_refuses_message_pickup() {
    let _g = crate::memory::block_pool::test_guard();
    static SEEN_OUTSTANDING_IN_WALK: AtomicUsize = AtomicUsize::new(usize::MAX);

    unsafe extern "C" fn checkpointing_probe(_obj: *mut Object) {
        // A checkpoint fires inside the walk — the shape of a
        // destructor reaching the compiler's poll. The pending
        // message must still be pending afterwards.
        checkpoint();
        SEEN_OUTSTANDING_IN_WALK.store(outstanding_verdicts(), Ordering::Relaxed);
    }

    DESTRUCTS.store(0, Ordering::Relaxed);
    SEEN_OUTSTANDING_IN_WALK.store(usize::MAX, Ordering::Relaxed);
    let live_cls = ClassBuilder::new("WalkGateLiveRing")
        .prop("child", true)
        .destructor(counting_destructor as *const ())
        .build();
    let dying_cls = ClassBuilder::new("WalkGateDyingRing")
        .prop("child", true)
        .destructor(checkpointing_probe as *const ())
        .build();

    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };
    // The posted component stays live through the walk (external
    // reference): only pickup timing is under test, not a verdict.
    let c1 = unsafe { new_constructed(&mut ctx, live_cls, MemoryCategory::GcHeap) };
    let c2 = unsafe { new_constructed(&mut ctx, live_cls, MemoryCategory::GcHeap) };
    let d1 = unsafe { new_constructed(&mut ctx, dying_cls, MemoryCategory::GcHeap) };
    let d2 = unsafe { new_constructed(&mut ctx, dying_cls, MemoryCategory::GcHeap) };
    unsafe {
        tie(c1, 16, c2);
        tie(c2, 16, c1);
        ll_retain(c1 as *mut RcHeader); // keeps the posted ring live
        tie(d1, 16, d2);
        tie(d2, 16, d1);
    }

    post_confirmation(vec![c1 as *mut RcHeader, c2 as *mut RcHeader]);
    // The d-ring is garbage: the walk collects it, running the
    // probing destructors while the walk-active bit is set.
    unsafe { crate::walk::collect_cycles() };

    assert_eq!(
        SEEN_OUTSTANDING_IN_WALK.load(Ordering::Relaxed),
        1,
        "the checkpoint inside the walk left the message pending"
    );
    assert_eq!(outstanding_verdicts(), 1, "still pending after the walk");
    assert_eq!(
        DESTRUCTS.load(Ordering::Relaxed),
        0,
        "the live ring untouched"
    );

    // Outside the walk the pickup proceeds: the live ring fails the
    // exact test and the message is dropped whole.
    checkpoint();
    assert_eq!(outstanding_verdicts(), 0);
    assert_eq!(DESTRUCTS.load(Ordering::Relaxed), 0, "dropped, not torn");
    let seen = walked_addresses();
    assert!(seen.contains(&(c1 as usize)) && seen.contains(&(c2 as usize)));

    // Cleanup: drop the external reference, the ring is garbage.
    assert!(!unsafe { ll_release(c1 as *mut RcHeader) });
    unsafe { crate::walk::collect_cycles() };
    assert_eq!(DESTRUCTS.load(Ordering::Relaxed), 2);
    arena.reset(|_| {});
}

/// Review finding (2026-07-27): between a death's committing zero
/// store and its dispose the entity is committed-dead with a live
/// weak cell. A message picked up at the death-branch checkpoint
/// runs drain destructors — user code — and one holding a
/// `WeakRef` to the dying entity would `get()` a strong reference
/// to it (resurrection after commit, or a double teardown). The
/// death branch acks only; pickup rides the outermost dispose's
/// exit, by which point the cell reads null.
#[test]
fn the_drain_never_sees_an_entity_between_commit_and_dispose() {
    let _g = crate::memory::block_pool::test_guard();
    static RING_DESTRUCTS: AtomicUsize = AtomicUsize::new(0);
    static GOT: AtomicUsize = AtomicUsize::new(usize::MAX);
    static CELL: AtomicUsize = AtomicUsize::new(0);

    unsafe extern "C" fn probing_destructor(_obj: *mut Object) {
        RING_DESTRUCTS.fetch_add(1, Ordering::Relaxed);
        let cell = CELL.load(Ordering::Relaxed) as *mut crate::weak::LLWeakRef;
        if GOT.load(Ordering::Relaxed) == usize::MAX {
            let got = unsafe { crate::weak::ll_weakref_get(cell) };
            GOT.store(got as usize, Ordering::Relaxed);
            if !got.is_null() {
                // Drop the strong reference `get` handed out; on
                // the broken interleaving this very release is the
                // second teardown the fix removes.
                unsafe {
                    if ll_release(got) {
                        crate::object::ll_entity_die(got);
                    }
                }
            }
        }
    }

    RING_DESTRUCTS.store(0, Ordering::Relaxed);
    GOT.store(usize::MAX, Ordering::Relaxed);
    DESTRUCTS.store(0, Ordering::Relaxed);
    let ring_cls = ClassBuilder::new("CommitWindowRing")
        .prop("child", true)
        .destructor(probing_destructor as *const ())
        .build();
    let target_cls = ClassBuilder::new("CommitWindowTarget")
        .destructor(counting_destructor as *const ())
        .build();

    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };
    let r1 = unsafe { new_constructed(&mut ctx, ring_cls, MemoryCategory::GcHeap) };
    let r2 = unsafe { new_constructed(&mut ctx, ring_cls, MemoryCategory::GcHeap) };
    let x = unsafe { new_constructed(&mut ctx, target_cls, MemoryCategory::GcHeap) };
    let x_addr = x as usize;
    unsafe {
        tie(r1, 16, r2);
        tie(r2, 16, r1);
    }

    let cell = unsafe { crate::weak::ll_weakref_create(&mut ctx, x as *mut RcHeader) };
    CELL.store(cell as usize, Ordering::Relaxed);

    // Epoch in flight (identity holds), the ring posted confirmed.
    crate::memory::deferred_free::begin_epoch();
    post_confirmation(vec![r1 as *mut RcHeader, r2 as *mut RcHeader]);

    // X's final release: the death branch must NOT pick the ring
    // up here — X is committed-dead, its cell still live.
    unsafe {
        assert!(ll_release(x as *mut RcHeader));
        crate::object::ll_object_die(x);
        // The dispose's exit picked up and drained the ring.
    }

    assert_eq!(
        outstanding_verdicts(),
        0,
        "the ring drained at the dispose's exit"
    );
    assert_eq!(RING_DESTRUCTS.load(Ordering::Relaxed), 2);
    assert_eq!(
        GOT.load(Ordering::Relaxed),
        0,
        "the drain destructor's get() read a nulled cell, never the corpse"
    );
    assert_eq!(DESTRUCTS.load(Ordering::Relaxed), 1, "X torn exactly once");

    crate::memory::deferred_free::end_epoch();
    checkpoint(); // flush

    // Exactly-once through the free list, as in DC0.
    let a = unsafe { new_constructed(&mut ctx, target_cls, MemoryCategory::GcHeap) };
    let b = unsafe { new_constructed(&mut ctx, target_cls, MemoryCategory::GcHeap) };
    assert_eq!(a as usize, x_addr, "LIFO: X's slot back in circulation");
    assert_ne!(b as usize, x_addr, "and was enqueued exactly once");
    unsafe {
        for &e in &[a, b] {
            assert!(ll_release(e as *mut RcHeader));
            crate::object::ll_object_die(e);
        }

        assert!(ll_release(cell as *mut RcHeader));
        crate::object::ll_entity_die(cell as *mut RcHeader);
    }

    arena.reset(|_| {});
}

/// Finding F8: an allocation inside a draining destructor is a
/// checkpoint inside the drain — it must serve memory and never pick
/// up the next message. The second message drains only after the
/// first completes, in the outer loop.
#[test]
fn the_drain_is_not_reentrant() {
    let _g = crate::memory::block_pool::test_guard();
    static SEEN_OUTSTANDING_INSIDE: AtomicUsize = AtomicUsize::new(usize::MAX);
    static PEER_DESTRUCTS_INSIDE: AtomicUsize = AtomicUsize::new(usize::MAX);

    unsafe extern "C" fn allocating_destructor(_obj: *mut Object) {
        DESTRUCTS.fetch_add(1, Ordering::Relaxed);
        // Memory is served mid-drain (the factory itself carries
        // no checkpoint since the death-branch move, 2026-07-27),
        // and a checkpoint firing here — the compiler's poll — must
        // not pick up the pending second message.
        let p = unsafe { crate::memory::heap::entity_alloc(24) };
        assert!(!p.is_null(), "the nested entry still serves memory");
        unsafe { crate::memory::stdapi::ll_free(p) };
        checkpoint();
        SEEN_OUTSTANDING_INSIDE.store(outstanding_verdicts(), Ordering::Relaxed);
        PEER_DESTRUCTS_INSIDE.store(DESTRUCTS.load(Ordering::Relaxed), Ordering::Relaxed);
    }

    DESTRUCTS.store(0, Ordering::Relaxed);
    let alloc_cls = ClassBuilder::new("EpochNestedAllocator")
        .prop("child", true)
        .destructor(allocating_destructor as *const ())
        .build();
    let plain_cls = ClassBuilder::new("EpochNestedPeer")
        .prop("child", true)
        .destructor(counting_destructor as *const ())
        .build();

    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };
    let mk =
        |ctx: &mut LLContext, cls| unsafe { new_constructed(ctx, cls, MemoryCategory::GcHeap) };
    // Two independent condemned rings, one message each.
    let (a1, a2) = (mk(&mut ctx, alloc_cls), mk(&mut ctx, alloc_cls));
    let (b1, b2) = (mk(&mut ctx, plain_cls), mk(&mut ctx, plain_cls));
    unsafe {
        tie(a1, 16, a2);
        tie(a2, 16, a1);
        tie(b1, 16, b2);
        tie(b2, 16, b1);
    }

    post_confirmation(vec![a1 as *mut RcHeader, a2 as *mut RcHeader]);
    post_confirmation(vec![b1 as *mut RcHeader, b2 as *mut RcHeader]);
    checkpoint();

    // Both messages still count as outstanding inside the first
    // drain: a message acks only when its drain completes, so the
    // nested checkpoint saw 2 — its own, undecremented, plus the
    // second it must not touch.
    assert_eq!(SEEN_OUTSTANDING_INSIDE.load(Ordering::Relaxed), 2);
    assert_eq!(
        PEER_DESTRUCTS_INSIDE.load(Ordering::Relaxed),
        2,
        "only the first ring's destructors had run at that point"
    );
    assert_eq!(
        DESTRUCTS.load(Ordering::Relaxed),
        4,
        "outer loop drained the rest"
    );
    assert_eq!(outstanding_verdicts(), 0);
    let seen = walked_addresses();
    for &e in &[a1, a2, b1, b2] {
        assert!(!seen.contains(&(e as usize)));
    }

    arena.reset(|_| {});
}
